//! Startup sweep for orphaned distill children. `claude -p` extract calls run
//! in their own process group (claude_cli.rs — needed for timeout-kill), so a
//! launchd kill of the harness group orphans them to PID 1 where the
//! parent-enforced timeout no longer exists (observed: PID 14882 alive 2h+
//! after its parent died, 2026-06-11). Pidfiles make them findable; serve
//! startup reaps them.

use std::path::{Path, PathBuf};

pub struct SweepReport {
    pub killed: u32,
    pub removed_stale: u32,
}

pub fn run_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex/run/distill")
}

pub fn pid_from_filename(name: &str) -> Option<i32> {
    name.strip_prefix("distill-")?
        .strip_suffix(".pid")?
        .parse()
        .ok()
}

fn alive(pid: i32) -> bool {
    // kill(pid, 0): signal 0 probes existence without sending anything
    unsafe { libc::kill(pid, 0) == 0 }
}

fn orphaned(pid: i32) -> bool {
    // macOS: ppid via `ps`; ppid==1 means reparented to launchd
    std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i32>().ok())
        .map(|ppid| ppid == 1)
        .unwrap_or(false)
}

pub fn sweep(hex_dir: &Path) -> SweepReport {
    let mut report = SweepReport { killed: 0, removed_stale: 0 };
    let dir = run_dir(hex_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else { return report };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = pid_from_filename(&name) else { continue };
        if alive(pid) && orphaned(pid) {
            eprintln!("reaper: killing orphaned distill child pid={pid} (pgid kill)");
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
            report.killed += 1;
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "reaper".into(),
                event: "reaper::killed-orphan".into(),
                status: "ok".into(),
                duration_ms: None,
                exit_code: None,
                detail: Some(format!("pid={pid}")),
            });
        } else if !alive(pid) {
            report.removed_stale += 1;
        } else {
            continue; // alive with a live parent — in-flight, leave it alone
        }
        let _ = std::fs::remove_file(entry.path());
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_pidfile_name() {
        assert_eq!(pid_from_filename("distill-12345.pid"), Some(12345));
        assert_eq!(pid_from_filename("garbage.txt"), None);
    }
    #[test]
    fn sweep_removes_stale_pidfiles() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join(".hex/run/distill");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("distill-999999.pid"), b"").unwrap(); // dead pid
        let report = sweep(tmp.path());
        assert_eq!(report.removed_stale, 1);
        assert_eq!(report.killed, 0);
        assert!(std::fs::read_dir(&run_dir).unwrap().next().is_none());
    }
}
