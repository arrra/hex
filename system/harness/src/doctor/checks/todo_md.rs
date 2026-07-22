use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_10: todo.md exists under HEX_DIR.
pub struct TodoMdExists;

impl DoctorCheck for TodoMdExists {
    fn name(&self) -> &str {
        "todo-md"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join("todo.md");
        if path.is_file() {
            return CheckResult::pass("todo.md exists");
        }
        if ctx.fix {
            if fs::write(&path, "# TODO\n").is_ok() {
                return CheckResult::fixed("todo.md created");
            }
        }
        CheckResult::warn("todo.md missing")
    }
}
