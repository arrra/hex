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
    /// Kill attempts whose signal could NOT be delivered (killpg AND the
    /// plain-kill fallback both failed). Recorded as telemetry errors; the
    /// pidfile is kept so the next sweep retries.
    pub kill_failed: u32,
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
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<i32>()
                .ok()
        })
        .map(|ppid| ppid == 1)
        .unwrap_or(false)
}

/// Full command line of `pid` via `ps -o command=`. `None` when the process
/// is gone or `ps` fails.
fn command_line(pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Identity gate before any kill. Pidfiles can outlive their process across
/// reboots/downtime (PidfileGuard::Drop never runs when the harness group is
/// SIGKILLed), and the recorded PID may have been recycled by an unrelated
/// process. The orphan check alone cannot distinguish — on macOS virtually
/// every launchd-spawned daemon has ppid==1. claude_cli only ever pidfiles
/// `claude -p` children, so the candidate's command line must mention
/// `claude` (covers both a native binary and `node …/claude` shims).
fn is_claude_child(pid: i32) -> bool {
    command_line(pid)
        .map(|c| c.contains("claude"))
        .unwrap_or(false)
}

pub fn sweep(hex_dir: &Path) -> SweepReport {
    let mut report = SweepReport {
        killed: 0,
        removed_stale: 0,
        kill_failed: 0,
    };
    let dir = run_dir(hex_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return report;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = pid_from_filename(&name) else {
            continue;
        };
        if !alive(pid) {
            report.removed_stale += 1;
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        if !orphaned(pid) {
            continue; // alive with a live parent — in-flight, leave it alone
        }
        if !is_claude_child(pid) {
            // PID reuse: the pidfile is stale and its PID now belongs to an
            // unrelated process — killing it (or its group) would take out an
            // innocent. Loud, then treat the pidfile as stale.
            let cmd = command_line(pid).unwrap_or_else(|| "<gone>".into());
            eprintln!(
                "reaper: pidfile {name} points at non-claude pid={pid} ({cmd}) — \
                 PID reuse, skipping kill"
            );
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "reaper".into(),
                event: "reaper::skipped-pid-reuse".into(),
                status: "ok".into(),
                duration_ms: None,
                exit_code: None,
                detail: Some(format!("pid={pid} command={cmd}")),
            });
            report.removed_stale += 1;
            let _ = std::fs::remove_file(entry.path());
            continue;
        }

        eprintln!("reaper: killing orphaned distill child pid={pid} (pgid kill)");
        // claude_cli sets process_group(0) on the child, so pid == pgid for
        // genuine distill children; killpg reaps any helpers it forked. If
        // killpg fails (e.g. the child never became a group leader), fall
        // back to a plain kill(pid) — mirrors claude_cli::kill_process_tree.
        // The rc is checked: a kill that was not delivered must NOT be
        // recorded as a success (S6 — no false-success telemetry).
        let pg_rc = unsafe { libc::killpg(pid, libc::SIGKILL) };
        let (delivered, via) = if pg_rc == 0 {
            (true, "killpg")
        } else {
            let kill_rc = unsafe { libc::kill(pid, libc::SIGKILL) };
            (kill_rc == 0, "kill-fallback")
        };
        if delivered {
            report.killed += 1;
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "reaper".into(),
                event: "reaper::killed-orphan".into(),
                status: "ok".into(),
                duration_ms: None,
                exit_code: None,
                detail: Some(format!("pid={pid} via={via}")),
            });
            let _ = std::fs::remove_file(entry.path());
        } else {
            let err = std::io::Error::last_os_error();
            report.kill_failed += 1;
            eprintln!("reaper: kill FAILED for orphaned distill child pid={pid}: {err}");
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "reaper".into(),
                event: "reaper::killed-orphan".into(),
                status: "error".into(),
                duration_ms: None,
                exit_code: None,
                detail: Some(format!("pid={pid} kill failed: {err}")),
            });
            // Keep the pidfile: the next sweep retries (or clears it as
            // stale once the pid is gone).
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a command line via `sh -c "<cmd> & echo $!"` so the child is
    /// immediately orphaned (sh exits without waiting; the child reparents to
    /// PID 1). Returns the orphan's pid once `orphaned()` confirms ppid==1.
    fn spawn_orphan(cmd: &str) -> i32 {
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("{cmd} >/dev/null 2>&1 & echo $!")])
            .output()
            .expect("spawn sh");
        let pid: i32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("inner pid on stdout");
        let mut waited_ms = 0u32;
        while !orphaned(pid) && waited_ms < 5_000 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            waited_ms += 50;
        }
        assert!(
            orphaned(pid),
            "test setup: child {pid} never orphaned to ppid==1"
        );
        pid
    }

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

    /// Review-fix 2026-06-11 (PID-reuse hazard): a stale pidfile whose PID was
    /// recycled by an unrelated process (alive, ppid==1 — which on macOS
    /// matches virtually every daemon) must NOT be killed. The sweep must
    /// verify process identity before any signal.
    #[test]
    fn sweep_spares_recycled_pid_pointing_at_innocent_process() {
        // isolate(): telemetry rows from the sweep must land in a temp
        // HEX_DIR, never the production events.db (review finding 7).
        let (hex_tmp, _guard) = crate::telemetry::test_support::isolate();
        let run_dir = run_dir(hex_tmp.path());
        std::fs::create_dir_all(&run_dir).unwrap();

        // An orphaned `sleep` stands in for the innocent process that
        // happens to hold a recycled PID recorded in a stale pidfile.
        let pid = spawn_orphan("sleep 30");
        std::fs::write(run_dir.join(format!("distill-{pid}.pid")), b"").unwrap();

        let report = sweep(hex_tmp.path());

        assert_eq!(
            report.killed, 0,
            "an innocent recycled PID must not be killed"
        );
        assert!(
            alive(pid),
            "the innocent process must survive the sweep (PID-reuse guard)"
        );
        assert_eq!(
            report.removed_stale, 1,
            "the stale pidfile is cleared without killing anyone"
        );
        assert!(std::fs::read_dir(&run_dir).unwrap().next().is_none());

        unsafe { libc::kill(pid, libc::SIGKILL) }; // cleanup
    }

    /// Review-fix 2026-06-11 (false-success kill): the sweep must VERIFY the
    /// kill. Here the orphan is a genuine claude-named child but NOT a
    /// process-group leader, so `killpg(pid, …)` fails (ESRCH) — the old code
    /// ignored the rc, recorded "killed-orphan ok", and left the process
    /// running. The fix falls back to a plain `kill(pid)` and only counts a
    /// kill whose signal was actually delivered.
    #[test]
    fn sweep_kills_orphaned_claude_child_via_fallback_and_verifies() {
        let (hex_tmp, _guard) = crate::telemetry::test_support::isolate();
        let run_dir = run_dir(hex_tmp.path());
        std::fs::create_dir_all(&run_dir).unwrap();

        // A fake `claude` executable (identity check matches on the command
        // line containing "claude") that just sleeps.
        let bin_dir = hex_tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let fake = bin_dir.join("claude");
        std::fs::write(&fake, "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let pid = spawn_orphan(&format!("'{}'", fake.display()));
        std::fs::write(run_dir.join(format!("distill-{pid}.pid")), b"").unwrap();

        let report = sweep(hex_tmp.path());

        assert_eq!(report.killed, 1, "the orphaned claude child must be killed");
        // SIGKILL delivery is asynchronous but near-immediate; poll briefly.
        let mut waited_ms = 0u32;
        while alive(pid) && waited_ms < 5_000 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            waited_ms += 50;
        }
        assert!(
            !alive(pid),
            "the orphan must actually be dead — a recorded kill that left the \
             process running is the false-success bug"
        );
        assert!(std::fs::read_dir(&run_dir).unwrap().next().is_none());
    }
}
