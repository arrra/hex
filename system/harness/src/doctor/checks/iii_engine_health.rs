//! iii-engine-health: is the hex iii engine up?
//!
//! The iii engine exposes no `/health` route, so liveness = the HTTP API port
//! (127.0.0.1:3111) accepts a TCP connection. This is endpoint-agnostic: any
//! listener answering on the port means the engine is serving.
//!
//! - `iii` not on PATH        -> Skip   (the iii substrate is optional today)
//! - port not accepting conns -> Warn   (LOUD + actionable; Standing Order S6)
//! - connection succeeds      -> Pass

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::Duration;

const ENGINE_ADDR: &str = "127.0.0.1:3111";

pub struct IiiEngineHealth;

impl DoctorCheck for IiiEngineHealth {
    fn name(&self) -> &str { "iii-engine-health" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, _ctx: &Context) -> CheckResult {
        // Is iii installed? If not, the substrate isn't in use — skip quietly.
        let iii_present = Command::new("iii")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !iii_present {
            return CheckResult::skip("iii not installed — engine check skipped");
        }

        if engine_listening(ENGINE_ADDR) {
            CheckResult::pass("iii engine up (HTTP API accepting connections on :3111)")
        } else {
            CheckResult::warn(
                "iii engine DOWN — nothing listening on 127.0.0.1:3111. Fix: \
                 `.hex/scripts/iii-engine.sh install` then the printed launchctl \
                 bootstrap, or `iii-engine.sh start`",
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
        Context { hex_dir: PathBuf::from("/tmp/fake-hex"), home: PathBuf::from("/tmp"), fix: false }
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
