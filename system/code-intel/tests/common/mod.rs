//! Shared real-binary test harness for the live suites (tests/cli_live.rs,
//! tests/golden_live.rs): hermetic `cq`/`scipd` spawning, the golden fixture
//! repo, expectation loading, and the scipd daemon guard.
//!
//! Each integration-test binary compiles its own copy of this module and
//! uses a subset of it, hence the file-wide `dead_code` allow.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Generous budget for one real rust-analyzer prime on the golden fixture.
pub const READY_BUDGET: Duration = Duration::from_secs(120);

/// PATH with /opt/homebrew/bin prepended — BOI verify subshells strip PATH;
/// rust-analyzer/git/cargo must still resolve (CLAUDE.md verify-gate rules).
pub fn full_path_env() -> String {
    format!(
        "/opt/homebrew/bin:{}",
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Spawn the real `cq` binary with a hermetic CODEINTEL_HOME.
pub fn cq(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    cq_with_path(home, cwd, args, &full_path_env())
}

pub fn cq_with_path(home: &Path, cwd: &Path, args: &[&str], path_env: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cq"))
        .args(args)
        .current_dir(cwd)
        .env("CODEINTEL_HOME", home)
        .env("PATH", path_env)
        .output()
        .unwrap_or_else(|e| panic!("spawning cq {args:?}: {e}"))
}

pub fn stdout_json(out: &Output) -> serde_json::Value {
    let raw = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(raw.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON object: {e}\nstdout: {raw}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

pub fn stderr_json(out: &Output) -> serde_json::Value {
    let raw = String::from_utf8_lossy(&out.stderr);
    for line in raw.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            return v;
        }
    }
    panic!("no JSON object on stderr\nstderr: {raw}");
}

pub fn assert_exit(out: &Output, want: i32) {
    assert_eq!(
        out.status.code().expect("cq terminated by signal"),
        want,
        "exit code\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn run_cmd(cwd: &Path, prog: &str, args: &[&str]) -> Output {
    let out = Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .env("PATH", full_path_env())
        .output()
        .unwrap_or_else(|e| panic!("spawning {prog}: {e}"));
    assert!(
        out.status.success(),
        "{prog} {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

pub fn copy_dir(src: &Path, dst: &Path) {
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

/// Golden fixture crate copied to a tempdir, git-initialized + committed.
pub fn golden_repo() -> TempDir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixture, dir.path());
    run_cmd(dir.path(), "git", &["init", "-q", "-b", "main"]);
    run_cmd(dir.path(), "git", &["config", "user.email", "cq@test"]);
    run_cmd(dir.path(), "git", &["config", "user.name", "cq-test"]);
    run_cmd(dir.path(), "git", &["add", "-A"]);
    run_cmd(dir.path(), "git", &["commit", "-q", "-m", "golden"]);
    dir
}

pub fn register(home: &Path, repo: &Path) {
    let out = cq(home, repo, &["register", repo.to_str().unwrap()]);
    assert_exit(&out, 0);
}

pub fn register_and_index(home: &Path, repo: &Path) {
    register(home, repo);
    let out = cq(home, repo, &["index"]);
    assert_exit(&out, 0);
}

/// Append a brand-new `double` call site to src/ops.rs: the file goes
/// stale relative to the index AND live answers must see the new site.
pub fn append_brand_new_caller(repo: &Path) -> u32 {
    let ops = repo.join("src/ops.rs");
    let mut content = std::fs::read_to_string(&ops).unwrap();
    let new_line_number = content.lines().count() as u32 + 1;
    content.push_str("pub fn brand_new() -> i32 { double(7) }\n");
    std::fs::write(&ops, content).unwrap();
    new_line_number
}

/// 1-based (line, col) of `needle`'s first occurrence in `file`.
pub fn find_pos(file: &Path, needle: &str) -> (u32, u32) {
    let content = std::fs::read_to_string(file).unwrap();
    for (idx, line) in content.lines().enumerate() {
        if let Some(byte) = line.find(needle) {
            return (idx as u32 + 1, byte as u32 + 1);
        }
    }
    panic!("{needle:?} not found in {}", file.display());
}

// ---------------------------------------------------------------------------
// Expectation fixtures (tests/golden.rs pattern)
// ---------------------------------------------------------------------------

pub fn load_fixture_json(rel: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

pub fn expectations() -> serde_json::Value {
    load_fixture_json("tests/fixtures/golden-expectations.json")
}

/// `FILE:LINE:COL` selector for a symbol's recorded definition site —
/// position-based selection is unambiguous even for symbols sharing a
/// display name (the three `area` definitions).
pub fn def_selector(sym: &serde_json::Value) -> String {
    format!(
        "{}:{}:{}",
        sym["def"]["path"].as_str().unwrap(),
        sym["def"]["line"].as_u64().unwrap(),
        sym["def"]["col"].as_u64().unwrap()
    )
}

// ---------------------------------------------------------------------------
// scipd harness (pattern from tests/scipd.rs)
// ---------------------------------------------------------------------------

pub struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn spawn_scipd(home: &Path) -> DaemonGuard {
    let child = Command::new(env!("CARGO_BIN_EXE_scipd"))
        .env("CODEINTEL_HOME", home)
        .env("PATH", full_path_env())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning scipd");
    DaemonGuard { child }
}

pub fn wait_daemon_ready(home: &Path) {
    let socket = home.join("scipd.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return;
        }
        assert!(Instant::now() < deadline, "scipd never became ready");
        std::thread::sleep(Duration::from_millis(25));
    }
}
