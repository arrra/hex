//! iii-engine-health: is the hex harness engine up?
//!
//! The iii engine is now BAKED INTO the hex binary and runs in-process inside
//! `com.hex.harness`. Liveness = the engine's WebSocket port (127.0.0.1:49134,
//! what the worker runtime + `hex triggers emit` connect to) accepts a TCP
//! connection. Any listener answering means the harness's engine is serving.
//!
//! Install detection is substrate-aware via daemon-green (mrap/hex#9): launchd
//! plist on macOS, `systemctl --user` unit on Linux. The old plist-only probe
//! made every Linux host report "not installed" while the systemd service was
//! active — hiding real engine-health signal behind a permanent Skip.
//!
//! - service not installed (per daemon-green) -> Skip (substrate not in use)
//! - port not accepting conns                 -> Warn (LOUD + actionable; S6)
//! - connection succeeds                      -> Pass

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const ENGINE_ADDR: &str = "127.0.0.1:49134";
const HARNESS_LABEL: &str = "com.hex.harness";

pub struct IiiEngineHealth;

impl DoctorCheck for IiiEngineHealth {
    fn name(&self) -> &str {
        "iii-engine-health"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, _ctx: &Context) -> CheckResult {
        // The engine runs inside com.hex.harness, installed as a per-user
        // service (launchd LaunchAgent on macOS, systemd user unit on Linux).
        // daemon-green owns that distinction — ask it, don't probe plists.
        match daemon_green::native().status(HARNESS_LABEL) {
            Ok(daemon_green::ServiceStatus::NotInstalled) => {
                return CheckResult::skip("com.hex.harness not installed — engine check skipped");
            }
            Ok(_) => {
                // Installed (running, stopped, or failed) — the liveness probe
                // below is the real verdict either way.
            }
            Err(e) => {
                // Can't query the service manager — loud, not a silent skip
                // (S6). Still probe: a live port is a live engine.
                eprintln!("iii-engine-health: service status query failed: {e}");
            }
        }

        if engine_listening(ENGINE_ADDR) {
            CheckResult::pass("hex harness engine up (WS accepting connections on :49134)")
        } else {
            CheckResult::warn(
                "hex harness engine DOWN — nothing listening on 127.0.0.1:49134. \
                 Fix: `hex harness start` (installs/restarts com.hex.harness).",
            )
        }
    }
}

/// True if a TCP connection to `addr` succeeds within 2s.
fn engine_listening(addr: &str) -> bool {
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };
    addrs.any(|sa| TcpStream::connect_timeout(&sa, Duration::from_secs(2)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> Context {
        Context {
            hex_dir: PathBuf::from("/tmp/fake-hex"),
            home: PathBuf::from("/tmp"),
            fix: false,
        }
    }

    #[test]
    fn name_and_category() {
        assert_eq!(IiiEngineHealth.name(), "iii-engine-health");
        assert_eq!(IiiEngineHealth.category(), Category::Health);
    }

    #[test]
    fn run_returns_non_empty_message() {
        // Skip / Pass / Warn — every branch must carry an actionable message.
        let r = IiiEngineHealth.run(&ctx());
        assert!(!r.message.is_empty(), "check message must not be empty");
    }

    #[test]
    fn listening_is_false_for_unbound_port() {
        // Port 1 is privileged and never bound by us — connect must fail fast.
        assert!(!engine_listening("127.0.0.1:1"));
    }
}
