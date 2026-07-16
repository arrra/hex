//! `scipd` UDS server, per SPEC-A2 §2/§3.
//!
//! Single-process, multi-threaded, std blocking IO (no tokio by design):
//! an accept loop plus one thread per connection. Task 5 wires the live
//! pool: `query`/`rename` resolve a worktree, `get_or_spawn` it, and either
//! answer from the Ready instance via `live/translate` or reply
//! `{ok:false,warming:{...}}` IMMEDIATELY — the daemon never queues a query
//! behind a prime (SPEC-A2 §3). `status` reports the real pool; `evict` is
//! the ops hatch.
//!
//! Socket lifecycle:
//! - bind at `<home>/scipd.sock`;
//! - a stale socket file (no listener behind it) is unlinked before bind;
//! - if a LIVE daemon already owns the socket, [`Daemon::bind`] refuses
//!   loudly ([`BindError::AlreadyRunning`]) — never two daemons;
//! - SIGTERM (flag set by the bin) drains the accept loop, removes the
//!   socket file, and returns; the bin then calls `pool.shutdown_all()` so
//!   no rust-analyzer child is ever orphaned.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::live::{translate, LiveBackend, LiveError, Pool};
use crate::proto::{parse_request, Op, QueryVerb, Reply, Request, Warming};

/// Per-connection socket read/write timeouts (SPEC-A2 plan Task 1 band:
/// 500ms–5s). Reads get the long end (a client may hold the connection
/// between requests); writes are local and short.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// Accept-loop poll interval while checking the shutdown flag.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// How often the daemon-owned reaper thread runs one pool policy pass
/// (SPEC-A2 §4: idle TTL, vanish reap, memory watchdog — all in `sweep`).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// The daemon socket lives directly under the codeintel home (SPEC-A2 §2).
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("scipd.sock")
}

/// Why `bind` refused.
#[derive(Debug)]
pub enum BindError {
    /// A live daemon already answers on the socket — second daemon must
    /// refuse loudly (plan Task 1).
    AlreadyRunning { socket: PathBuf },
    Io(std::io::Error),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::AlreadyRunning { socket } => write!(
                f,
                "another scipd is already serving {}; refusing to start a second daemon",
                socket.display()
            ),
            BindError::Io(e) => write!(f, "binding scipd socket: {e}"),
        }
    }
}

impl std::error::Error for BindError {}

/// A bound, not-yet-running daemon. Generic over the pool's backend so the
/// dispatch logic is unit-testable without a real rust-analyzer.
pub struct Daemon<B: LiveBackend + 'static> {
    listener: UnixListener,
    socket: PathBuf,
    pool: Arc<Pool<B>>,
    shutdown: Arc<AtomicBool>,
}

impl<B: LiveBackend + 'static> Daemon<B> {
    /// Bind the UDS at `<home>/scipd.sock` with stale-socket handling:
    /// try-connect first — a successful connect means a live daemon owns the
    /// socket (refuse); a refused/failed connect means the file is stale
    /// (unlink, then bind).
    pub fn bind(home: &Path, pool: Arc<Pool<B>>) -> Result<Daemon<B>, BindError> {
        let socket = socket_path(home);
        if socket.exists() {
            match UnixStream::connect(&socket) {
                Ok(_) => return Err(BindError::AlreadyRunning { socket }),
                Err(e) => {
                    eprintln!(
                        "scipd: removing stale socket {} (connect failed: {e})",
                        socket.display()
                    );
                    std::fs::remove_file(&socket).map_err(BindError::Io)?;
                }
            }
        }
        let listener = UnixListener::bind(&socket).map_err(BindError::Io)?;
        // Nonblocking accept so the loop can observe the shutdown flag.
        listener.set_nonblocking(true).map_err(BindError::Io)?;
        Ok(Daemon { listener, socket, pool, shutdown: Arc::new(AtomicBool::new(false)) })
    }

    /// Flag the bin wires to SIGTERM: once set, `run` drains and returns.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    /// The shared pool — the bin's reaper thread sweeps it and the SIGTERM
    /// path shuts it down after `run` returns.
    pub fn pool(&self) -> Arc<Pool<B>> {
        Arc::clone(&self.pool)
    }

    /// Accept loop. Returns after the shutdown flag is set; removes the
    /// socket file on the way out (clean shutdown, SPEC-A2 plan Task 1).
    pub fn run(self) -> std::io::Result<()> {
        eprintln!("scipd: listening on {}", self.socket.display());
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                break;
            }
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    let pool = Arc::clone(&self.pool);
                    std::thread::spawn(move || {
                        if let Err(e) = serve_connection(stream, &pool) {
                            // Per-connection failures are loud but never
                            // take the daemon down (Standing Order S6).
                            eprintln!("scipd: connection error: {e}");
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    eprintln!("scipd: accept error: {e}");
                    std::thread::sleep(ACCEPT_POLL);
                }
            }
        }
        eprintln!("scipd: shutdown requested; removing {}", self.socket.display());
        if let Err(e) = std::fs::remove_file(&self.socket) {
            eprintln!("scipd: removing socket on shutdown: {e}");
        }
        Ok(())
    }
}

/// Handle one connection: newline-JSON requests in, one reply line each.
fn serve_connection<B: LiveBackend>(stream: UnixStream, pool: &Pool<B>) -> std::io::Result<()> {
    // The accepted stream can inherit the listener's nonblocking mode
    // (platform-dependent); per-connection IO must be blocking-with-timeout.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            // Timeout or peer hangup: close the connection quietly — the
            // client owns the connection lifetime.
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = dispatch(&line, pool);
        let mut payload = serde_json::to_string(&reply).expect("Reply serializes");
        payload.push('\n');
        writer.write_all(payload.as_bytes())?;
        writer.flush()?;
    }
    Ok(())
}

/// Pure request → reply dispatch (unit-tested without sockets, against a
/// fake-backed pool). SPEC-A2 §3 semantics: a warming instance answers
/// `{ok:false,warming:{elapsed_secs}}` IMMEDIATELY while its prime proceeds
/// in the pool — never queue a query behind a prime.
pub fn dispatch<B: LiveBackend>(line: &str, pool: &Pool<B>) -> Reply {
    let req: Request = match parse_request(line) {
        Ok(req) => req,
        Err((id, reason)) => {
            return Reply::error(
                id,
                "BAD_REQUEST",
                format!("unparseable request: {reason}"),
                "send one JSON object per line, shaped per SPEC-A2 §3",
            );
        }
    };
    match req.op {
        Op::Ping => Reply::pong(req.id),
        Op::Status => Reply::status(req.id, pool.status()),
        Op::Query { verb, worktree, path, line, col, name: _ } => {
            let root = match resolve_worktree(req.id, &worktree) {
                Ok(root) => root,
                Err(reply) => return *reply,
            };
            with_instance(req.id, pool, &root, |backend| {
                let results = match verb {
                    QueryVerb::Def => translate::live_def(backend, &root, &path, line, col)?,
                    QueryVerb::Refs => translate::live_refs(backend, &root, &path, line, col)?,
                    QueryVerb::Callers => {
                        translate::live_callers(backend, &root, &path, line, col)?
                    }
                };
                Ok(Reply::results(req.id, results))
            })
        }
        Op::Rename { worktree, path, line, col, new_name } => {
            let root = match resolve_worktree(req.id, &worktree) {
                Ok(root) => root,
                Err(reply) => return *reply,
            };
            with_instance(req.id, pool, &root, |backend| {
                let edits = translate::live_rename(backend, &root, &path, line, col, &new_name)?;
                Ok(Reply::edits(req.id, edits))
            })
        }
        Op::Evict { worktree } => {
            let existed = pool.evict(Path::new(&worktree));
            if !existed {
                eprintln!("scipd: evict for {worktree} — no resident instance (no-op)");
            }
            Reply::pong(req.id)
        }
    }
}

/// Resolve a request's worktree to its canonical root. Nonexistent or
/// relative paths are rejected loudly (SPEC-A2 §2: one instance == one real
/// worktree) — boxed Reply keeps the success path lean.
fn resolve_worktree(id: u64, worktree: &str) -> Result<PathBuf, Box<Reply>> {
    let path = Path::new(worktree);
    if !path.is_absolute() {
        return Err(Box::new(Reply::error(
            id,
            "BAD_WORKTREE",
            format!("worktree must be an absolute path, got {worktree}"),
            "send the worktree's absolute root path",
        )));
    }
    path.canonicalize().map_err(|e| {
        Box::new(Reply::error(
            id,
            "BAD_WORKTREE",
            format!("cannot canonicalize worktree {worktree}: {e}"),
            "the worktree must exist on disk before the daemon can root an instance at it",
        ))
    })
}

/// `get_or_spawn` + run `op` against the locked backend, mapping the error
/// taxonomy: Warming → immediate warming reply (the spawn keeps priming in
/// the pool), Dead → INSTANCE_DEAD (next query respawns), anything else →
/// LIVE_ERROR. All errors are structured and loud (Standing Order S6).
fn with_instance<B: LiveBackend>(
    id: u64,
    pool: &Pool<B>,
    root: &Path,
    op: impl FnOnce(&B) -> anyhow::Result<Reply>,
) -> Reply {
    let backend = match pool.get_or_spawn(root) {
        Ok(backend) => backend,
        Err(e) => {
            return Reply::error(
                id,
                "SPAWN_FAILED",
                format!("{e:#}"),
                "check the daemon log; is rust-analyzer installed and the worktree a \
                 cargo workspace?",
            );
        }
    };
    let guard = backend.lock().unwrap();
    match op(&*guard) {
        Ok(reply) => reply,
        Err(e) => match e.downcast_ref::<LiveError>() {
            Some(LiveError::Warming { elapsed_secs }) => Reply::warming(
                id,
                Warming {
                    elapsed_secs: *elapsed_secs,
                    workspace: Some(root.display().to_string()),
                },
            ),
            Some(LiveError::Dead { .. }) => Reply::error(
                id,
                "INSTANCE_DEAD",
                format!("{e:#}"),
                "retry — the next query respawns the instance",
            ),
            _ => Reply::error(
                id,
                "LIVE_ERROR",
                format!("{e:#}"),
                "see the scipd log for the rust-analyzer exchange",
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScipdConfig;
    use crate::live::{InstanceState, LiveResult};
    use crate::proto::PoolStatus;
    use serde_json::{json, Value};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::Instant;
    use tempfile::TempDir;

    /// Scripted fake backend: dispatch tests drive the protocol semantics
    /// without a rust-analyzer (the pool's own policy has its tests in
    /// `live/pool.rs`; real-instance coverage lives in `tests/scipd.rs`).
    struct FakeBackend {
        state: Arc<Mutex<InstanceState>>,
        /// Canned response per LSP method.
        responses: Arc<Mutex<std::collections::HashMap<String, Value>>>,
        requests_seen: Arc<AtomicUsize>,
    }

    impl LiveBackend for FakeBackend {
        fn state(&self) -> InstanceState {
            *self.state.lock().unwrap()
        }

        fn request(&self, method: &str, _params: Value) -> LiveResult<Value> {
            match self.state() {
                InstanceState::Warming => return Err(LiveError::Warming { elapsed_secs: 42 }),
                InstanceState::Dead => {
                    return Err(LiveError::Dead { reason: "fake died".into() })
                }
                InstanceState::Ready => {}
            }
            self.requests_seen.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .responses
                .lock()
                .unwrap()
                .get(method)
                .cloned()
                .unwrap_or(Value::Null))
        }

        fn shutdown(&mut self) {
            *self.state.lock().unwrap() = InstanceState::Dead;
        }

        fn rss_mb(&self) -> Option<u64> {
            Some(1)
        }

        fn footprint_mb(&self) -> Option<u64> {
            Some(1)
        }

        fn last_used(&self) -> Instant {
            Instant::now()
        }
    }

    struct Harness {
        pool: Pool<FakeBackend>,
        state: Arc<Mutex<InstanceState>>,
        responses: Arc<Mutex<std::collections::HashMap<String, Value>>>,
        worktree: TempDir,
    }

    impl Harness {
        fn worktree_str(&self) -> String {
            self.worktree.path().canonicalize().unwrap().display().to_string()
        }
    }

    fn harness(initial: InstanceState) -> Harness {
        let state = Arc::new(Mutex::new(initial));
        let responses: Arc<Mutex<std::collections::HashMap<String, Value>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pool = Pool::new(ScipdConfig::default(), {
            let state = Arc::clone(&state);
            let responses = Arc::clone(&responses);
            move |_root: &Path| {
                Ok(FakeBackend {
                    state: Arc::clone(&state),
                    responses: Arc::clone(&responses),
                    requests_seen: Arc::new(AtomicUsize::new(0)),
                })
            }
        });
        let worktree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(worktree.path().join("src")).unwrap();
        std::fs::write(
            worktree.path().join("src/a.rs"),
            "pub fn double(x: i32) -> i32 { x * 2 }\n",
        )
        .unwrap();
        Harness { pool, state, responses, worktree }
    }

    fn query_line(id: u64, verb: &str, worktree: &str) -> String {
        format!(
            r#"{{"id":{id},"op":"query","verb":"{verb}","worktree":"{worktree}","path":"src/a.rs","line":1,"col":8}}"#
        )
    }

    #[test]
    fn dispatch_ping_pongs() {
        let h = harness(InstanceState::Ready);
        let reply = dispatch(r#"{"id":1,"op":"ping"}"#, &h.pool);
        assert_eq!(reply, Reply::pong(1));
    }

    #[test]
    fn dispatch_status_reports_empty_pool_with_configured_cap() {
        let h = harness(InstanceState::Ready);
        let reply = dispatch(r#"{"id":2,"op":"status"}"#, &h.pool);
        assert!(reply.ok);
        let status: PoolStatus = reply.status.expect("status section");
        assert_eq!(status.pool_cap, ScipdConfig::default().pool_cap);
        assert!(status.instances.is_empty());
    }

    #[test]
    fn dispatch_unknown_op_is_loud_error_with_echoed_id() {
        let h = harness(InstanceState::Ready);
        let reply = dispatch(r#"{"id":9,"op":"explode"}"#, &h.pool);
        assert_eq!(reply.id, 9);
        assert!(!reply.ok);
        assert_eq!(reply.error.unwrap().code, "BAD_REQUEST");
    }

    #[test]
    fn dispatch_garbage_is_loud_error_never_panic() {
        let h = harness(InstanceState::Ready);
        let reply = dispatch("}{ total garbage", &h.pool);
        assert_eq!(reply.id, 0);
        assert_eq!(reply.error.unwrap().code, "BAD_REQUEST");
    }

    #[test]
    fn query_on_warming_instance_replies_warming_immediately() {
        let h = harness(InstanceState::Warming);
        let reply = dispatch(&query_line(3, "def", &h.worktree_str()), &h.pool);
        assert_eq!(reply.id, 3);
        assert!(!reply.ok);
        let warming = reply.warming.expect("warming section");
        assert_eq!(warming.elapsed_secs, 42);
        assert_eq!(warming.workspace.as_deref(), Some(h.worktree_str().as_str()));
        // The spawn proceeded: the instance is resident in the pool.
        assert_eq!(h.pool.status().instances.len(), 1);
    }

    #[test]
    fn query_on_ready_instance_returns_live_results() {
        let h = harness(InstanceState::Ready);
        // Canned definition: src/a.rs:1:8 (LSP 0:7).
        let uri = format!("file://{}/src/a.rs", h.worktree_str());
        h.responses.lock().unwrap().insert(
            crate::live::lsp::methods::DEFINITION.into(),
            json!([{ "uri": uri, "range": {
                "start": {"line": 0, "character": 7},
                "end": {"line": 0, "character": 13}
            }}]),
        );
        let reply = dispatch(&query_line(4, "def", &h.worktree_str()), &h.pool);
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.source.as_deref(), Some("live"));
        let results = reply.results.expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "src/a.rs");
        assert_eq!((results[0].line, results[0].col), (1, 8));
        assert_eq!(results[0].role, "definition");
    }

    #[test]
    fn rename_on_ready_instance_returns_edits_with_old_text() {
        let h = harness(InstanceState::Ready);
        let uri = format!("file://{}/src/a.rs", h.worktree_str());
        h.responses.lock().unwrap().insert(
            crate::live::lsp::methods::RENAME.into(),
            json!({ "changes": { uri: [{
                "range": {
                    "start": {"line": 0, "character": 7},
                    "end": {"line": 0, "character": 13}
                },
                "newText": "twice"
            }]}}),
        );
        let line = format!(
            r#"{{"id":5,"op":"rename","worktree":"{}","path":"src/a.rs","line":1,"col":8,"new_name":"twice"}}"#,
            h.worktree_str()
        );
        let reply = dispatch(&line, &h.pool);
        assert!(reply.ok, "{reply:?}");
        let edits = reply.edits.expect("edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].old_text, "double");
        assert_eq!(edits[0].new_text, "twice");
        assert_eq!((edits[0].line, edits[0].col), (1, 8));
    }

    #[test]
    fn rename_on_warming_instance_replies_warming() {
        let h = harness(InstanceState::Warming);
        let line = format!(
            r#"{{"id":6,"op":"rename","worktree":"{}","path":"src/a.rs","line":1,"col":8,"new_name":"twice"}}"#,
            h.worktree_str()
        );
        let reply = dispatch(&line, &h.pool);
        assert!(!reply.ok);
        assert!(reply.warming.is_some(), "{reply:?}");
    }

    #[test]
    fn query_for_nonexistent_worktree_is_loud_bad_worktree() {
        let h = harness(InstanceState::Ready);
        let reply = dispatch(&query_line(7, "def", "/nonexistent/worktree/xyz"), &h.pool);
        assert!(!reply.ok);
        let err = reply.error.expect("error section");
        assert_eq!(err.code, "BAD_WORKTREE");
        assert!(err.message.contains("/nonexistent/worktree/xyz"));
        assert!(h.pool.status().instances.is_empty(), "nothing may be spawned");
    }

    #[test]
    fn query_for_relative_worktree_is_rejected() {
        let h = harness(InstanceState::Ready);
        let reply = dispatch(&query_line(8, "def", "relative/path"), &h.pool);
        assert_eq!(reply.error.expect("error").code, "BAD_WORKTREE");
    }

    #[test]
    fn dead_instance_maps_to_instance_dead() {
        let h = harness(InstanceState::Ready);
        // First query spawns; then the instance dies mid-life. The pool
        // respawns on get_or_spawn, but the shared scripted state keeps the
        // respawned fake Dead too — request() then yields LiveError::Dead.
        dispatch(&query_line(9, "def", &h.worktree_str()), &h.pool);
        *h.state.lock().unwrap() = InstanceState::Dead;
        let reply = dispatch(&query_line(10, "def", &h.worktree_str()), &h.pool);
        assert!(!reply.ok);
        assert_eq!(reply.error.expect("error").code, "INSTANCE_DEAD");
    }

    #[test]
    fn evict_drops_instance_and_is_idempotent() {
        let h = harness(InstanceState::Ready);
        dispatch(&query_line(11, "def", &h.worktree_str()), &h.pool);
        assert_eq!(h.pool.status().instances.len(), 1);
        let line = format!(r#"{{"id":12,"op":"evict","worktree":"{}"}}"#, h.worktree_str());
        let reply = dispatch(&line, &h.pool);
        assert!(reply.ok);
        assert!(h.pool.status().instances.is_empty());
        // Idempotent: evicting again is still ok:true.
        let reply = dispatch(&line, &h.pool);
        assert!(reply.ok);
    }

    #[test]
    fn all_three_verbs_dispatch_to_translate() {
        let h = harness(InstanceState::Ready);
        for verb in ["def", "refs", "callers"] {
            let reply = dispatch(&query_line(13, verb, &h.worktree_str()), &h.pool);
            assert!(reply.ok, "verb {verb}: {reply:?}");
            assert_eq!(reply.source.as_deref(), Some("live"), "verb {verb}");
        }
    }

    #[test]
    fn socket_path_is_under_home() {
        assert_eq!(socket_path(Path::new("/x")), PathBuf::from("/x/scipd.sock"));
    }

    fn test_pool() -> Arc<Pool<FakeBackend>> {
        Arc::new(Pool::new(ScipdConfig::default(), |_root: &Path| {
            anyhow::bail!("bind tests never spawn")
        }))
    }

    #[test]
    fn bind_refuses_when_live_daemon_owns_socket() {
        let home = tempfile::tempdir().unwrap();
        let first = Daemon::bind(home.path(), test_pool()).unwrap();
        let err = match Daemon::bind(home.path(), test_pool()) {
            Ok(_) => panic!("second bind must refuse"),
            Err(e) => e,
        };
        assert!(matches!(err, BindError::AlreadyRunning { .. }), "{err}");
        drop(first);
    }

    #[test]
    fn bind_unlinks_stale_socket_and_succeeds() {
        let home = tempfile::tempdir().unwrap();
        // A socket file nobody listens on (listener dropped) is stale.
        {
            let d = Daemon::bind(home.path(), test_pool()).unwrap();
            drop(d);
        }
        assert!(socket_path(home.path()).exists(), "dropped daemon leaves the file");
        let d = Daemon::bind(home.path(), test_pool()).unwrap();
        drop(d);
    }

    /// RED TEST — pins the concrete race behind the load-sensitive flake in
    /// `bind_unlinks_stale_socket_and_succeeds`.
    ///
    /// Root cause: `Daemon::bind`'s liveness probe is a raw
    /// `UnixStream::connect(&socket)` — it decides "another daemon owns this
    /// socket" purely by whether the kernel socket associated with the path
    /// still accepts connections. That's not the same question as "is the
    /// daemon process that used to own this alive?": any other file
    /// descriptor referencing the same kernel socket keeps it connectable,
    /// even after the daemon that bound it has dropped its listener.
    ///
    /// Under load, Rust std's `UnixListener::bind` on macOS creates the
    /// socket via `socket(2)` and then sets `FD_CLOEXEC` via a follow-up
    /// `fcntl(2)` — a tiny race window during which any concurrent `fork` /
    /// `posix_spawn` in the process (cargo test parallelism spawns plenty:
    /// `Command::new("git")`, `Command::new("cargo")`, `launchctl`, `id`,
    /// etc. — see grep of `Command::new` across the crate) leaks the
    /// listener fd into a child. When the daemon later drops the listener,
    /// the child's inherited fd still holds the kernel socket alive, so the
    /// bind probe's `connect()` succeeds and returns `AlreadyRunning`
    /// against a socket in a *fresh tempdir* whose owning daemon is gone.
    ///
    /// This test reproduces the same failure mode deterministically without
    /// needing to actually race with a fork: it dups the listener fd in the
    /// same process to keep the kernel socket alive after the parent drops
    /// its listener. The connect probe cannot tell that apart from an
    /// inherited-fd leak. A correct probe (e.g. a pidfile + `kill(pid, 0)`
    /// liveness check, or a ping request + read within timeout) reports the
    /// socket stale and rebinds successfully.
    #[test]
    fn bind_probe_ignores_orphaned_fd_holding_socket_alive() {
        use std::os::unix::io::AsRawFd;

        let home = tempfile::tempdir().unwrap();
        let first = Daemon::bind(home.path(), test_pool()).unwrap();

        // Simulate the leaked-into-child fd: dup the listener so the kernel
        // socket persists after the parent's listener drops. This is exactly
        // what happens under load when the FD_CLOEXEC window is lost to a
        // concurrent posix_spawn — the child's inherited fd is the "extra
        // reference" that keeps the socket connectable.
        let _fd_hostage = first
            .listener
            .try_clone()
            .expect("dup listener to simulate inherited fd");
        // Sanity: the dup really is a distinct fd referencing the same
        // socket (not the same fd number).
        assert_ne!(_fd_hostage.as_raw_fd(), first.listener.as_raw_fd());
        drop(first);

        // The daemon that bound this socket is gone. A correct liveness
        // probe must recognize that and rebind. The current connect-based
        // probe cannot — the dup keeps the socket answering, so the probe
        // returns Ok and bind refuses with AlreadyRunning against a socket
        // in a fresh tempdir.
        let result = Daemon::bind(home.path(), test_pool());
        drop(_fd_hostage);
        match result {
            Ok(d) => drop(d),
            Err(e) => panic!(
                "bind must not be fooled by an orphaned fd keeping the socket \
                 reachable — this is the concrete race behind the flake. \
                 The probe needs to check daemon-process liveness, not just \
                 socket reachability. Got: {e}"
            ),
        }
    }
}
