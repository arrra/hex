/// Port of .hex/scripts/hex-integration-check-all.sh
/// Runs integration checks for a given tier from the integrations manifest.
use std::path::Path;

pub fn run(hex_dir: &Path, tier: &str) -> i32 {
    // Try legacy script first (preferred — preserves flock + xargs -P logic)
    let legacy = hex_dir.join("system/scripts/hex-integration-check-all.sh.legacy.sh");
    let primary = hex_dir.join("system/scripts/hex-integration-check-all.sh");
    let script = if legacy.is_file() { legacy } else { primary };

    if script.is_file() {
        let mut cmd = std::process::Command::new("bash");
        cmd.arg(&script);
        if tier != "all" {
            cmd.arg("--tier").arg(tier);
        }
        cmd.env("HEX_DIR", hex_dir);
        let status = cmd.status().unwrap_or_else(|e| {
            eprintln!("hex integration check-all: failed to exec script: {e}");
            std::process::exit(1);
        });
        return status.code().unwrap_or(1);
    }

    // Fallback: no script available
    eprintln!("hex integration check-all: script not found at {}", script.display());
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_nonzero_when_no_script() {
        let tmp = std::env::temp_dir().join("hex_check_all_test");
        std::fs::create_dir_all(&tmp).ok();
        let code = run(&tmp, "all");
        assert_ne!(code, 0);
    }
}
