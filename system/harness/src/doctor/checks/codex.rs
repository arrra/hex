use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

const AGENTS_MD_REQUIRED_SECTIONS: &[&str] = &["Standing Orders", "BOI", "Memory"];

fn codex_on_path() -> bool {
    Command::new("which")
        .arg("codex")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// check_51: codex binary on PATH.
pub struct CodexCliOnPath;

impl DoctorCheck for CodexCliOnPath {
    fn name(&self) -> &str { "codex.cli-on-path" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, _ctx: &Context) -> CheckResult {
        if let Ok(output) = Command::new("which").arg("codex").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return CheckResult::pass(format!("codex found at {}", path));
            }
        }
        CheckResult::fail("codex CLI not found — install: npm install -g @openai/codex")
    }
}

/// check_52: codex --version exits 0.
pub struct CodexVersionOk;

impl DoctorCheck for CodexVersionOk {
    fn name(&self) -> &str { "codex.version-ok" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, _ctx: &Context) -> CheckResult {
        if !codex_on_path() {
            return CheckResult::skip("skipped (codex not on PATH)");
        }
        match Command::new("codex").arg("--version").output() {
            Ok(o) if o.status.success() => {
                let ver = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                CheckResult::pass(ver)
            }
            _ => CheckResult::fail("'codex --version' exited non-zero — fix: reinstall codex CLI"),
        }
    }
}

/// check_53: OPENAI_API_KEY present in env or ~/.hex-test.env.
pub struct CodexApiKey;

impl DoctorCheck for CodexApiKey {
    fn name(&self) -> &str { "codex.api-key" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        if std::env::var("OPENAI_API_KEY").map(|v| !v.is_empty()).unwrap_or(false) {
            return CheckResult::pass("OPENAI_API_KEY found via environment variable");
        }
        let hex_test_env = ctx.home.join(".hex-test.env");
        if hex_test_env.is_file() {
            if let Ok(text) = std::fs::read_to_string(&hex_test_env) {
                if text.lines().any(|l| l.starts_with("OPENAI_API_KEY=")) {
                    return CheckResult::pass("OPENAI_API_KEY found via ~/.hex-test.env");
                }
            }
        }
        CheckResult::warn("OPENAI_API_KEY not set — Codex will fail at runtime")
            .with_details("Fix: export OPENAI_API_KEY=sk-... in ~/.hex/scripts/env.sh or add to ~/.hex-test.env")
    }
}

/// check_54: AGENTS.md exists at $HEX_DIR/AGENTS.md.
pub struct CodexAgentsMdExists;

impl DoctorCheck for CodexAgentsMdExists {
    fn name(&self) -> &str { "codex.agents-md-exists" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        let agents_md = ctx.hex_dir.join("AGENTS.md");
        if agents_md.is_file() {
            let size = std::fs::metadata(&agents_md).map(|m| m.len()).unwrap_or(0);
            CheckResult::pass(format!("AGENTS.md found ({} bytes)", size))
        } else {
            CheckResult::warn(format!(
                "AGENTS.md missing at {} — Codex reads this as its primary instruction file",
                agents_md.display()
            ))
        }
    }
}

/// check_55: AGENTS.md contains all required sections.
pub struct CodexAgentsMdComplete;

impl DoctorCheck for CodexAgentsMdComplete {
    fn name(&self) -> &str { "codex.agents-md-complete" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        let agents_md = ctx.hex_dir.join("AGENTS.md");
        if !agents_md.is_file() {
            return CheckResult::skip("skipped (AGENTS.md not present)");
        }
        let text = match std::fs::read_to_string(&agents_md) {
            Ok(t) => t.to_lowercase(),
            Err(e) => {
                return CheckResult::fail(format!("cannot read AGENTS.md: {}", e));
            }
        };
        let missing: Vec<&str> = AGENTS_MD_REQUIRED_SECTIONS
            .iter()
            .filter(|&&section| !text.contains(&section.to_lowercase()))
            .copied()
            .collect();
        if missing.is_empty() {
            CheckResult::pass("all required sections present")
        } else {
            CheckResult::warn(format!("AGENTS.md missing sections: {}", missing.join(", ")))
                .with_details(format!(
                    "Required sections: {}",
                    AGENTS_MD_REQUIRED_SECTIONS.join(", ")
                ))
        }
    }
}
