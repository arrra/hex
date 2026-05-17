use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;
use std::process::Command;

/// check_21: Detect per-agent failure streaks via `hex agent liveness` or log scan.
pub struct AgentLiveness;

impl DoctorCheck for AgentLiveness {
    fn name(&self) -> &str { "agent-liveness" }
    fn category(&self) -> Category { Category::Fleet }
    fn run(&self, ctx: &Context) -> CheckResult {
        // Try `hex agent liveness` first
        let hex_bin = ctx.hex_dir.join(".hex/bin/hex");
        let bin = if hex_bin.is_file() { hex_bin } else { std::path::PathBuf::from("hex") };

        let result = Command::new(&bin)
            .args(["agent", "liveness"])
            .env("HEX_DIR", &ctx.hex_dir)
            .output();

        match result {
            Ok(out) if out.status.success() => {
                CheckResult::pass("all agents live (no failure streaks)")
            }
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                let details = [stdout, stderr]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if out.status.code() == Some(2) {
                    CheckResult::fail("agent failure streak detected").with_details(details)
                } else {
                    CheckResult::warn("agent liveness issues").with_details(details)
                }
            }
            Err(_) => {
                // Fallback: scan log.jsonl files for recent failures
                self.scan_logs(ctx)
            }
        }
    }
}

impl AgentLiveness {
    fn scan_logs(&self, ctx: &Context) -> CheckResult {
        let projects_dir = ctx.hex_dir.join("projects");
        if !projects_dir.is_dir() {
            return CheckResult::skip("hex binary not available and no projects/ dir to scan");
        }

        let mut streak_agents: Vec<String> = Vec::new();

        if let Ok(entries) = fs::read_dir(&projects_dir) {
            for entry in entries.flatten() {
                let log = entry.path().join("log.jsonl");
                if !log.is_file() {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&log) {
                    let failures: Vec<_> = content
                        .lines()
                        .rev()
                        .take(10)
                        .filter(|l| l.contains("\"status\":\"failed\""))
                        .collect();
                    if failures.len() >= 3 {
                        streak_agents.push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
            }
        }

        if streak_agents.is_empty() {
            CheckResult::pass("no failure streaks detected (log scan)")
        } else {
            CheckResult::warn(format!(
                "{} agent(s) with failure streaks", streak_agents.len()
            )).with_details(streak_agents.join("\n"))
        }
    }
}
