use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_7: Agent fleet validation — runs `hex fleet validate`.
pub struct AgentFleet;

impl DoctorCheck for AgentFleet {
    fn name(&self) -> &str { "agent-fleet" }
    fn category(&self) -> Category { Category::Fleet }
    fn run(&self, ctx: &Context) -> CheckResult {
        // Delegate to `hex fleet validate` if hex is available
        let hex_bin = ctx.hex_dir.join(".hex/bin/hex");
        let bin = if hex_bin.is_file() { hex_bin } else { std::path::PathBuf::from("hex") };

        let result = Command::new(&bin)
            .args(["fleet", "validate"])
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
                CheckResult::warn("agent fleet has issues").with_details(details)
            }
            Err(_) => CheckResult::skip("hex binary not available for fleet validation"),
        }
    }
}
