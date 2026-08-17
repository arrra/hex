use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

/// check_23: HEX_DIR env var is set and points to a valid directory.
pub struct HexDirSet;

impl DoctorCheck for HexDirSet {
    fn name(&self) -> &str {
        "hex-dir-set"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        if ctx.hex_dir.as_os_str().is_empty() {
            return CheckResult::fail("HEX_DIR is not set");
        }
        if !ctx.hex_dir.is_dir() {
            return CheckResult::fail(format!("HEX_DIR={} does not exist", ctx.hex_dir.display()));
        }
        CheckResult::pass(format!("HEX_DIR={}", ctx.hex_dir.display()))
    }
}
