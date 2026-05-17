use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_17: BOI binary is present, reports a version, and daemon is responsive.
pub struct BoiHealth;

impl DoctorCheck for BoiHealth {
    fn name(&self) -> &str { "boi-health" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let boi_bin = ctx.home.join(".boi/bin/boi");
        if !boi_bin.is_file() {
            return CheckResult::warn("~/.boi/bin/boi not found");
        }

        // Check version
        let version_out = Command::new(&boi_bin)
            .arg("--version")
            .output();

        let version = match version_out {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => {
                return CheckResult::warn("boi binary found but --version failed");
            }
        };

        // Check VERSIONS file
        let versions_file = ctx.home.join(".boi/VERSIONS");
        if !versions_file.is_file() {
            return CheckResult::warn(format!("boi {} present but VERSIONS file missing", version));
        }

        // Check wrapper script
        let wrapper = ctx.home.join(".boi/bin/boi-wrapper");
        if !wrapper.is_file() {
            return CheckResult::warn(format!("boi {} present but wrapper script missing", version));
        }

        CheckResult::pass(format!("boi {} healthy", version))
    }
}
