use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_19: .hex/settings.json exists and is valid JSON.
pub struct SettingsJsonValid;

impl DoctorCheck for SettingsJsonValid {
    fn name(&self) -> &str {
        "settings-json"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join(".hex/settings.json");
        if !path.is_file() {
            return CheckResult::warn(".hex/settings.json missing");
        }
        match fs::read_to_string(&path) {
            Ok(content) => {
                if serde_json::from_str::<serde_json::Value>(&content).is_ok() {
                    CheckResult::pass(".hex/settings.json is valid JSON")
                } else {
                    CheckResult::warn(".hex/settings.json is not valid JSON")
                }
            }
            Err(e) => CheckResult::warn(format!("cannot read .hex/settings.json: {}", e)),
        }
    }
}
