use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

/// check_env_sh: .hex/scripts/env.sh exists and is executable.
pub struct EnvSh;

impl DoctorCheck for EnvSh {
    fn name(&self) -> &str {
        "env-sh"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join(".hex/scripts/env.sh");
        if !path.exists() {
            return CheckResult::fail(
                ".hex/scripts/env.sh missing — agents have no shared environment",
            );
        }
        CheckResult::pass(".hex/scripts/env.sh present")
    }
}
