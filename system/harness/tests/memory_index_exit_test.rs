//! Regression: `hex memory index` must EXIT promptly after finishing its work.
//!
//! Bug (2026-06-06): the index process completed (`Done in Xs`, returned 0) and
//! then hung in `__cxa_finalize` — onnxruntime's global thread-pool static
//! destructor (pulled in via fastembed/ort in src/memory/embed.rs) blocks while
//! joining its worker threads at process exit. A live run wedged for 2h40m
//! holding `.hex/memory-index.lock`; because the harness worker waits on the
//! child with `Ctx::run` → `.output()`, EVERY memory-maintenance job (index,
//! consolidate-quick, parse-transcripts, nightly full-consolidate) stalled
//! behind the one hung child.
//!
//! Fix A: after embedder-using commands finish, flush stdio and `libc::_exit`
//! to bypass the C++ atexit chain (the onnxruntime static destructor).
//!
//! This spawns the real binary against a throwaway HEX_DIR and asserts it exits
//! within a wall-clock bound. Pre-fix it times out (the hang); post-fix it
//! exits in a few seconds (model cold-load + a zero-file index).
//!
//! `#[ignore]` — model-dependent (loads the nomic ONNX weights). Run with
//! `cargo test --test memory_index_exit_test -- --ignored` on a host with the
//! fastembed cache, and in the nightly / Docker E2E. Mirrors the other
//! model-gated `#[ignore]` tests in src/memory/embed.rs.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

#[test]
#[ignore]
fn memory_index_exits_within_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let hex_dir = tmp.path();
    std::fs::write(hex_dir.join("CLAUDE.md"), "# Hex test workspace\n").unwrap();

    // Reuse the real fastembed cache if present so the model loads from disk
    // instead of downloading. embed.rs looks for `<HEX_DIR>/.fastembed_cache`.
    if let Ok(home) = std::env::var("HOME") {
        let real_cache = PathBuf::from(home).join("hex/.fastembed_cache");
        if real_cache.is_dir() {
            let _ = std::os::unix::fs::symlink(&real_cache, hex_dir.join(".fastembed_cache"));
        }
    }

    // Default: the freshly-built binary against the throwaway workspace. Both
    // are overridable for investigation / running against the deployed binary
    // and the real workspace (where scale-dependent hangs would surface):
    //   HEX_TEST_BIN=/path/to/hex   HEX_TEST_DIR=/path/to/real/workspace
    let bin = std::env::var("HEX_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_hex").to_string());
    let run_dir = std::env::var("HEX_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| hex_dir.to_path_buf());
    let child = Command::new(&bin)
        .args(["memory", "index"])
        .env("HEX_DIR", &run_dir)
        .spawn()
        .expect("spawn hex memory index");
    let pid = child.id();

    // Wall-clock bound. The model cold-load + a zero-file index is a few
    // seconds; the exit-hang is unbounded. 60s cleanly separates the two.
    let bound_secs: u64 = std::env::var("HEX_TEST_BOUND_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let bound = Duration::from_secs(bound_secs);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut child = child;
        let _ = tx.send(child.wait());
    });

    match rx.recv_timeout(bound) {
        Ok(Ok(status)) => assert!(
            status.success(),
            "`hex memory index` exited non-zero: {status:?}"
        ),
        Ok(Err(e)) => panic!("failed to wait on child: {e}"),
        Err(_) => {
            // Hung past the bound — kill the orphan so we don't leak it, then fail.
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            panic!(
                "`hex memory index` did not exit within {bound:?} — hung at process exit \
                 (onnxruntime static-destructor thread join). This is the regression."
            );
        }
    }
}
