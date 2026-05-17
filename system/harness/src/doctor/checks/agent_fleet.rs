use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_7: Agent fleet validation — runs `hex agent fleet`.
pub struct AgentFleet;

impl DoctorCheck for AgentFleet {
    fn name(&self) -> &str { "agent-fleet" }
    fn category(&self) -> Category { Category::Fleet }
    fn run(&self, ctx: &Context) -> CheckResult {
        let hex_bin = ctx.hex_dir.join(".hex/bin/hex");
        let bin = if hex_bin.is_file() { hex_bin } else { std::path::PathBuf::from("hex") };

        let result = Command::new(&bin)
            .args(["agent", "fleet"])
            .env("HEX_DIR", &ctx.hex_dir)
            .output();

        match result {
            Ok(out) if out.status.success() => {
                CheckResult::pass("agent fleet valid")
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let details = [stdout, stderr]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                // Count lines starting with ERROR to determine severity
                let error_lines = details.lines().filter(|l| l.trim_start().starts_with("ERROR")).count();
                if error_lines > 0 {
                    CheckResult::fail(format!("agent fleet has {} charter error(s)", error_lines))
                        .with_details(details)
                } else {
                    CheckResult::warn("agent fleet has issues").with_details(details)
                }
            }
            Err(_) => CheckResult::skip("hex binary not available for fleet validation"),
        }
    }
}
