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
