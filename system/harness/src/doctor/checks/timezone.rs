use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;
use std::process::Command;

/// check_20: .hex/timezone contains a valid TZ identifier.
pub struct TimezoneValid;

impl DoctorCheck for TimezoneValid {
    fn name(&self) -> &str {
        "timezone-valid"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let path = ctx.hex_dir.join(".hex/timezone");
        let tz = match fs::read_to_string(&path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => return CheckResult::warn(".hex/timezone missing"),
        };

        if tz.is_empty() {
            return CheckResult::warn(".hex/timezone is empty");
        }

        // Validate by checking the zoneinfo database
        let zoneinfo = std::path::Path::new("/usr/share/zoneinfo").join(&tz);
        if zoneinfo.exists() {
            return CheckResult::pass(format!(".hex/timezone={} (valid)", tz));
        }

        // Fallback: try `date` command with TZ env
        let ok = Command::new("date")
            .env("TZ", &tz)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if ok {
            CheckResult::pass(format!(".hex/timezone={} (valid)", tz))
        } else {
            CheckResult::warn(format!(".hex/timezone={} may be invalid", tz))
        }
    }
}
