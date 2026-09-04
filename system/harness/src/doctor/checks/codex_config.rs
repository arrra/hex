use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_8: .codex/config.toml exists.
pub struct CodexConfigExists;

impl DoctorCheck for CodexConfigExists {
    fn name(&self) -> &str {
        "codex-config"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join(".codex/config.toml");
        if path.is_file() {
            return CheckResult::pass(".codex/config.toml exists");
        }
        if ctx.fix {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            // Write a minimal stub
            let stub = "[model]\nname = \"o4-mini\"\n";
            if fs::write(&path, stub).is_ok() {
                return CheckResult::fixed(".codex/config.toml created (stub)");
            }
        }
        CheckResult::warn(".codex/config.toml missing")
    }
}
