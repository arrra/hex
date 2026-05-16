/// Port of .hex/scripts/integrations/tailscale.sh
/// Two-step health probe: verify tailscale daemon is up, then ping a known stable peer.
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const INTEGRATION: &str = "tailscale";
const STABLE_PEER: &str = "100.101.9.109";

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let year = 1970 + days / 365;
    let day_of_year = days % 365;
    let month = day_of_year / 30 + 1;
    let day = day_of_year % 30 + 1;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, h, m, s
    )
}

fn emit_event(event: &str, status: &str, msg: &str) {
    eprintln!(
        r#"{{"event":"{}","status":"{}","message":"{}","ts":"{}"}}"#,
        event,
        status,
        msg,
        now_rfc3339()
    );
}

fn check_daemon() -> bool {
    Command::new("tailscale")
        .arg("status")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ping_peer(peer: &str) -> bool {
    // macOS: ping uses -W ms for timeout; try -W 5000 first, fallback to -t 5
    let ok = Command::new("ping")
        .args(["-c", "1", "-W", "5000", peer])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        return true;
    }
    Command::new("ping")
        .args(["-c", "1", "-t", "5", peer])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn run_probe() -> i32 {
    println!("[{}/probe] checking tailscale status...", INTEGRATION);

    if !check_daemon() {
        emit_event(
            "hex.integration.tailscale.probe_fail",
            "fail",
            "tailscale status failed — daemon not running or not authenticated",
        );
        eprintln!(
            "[{}/probe] FAIL: tailscale daemon not running or not authenticated",
            INTEGRATION
        );
        return 1;
    }

    if !ping_peer(STABLE_PEER) {
        emit_event(
            "hex.integration.tailscale.probe_fail",
            "fail",
            &format!("peer {} unreachable", STABLE_PEER),
        );
        eprintln!(
            "[{}/probe] FAIL: peer {} unreachable",
            INTEGRATION, STABLE_PEER
        );
        return 1;
    }

    emit_event(
        "hex.integration.tailscale.probe_ok",
        "ok",
        "tailscale up, peer reachable",
    );
    println!("[{}/probe] OK", INTEGRATION);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_shell_script() {
        assert_eq!(INTEGRATION, "tailscale");
        assert_eq!(STABLE_PEER, "100.101.9.109");
    }

    #[test]
    fn now_rfc3339_has_expected_format() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 20, "timestamp len: {}", ts);
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
    }
}
