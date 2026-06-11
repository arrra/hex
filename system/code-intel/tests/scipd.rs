//! Integration tests for the `scipd` daemon skeleton (SPEC-A2 plan Task 1).
//!
//! Every test spawns the REAL `scipd` binary via `env!("CARGO_BIN_EXE_scipd")`
//! with `CODEINTEL_HOME` pointed at a hermetic tempdir, talks to it over the
//! unix socket, and asserts wire shapes from SPEC-A2 §3.

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
    let child = Command::new(env!("CARGO_BIN_EXE_scipd"))
        .env("CODEINTEL_HOME", home)
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
        assert!(Instant::now() < deadline, "scipd never became ready at {}", socket.display());
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One request line → one reply object over a fresh connection.
fn request(home: &Path, line: &str) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path(home)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut writer = stream.try_clone().unwrap();
    writer.write_all(line.as_bytes()).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).expect("read reply line");
    serde_json::from_str(reply.trim())
        .unwrap_or_else(|e| panic!("reply is not JSON: {e}\nreply: {reply}"))
}

fn wait_exit(guard: &mut DaemonGuard, budget: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = guard.child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(Instant::now() < deadline, "scipd did not exit within {budget:?}");
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
    assert_eq!(status["status"]["pool_cap"], 2, "default cap per SPEC-A2 §4");
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
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
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

#[test]
fn malformed_config_is_fatal_and_loud() {
    let home = TempDir::new().unwrap();
    std::fs::write(home.path().join("scipd.toml"), "pool_cap = \"lots\"\n").unwrap();
    let mut daemon = spawn_scipd(home.path());
    let status = wait_exit(&mut daemon, Duration::from_secs(10));
    assert!(!status.success(), "malformed config must never default silently");
    let stderr = collect_stderr(&mut daemon);
    assert!(stderr.contains("BAD_CONFIG"), "stderr: {stderr}");
}
