use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;
use std::process::Command;

/// check_21: Agent liveness — env.sh sourced, claude reachable, no failure streaks.
pub struct AgentLiveness;

impl DoctorCheck for AgentLiveness {
    fn name(&self) -> &str { "agent-liveness" }
    fn category(&self) -> Category { Category::Fleet }
    fn run(&self, ctx: &Context) -> CheckResult {
        let env_file = ctx.hex_dir.join(".hex/scripts/env.sh");
        if !env_file.is_file() {
            // env-sh check already surfaces this error — skip here to avoid double-reporting
            return CheckResult::skip("env.sh missing (reported by env-sh check)");
        }

        // Check claude is reachable via env.sh
        let claude_check = Command::new("bash")
            .args(["-c", &format!("source '{}' && command -v claude", env_file.display())])
            .output();
        match claude_check {
            Ok(o) if !o.status.success() => {
                return CheckResult::fail(
                    "claude not reachable after sourcing .hex/scripts/env.sh — check PATH in env.sh"
                );
            }
            Err(_) => {
                return CheckResult::fail("failed to source .hex/scripts/env.sh");
            }
            _ => {}
        }

        // Use hex agent list to enumerate agents and check for failure streaks
        let hex_bin = ctx.hex_dir.join(".hex/bin/hex");
        let bin = if hex_bin.is_file() { hex_bin } else { std::path::PathBuf::from("hex") };

        let list_out = Command::new(&bin)
            .args(["agent", "list"])
            .env("HEX_DIR", &ctx.hex_dir)
            .output();

        let agent_ids: Vec<String> = match list_out {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            }
            _ => {
                return CheckResult::warn("hex binary unavailable — skipping per-agent liveness checks");
            }
        };

        let total = agent_ids.len();
        let mut dead_agents: Vec<String> = Vec::new();

        for agent_id in &agent_ids {
            let log_path = ctx.hex_dir.join("projects").join(agent_id).join("log.jsonl");
            if !log_path.is_file() {
                continue;
            }
            if let Ok(content) = fs::read_to_string(&log_path) {
                let last_5: Vec<&str> = content.lines().rev().take(5).collect();
                let fail_streak = last_5.iter().take_while(|l| {
                    l.contains("\"status\":\"failed\"") || l.contains("\"status\":\"throttled\"")
                }).count();
                if fail_streak >= 5 {
                    dead_agents.push(agent_id.clone());
                }
            }
        }

        if dead_agents.is_empty() {
            CheckResult::pass(format!("all {} agents healthy (env.sh OK, no failure streaks)", total))
        } else {
            CheckResult::fail(format!("{}/{} agents dead", dead_agents.len(), total))
                .with_details(dead_agents.join("\n"))
        }
    }
}
