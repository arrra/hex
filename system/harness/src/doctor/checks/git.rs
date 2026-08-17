use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_2: HEX_DIR is a git repository.
pub struct GitInitialized;

impl DoctorCheck for GitInitialized {
    fn name(&self) -> &str {
        "git-initialized"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let git_dir = ctx.hex_dir.join(".git");
        if git_dir.exists() {
            return CheckResult::pass(".git/ initialized");
        }
        // Fallback: try `git rev-parse`
        let ok = Command::new("git")
            .arg("rev-parse")
            .arg("--git-dir")
            .current_dir(&ctx.hex_dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            CheckResult::pass(".git/ initialized (worktree)")
        } else {
            CheckResult::fail(".git/ missing — run `git init` to fix")
        }
    }
}
