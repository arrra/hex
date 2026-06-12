use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_11: .hex/memory.db exists (creates via memory_index.py if missing and --fix).
pub struct MemoryDbExists;

impl DoctorCheck for MemoryDbExists {
    fn name(&self) -> &str { "memory-db" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let db = ctx.hex_dir.join(".hex/memory.db");
        if db.is_file() {
            return CheckResult::pass(".hex/memory.db exists");
        }
        if ctx.fix {
            // Try to create via memory_index.py
            let script = ctx.hex_dir.join("system/scripts/memory_index.py");
            if script.is_file() {
                let ok = Command::new("python3")
                    .arg(&script)
                    .arg("--init")
                    .env("HEX_DIR", &ctx.hex_dir)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok && db.is_file() {
                    return CheckResult::fixed(".hex/memory.db created via memory_index.py");
                }
            }
        }
        CheckResult::warn(".hex/memory.db missing — run `hex memory index --full` to create")
    }
}
