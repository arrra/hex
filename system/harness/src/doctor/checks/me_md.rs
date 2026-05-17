use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_9: me/me.md has meaningful content (>100 bytes).
pub struct MeMdContent;

impl DoctorCheck for MeMdContent {
    fn name(&self) -> &str { "me-md" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join("me/me.md");
        match fs::metadata(&path) {
            Ok(meta) if meta.len() > 100 => {
                CheckResult::info(format!("me/me.md present ({} bytes)", meta.len()))
            }
            Ok(meta) => CheckResult::info(format!(
                "me/me.md exists but is small ({} bytes) — consider populating it", meta.len()
            )),
            Err(_) => CheckResult::info("me/me.md not found — consider creating it"),
        }
    }
}
