use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::thread;
use std::time::Duration;

const TIMEOUT_SECS: u64 = 300;

pub fn run(hex_dir: &Path) -> i32 {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("[introspection] ERROR: HOME not set");
            return 1;
        }
    };
    let claude_bin = PathBuf::from(&home).join(".local/bin/claude");

    let report_dir = hex_dir.join("raw/research/introspection");
    let log_dir = hex_dir.join("raw/research/introspection/logs");
    if let Err(e) = fs::create_dir_all(&report_dir) {
        eprintln!("[introspection] ERROR: cannot create report dir: {e}");
        return 1;
    }
    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("[introspection] ERROR: cannot create log dir: {e}");
        return 1;
    }

    // Mirror the shell script's TZ setup: if TZ unset, read from .hex/timezone
    if std::env::var("TZ").is_err() {
        let tz_file = hex_dir.join(".hex/timezone");
        if tz_file.exists() {
            if let Ok(tz) = fs::read_to_string(&tz_file) {
                let tz = tz.split_whitespace().collect::<String>();
                if !tz.is_empty() {
                    std::env::set_var("TZ", &tz);
                }
            }
        }
    }

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let report_path = report_dir.join(format!("{date}.md"));
    let error_log = log_dir.join(format!("{date}.err.log"));
    let report_tmp = report_dir.join(format!("{date}.md.tmp"));

    if !claude_bin.exists() {
        let msg = format!(
            "[introspection] ERROR: claude binary not found at {}",
            claude_bin.display()
        );
        log_err(&error_log, &msg);
        return 1;
    }

    let prompt = build_prompt(hex_dir);

    let err_log_file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&error_log)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[introspection] ERROR: cannot open error log: {e}");
            return 1;
        }
    };

    let stdout_file = match fs::File::create(&report_tmp) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[introspection] ERROR: cannot create tmp file: {e}");
            return 1;
        }
    };

    let mut child = match Command::new(&claude_bin)
        .args(["--dangerously-skip-permissions", "-p", &prompt])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(err_log_file))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("[introspection] ERROR: failed to spawn claude: {e}");
            log_err(&error_log, &msg);
            return 1;
        }
    };

    let child_pid = child.id();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_clone = Arc::clone(&timed_out);
    let (done_tx, done_rx) = mpsc::channel::<()>();

    let watchdog = thread::spawn(move || {
        match done_rx.recv_timeout(Duration::from_secs(TIMEOUT_SECS)) {
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = Command::new("kill")
                    .args(["-9", &child_pid.to_string()])
                    .output();
                timed_out_clone.store(true, Ordering::SeqCst);
            }
            Err(_) => {}
        }
    });

    let claude_exit = child.wait();
    let _ = done_tx.send(());
    let _ = watchdog.join();

    if timed_out.load(Ordering::SeqCst) {
        let msg = format!(
            "[introspection] ERROR: claude timed out after {TIMEOUT_SECS}s"
        );
        log_err(&error_log, &msg);
        let _ = fs::remove_file(&report_tmp);
        return 1;
    }

    let exit_code = match claude_exit {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            let msg = format!("[introspection] ERROR: wait failed: {e}");
            log_err(&error_log, &msg);
            let _ = fs::remove_file(&report_tmp);
            return 1;
        }
    };

    if exit_code != 0 {
        let msg = format!("[introspection] ERROR: claude exited with code {exit_code}");
        log_err(&error_log, &msg);
        let _ = fs::remove_file(&report_tmp);
        return 1;
    }

    // Validate report has meaningful content (>50 non-whitespace chars)
    let content = match fs::read_to_string(&report_tmp) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("[introspection] ERROR: cannot read tmp report: {e}");
            log_err(&error_log, &msg);
            let _ = fs::remove_file(&report_tmp);
            return 1;
        }
    };
    let non_ws_count = content.chars().filter(|c| !c.is_whitespace()).count();
    if non_ws_count < 50 {
        let msg = format!(
            "[introspection] ERROR: report has insufficient content ({non_ws_count} non-whitespace chars)"
        );
        log_err(&error_log, &msg);
        let _ = fs::remove_file(&report_tmp);
        return 1;
    }

    if let Err(e) = fs::rename(&report_tmp, &report_path) {
        let msg = format!("[introspection] ERROR: cannot move report into place: {e}");
        log_err(&error_log, &msg);
        return 1;
    }

    let issues = count_issues(&report_path);
    println!(
        "[introspection] Report written: {} ({} issues)",
        report_path.display(),
        issues
    );

    0
}

fn build_prompt(hex_dir: &Path) -> String {
    let d = hex_dir.display();
    format!(
        "You are hex's system introspection agent. Perform a thorough nightly audit. \
Your working directory is {d}.

Audit checklist:
1. Log Audit: Read the last 50 lines of the 3 most recent files in ~/.boi/logs/. \
Flag errors, crashes, recurring warnings.
2. Repo Hygiene: For each repo in ~/github.com/mrap/ (hex, hex-core, boi), \
run git status --short. Flag uncommitted changes.
3. Daemon Health: Run bash {d}/.hex/scripts/hex-daemons.sh status. Flag daemons down.
4. Research Feed Health: Check {d}/raw/research/bookmarks/ and {d}/raw/research/scout/ \
for files modified in last 48h.
5. Stale Work: Check {d}/todo.md for items with stale since older than 30 days.

Output a markdown report with: date, issues found (CRITICAL/WARNING/INFO), \
recommendations, and a health score 1-10. Be concise — max 100 lines."
    )
}

fn log_err(error_log: &Path, msg: &str) {
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(error_log)
    {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
    eprintln!("{msg}");
}

fn count_issues(report_path: &Path) -> usize {
    let content = match fs::read_to_string(report_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    content
        .lines()
        .filter(|line| line.contains("CRITICAL") || line.contains("WARNING"))
        .count()
}
