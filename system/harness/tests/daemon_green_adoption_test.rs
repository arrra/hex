//! Red test for task Tcr36yttb (spec S2ya3pd23): the harness must adopt the
//! `daemon-green` crate for install/start/stop/status/restart/logs and delete
//! the hand-rolled launchctl + plist helpers.
//!
//! Fails before the rewrite, passes after.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hex")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn cargo_toml_pins_daemon_green_at_expected_rev() {
    let cargo = std::fs::read_to_string(
        workspace_root().join("system/harness/Cargo.toml"),
    )
    .expect("read system/harness/Cargo.toml");
    assert!(
        cargo.contains("daemon-green"),
        "system/harness/Cargo.toml must depend on daemon-green; got:\n{cargo}"
    );
    assert!(
        cargo.contains("df4ab27"),
        "daemon-green dep must be pinned to rev df4ab27 for reproducibility; got:\n{cargo}"
    );
}

#[test]
fn main_rs_uses_daemon_green_and_drops_handrolled_plist_helpers() {
    let main = std::fs::read_to_string(
        workspace_root().join("system/harness/src/main.rs"),
    )
    .expect("read system/harness/src/main.rs");
    assert!(
        main.contains("daemon_green"),
        "main.rs must reference daemon_green (e.g. daemon_green::native()); got body without it"
    );
    // The hand-rolled helpers must be gone — daemon-green owns them now.
    assert!(
        !main.contains("fn render_harness_plist"),
        "main.rs must DELETE fn render_harness_plist (daemon-green renders the plist)"
    );
    assert!(
        !main.contains("fn harness_plist_path"),
        "main.rs must DELETE fn harness_plist_path (daemon-green owns the path)"
    );
    assert!(
        !main.contains("fn gui_domain"),
        "main.rs must DELETE fn gui_domain (daemon-green owns the launchctl domain)"
    );
    // CRITICAL: SessionCreate must not be reintroduced anywhere in the harness source.
    assert!(
        !main.contains("SessionCreate"),
        "main.rs must NOT mention SessionCreate (blocks login keychain)"
    );
}

#[test]
fn hex_harness_help_lists_restart_and_logs() {
    let out = Command::new(bin())
        .args(["harness", "--help"])
        .output()
        .expect("run hex harness --help");
    assert!(
        out.status.success(),
        "`hex harness --help` must succeed. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("restart"),
        "harness --help must list `restart`; got:\n{help}"
    );
    assert!(
        help.contains("logs"),
        "harness --help must list `logs`; got:\n{help}"
    );
}

#[test]
fn hex_harness_restart_subcommand_resolves() {
    let out = Command::new(bin())
        .args(["harness", "restart", "--help"])
        .output()
        .expect("run hex harness restart --help");
    assert!(
        out.status.success(),
        "`hex harness restart --help` must succeed (subcommand wired). stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn hex_harness_logs_subcommand_resolves() {
    let out = Command::new(bin())
        .args(["harness", "logs", "--help"])
        .output()
        .expect("run hex harness logs --help");
    assert!(
        out.status.success(),
        "`hex harness logs --help` must succeed (subcommand wired). stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
