//! `scipd` — the live code-intel daemon (SPEC-A2 §2).
//!
//! Launchd-supervised (com.hex.scipd, KeepAlive). Owns the capped pool of
//! live rust-analyzer instances and serves cq over `<home>/scipd.sock`.
//! Task 1 ships the UDS skeleton: ping, stub status, stale-socket handling,
//! second-daemon refusal, SIGTERM clean shutdown.

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use scipd_core::config::ScipdConfig;
use scipd_core::daemon::{BindError, Daemon};
use scipd_core::workspace::codeintel_home;

fn fail(code: &str, message: String, hint: &str) -> ExitCode {
    // Loud structured stderr, same code/message/hint triple as cq errors.
    eprintln!(
        "{}",
        serde_json::json!({"error": {"code": code, "message": message, "hint": hint}})
    );
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let home = match codeintel_home() {
        Ok(home) => home,
        Err(e) => {
            return fail(
                e.code_str(),
                e.to_string(),
                "set $CODEINTEL_HOME or $HOME so scipd can find its home",
            );
        }
    };
    if let Err(e) = std::fs::create_dir_all(&home) {
        return fail(
            "DAEMON_START_FAILED",
            format!("creating {}: {e}", home.display()),
            "check permissions on the codeintel home directory",
        );
    }

    // Malformed config is fatal and loud — never default-on-parse-failure
    // (SPEC-A2 §4, Standing Order S6).
    let config = match ScipdConfig::load(&home) {
        Ok(config) => config,
        Err(e) => {
            return fail(
                "BAD_CONFIG",
                e,
                "fix or remove scipd.toml; missing file means defaults, malformed never does",
            );
        }
    };

    let daemon = match Daemon::bind(&home, config) {
        Ok(daemon) => daemon,
        Err(e @ BindError::AlreadyRunning { .. }) => {
            return fail(
                "ALREADY_RUNNING",
                e.to_string(),
                "exactly one scipd per codeintel home; stop the other instance first",
            );
        }
        Err(e) => {
            return fail(
                "DAEMON_START_FAILED",
                e.to_string(),
                "check the codeintel home is writable and the socket path is free",
            );
        }
    };

    // SIGTERM (launchd stop) and SIGINT (interactive) drain the accept loop.
    let flag = daemon.shutdown_flag();
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        if let Err(e) = signal_hook::flag::register(sig, flag.clone()) {
            return fail(
                "DAEMON_START_FAILED",
                format!("registering signal {sig}: {e}"),
                "this should never fail; report it",
            );
        }
    }

    match daemon.run() {
        Ok(()) => {
            // Reached only via the shutdown flag — confirm it for the log.
            debug_assert!(flag.load(Ordering::SeqCst));
            eprintln!("scipd: clean shutdown");
            ExitCode::SUCCESS
        }
        Err(e) => fail(
            "DAEMON_CRASHED",
            format!("accept loop failed: {e}"),
            "launchd KeepAlive will restart scipd; check ~/.codeintel/logs",
        ),
    }
}
