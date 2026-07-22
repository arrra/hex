//! Integration tests for the `scipd` daemon (SPEC-A2 plan Tasks 1 and 5).
//!
//! Every test spawns the REAL `scipd` binary via `env!("CARGO_BIN_EXE_scipd")`
//! with `CODEINTEL_HOME` pointed at a hermetic tempdir, talks to it over the
//! unix socket, and asserts wire shapes from SPEC-A2 §3. The live-pool
//! lifecycle test additionally pays one real rust-analyzer prime on the
//! golden fixture (it primes in seconds).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn socket_path(home: &Path) -> PathBuf {
    home.join("scipd.sock")
}

/// Spawned daemon that is force-killed on drop so failed tests never leak
/// processes.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_scipd(home: &Path) -> DaemonGuard {
    // rust-analyzer must be reachable from the daemon's PATH (Homebrew's
    // bin is not always on a test runner's default PATH).
    let path = format!(
        "/opt/homebrew/bin:{}",
        std::env::var("PATH").unwrap_or_default()
    );
    let child = Command::new(env!("CARGO_BIN_EXE_scipd"))
        .env("CODEINTEL_HOME", home)
        .env("PATH", path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning scipd");
    DaemonGuard { child }
}

/// Wait for the daemon to be answering (socket exists AND accepts a connect).
fn wait_ready(home: &Path) {
    let socket = socket_path(home);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if socket.exists() && UnixStream::connect(&socket).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "scipd never became ready at {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One request line → one reply object over a fresh connection.
fn request(home: &Path, line: &str) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path(home)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    writer.write_all(line.as_bytes()).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .expect("read reply line");
    serde_json::from_str(reply.trim())
        .unwrap_or_else(|e| panic!("reply is not JSON: {e}\nreply: {reply}"))
}

fn wait_exit(guard: &mut DaemonGuard, budget: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = guard.child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "scipd did not exit within {budget:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn collect_stderr(guard: &mut DaemonGuard) -> String {
    let mut buf = String::new();
    use std::io::Read;
    if let Some(stderr) = guard.child.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut buf);
    }
    buf
}

#[test]
fn ping_status_and_bad_op_over_the_socket() {
    let home = TempDir::new().unwrap();
    let _daemon = spawn_scipd(home.path());
    wait_ready(home.path());

    // ping (SPEC-A2 §3 wire shape)
    let pong = request(home.path(), r#"{"id":1,"op":"ping"}"#);
    assert_eq!(pong["id"], 1);
    assert_eq!(pong["ok"], true);

    // stub status: empty pool, configured cap surfaced
    let status = request(home.path(), r#"{"id":2,"op":"status"}"#);
    assert_eq!(status["id"], 2);
    assert_eq!(status["ok"], true);
    assert_eq!(
        status["status"]["pool_cap"], 2,
        "default cap per SPEC-A2 §4"
    );
    assert_eq!(status["status"]["instances"], serde_json::json!([]));

    // unknown op: structured error reply, daemon survives
    let bad = request(home.path(), r#"{"id":9,"op":"explode"}"#);
    assert_eq!(bad["id"], 9);
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["error"]["code"], "BAD_REQUEST");
    assert!(!bad["error"]["hint"].as_str().unwrap().is_empty());

    // total garbage: still a structured reply, never a hang or crash
    let garbage = request(home.path(), "}{ not json");
    assert_eq!(garbage["ok"], false);
    assert_eq!(garbage["error"]["code"], "BAD_REQUEST");

    // daemon still alive and answering after the malformed traffic
    let pong = request(home.path(), r#"{"id":3,"op":"ping"}"#);
    assert_eq!(pong["ok"], true);
}

#[test]
fn multiple_requests_on_one_connection() {
    let home = TempDir::new().unwrap();
    let _daemon = spawn_scipd(home.path());
    wait_ready(home.path());

    let stream = UnixStream::connect(socket_path(home.path())).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    for id in 1..=3u64 {
        writer
            .write_all(format!("{{\"id\":{id},\"op\":\"ping\"}}\n").as_bytes())
            .unwrap();
        writer.flush().unwrap();
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        let j: serde_json::Value = serde_json::from_str(reply.trim()).unwrap();
        assert_eq!(j["id"], id);
        assert_eq!(j["ok"], true);
    }
}

#[test]
fn second_daemon_refuses_loudly_while_first_keeps_serving() {
    let home = TempDir::new().unwrap();
    let _first = spawn_scipd(home.path());
    wait_ready(home.path());

    let mut second = spawn_scipd(home.path());
    let status = wait_exit(&mut second, Duration::from_secs(10));
    assert!(!status.success(), "second daemon must refuse");
    let stderr = collect_stderr(&mut second);
    assert!(
        stderr.contains("ALREADY_RUNNING"),
        "refusal must be loud and structured; stderr: {stderr}"
    );

    // First daemon unaffected.
    let pong = request(home.path(), r#"{"id":1,"op":"ping"}"#);
    assert_eq!(pong["ok"], true);
}

#[test]
fn stale_socket_is_unlinked_and_rebound() {
    let home = TempDir::new().unwrap();
    {
        let mut first = spawn_scipd(home.path());
        wait_ready(home.path());
        // SIGKILL: no chance to clean up — leaves a stale socket file.
        first.child.kill().unwrap();
        first.child.wait().unwrap();
    }
    assert!(
        socket_path(home.path()).exists(),
        "SIGKILLed daemon must leave the socket file (the stale case)"
    );

    let _second = spawn_scipd(home.path());
    wait_ready(home.path());
    let pong = request(home.path(), r#"{"id":1,"op":"ping"}"#);
    assert_eq!(pong["ok"], true, "new daemon must recover the stale socket");
}

#[test]
fn sigterm_shuts_down_cleanly_and_removes_socket() {
    let home = TempDir::new().unwrap();
    let mut daemon = spawn_scipd(home.path());
    wait_ready(home.path());

    let pid = daemon.child.id().to_string();
    let killed = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("running kill");
    assert!(killed.success());

    let status = wait_exit(&mut daemon, Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "SIGTERM must be a clean exit 0");
    assert!(
        !socket_path(home.path()).exists(),
        "clean shutdown must remove the socket file"
    );
}

// ---------------------------------------------------------------------------
// Task 5: live pool over the socket (real rust-analyzer, golden fixture)
// ---------------------------------------------------------------------------

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

/// Copy the golden fixture crate to a tempdir and git-init + commit it (same
/// helper pattern as the T2/T4 live tests).
fn fixture_repo() -> TempDir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
    let dir = TempDir::new().unwrap();
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

/// Drain the daemon's stderr on a background thread so a long-lived test
/// can never deadlock on a full pipe; the collected log is also asserted on.
fn drain_stderr(guard: &mut DaemonGuard) -> std::sync::Arc<std::sync::Mutex<String>> {
    let stderr = guard.child.stderr.take().expect("scipd stderr piped");
    let buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = std::sync::Arc::clone(&buf);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = std::io::BufReader::new(stderr);
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&chunk[..n])),
            }
        }
    });
    buf
}

fn pid_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Pids of the daemon's direct rust-analyzer children.
fn ra_children(daemon_pid: u32) -> Vec<String> {
    let out = Command::new("pgrep")
        .args(["-P", &daemon_pid.to_string()])
        .output()
        .expect("running pgrep");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// SPEC-A2 §3/§4 live-pool lifecycle over the socket, one prime end to end:
/// query-while-warming answers ≤2s and NEVER queues behind the prime; the
/// same query after quiescence returns live def results; `evict` empties the
/// pool (and kills the child); SIGTERM exits 0 with zero orphaned
/// rust-analyzer processes.
#[test]
fn live_pool_lifecycle_over_the_socket() {
    let repo = fixture_repo();
    let worktree = repo.path().canonicalize().unwrap();
    let home = TempDir::new().unwrap();
    let mut daemon = spawn_scipd(home.path());
    let stderr = drain_stderr(&mut daemon);
    wait_ready(home.path());

    // Query target: the `ops::double(x)` call site in the fixture's lib.rs.
    let content = std::fs::read_to_string(worktree.join("src/lib.rs")).unwrap();
    let (line_idx, line) = content
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("ops::double"))
        .expect("fixture lib.rs lost its ops::double call site");
    let (line1, col1) = (line_idx + 1, line.find("double").unwrap() + 1);
    let query = format!(
        r#"{{"id":3,"op":"query","verb":"def","worktree":"{}","path":"src/lib.rs","line":{line1},"col":{col1}}}"#,
        worktree.display()
    );

    // 1. Query while warming: the FIRST query triggers the spawn and must
    //    answer ≤2s with a warming reply — never queued behind the prime.
    let t0 = Instant::now();
    let first = request(home.path(), &query);
    let elapsed = t0.elapsed();
    assert!(
        elapsed <= Duration::from_secs(2),
        "query during prime took {elapsed:?} (> 2s) — reply queued behind the prime?"
    );
    assert_eq!(first["id"], 3);
    assert_eq!(
        first["ok"], false,
        "fresh spawn cannot be quiescent yet: {first}"
    );
    assert!(
        first["warming"]["elapsed_secs"].is_u64(),
        "warming reply must carry elapsed_secs: {first}"
    );
    assert_eq!(
        first["warming"]["workspace"],
        worktree.display().to_string().as_str(),
        "warming reply names the priming workspace"
    );

    // The spawn proceeded: a rust-analyzer child exists and status shows the
    // warming instance.
    let first_ra = ra_children(daemon.child.id());
    assert_eq!(
        first_ra.len(),
        1,
        "exactly one rust-analyzer child, got {first_ra:?}"
    );
    let status = request(home.path(), r#"{"id":4,"op":"status"}"#);
    assert_eq!(
        status["status"]["instances"][0]["state"], "warming",
        "{status}"
    );
    assert_eq!(
        status["status"]["instances"][0]["worktree"],
        worktree.display().to_string().as_str()
    );

    // 2. Re-query after ready (poll up to 120s): real live def results.
    let deadline = Instant::now() + Duration::from_secs(120);
    let live = loop {
        let reply = request(home.path(), &query);
        if reply["ok"] == true {
            break reply;
        }
        assert!(
            reply["warming"].is_object(),
            "while not ok the reply must be a warming reply: {reply}"
        );
        assert!(
            Instant::now() < deadline,
            "instance never became ready within 120s; last reply: {reply}"
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(live["source"], "live", "{live}");
    let results = live["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1, "one definition for double: {live}");
    assert_eq!(results[0]["path"], "src/ops.rs");
    assert_eq!(
        results[0]["line"], 1,
        "`pub fn double` sits on ops.rs:1 (1-based)"
    );
    assert_eq!(results[0]["role"], "definition");

    let status = request(home.path(), r#"{"id":5,"op":"status"}"#);
    assert_eq!(
        status["status"]["instances"][0]["state"], "ready",
        "{status}"
    );

    // 3. Evict: pool empties and the rust-analyzer child dies.
    let evict = request(
        home.path(),
        &format!(
            r#"{{"id":6,"op":"evict","worktree":"{}"}}"#,
            worktree.display()
        ),
    );
    assert_eq!(evict["ok"], true, "{evict}");
    let status = request(home.path(), r#"{"id":7,"op":"status"}"#);
    assert_eq!(
        status["status"]["instances"],
        serde_json::json!([]),
        "evict must empty the pool: {status}"
    );
    assert!(
        status["status"]["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("evicted")),
        "eviction must be visible in status notes: {status}"
    );
    assert!(
        !pid_alive(&first_ra[0]),
        "evicted instance's rust-analyzer (pid {}) survived",
        first_ra[0]
    );

    // 4. A new query respawns (warming again), then SIGTERM: daemon exits 0
    //    and the fresh rust-analyzer child is gone — no orphans.
    let respawn = request(home.path(), &query);
    assert_eq!(respawn["ok"], false);
    assert!(
        respawn["warming"].is_object(),
        "respawn must warm again: {respawn}"
    );
    let second_ra = ra_children(daemon.child.id());
    assert_eq!(
        second_ra.len(),
        1,
        "respawn must yield one child, got {second_ra:?}"
    );

    let killed = Command::new("kill")
        .args(["-TERM", &daemon.child.id().to_string()])
        .status()
        .expect("running kill");
    assert!(killed.success());
    let status = wait_exit(&mut daemon, Duration::from_secs(20));
    assert_eq!(status.code(), Some(0), "SIGTERM must be a clean exit 0");
    assert!(
        !socket_path(home.path()).exists(),
        "clean shutdown must remove the socket file"
    );
    assert!(
        !pid_alive(&second_ra[0]),
        "rust-analyzer (pid {}) orphaned after SIGTERM",
        second_ra[0]
    );

    let log = stderr.lock().unwrap().clone();
    assert!(
        log.contains("spawned instance for"),
        "pool transitions must be logged:\n{log}"
    );
    assert!(
        log.contains("pool shutdown"),
        "SIGTERM shutdown must be logged:\n{log}"
    );
}

#[test]
fn malformed_config_is_fatal_and_loud() {
    let home = TempDir::new().unwrap();
    std::fs::write(home.path().join("scipd.toml"), "pool_cap = \"lots\"\n").unwrap();
    let mut daemon = spawn_scipd(home.path());
    let status = wait_exit(&mut daemon, Duration::from_secs(10));
    assert!(
        !status.success(),
        "malformed config must never default silently"
    );
    let stderr = collect_stderr(&mut daemon);
    assert!(stderr.contains("BAD_CONFIG"), "stderr: {stderr}");
}
