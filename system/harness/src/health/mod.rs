pub mod budget_reset;

/// Port of .hex/scripts/run-health-tier.sh
/// Runs health/check-*.sh scripts for a tier, emits integrations.health.* events.
pub fn run_tier(hex_dir: &std::path::Path, tier: &str) -> i32 {
    let health_dir = hex_dir.join(".hex/scripts/health");
    let hex_emit = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".hex-events/hex_emit.py");

    let names: &[&str] = match tier {
        "critical"  => &["check-cc-connect", "check-slack-bot", "check-hex-eventd"],
        "important" => &["check-mcp-servers", "check-secrets", "check-tailscale"],
        "standard"  => &["check-kalshi"],
        _ => {
            eprintln!("Usage: hex health run-tier <critical|important|standard>");
            return 1;
        }
    };

    let mut overall = 0i32;

    for name in names {
        let script = health_dir.join(format!("{name}.sh"));
        if !script.is_file() {
            eprintln!("[WARN] health script not found: {}, skipping", script.display());
            continue;
        }

        let result = std::process::Command::new("bash")
            .arg(&script)
            .output();

        let (output, exit_code) = match result {
            Ok(o) => {
                let code = o.status.code().unwrap_or(1);
                let out = String::from_utf8_lossy(&o.stdout).to_string()
                    + &String::from_utf8_lossy(&o.stderr);
                (out, code)
            }
            Err(e) => {
                eprintln!("[ERROR] failed to run {}: {e}", script.display());
                overall = 1;
                continue;
            }
        };

        let event = if exit_code == 0 {
            "integrations.health.ok"
        } else {
            overall = 1;
            "integrations.health.failed"
        };

        // Emit event best-effort
        if hex_emit.is_file() {
            let payload = serde_json::json!({
                "integration": name,
                "check": script.to_string_lossy(),
                "output": output.trim(),
                "exit_code": exit_code,
            })
            .to_string();
            let _ = std::process::Command::new("python3")
                .arg(&hex_emit)
                .arg(event)
                .arg(&payload)
                .arg("hex:integrations-health-monitor")
                .status();
        }

        if exit_code == 0 {
            println!("[OK] {name}");
        } else {
            eprintln!("[FAIL] {name} (exit {exit_code})");
        }
    }

    overall
}

#[cfg(test)]
mod health_run_tier_tests {
    use super::*;

    #[test]
    fn unknown_tier_returns_nonzero() {
        let tmp = std::env::temp_dir().join("hex_run_tier_test");
        std::fs::create_dir_all(&tmp).ok();
        let code = run_tier(&tmp, "bogus-tier");
        assert_ne!(code, 0, "unknown tier must return nonzero");
    }
}

/// Port of .hex/scripts/health/check-agent-memory.sh
///
/// Health check for the agent memory system. Verifies:
///   1. Shared memory directory exists and is readable
///   2. SHARED.md index is present and readable
///   3. Claude projects directory is accessible
///   4. Reports counts of shared memory files and project memory dirs
use std::path::PathBuf;

pub fn check_agent_memory() {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("FAIL: HOME environment variable not set");
            std::process::exit(1);
        }
    };

    let shared_mem = PathBuf::from(&home).join(".claude/shared-memory");
    let projects_dir = PathBuf::from(&home).join(".claude/projects");

    // 1. Shared memory directory must exist
    if !shared_mem.is_dir() {
        eprintln!("FAIL: shared memory directory missing: {}", shared_mem.display());
        std::process::exit(1);
    }

    // 2. SHARED.md must be readable
    let shared_index = shared_mem.join("SHARED.md");
    if !shared_index.is_file() {
        eprintln!("FAIL: SHARED.md missing: {}", shared_index.display());
        std::process::exit(1);
    }
    let shared_content = match std::fs::read_to_string(&shared_index) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: cannot read SHARED.md: {e}");
            std::process::exit(1);
        }
    };

    // 3. Projects directory must exist and be accessible
    if !projects_dir.is_dir() {
        eprintln!("FAIL: Claude projects directory missing: {}", projects_dir.display());
        std::process::exit(1);
    }

    // 4. Count accessible memory directories (non-fatal if zero)
    let mem_dirs = count_memory_dirs(&projects_dir);

    // 5. Count shared memory files
    let shared_files = match std::fs::read_dir(&shared_mem) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".md")
            })
            .count(),
        Err(e) => {
            eprintln!("FAIL: cannot list shared memory directory: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "agent-memory: ok (SHARED.md={}B, {} shared files, {} project memory dirs)",
        shared_content.len(),
        shared_files,
        mem_dirs,
    );
}

fn count_memory_dirs(projects_dir: &std::path::Path) -> usize {
    let entries = match std::fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    entries
        .flatten()
        .filter(|e| {
            let mem = e.path().join("memory");
            mem.is_dir()
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_memory_dirs_nonexistent_returns_zero() {
        let count = count_memory_dirs(std::path::Path::new("/nonexistent/path/projects"));
        assert_eq!(count, 0, "nonexistent dir must return 0");
    }
}

/// Port of .hex/scripts/health/compute-mttd.py
///
/// Computes mean time from breakage detection to alert (minutes).
/// Queries ~/.hex-events/events.db for integrations.health.failed events
/// paired with subsequent doctor.alert / integrations.alert.sent events.
/// Falls back to median health-check interval if no failures found.
/// Returns 999 if no data is available. Always exits 0.
pub fn compute_mttd() -> i32 {
    use rusqlite::OptionalExtension;
    let home = std::env::var("HOME").unwrap_or_default();
    let db_path = PathBuf::from(&home).join(".hex-events/events.db");

    if !db_path.is_file() {
        println!("999");
        return 0;
    }

    let conn = match rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => {
            println!("999");
            return 0;
        }
    };

    let cutoff = chrono::Utc::now() - chrono::Duration::days(7);
    let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();

    // Try real MTTD from failure → alert pairs
    let failures: Vec<(i64, String)> = conn
        .prepare(
            "SELECT id, created_at FROM events \
             WHERE event_type = 'integrations.health.failed' \
               AND created_at >= ?1 \
             ORDER BY created_at ASC",
        )
        .and_then(|mut stmt| {
            stmt.query_map([&cutoff_str], |row| Ok((row.get(0)?, row.get(1)?)))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();

    if !failures.is_empty() {
        let alert_types = "('doctor.alert','integrations.alert.sent')";
        let mut mttds: Vec<f64> = Vec::new();
        for (_id, fail_ts) in &failures {
            let alert_row: Option<String> = conn
                .prepare(&format!(
                    "SELECT created_at FROM events \
                     WHERE event_type IN {alert_types} \
                       AND created_at > ?1 \
                     ORDER BY created_at ASC LIMIT 1"
                ))
                .and_then(|mut stmt| stmt.query_row([fail_ts], |r| r.get(0)).optional())
                .unwrap_or(None);

            if let Some(alert_ts) = alert_row {
                if let (Ok(t_fail), Ok(t_alert)) = (
                    chrono::DateTime::parse_from_str(
                        &format!("{fail_ts} +0000"),
                        "%Y-%m-%d %H:%M:%S %z",
                    )
                    .or_else(|_| {
                        chrono::DateTime::parse_from_rfc3339(fail_ts)
                    }),
                    chrono::DateTime::parse_from_str(
                        &format!("{alert_ts} +0000"),
                        "%Y-%m-%d %H:%M:%S %z",
                    )
                    .or_else(|_| {
                        chrono::DateTime::parse_from_rfc3339(&alert_ts)
                    }),
                ) {
                    let gap = (t_alert - t_fail).num_seconds() as f64 / 60.0;
                    if (0.0..=60.0).contains(&gap) {
                        mttds.push(gap);
                    }
                }
            }
        }
        if !mttds.is_empty() {
            println!("{}", median_f64(&mttds).round().max(1.0) as i64);
            return 0;
        }
    }

    // Fallback: estimate from health-check run frequency
    let ok_events: Vec<String> = conn
        .prepare(
            "SELECT created_at FROM events \
             WHERE event_type IN ('integrations.health.ok','integrations.health.failed') \
               AND created_at >= ?1 \
             ORDER BY created_at ASC",
        )
        .and_then(|mut stmt| {
            stmt.query_map([&cutoff_str], |row| row.get(0))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();

    if ok_events.len() < 2 {
        println!("999");
        return 0;
    }

    let times: Vec<chrono::DateTime<chrono::FixedOffset>> = ok_events
        .iter()
        .filter_map(|ts| {
            chrono::DateTime::parse_from_str(&format!("{ts} +0000"), "%Y-%m-%d %H:%M:%S %z")
                .or_else(|_| chrono::DateTime::parse_from_rfc3339(ts))
                .ok()
        })
        .collect();

    if times.len() < 2 {
        println!("999");
        return 0;
    }

    // Group into batches (events within 60s = same run)
    let mut batch_starts: Vec<chrono::DateTime<chrono::FixedOffset>> = vec![times[0]];
    for i in 1..times.len() {
        if (times[i] - times[i - 1]).num_seconds() > 60 {
            batch_starts.push(times[i]);
        }
    }

    if batch_starts.len() < 2 {
        println!("999");
        return 0;
    }

    let mut intervals: Vec<f64> = (0..batch_starts.len() - 1)
        .map(|i| (batch_starts[i + 1] - batch_starts[i]).num_seconds() as f64 / 60.0)
        .filter(|&x| x <= 120.0)
        .collect();

    if intervals.is_empty() {
        println!("999");
        return 0;
    }

    intervals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("{}", median_f64(&intervals).round().max(1.0) as i64);
    0
}

fn median_f64(values: &[f64]) -> f64 {
    let mut s = values.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    let mid = n / 2;
    if n % 2 == 1 { s[mid] } else { (s[mid - 1] + s[mid]) / 2.0 }
}

/// Port of .hex/scripts/health/check-secrets.sh
///
/// Verifies required secret files exist and are non-empty.
/// Exits 0 if all required secrets are present, 1 if any are missing.
pub fn check_secrets() -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let hex_root = std::env::var("CLAUDE_PROJECT_DIR")
        .or_else(|_| std::env::var("HEX_ROOT"))
        .unwrap_or_else(|_| format!("{home}/hex"));

    let secrets_dir = PathBuf::from(&hex_root).join(".hex/secrets");
    let hex_events_dir = PathBuf::from(&home).join(".hex-events");

    let required: &[(&str, bool)] = &[
        ("x-api.env", false),
        ("fal.env", false),
        ("openrouter.env", false),
        ("excalidraw.env", false),
    ];

    let mut missing: Vec<String> = Vec::new();

    for (filename, _) in required {
        let path = secrets_dir.join(filename);
        if !is_nonempty_file(&path) {
            missing.push(path.to_string_lossy().into_owned());
        }
    }

    // scheduler.yaml in hex-events dir
    let scheduler = hex_events_dir.join("adapters/scheduler.yaml");
    if !is_nonempty_file(&scheduler) {
        missing.push(scheduler.to_string_lossy().into_owned());
    }

    // Optional kalshi PEM: if present, must look like PEM
    let kalshi_pem = secrets_dir.join("kalshi-private.pem");
    if kalshi_pem.is_file() {
        let content = std::fs::read_to_string(&kalshi_pem).unwrap_or_default();
        if !content.contains("BEGIN") {
            missing.push(format!("{} (present but not valid PEM)", kalshi_pem.display()));
        }
    }

    if !missing.is_empty() {
        eprintln!("secrets: FAIL - missing/empty: {}", missing.join(", "));
        return 1;
    }

    println!("secrets: ok ({} required files present and non-empty)", required.len() + 1);
    0
}

fn is_nonempty_file(path: &std::path::Path) -> bool {
    path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

#[cfg(test)]
mod compute_mttd_tests {
    use super::*;

    #[test]
    fn median_f64_odd() {
        assert_eq!(median_f64(&[1.0, 3.0, 5.0]), 3.0);
    }

    #[test]
    fn median_f64_even() {
        assert_eq!(median_f64(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }
}
