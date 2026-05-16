/// Port of .hex/scripts/doctor-checks/codex.sh
///
/// Codex CLI + config health checks. Verifies:
///   1. codex binary on PATH
///   2. codex --version exits 0
///   3. OPENAI_API_KEY is set (env or ~/.hex-test.env)
///   4. AGENTS.md exists at $HEX_DIR/AGENTS.md
///   5. AGENTS.md contains all required sections
use std::path::PathBuf;
use std::process::Command;

const AGENTS_MD_REQUIRED_SECTIONS: &[&str] = &[
    "Standing Orders",
    "Session Lifecycle",
    "BOI",
    "Memory",
];

fn pass(msg: &str) {
    println!("PASS  {}", msg);
}

fn warn(msg: &str) {
    println!("WARN  {}", msg);
}

fn error(msg: &str) {
    eprintln!("ERROR {}", msg);
}

fn info(msg: &str) {
    println!("INFO  {}", msg);
}

fn codex_on_path() -> bool {
    Command::new("which")
        .arg("codex")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn check_codex_1(failed: &mut bool) {
    if let Ok(output) = Command::new("which").arg("codex").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            pass(&format!("codex.cli-on-path: found at {}", path));
            return;
        }
    }
    error("codex.cli-on-path: codex CLI not found — install with: npm install -g @openai/codex");
    *failed = true;
}

fn check_codex_2(failed: &mut bool) {
    if !codex_on_path() {
        info("codex.version-ok: skipped (codex not on PATH)");
        return;
    }
    match Command::new("codex").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            pass(&format!("codex.version-ok: {}", ver));
        }
        _ => {
            error("codex.version-ok: 'codex --version' exited non-zero — fix: reinstall codex CLI");
            *failed = true;
        }
    }
}

fn check_codex_3(failed: &mut bool) {
    if std::env::var("OPENAI_API_KEY").map(|v| !v.is_empty()).unwrap_or(false) {
        pass("codex.api-key: OPENAI_API_KEY found via environment variable");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let hex_test_env = PathBuf::from(&home).join(".hex-test.env");
    if hex_test_env.is_file() {
        if let Ok(text) = std::fs::read_to_string(&hex_test_env) {
            if text.lines().any(|l| l.starts_with("OPENAI_API_KEY=")) {
                pass("codex.api-key: OPENAI_API_KEY found via ~/.hex-test.env");
                return;
            }
        }
    }
    warn("codex.api-key: OPENAI_API_KEY not set — Codex will fail at runtime");
    info("  Fix: export OPENAI_API_KEY=sk-... in ~/.hex/scripts/env.sh or add to ~/.hex-test.env");
    *failed = true;
}

fn check_codex_4(hex_dir: &std::path::Path, failed: &mut bool) {
    let agents_md = hex_dir.join("AGENTS.md");
    if agents_md.is_file() {
        let size = std::fs::metadata(&agents_md).map(|m| m.len()).unwrap_or(0);
        pass(&format!("codex.agents-md-exists: AGENTS.md found ({} bytes)", size));
    } else {
        warn(&format!(
            "codex.agents-md-exists: AGENTS.md missing at {}",
            agents_md.display()
        ));
        info("  Fix: create AGENTS.md — Codex reads this as its primary instruction file");
        *failed = true;
    }
}

fn check_codex_5(hex_dir: &std::path::Path, failed: &mut bool) {
    let agents_md = hex_dir.join("AGENTS.md");
    if !agents_md.is_file() {
        info("codex.agents-md-complete: skipped (AGENTS.md not present)");
        return;
    }
    let text = match std::fs::read_to_string(&agents_md) {
        Ok(t) => t.to_lowercase(),
        Err(e) => {
            error(&format!("codex.agents-md-complete: cannot read AGENTS.md: {e}"));
            *failed = true;
            return;
        }
    };
    let missing: Vec<&str> = AGENTS_MD_REQUIRED_SECTIONS
        .iter()
        .filter(|&&section| !text.contains(&section.to_lowercase()))
        .copied()
        .collect();
    if missing.is_empty() {
        pass("codex.agents-md-complete: all required sections present");
    } else {
        warn(&format!(
            "codex.agents-md-complete: AGENTS.md missing sections: {}",
            missing.join(", ")
        ));
        info(&format!(
            "  Required sections: {}",
            AGENTS_MD_REQUIRED_SECTIONS.join(", ")
        ));
        *failed = true;
    }
}

pub fn check_codex(hex_dir: &std::path::Path) {
    let mut failed = false;
    check_codex_1(&mut failed);
    check_codex_2(&mut failed);
    check_codex_3(&mut failed);
    check_codex_4(hex_dir, &mut failed);
    check_codex_5(hex_dir, &mut failed);
    if failed {
        std::process::exit(1);
    }
}

/// Port of .hex/scripts/quality-check.py
/// Gaming detector for BOI initiative loop specs. Delegates to the Python script.
pub fn quality_check(hex_dir: &std::path::Path, spec: Option<&str>, sweep: bool, kr: Option<&str>) -> i32 {
    // Try legacy script (after quarantine) first
    let legacy = hex_dir.join("system/scripts/quality-check.py.legacy.py");
    let primary = hex_dir.join("system/scripts/quality-check.py");
    let script = if legacy.is_file() { legacy } else { primary };

    if !script.is_file() {
        eprintln!("hex doctor quality-check: script not found at {}", script.display());
        return 1;
    }

    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script);
    if let Some(s) = spec {
        cmd.arg("--spec").arg(s);
    }
    if sweep {
        cmd.arg("--sweep");
    }
    if let Some(k) = kr {
        cmd.arg("--kr").arg(k);
    }
    cmd.env("HEX_DIR", hex_dir);
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    let status = cmd.status().unwrap_or_else(|e| {
        eprintln!("hex doctor quality-check: failed to exec script: {e}");
        std::process::exit(1);
    });
    status.code().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_sections_nonempty() {
        assert!(!AGENTS_MD_REQUIRED_SECTIONS.is_empty());
        assert!(AGENTS_MD_REQUIRED_SECTIONS.contains(&"Standing Orders"));
        assert!(AGENTS_MD_REQUIRED_SECTIONS.contains(&"BOI"));
    }

    #[test]
    fn check_codex_5_passes_when_all_sections_present() {
        let dir = std::env::temp_dir().join("codex_test_agents_ok");
        std::fs::create_dir_all(&dir).unwrap();
        let content = "# Standing Orders\n# Session Lifecycle\n# BOI\n# Memory\n";
        std::fs::write(dir.join("AGENTS.md"), content).unwrap();
        let mut failed = false;
        check_codex_5(&dir, &mut failed);
        assert!(!failed, "all sections present → should not fail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_codex_5_fails_when_section_missing() {
        let dir = std::env::temp_dir().join("codex_test_agents_missing");
        std::fs::create_dir_all(&dir).unwrap();
        let content = "# Standing Orders\n# BOI\n";
        std::fs::write(dir.join("AGENTS.md"), content).unwrap();
        let mut failed = false;
        check_codex_5(&dir, &mut failed);
        assert!(failed, "missing sections → should fail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_codex_4_fails_when_agents_md_missing() {
        let dir = std::env::temp_dir().join("codex_test_no_agents");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("AGENTS.md"));
        let mut failed = false;
        check_codex_4(&dir, &mut failed);
        assert!(failed, "missing AGENTS.md → should fail");
        std::fs::remove_dir_all(&dir).ok();
    }
}
