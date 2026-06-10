//! Doctor check for `.hex/config/claude-runs.toml` (spec Sf5bj7y1d).
//!
//! Policy:
//! - Absent → PASS (built-in lean profiles apply; this IS the intended default).
//! - Present → parse it, then validate every profile that names mcp_servers:
//!   each name must resolve in the workspace MCP config (`.mcp.json` or
//!   `.claude/mcp.json`). Loud failure on any error (Standing Order S6: no
//!   quiet failures).

use crate::claude_runs::{self, McpConfig};
use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

pub struct ClaudeRunsConfig;

impl DoctorCheck for ClaudeRunsConfig {
    fn name(&self) -> &str { "claude-runs-config" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        let cfg_path = ctx.hex_dir.join(".hex").join("config").join("claude-runs.toml");
        if !cfg_path.exists() {
            return CheckResult::pass(
                "claude-runs.toml absent — built-in lean profiles apply (this is the intended default)",
            );
        }

        // Load workspace MCP config once. Loud failure if it's malformed.
        let mcp = match McpConfig::load(&ctx.hex_dir) {
            Ok(m) => m,
            Err(e) => {
                return CheckResult::fail(format!(
                    "claude-runs: workspace MCP config invalid: {e}"
                ));
            }
        };

        // Resolve every known + custom profile that the config touches.
        // Parsing happens inside `resolve`; we trigger it for each of the
        // built-in names plus any user-defined names by reading the file
        // ourselves first to list `[runs.X]` headers.
        let body = match std::fs::read_to_string(&cfg_path) {
            Ok(s) => s,
            Err(e) => {
                return CheckResult::fail(format!(
                    "claude-runs: cannot read {}: {e}",
                    cfg_path.display()
                ));
            }
        };

        // Collect profile names from `[runs.NAME]` headers — only validate
        // profiles the FILE itself references. Built-ins untouched by the
        // file remain implicitly valid (their hardcoded defaults are tested
        // in claude_runs unit tests).
        let mut profiles: Vec<String> = Vec::new();
        for line in body.lines() {
            let t = line.trim();
            if let Some(inner) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                if let Some(name) = inner.trim().strip_prefix("runs.") {
                    let n = name.trim().to_string();
                    if !profiles.contains(&n) {
                        profiles.push(n);
                    }
                }
            }
        }

        let mut problems = Vec::new();
        let mut checked = 0;
        // Always parse the file by resolving the built-in `default` profile —
        // catches malformed TOML even if the user hasn't defined any
        // `[runs.X]` sections.
        if let Err(e) = claude_runs::resolve("default", Some(&ctx.hex_dir)) {
            problems.push(format!("config file: {e}"));
        }
        for name in &profiles {
            match claude_runs::resolve(name, Some(&ctx.hex_dir)) {
                Ok(r) => {
                    checked += 1;
                    // Verify mcp_servers resolve in workspace MCP config.
                    if !r.mcp_servers.is_empty() {
                        if let Err(e) = r.to_cli_flags(&mcp) {
                            problems.push(format!("profile {name:?}: {e}"));
                        }
                    }
                }
                Err(e) => {
                    problems.push(format!("profile {name:?}: {e}"));
                }
            }
        }

        if !problems.is_empty() {
            return CheckResult::fail(format!(
                "claude-runs.toml invalid ({} problem(s))",
                problems.len()
            ))
            .with_details(problems.join("\n"));
        }

        CheckResult::pass(format!(
            "claude-runs.toml valid ({checked} profile(s) resolved, all mcp_servers found)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx_for(tmp: &tempfile::TempDir) -> Context {
        Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        }
    }

    #[test]
    fn absent_config_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(&tmp);
        let r = ClaudeRunsConfig.run(&ctx);
        assert_eq!(r.status, crate::doctor::check::Status::Pass);
    }

    #[test]
    fn present_valid_config_with_no_mcp_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".hex/config");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("claude-runs.toml"),
            "[defaults]\nbare = true\n\n[runs.harness_worker]\n",
        )
        .unwrap();
        let r = ClaudeRunsConfig.run(&ctx_for(&tmp));
        assert_eq!(r.status, crate::doctor::check::Status::Pass);
    }

    #[test]
    fn config_with_missing_mcp_server_fails_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".hex/config");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("claude-runs.toml"),
            "[runs.harness_worker]\nmcp_servers = [\"nonexistent\"]\n",
        )
        .unwrap();
        // No .mcp.json present → lookup must fail.
        let r = ClaudeRunsConfig.run(&ctx_for(&tmp));
        assert_eq!(r.status, crate::doctor::check::Status::Fail);
        assert!(r.message.contains("invalid"));
    }
}
