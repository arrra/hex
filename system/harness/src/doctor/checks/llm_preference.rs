use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// Legacy `.hex/llm-preference` placeholder check.
///
/// The placeholder file is no longer read by anything in the harness — per-use-case
/// LLM configuration lives in `$HEX_DIR/.hex/config/llm.toml` (see `llm_config`
/// module and the `llm-config` doctor check). This check is preserved for
/// historical reasons but is intentionally a no-op: it never creates the file,
/// even when `fix=true`, and reports an informational/pass status. The
/// `StaleLlmPreferenceCheck` (in `llm_config.rs`) warns if the placeholder is
/// still on disk and offers to remove it.
pub struct LlmPreferenceExists;

impl DoctorCheck for LlmPreferenceExists {
    fn name(&self) -> &str {
        "llm-preference"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let _ = &ctx.hex_dir; // silence unused warning; behavior is intentionally a no-op
        CheckResult::pass("llm-preference placeholder no longer used (see llm-config check)")
    }
}

/// check_15: No stale root-level llm-preference at HEX_DIR/llm-preference.
pub struct NoStaleLlmPreference;

impl DoctorCheck for NoStaleLlmPreference {
    fn name(&self) -> &str {
        "no-stale-llm-preference"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let stale = ctx.hex_dir.join("llm-preference");
        if !stale.exists() {
            return CheckResult::pass("no stale root llm-preference");
        }
        if ctx.fix && fs::remove_file(&stale).is_ok() {
            return CheckResult::fixed("Removed stale root llm-preference");
        }
        CheckResult::warn(format!(
            "stale llm-preference at root: {} — remove it",
            stale.display()
        ))
    }
}
