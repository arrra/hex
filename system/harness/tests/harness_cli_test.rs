//! CLI surface guards for the new `hex harness` command tree (spec S5yw25n5y, task Tr5zx0eay).
//!  - `hex harness --help` must succeed and advertise start/stop/status (NOT serve — serve is hidden).
//!  - A `serve` subcommand must still exist (hidden) so launchd can invoke `hex harness serve`.
//!  - The old `hex worker` command must be GONE (subtractive cleanup — YAML host removed).
//!  - system/templates/launchd/harness.plist must exist with ProgramArguments [hex, harness, serve].
//!
//! These tests fail before the CLI is wired and pass after.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hex")
}

#[test]
fn hex_harness_help_lists_start_stop_status_and_hides_serve() {
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
        help.contains("start"),
        "harness --help must list `start`; got:\n{help}"
    );
    assert!(
        help.contains("stop"),
        "harness --help must list `stop`; got:\n{help}"
    );
    assert!(
        help.contains("status"),
        "harness --help must list `status`; got:\n{help}"
    );
    assert!(
        !help.contains("serve"),
        "harness --help must NOT advertise `serve` (must be hidden); got:\n{help}"
    );
}

#[test]
fn hex_harness_serve_subcommand_exists_but_hidden() {
    // Even though hidden, `hex harness serve --help` must resolve (so launchd can invoke it).
    let out = Command::new(bin())
        .args(["harness", "serve", "--help"])
        .output()
        .expect("run hex harness serve --help");
    assert!(
        out.status.success(),
        "`hex harness serve --help` must succeed (hidden but runnable). \
         stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn hex_worker_command_is_removed() {
    // Subtractive cleanup: the YAML worker host (`hex worker`) is gone — the
    // typed Rust worker registry hosted by `hex harness` replaces it. The old
    // subcommand must no longer resolve.
    let out = Command::new(bin())
        .args(["worker", "run", "--help"])
        .output()
        .expect("run hex worker run --help");
    assert!(
        !out.status.success(),
        "`hex worker` must be REMOVED (subtractive cleanup), but the command succeeded. \
         stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unrecognized subcommand") || stderr.contains("worker"),
        "removing `hex worker` should yield an unrecognized-subcommand error; got:\n{stderr}"
    );
}

#[test]
fn harness_plist_template_exists_and_invokes_hex_harness_serve() {
    // Locate the workspace root: CARGO_MANIFEST_DIR is system/harness/.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let plist = manifest
        .parent() // system/
        .and_then(|p| p.parent()) // workspace root
        .map(|root| root.join("system/templates/launchd/harness.plist"))
        .expect("compute plist path");
    assert!(
        plist.exists(),
        "system/templates/launchd/harness.plist must exist; looked at {}",
        plist.display()
    );
    let body = std::fs::read_to_string(&plist).expect("read harness.plist");
    assert!(
        body.contains("ProgramArguments"),
        "harness.plist must declare ProgramArguments; got:\n{body}"
    );
    // ProgramArguments must contain `harness` and `serve` (the hidden serve entry).
    assert!(
        body.contains("<string>harness</string>"),
        "harness.plist ProgramArguments must include <string>harness</string>; got:\n{body}"
    );
    assert!(
        body.contains("<string>serve</string>"),
        "harness.plist ProgramArguments must include <string>serve</string>; got:\n{body}"
    );
    assert!(
        body.contains("com.hex.harness"),
        "harness.plist must use the com.hex.harness label; got:\n{body}"
    );
    // gui LaunchAgent form: must NOT declare SessionCreate — it detaches the job
    // from the Aqua login session and BLOCKS login-keychain access (verified
    // 2026-06-05: rc=36 with it, rc=0 without). A plain gui/<uid> agent inherits
    // the keychain.
    assert!(
        !body.contains("<key>SessionCreate</key>"),
        "harness.plist must NOT set SessionCreate (it blocks login-keychain access); got:\n{body}"
    );
    // The folded-in backup worker needs the file keyring backend (gws can't reach
    // the login keychain from a headless daemon).
    assert!(
        body.contains("GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND"),
        "harness.plist must set GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND for headless gws; got:\n{body}"
    );
}
