use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn run(hex_dir: &Path, days: u64) -> i32 {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("cleanup-projects: HOME not set");
            return 1;
        }
    };
    let projects_dir = PathBuf::from(&home).join(".claude/projects");

    let log_file = hex_dir.join(".hex/hooks/logs/cleanup-project-jsonl.log");
    if let Some(parent) = log_file.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let cutoff = SystemTime::now() - Duration::from_secs(days * 86400);
    let mut deleted: u64 = 0;
    let mut freed: u64 = 0;

    if projects_dir.is_dir() {
        collect_and_delete(&projects_dir, cutoff, 0, 2, &mut deleted, &mut freed);

        // Remove empty subdirectories at depth 1 (mirrors find -mindepth 1 -maxdepth 1 -type d -empty -delete)
        if let Ok(entries) = fs::read_dir(&projects_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = fs::remove_dir(&path);
                }
            }
        }
    }

    let ts = utc_timestamp();
    let log_line = format!(
        "{} deleted={} freed_bytes={} retention_days={}\n",
        ts, deleted, freed, days
    );
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        let _ = f.write_all(log_line.as_bytes());
    }

    println!(
        r#"{{"deleted":{},"freed_bytes":{},"retention_days":{}}}"#,
        deleted, freed, days
    );

    0
}

fn collect_and_delete(
    dir: &Path,
    cutoff: SystemTime,
    depth: usize,
    max_depth: usize,
    deleted: &mut u64,
    freed: &mut u64,
) {
    if depth > max_depth {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < max_depth {
            collect_and_delete(&path, cutoff, depth + 1, max_depth, deleted, freed);
        } else if path.is_file() {
            let is_jsonl = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "jsonl")
                .unwrap_or(false);
            if !is_jsonl {
                continue;
            }
            let mtime = match entry.metadata().and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if mtime < cutoff {
                let sz = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(&path).is_ok() {
                    *deleted += 1;
                    *freed += sz;
                }
            }
        }
    }
}

fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = secs_to_ymdhms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

fn secs_to_ymdhms(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = secs % 60;
    let mins = secs / 60;
    let mi = mins % 60;
    let hours = mins / 60;
    let h = hours % 24;
    let days = hours / 24;

    let mut y = 1970u64;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = is_leap(y);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut mo = 1u64;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        mo += 1;
    }
    let d = remaining + 1;
    (y, mo, d, h, mi, s)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    fn make_old_file(path: &Path, _days_old: u64) {
        fs::write(path, b"test data").unwrap();
        // Set mtime to 2020-01-01 00:00 — always older than any 30-day cutoff
        // touch -t format: [[CC]YY]MMDDhhmm[.SS]
        let _ = std::process::Command::new("touch")
            .args(["-t", "202001010000", path.to_str().unwrap()])
            .output();
    }

    #[test]
    fn cleanup_removes_old_jsonl_keeps_recent() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("proj-abc");
        fs::create_dir_all(&proj).unwrap();

        let old_file = proj.join("session-old.jsonl");
        make_old_file(&old_file, 40);

        let new_file = proj.join("session-new.jsonl");
        fs::write(&new_file, b"new data").unwrap();
        // recent file — mtime is now

        let cutoff = SystemTime::now() - Duration::from_secs(30 * 86400);
        let mut deleted = 0u64;
        let mut freed = 0u64;
        collect_and_delete(tmp.path(), cutoff, 0, 2, &mut deleted, &mut freed);

        assert!(!old_file.exists(), "old jsonl should be deleted");
        assert!(new_file.exists(), "recent jsonl should be kept");
        assert_eq!(deleted, 1);
        assert!(freed > 0);
    }

    #[test]
    fn cleanup_leaves_non_jsonl_untouched() {
        let tmp = TempDir::new().unwrap();
        let txt = tmp.path().join("notes.txt");
        make_old_file(&txt, 60);

        let cutoff = SystemTime::now() - Duration::from_secs(30 * 86400);
        let mut deleted = 0u64;
        let mut freed = 0u64;
        collect_and_delete(tmp.path(), cutoff, 0, 2, &mut deleted, &mut freed);

        assert!(txt.exists(), "non-jsonl must not be deleted");
        assert_eq!(deleted, 0);
    }

    #[test]
    fn cleanup_zero_deleted_when_all_files_recent() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("session.jsonl");
        fs::write(&f, b"data").unwrap();

        let cutoff = SystemTime::now() - Duration::from_secs(30 * 86400);
        let mut deleted = 0u64;
        let mut freed = 0u64;
        collect_and_delete(tmp.path(), cutoff, 0, 2, &mut deleted, &mut freed);

        assert!(f.exists());
        assert_eq!(deleted, 0);
    }
}
