use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_6: CLAUDE.md exists and is at least 1000 bytes.
pub struct ClaudeMdExists;

impl DoctorCheck for ClaudeMdExists {
    fn name(&self) -> &str { "claude-md" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join("CLAUDE.md");
        match fs::metadata(&path) {
            Ok(meta) if meta.len() >= 1000 => {
                CheckResult::pass(format!("CLAUDE.md exists ({} bytes)", meta.len()))
            }
            Ok(meta) => CheckResult::fail(format!(
                "CLAUDE.md too small ({} bytes, need ≥1000)", meta.len()
            )),
            Err(_) => CheckResult::fail("CLAUDE.md missing"),
        }
    }
}
