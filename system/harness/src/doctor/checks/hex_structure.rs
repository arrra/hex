use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_1: .hex/ directory exists under HEX_DIR.
pub struct HexExists;

impl DoctorCheck for HexExists {
    fn name(&self) -> &str {
        "hex-dir-exists"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let hex = ctx.hex_dir.join(".hex");
        if hex.is_dir() {
            return CheckResult::pass(".hex/ exists");
        }
        if ctx.fix {
            if fs::create_dir_all(&hex).is_ok() {
                return CheckResult::fixed(".hex/ created");
            }
        }
        CheckResult::fail(".hex/ directory missing — run bootstrap to fix")
    }
}

/// check_3: .hex/skills/ directory exists.
pub struct HexSkillsExists;

impl DoctorCheck for HexSkillsExists {
    fn name(&self) -> &str {
        "hex-skills-dir"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let skills = ctx.hex_dir.join(".hex/skills");
        if skills.is_dir() {
            CheckResult::pass(".hex/skills/ exists")
        } else {
            CheckResult::fail(".hex/skills/ missing — re-run bootstrap to fix")
        }
    }
}

/// check_4: .hex/skills/ has at least one skill entry.
pub struct HexSkillsPopulated;

impl DoctorCheck for HexSkillsPopulated {
    fn name(&self) -> &str {
        "hex-skills-populated"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let skills = ctx.hex_dir.join(".hex/skills");
        if !skills.is_dir() {
            return CheckResult::skip(".hex/skills/ does not exist");
        }
        match fs::read_dir(&skills) {
            Ok(entries) => {
                let count = entries.filter_map(|e| e.ok()).count();
                if count > 0 {
                    CheckResult::pass(format!(".hex/skills/ has {} skill(s)", count))
                } else {
                    CheckResult::fail(".hex/skills/ is empty — no skills found")
                }
            }
            Err(e) => CheckResult::fail(format!("cannot read .hex/skills/: {}", e)),
        }
    }
}
