use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_22: `hex` binary is on PATH and functional.
pub struct HexBinaryOnPath;

impl DoctorCheck for HexBinaryOnPath {
    fn name(&self) -> &str { "hex-binary-on-path" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        // Check if hex is on PATH
        let which = Command::new("which").arg("hex").output();

        match which {
            Ok(out) if out.status.success() => {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                // Verify it's the right one (from HEX_DIR)
                let expected = ctx.hex_dir.join(".hex/bin/hex");
                if expected.is_file() && path != expected.display().to_string() {
                    return CheckResult::warn(format!(
                        "hex found at {} but expected {}", path, expected.display()
                    ));
                }
                CheckResult::pass(format!("hex on PATH: {}", path))
            }
            Ok(_) => {
                // Not on PATH — check if it exists in the expected location
                let expected = ctx.hex_dir.join(".hex/bin/hex");
                if expected.is_file() {
                    CheckResult::warn(format!(
                        "hex exists at {} but is not on PATH", expected.display()
                    ))
                } else {
                    CheckResult::fail("hex binary not found on PATH")
                }
            }
            Err(_) => CheckResult::warn("cannot check PATH for hex binary"),
        }
    }
}
