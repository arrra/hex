use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_18: Python 3.10+ is available on PATH.
pub struct PythonVersion;

impl DoctorCheck for PythonVersion {
    fn name(&self) -> &str { "python-version" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, _ctx: &Context) -> CheckResult {
        let out = Command::new("python3")
            .arg("--version")
            .output();

        match out {
            Ok(o) if o.status.success() => {
                let ver = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // Parse "Python X.Y.Z"
                if let Some(ver_str) = ver.strip_prefix("Python ") {
                    let parts: Vec<u32> = ver_str
                        .split('.')
                        .filter_map(|p| p.parse().ok())
                        .collect();
                    if parts.len() >= 2 && (parts[0] > 3 || (parts[0] == 3 && parts[1] >= 10)) {
                        return CheckResult::pass(format!("python3 {} (≥3.10)", ver_str));
                    }
                    return CheckResult::fail(format!(
                        "python3 {} is too old — need 3.10+", ver_str
                    ));
                }
                CheckResult::pass(format!("python3 present: {}", ver))
            }
            Ok(_) => CheckResult::fail("python3 --version failed"),
            Err(_) => CheckResult::fail("python3 not found on PATH"),
        }
    }
}
