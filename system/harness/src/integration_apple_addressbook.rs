/// Port of .hex/scripts/integrations/apple-addressbook.sh
/// Probes Apple Contacts via osascript to verify TCC access is granted.
/// Requires: System Preferences > Privacy > Contacts → allow Terminal/claude
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const INTEGRATION: &str = "apple-addressbook";
const PROBE_QUERY: &str = "Mike";

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Format as ISO-8601 UTC (seconds precision, no chrono dep)
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Approximate date from days since epoch (good enough for log output)
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

fn count_contacts(query: &str) -> Option<u32> {
    let applescript = format!(
        r#"tell application "Contacts"
  set matches to (every person whose name contains "{}")
  return count of matches
end tell"#,
        query
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&applescript)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout.trim().parse::<u32>().ok()
}

pub fn run_probe() -> i32 {
    println!("[{}/probe] searching contacts for '{}'...", INTEGRATION, PROBE_QUERY);

    let count = count_contacts(PROBE_QUERY).unwrap_or(0);

    if count > 0 {
        emit_event(
            "hex.integration.apple-addressbook.probe_ok",
            "ok",
            &format!("found {} contact(s) matching '{}'", count, PROBE_QUERY),
        );
        println!("[{}/probe] OK ({} contacts found)", INTEGRATION, count);
        0
    } else {
        emit_event(
            "hex.integration.apple-addressbook.probe_fail",
            "fail",
            "no contacts found — TCC grant may be missing (see A-3)",
        );
        eprintln!(
            "[{}/probe] FAIL: 0 contacts found — check TCC grant (A-3)",
            INTEGRATION
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_shell_script() {
        assert_eq!(INTEGRATION, "apple-addressbook");
        assert_eq!(PROBE_QUERY, "Mike");
    }

    #[test]
    fn now_rfc3339_has_expected_format() {
        let ts = now_rfc3339();
        // Must look like YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "timestamp len: {}", ts);
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
    }
}
