//! Live rust-analyzer instance lifecycle (SPEC-A2 §2, §7).
//!
//! `LiveInstance::spawn(worktree_root)` starts one rust-analyzer child rooted
//! at exactly ONE worktree, runs the LSP handshake, and tracks warm-up from
//! `experimental/serverStatus`: the instance is **Warming** until the server
//! reports quiescent, then **Ready**. Warming instances answer nothing —
//! `request()` returns `LiveError::Warming { elapsed_secs }` immediately so
//! the daemon never queues a query behind a prime (SPEC-A2 §3).
//!
//! A background reader thread routes responses by request id and applies
//! serverStatus transitions; everything is std threads + blocking IO.

use crate::live::lsp;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub const RUST_ANALYZER_BIN: &str = "rust-analyzer";

/// Per-request answer deadline once Ready (rust-analyzer answers in ms on a
/// primed workspace; 30s means something is deeply wrong, not slow).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The initialize response arrives before indexing starts; generous anyway.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// Grace given to `shutdown` acknowledgement AND to process exit after
/// `exit`, each, before escalating to SIGKILL (plan T2: 2s wait → SIGKILL).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

pub type LiveResult<T> = std::result::Result<T, LiveError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    /// Spawned, handshake done, rust-analyzer not yet quiescent.
    Warming,
    /// Quiescent — answers queries.
    Ready,
    /// Process exited, stream broke, or instance was shut down.
    Dead,
}

impl std::fmt::Display for InstanceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            InstanceState::Warming => "warming",
            InstanceState::Ready => "ready",
            InstanceState::Dead => "dead",
        })
    }
}

#[derive(Debug)]
pub enum LiveError {
    /// Not quiescent yet. The daemon maps this to the immediate
    /// `{ok:false, warming:{elapsed_secs}}` reply (SPEC-A2 §3).
    Warming { elapsed_secs: u64 },
    Dead { reason: String },
    /// JSON-RPC error from the server.
    Server { code: i64, message: String },
    /// Broken pipe / framing failure while talking to the child.
    Transport(String),
    Timeout { method: String, after: Duration },
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiveError::Warming { elapsed_secs } => write!(
                f,
                "instance warming ({elapsed_secs}s elapsed); answering nothing until quiescent"
            ),
            LiveError::Dead { reason } => write!(f, "instance dead: {reason}"),
            LiveError::Server { code, message } => {
                write!(f, "LSP server error {code}: {message}")
            }
            LiveError::Transport(msg) => write!(f, "LSP transport failure: {msg}"),
            LiveError::Timeout { method, after } => {
                write!(f, "no response to {method} within {after:?}")
            }
        }
    }
}

impl std::error::Error for LiveError {}

/// The synchronous surface the pool (Task 3) manages. Spawning is NOT part of
/// the trait — `LiveInstance::spawn` is an associated fn; the pool takes a
/// spawn closure so its tests substitute a fake without paying a real
/// rust-analyzer prime.
pub trait LiveBackend: Send {
    fn state(&self) -> InstanceState;
    /// Raw LSP request. Warming/Dead instances answer nothing but an error —
    /// never blocks on a prime.
    fn request(&self, method: &str, params: Value) -> LiveResult<Value>;
    /// Graceful shutdown: shutdown → exit → grace wait → SIGKILL, then reap.
    /// Idempotent; failures are logged loudly, never swallowed silently.
    fn shutdown(&mut self);
    /// Resident set size in MB via `ps -o rss=`; `None` if unmeasurable.
    /// NOT the watchdog metric — `ps` RSS under-reports idle rust-analyzer
    /// by >50x on macOS (SPEC-A2 §4 "Memory metric").
    fn rss_mb(&self) -> Option<u64>;
    /// Physical footprint in MB — THE watchdog metric (SPEC-A2 §4): tries
    /// `footprint -p <pid>`, falls back to `ps` RSS with a once-logged
    /// under-reporting caveat. `None` if both are unmeasurable.
    fn footprint_mb(&self) -> Option<u64>;
    /// Last time `request()` was attempted past the state gate (LRU input).
    fn last_used(&self) -> Instant;
}

/// Reader-thread-shared state: lifecycle + pending response routing.
#[derive(Debug)]
struct Inner {
    state: InstanceState,
    /// When the current warming period began (spawn, or a Ready→Warming
    /// re-index transition). Source of `Warming::elapsed_secs`.
    warming_since: Instant,
    dead_reason: Option<String>,
    pending: HashMap<i64, mpsc::Sender<LiveResult<Value>>>,
}

#[derive(Debug)]
pub struct LiveInstance {
    child: Child,
    pid: u32,
    worktree_root: PathBuf,
    stdin: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Inner>>,
    next_id: AtomicI64,
    last_used: Mutex<Instant>,
    shut_down: bool,
}

impl LiveInstance {
    /// Spawn rust-analyzer (resolved from PATH) rooted at `worktree_root` and
    /// complete the LSP handshake. Returns while still Warming.
    pub fn spawn(worktree_root: &Path) -> Result<Self> {
        Self::spawn_with_binary(RUST_ANALYZER_BIN, worktree_root)
    }

    /// Test seam: spawn an arbitrary LSP-speaking binary as the server.
    pub fn spawn_with_binary(binary: &str, worktree_root: &Path) -> Result<Self> {
        let root = worktree_root.canonicalize().with_context(|| {
            format!("canonicalizing worktree root {}", worktree_root.display())
        })?;
        let mut child = Command::new(binary)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr inherited: rust-analyzer's own logging flows to the
            // daemon's stderr log — visible, and never a blocked pipe.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::anyhow!(
                        "`{binary}` not found on PATH — install rust-analyzer (e.g. `brew \
                         install rust-analyzer`) and make sure /opt/homebrew/bin is on the \
                         daemon's PATH"
                    )
                } else {
                    anyhow::Error::from(e).context(format!("spawning {binary}"))
                }
            })?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("child stdout is piped");
        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("child stdin is piped")));
        let shared = Arc::new(Mutex::new(Inner {
            state: InstanceState::Warming,
            warming_since: Instant::now(),
            dead_reason: None,
            pending: HashMap::new(),
        }));
        {
            let shared = Arc::clone(&shared);
            let stdin = Arc::clone(&stdin);
            std::thread::Builder::new()
                .name(format!("ra-reader-{pid}"))
                .spawn(move || reader_loop(stdout, shared, stdin, pid))
                .context("spawning LSP reader thread")?;
        }
        let instance = LiveInstance {
            child,
            pid,
            worktree_root: root.clone(),
            stdin,
            shared,
            next_id: AtomicI64::new(1),
            last_used: Mutex::new(Instant::now()),
            shut_down: false,
        };
        eprintln!(
            "live: pid {pid} spawned ({binary}) rooted at {} — warming",
            root.display()
        );
        // Handshake. Failure is loud and fatal; Drop reaps the child.
        let params = lsp::InitializeParams::new(std::process::id(), &root);
        instance
            .raw_request(
                lsp::methods::INITIALIZE,
                serde_json::to_value(params).context("serializing initialize params")?,
                INITIALIZE_TIMEOUT,
            )
            .map_err(|e| anyhow::anyhow!("initialize handshake with {binary} (pid {pid}) failed: {e}"))?;
        instance
            .notify(lsp::methods::INITIALIZED, serde_json::json!({}))
            .map_err(|e| anyhow::anyhow!("sending initialized to pid {pid}: {e}"))?;
        Ok(instance)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    /// Send a request and block for its response — no state gate (used by the
    /// handshake and shutdown, which must run while Warming/Dead).
    fn raw_request(&self, method: &str, params: Value, timeout: Duration) -> LiveResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.shared.lock().unwrap().pending.insert(id, tx);
        let msg = lsp::request(id, method, params);
        let write_result = {
            let mut writer = self.stdin.lock().unwrap();
            lsp::write_message(&mut *writer, &msg)
        };
        if let Err(e) = write_result {
            self.shared.lock().unwrap().pending.remove(&id);
            return Err(LiveError::Transport(format!(
                "writing {method} to pid {}: {e}",
                self.pid
            )));
        }
        match rx.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.shared.lock().unwrap().pending.remove(&id);
                Err(LiveError::Timeout {
                    method: method.to_string(),
                    after: timeout,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(LiveError::Dead {
                reason: self.dead_reason(),
            }),
        }
    }

    fn notify(&self, method: &str, params: Value) -> std::io::Result<()> {
        let msg = lsp::notification(method, params);
        let mut writer = self.stdin.lock().unwrap();
        lsp::write_message(&mut *writer, &msg)
    }

    fn dead_reason(&self) -> String {
        self.shared
            .lock()
            .unwrap()
            .dead_reason
            .clone()
            .unwrap_or_else(|| "unknown".into())
    }
}

impl LiveBackend for LiveInstance {
    fn state(&self) -> InstanceState {
        self.shared.lock().unwrap().state
    }

    fn request(&self, method: &str, params: Value) -> LiveResult<Value> {
        {
            let inner = self.shared.lock().unwrap();
            match inner.state {
                InstanceState::Warming => {
                    return Err(LiveError::Warming {
                        elapsed_secs: inner.warming_since.elapsed().as_secs(),
                    })
                }
                InstanceState::Dead => {
                    return Err(LiveError::Dead {
                        reason: inner
                            .dead_reason
                            .clone()
                            .unwrap_or_else(|| "unknown".into()),
                    })
                }
                InstanceState::Ready => {}
            }
        }
        *self.last_used.lock().unwrap() = Instant::now();
        self.raw_request(method, params, REQUEST_TIMEOUT)
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        {
            let mut inner = self.shared.lock().unwrap();
            if inner.state != InstanceState::Dead {
                eprintln!("live: pid {} {} → dead: shut down", self.pid, inner.state);
            }
            inner.state = InstanceState::Dead;
            inner.dead_reason.get_or_insert_with(|| "shut down".into());
        }
        if let Err(e) = self.raw_request(lsp::methods::SHUTDOWN, Value::Null, SHUTDOWN_GRACE) {
            eprintln!("live: pid {} shutdown request not acknowledged: {e}", self.pid);
        }
        if let Err(e) = self.notify(lsp::methods::EXIT, Value::Null) {
            eprintln!("live: pid {} exit notification failed: {e}", self.pid);
        }
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("live: pid {} exited: {status}", self.pid);
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        eprintln!(
                            "live: pid {} did not exit within {SHUTDOWN_GRACE:?} — SIGKILL",
                            self.pid
                        );
                        if let Err(e) = self.child.kill() {
                            eprintln!("live: pid {} SIGKILL failed: {e}", self.pid);
                        }
                        match self.child.wait() {
                            Ok(status) => eprintln!(
                                "live: pid {} reaped after SIGKILL: {status}",
                                self.pid
                            ),
                            Err(e) => eprintln!("live: pid {} reap failed: {e}", self.pid),
                        }
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    eprintln!("live: pid {} try_wait failed: {e} — SIGKILL", self.pid);
                    if let Err(e) = self.child.kill() {
                        eprintln!("live: pid {} SIGKILL failed: {e}", self.pid);
                    }
                    if let Err(e) = self.child.wait() {
                        eprintln!("live: pid {} reap failed: {e}", self.pid);
                    }
                    break;
                }
            }
        }
    }

    fn rss_mb(&self) -> Option<u64> {
        rss_mb_of(self.pid)
    }

    fn footprint_mb(&self) -> Option<u64> {
        footprint_mb_of(self.pid)
    }

    fn last_used(&self) -> Instant {
        *self.last_used.lock().unwrap()
    }
}

impl Drop for LiveInstance {
    fn drop(&mut self) {
        if self.shut_down {
            return;
        }
        // Never leak an orphan rust-analyzer: dropped without shutdown()
        // (e.g. handshake failure, panic unwind) → hard kill + reap.
        eprintln!("live: pid {} dropped without shutdown — SIGKILL", self.pid);
        if let Err(e) = self.child.kill() {
            eprintln!("live: pid {} SIGKILL on drop failed: {e}", self.pid);
        }
        if let Err(e) = self.child.wait() {
            eprintln!("live: pid {} reap on drop failed: {e}", self.pid);
        }
    }
}

/// Physical footprint in MB (SPEC-A2 §4 "Memory metric"): `footprint -p
/// <pid>` when it runs unprivileged, falling back to `ps` RSS with the
/// under-reporting caveat logged once per daemon process. `None` only when
/// both paths fail (process gone).
fn footprint_mb_of(pid: u32) -> Option<u64> {
    if let Some(mb) = Command::new("footprint")
        .args(["-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| parse_footprint_mb(&String::from_utf8_lossy(&out.stdout)))
    {
        return Some(mb);
    }
    static FOOTPRINT_FALLBACK_CAVEAT: std::sync::Once = std::sync::Once::new();
    FOOTPRINT_FALLBACK_CAVEAT.call_once(|| {
        eprintln!(
            "live: `footprint -p` unavailable or unparseable — memory watchdog falling back \
             to `ps` RSS, which under-reports idle rust-analyzer by >50x on macOS \
             (compressed/cold pages); mem_limit_mb will fire late"
        );
    });
    rss_mb_of(pid)
}

/// Parse the `footprint -p <pid>` summary line, e.g.
/// `rust-analyzer [12345]: 64-bit    Footprint: 2240 KB (16384 bytes per page)`
/// → MB. Case-sensitive `Footprint:` deliberately skips the lowercase
/// `phys_footprint:` auxiliary lines. `None` on any unexpected shape.
fn parse_footprint_mb(text: &str) -> Option<u64> {
    let rest = text
        .lines()
        .find_map(|line| line.split("Footprint:").nth(1))?;
    let mut parts = rest.split_whitespace();
    let value: f64 = parts.next()?.parse().ok()?;
    let mb = match parts.next()? {
        "B" => value / (1024.0 * 1024.0),
        "KB" => value / 1024.0,
        "MB" => value,
        "GB" => value * 1024.0,
        other => {
            eprintln!("live: footprint summary line has unknown unit `{other}`: {rest}");
            return None;
        }
    };
    Some(mb.round() as u64)
}

/// `ps -o rss= -p <pid>` (KB) → MB. `None` when the process is gone or the
/// output is unparseable.
fn rss_mb_of(pid: u32) -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb / 1024)
}

/// Background thread: reads every server message, routes responses by id,
/// applies serverStatus state transitions, and auto-answers server→client
/// requests (we support none) so the server never stalls waiting on us.
fn reader_loop(
    stdout: ChildStdout,
    shared: Arc<Mutex<Inner>>,
    stdin: Arc<Mutex<ChildStdin>>,
    pid: u32,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let msg = match lsp::read_message(&mut reader) {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                mark_dead(
                    &shared,
                    pid,
                    "rust-analyzer closed stdout (process exited or crashed)",
                );
                return;
            }
            Err(e) => {
                mark_dead(&shared, pid, &format!("LSP stream error: {e}"));
                return;
            }
        };
        let incoming: lsp::Incoming = match serde_json::from_value(msg.clone()) {
            Ok(incoming) => incoming,
            Err(e) => {
                eprintln!("live: pid {pid} unparseable LSP message ({e}): {msg}");
                continue;
            }
        };
        match (incoming.id, incoming.method) {
            // Response → route to the waiting request by id.
            (Some(id), None) => {
                let sender = id
                    .as_i64()
                    .and_then(|key| shared.lock().unwrap().pending.remove(&key));
                let outcome: LiveResult<Value> = match incoming.error {
                    Some(err) => Err(LiveError::Server {
                        code: err.code,
                        message: err.message,
                    }),
                    None => Ok(incoming.result.unwrap_or(Value::Null)),
                };
                match sender {
                    // Receiver gone == the caller already timed out loudly.
                    Some(tx) => drop(tx.send(outcome)),
                    None => eprintln!("live: pid {pid} response for unknown request id {id}"),
                }
            }
            // Server→client request: answer null (one null per item for
            // workspace/configuration) so the server never blocks on us.
            (Some(id), Some(method)) => {
                let result = if method == "workspace/configuration" {
                    let items = incoming
                        .params
                        .get("items")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    Value::Array(vec![Value::Null; items])
                } else {
                    Value::Null
                };
                let reply = lsp::response(&id, result);
                let write_result = {
                    let mut writer = stdin.lock().unwrap();
                    lsp::write_message(&mut *writer, &reply)
                };
                if let Err(e) = write_result {
                    eprintln!("live: pid {pid} failed replying to server request {method}: {e}");
                }
            }
            // Notification: serverStatus drives the state machine; all other
            // notifications ($/progress, window/showMessage, …) are ignored
            // by design.
            (None, Some(method)) => {
                if method == lsp::methods::SERVER_STATUS {
                    match serde_json::from_value::<lsp::ServerStatusParams>(incoming.params) {
                        Ok(status) => apply_server_status(&shared, pid, &status),
                        Err(e) => eprintln!("live: pid {pid} bad serverStatus params: {e}"),
                    }
                }
            }
            (None, None) => {
                eprintln!("live: pid {pid} LSP message with neither id nor method: {msg}")
            }
        }
    }
}

/// Quiescent ⇒ Ready; non-quiescent ⇒ Warming (rust-analyzer re-indexing
/// after edits goes briefly non-quiescent — answers pause rather than risk
/// stale-live results). Every transition is logged (SPEC-A2 A2-S9).
fn apply_server_status(shared: &Mutex<Inner>, pid: u32, status: &lsp::ServerStatusParams) {
    let mut inner = shared.lock().unwrap();
    if inner.state == InstanceState::Dead {
        return;
    }
    let new_state = if status.quiescent {
        InstanceState::Ready
    } else {
        InstanceState::Warming
    };
    if new_state != inner.state {
        let detail = status
            .message
            .as_deref()
            .map(|m| format!(", message={m}"))
            .unwrap_or_default();
        eprintln!(
            "live: pid {pid} {} → {} (health={}, quiescent={}{detail})",
            inner.state, new_state, status.health, status.quiescent
        );
        if new_state == InstanceState::Warming {
            inner.warming_since = Instant::now();
        }
        inner.state = new_state;
    }
}

/// Mark the instance Dead and fail every pending request loudly.
fn mark_dead(shared: &Mutex<Inner>, pid: u32, reason: &str) {
    let mut inner = shared.lock().unwrap();
    if inner.state != InstanceState::Dead {
        eprintln!("live: pid {pid} {} → dead: {reason}", inner.state);
    }
    inner.state = InstanceState::Dead;
    inner.dead_reason.get_or_insert_with(|| reason.to_string());
    for (_, tx) in inner.pending.drain() {
        drop(tx.send(Err(LiveError::Dead {
            reason: reason.to_string(),
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run(cwd: &Path, prog: &str, args: &[&str]) {
        let path = format!(
            "/opt/homebrew/bin:{}",
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(prog)
            .args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .output()
            .unwrap_or_else(|e| panic!("spawning {prog}: {e}"));
        assert!(
            out.status.success(),
            "{prog} {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn copy_dir(src: &Path, dst: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                copy_dir(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    /// Copy the golden fixture crate to a tempdir and git-init + commit it
    /// (same helper pattern as `src/ingest.rs` tests).
    fn fixture_repo() -> TempDir {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture, dir.path());
        run(dir.path(), "git", &["init", "-q", "-b", "main"]);
        run(dir.path(), "git", &["add", "-A"]);
        run(
            dir.path(),
            "git",
            &[
                "-c",
                "user.email=cq@test",
                "-c",
                "user.name=cq-test",
                "commit",
                "-q",
                "-m",
                "golden",
            ],
        );
        dir
    }

    fn ra_binary() -> String {
        let on_path = Command::new(RUST_ANALYZER_BIN)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if on_path {
            RUST_ANALYZER_BIN.to_string()
        } else {
            let fallback = "/opt/homebrew/bin/rust-analyzer";
            assert!(
                Path::new(fallback).exists(),
                "rust-analyzer not on PATH and not at {fallback}"
            );
            fallback.to_string()
        }
    }

    fn pid_alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Canned `footprint -p <pid>` output captured on macOS 2026-06-11 —
    /// tests never require the real tool (plan T3).
    const FOOTPRINT_OUTPUT: &str = "\
======================================================================
rust-analyzer [91533]: 64-bit    Footprint: 2240 KB (16384 bytes per page)
======================================================================

  Dirty      Clean  Reclaimable    Regions    Category
    ---        ---          ---        ---    ---
 976 KB        0 B          0 B          5    MALLOC_SMALL
    ---        ---          ---        ---    ---
2240 KB     560 KB          0 B        411    TOTAL

Auxiliary data:
    phys_footprint: 2256 KB
    phys_footprint_peak: 2320 KB
";

    #[test]
    fn footprint_parser_reads_summary_line_kb() {
        // 2240 KB → 2.19 MB → rounds to 2.
        assert_eq!(parse_footprint_mb(FOOTPRINT_OUTPUT), Some(2));
    }

    #[test]
    fn footprint_parser_handles_mb_and_gb_units() {
        let mb = "ra [1]: 64-bit    Footprint: 512 MB (16384 bytes per page)\n";
        assert_eq!(parse_footprint_mb(mb), Some(512));
        let gb = "ra [1]: 64-bit    Footprint: 2.0 GB (16384 bytes per page)\n";
        assert_eq!(parse_footprint_mb(gb), Some(2048));
        let gb_frac = "ra [1]: 64-bit    Footprint: 1.4 GB (16384 bytes per page)\n";
        assert_eq!(parse_footprint_mb(gb_frac), Some(1434));
    }

    #[test]
    fn footprint_parser_skips_lowercase_phys_footprint_aux_lines() {
        // No summary line — must NOT latch onto `phys_footprint:`.
        let aux_only = "Auxiliary data:\n    phys_footprint: 2256 KB\n";
        assert_eq!(parse_footprint_mb(aux_only), None);
    }

    #[test]
    fn footprint_parser_rejects_garbage_and_unknown_units() {
        assert_eq!(parse_footprint_mb(""), None);
        assert_eq!(parse_footprint_mb("no memory info here"), None);
        assert_eq!(parse_footprint_mb("x [1]: Footprint: lots KB"), None);
        assert_eq!(parse_footprint_mb("x [1]: Footprint: 12 parsecs"), None);
        assert_eq!(parse_footprint_mb("x [1]: Footprint:"), None);
    }

    #[test]
    fn footprint_mb_of_dead_pid_falls_back_then_none() {
        // Beyond macOS's pid range: `footprint` finds nothing, the RSS
        // fallback errors ("process id too large") → None, no panic.
        assert_eq!(footprint_mb_of(4_000_000), None);
    }

    #[test]
    fn spawn_missing_binary_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        let err = LiveInstance::spawn_with_binary("definitely-not-a-real-lsp-binary", dir.path())
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not found on PATH"), "unhelpful error: {msg}");
        assert!(msg.contains("rust-analyzer"), "no install hint: {msg}");
    }

    /// Fake LSP server: answers initialize (id 1), reports quiescent after
    /// 1s, then ignores everything — deterministic Warming→Ready coverage
    /// plus the SIGKILL branch of graceful shutdown.
    fn fake_server_script(dir: &Path) -> PathBuf {
        let path = dir.join("fake-ra.sh");
        let script = r#"#!/bin/bash
body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'
printf 'Content-Length: %d\r\n\r\n%s' "${#body}" "$body"
sleep 1
status='{"jsonrpc":"2.0","method":"experimental/serverStatus","params":{"health":"ok","quiescent":true}}'
printf 'Content-Length: %d\r\n\r\n%s' "${#status}" "$status"
sleep 30
"#;
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn warming_until_quiescent_then_ready_then_sigkill_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_server_script(dir.path());
        let mut inst =
            LiveInstance::spawn_with_binary(script.to_str().unwrap(), dir.path()).unwrap();

        // Quiescent arrives at ~1s; immediately post-handshake we are warming
        // and warming instances answer nothing.
        assert_eq!(inst.state(), InstanceState::Warming);
        match inst.request(lsp::methods::DEFINITION, serde_json::json!({})) {
            Err(LiveError::Warming { .. }) => {}
            other => panic!("warming instance must refuse with Warming, got {other:?}"),
        }

        let t0 = Instant::now();
        while inst.state() != InstanceState::Ready {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "never reached Ready from serverStatus quiescent"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        // Script ignores shutdown/exit → graceful path must escalate to
        // SIGKILL and still reap the child.
        let pid = inst.pid();
        inst.shutdown();
        assert!(!pid_alive(pid), "fake server survived shutdown");
        match inst.request(lsp::methods::DEFINITION, Value::Null) {
            Err(LiveError::Dead { .. }) => {}
            other => panic!("post-shutdown request must be Dead, got {other:?}"),
        }
    }

    /// Real rust-analyzer against the golden fixture (SPEC-A2 §2 semantics):
    /// spawn → Ready within 120s → definition on the `double` call site in
    /// src/lib.rs resolves to src/ops.rs → clean shutdown, child reaped.
    /// Runs in normal `cargo test` — the fixture primes in seconds (plan T2).
    #[test]
    fn golden_fixture_definition_via_live_instance() {
        let repo = fixture_repo();
        let root = repo.path().canonicalize().unwrap();
        let mut inst = LiveInstance::spawn_with_binary(&ra_binary(), &root).unwrap();
        let pid = inst.pid();

        // If we catch it pre-quiescence, it must refuse with Warming.
        if inst.state() == InstanceState::Warming {
            match inst.request(lsp::methods::DEFINITION, Value::Null) {
                Err(LiveError::Warming { .. }) => {}
                other => panic!("warming instance must refuse with Warming, got {other:?}"),
            }
        }

        let t0 = Instant::now();
        loop {
            match inst.state() {
                InstanceState::Ready => break,
                InstanceState::Dead => panic!("instance died during prime"),
                InstanceState::Warming => {
                    assert!(
                        t0.elapsed() < Duration::from_secs(120),
                        "rust-analyzer not quiescent within 120s"
                    );
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        eprintln!(
            "golden fixture primed in {:.1}s",
            t0.elapsed().as_secs_f64()
        );

        // Definition of `double` at its call site in src/lib.rs:
        //   pub fn top_level_fn(x: i32) -> i32 { ops::double(x) }
        let lib_rs = root.join("src/lib.rs");
        let content = std::fs::read_to_string(&lib_rs).unwrap();
        let (line_idx, line) = content
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("ops::double"))
            .expect("fixture lib.rs lost its ops::double call site");
        let col = line.find("double").unwrap(); // ASCII line: byte col == UTF-16 col
        let before = inst.last_used();
        let params = lsp::TextDocumentPositionParams {
            text_document: lsp::TextDocumentIdentifier {
                uri: lsp::path_to_uri(&lib_rs),
            },
            position: lsp::Position {
                line: line_idx as u32,
                character: col as u32,
            },
        };
        let result = inst
            .request(
                lsp::methods::DEFINITION,
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        let locations = lsp::definition_locations(&result).unwrap();
        assert_eq!(locations.len(), 1, "expected one definition: {locations:?}");
        let def_path = lsp::uri_to_path(&locations[0].uri).unwrap();
        assert!(
            def_path.ends_with("src/ops.rs"),
            "definition resolved to {def_path:?}, want src/ops.rs"
        );
        // ops.rs:1 (1-based) defines `pub fn double` → LSP line 0.
        assert_eq!(locations[0].range.start.line, 0);
        assert!(
            inst.last_used() > before,
            "last_used not updated by request()"
        );

        let rss = inst.rss_mb().expect("rss_mb on a live instance");
        assert!(rss > 0, "implausible rss {rss}MB");

        inst.shutdown();
        assert!(!pid_alive(pid), "rust-analyzer orphaned after shutdown");
    }
}
