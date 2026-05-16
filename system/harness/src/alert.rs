/// Port of .hex/scripts/hex-alert.sh
/// iMessage alert sender — sets env vars and execs alert/send.sh.
use std::path::PathBuf;
use std::process::Command;

pub fn run_send(hex_dir: &PathBuf, severity: &str, agent_id: &str, message: &str) {
    let send_sh = hex_dir.join(".hex/scripts/alert/send.sh");
    if !send_sh.exists() {
        eprintln!("ERROR: alert/send.sh not found at {}", send_sh.display());
        std::process::exit(1);
    }

    let status = Command::new(&send_sh)
        .env("HEX_ALERT_SEVERITY", severity)
        .env("HEX_ALERT_AGENT_ID", agent_id)
        .env("HEX_ALERT_MESSAGE", message)
        .env("HEX_ALERT_TIER", "tier:direct-ping")
        .env("HEX_ALERT_REASON_KIND", "watchdog-alert")
        .status()
        .unwrap_or_else(|e| {
            eprintln!("ERROR: failed to exec alert/send.sh: {e}");
            std::process::exit(1);
        });

    std::process::exit(status.code().unwrap_or(if status.success() { 0 } else { 1 }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_sh_path_construction() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let expected = hex_dir.join(".hex/scripts/alert/send.sh");
        assert_eq!(
            expected.to_str().unwrap(),
            "/Users/test/hex/.hex/scripts/alert/send.sh"
        );
    }
}
