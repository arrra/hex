use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_14: .hex/llm-preference file exists under HEX_SYSTEM_DIR.
pub struct LlmPreferenceExists;

impl DoctorCheck for LlmPreferenceExists {
    fn name(&self) -> &str { "llm-preference" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join(".hex/llm-preference");
        if path.is_file() {
            return CheckResult::pass(".hex/llm-preference exists");
        }
        if ctx.fix {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(&path, "claude\n").is_ok() {
                return CheckResult::fixed(".hex/llm-preference created with default");
            }
        }
        CheckResult::warn(".hex/llm-preference missing")
    }
}

/// check_15: No stale root-level llm-preference at HEX_DIR/llm-preference.
pub struct NoStaleLlmPreference;

impl DoctorCheck for NoStaleLlmPreference {
    fn name(&self) -> &str { "no-stale-llm-preference" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        let stale = ctx.hex_dir.join("llm-preference");
        if !stale.exists() {
            return CheckResult::pass("no stale root llm-preference");
        }
        if ctx.fix {
            if fs::remove_file(&stale).is_ok() {
                return CheckResult::fixed("Removed stale root llm-preference");
            }
        }
        CheckResult::warn(format!(
            "stale llm-preference at root: {} — remove it", stale.display()
        ))
    }
}
