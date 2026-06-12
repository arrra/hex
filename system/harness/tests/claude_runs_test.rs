//! Red tests for `hex claude-flags <profile>` (spec Sf5bj7y1d, task T2gd3bcmx).
//!
//! The claude_runs module must:
//!   - ship built-in lean profiles (no config file required)
//!   - emit `--bare` plus a `--strict-mcp-config` invocation by default
//!     (so plugins/MCP/skills/CLAUDE.md don't auto-load on headless runs)
//!   - hard-error on an unknown profile name (loud failure per SO #6)
//!
//! These tests pin the CLI surface that worker/run.rs and run_eval.py will
//! consume. They fail before claude_runs is wired and pass after.

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hex")
}

#[test]
fn claude_flags_builtin_harness_worker_is_lean_by_default() {
    // No config file present → built-in lean profile applies.
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = Command::new(bin())
        .args(["claude-flags", "harness_worker"])
        .env("HEX_DIR", tmp.path())
        .output()
        .expect("run hex claude-flags harness_worker");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`hex claude-flags harness_worker` must succeed with no config present.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("--bare"),
        "lean built-in must emit --bare; got: {stdout:?}"
    );
    assert!(
        stdout.contains("--strict-mcp-config"),
        "lean built-in must emit --strict-mcp-config to block MCP auto-discovery; got: {stdout:?}"
    );
    // Output must be a single line (eval-safe shell substitution contract).
    assert_eq!(
        stdout.trim_end_matches('\n').lines().count(),
        1,
        "claude-flags output must be a single line; got: {stdout:?}"
    );
}

#[test]
fn claude_flags_unknown_profile_is_a_hard_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = Command::new(bin())
        .args(["claude-flags", "definitely_not_a_profile"])
        .env("HEX_DIR", tmp.path())
        .output()
        .expect("run hex claude-flags <unknown>");
    assert!(
        !out.status.success(),
        "unknown profile must exit non-zero (no quiet failures); stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("profile")
            || stderr.to_lowercase().contains("unknown"),
        "stderr must explain the unknown profile; got: {stderr:?}"
    );
}
