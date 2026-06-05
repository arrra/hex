//! CLI surface guards for the iii→hex abstraction (spec Skt0r3dbg, task Ta5c1w89b):
//!  - the old `hex iii ...` command tree must be GONE (no alias).
//!  - `hex worker run --help` and `hex worker --help` must succeed.
//!  - `hex triggers emit --help` must succeed.
//!
//! These tests fail before the CLI is refactored and pass after.

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hex")
}

#[test]
fn hex_iii_command_tree_is_removed() {
    let out = Command::new(bin())
        .args(["iii", "--help"])
        .output()
        .expect("run hex iii --help");
    assert!(
        !out.status.success(),
        "`hex iii` must be removed entirely — no alias. \
         stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Also: the top-level help must not list `iii` as a subcommand.
    let top = Command::new(bin())
        .arg("--help")
        .output()
        .expect("run hex --help");
    assert!(top.status.success(), "`hex --help` must succeed");
    let help = String::from_utf8_lossy(&top.stdout);
    // Match a whole-word "iii" to avoid false hits inside other identifiers.
    let has_iii = help
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| tok == "iii");
    assert!(
        !has_iii,
        "top-level `hex --help` must not advertise an `iii` subcommand; got:\n{help}"
    );
}

#[test]
fn hex_worker_run_help_exists() {
    let out = Command::new(bin())
        .args(["worker", "--help"])
        .output()
        .expect("run hex worker --help");
    assert!(
        out.status.success(),
        "`hex worker --help` must succeed. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let run = Command::new(bin())
        .args(["worker", "run", "--help"])
        .output()
        .expect("run hex worker run --help");
    assert!(
        run.status.success(),
        "`hex worker run --help` must succeed. stdout: {} stderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}

#[test]
fn hex_triggers_emit_help_exists() {
    let out = Command::new(bin())
        .args(["triggers", "emit", "--help"])
        .output()
        .expect("run hex triggers emit --help");
    assert!(
        out.status.success(),
        "`hex triggers emit --help` must succeed. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        help.contains("--data") || help.contains("data"),
        "`hex triggers emit --help` should mention the --data flag; got:\n{help}"
    );
}
