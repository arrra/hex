//! Worker invocation: run the LLM worker so it returns EITHER a text answer OR a
//! structured question (Prompt), discriminated by a `kind` tag enforced via
//! `claude -p --json-schema`. See spikes/2026-06-05-json-schema-spike.md.
use crate::harness::Prompt;
use std::process::Command;

/// Decision computed by `decide_bare_auth_injection`: whether to inject
/// `ANTHROPIC_AUTH_TOKEN` into the child env and whether to emit a loud
/// stderr warning. See the bare-run auth injection notes below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BareAuthDecision {
    /// `Some(value)` → inject `ANTHROPIC_AUTH_TOKEN=<value>` into the spawned
    /// child's env (and ONLY that child's env). `None` → do not inject.
    pub inject_value: Option<String>,
    /// `true` → emit a loud stderr warning ("bare claude run has no auth
    /// path") before spawning anyway. Standing Order S6: no quiet failures.
    pub warn: bool,
}

/// Decide whether a headless `claude` spawn with a resolved profile needs
/// `ANTHROPIC_AUTH_TOKEN` injected from the harness's
/// `CLAUDE_CODE_OAUTH_TOKEN`.
///
/// Background: `claude --bare` skips keychain reads AND ignores
/// `CLAUDE_CODE_OAUTH_TOKEN` (upstream issue #51047 — closed not-planned).
/// `--bare` DOES honor `ANTHROPIC_AUTH_TOKEN`, and the setup-token works as
/// that bearer (verified 2026-06-10). Decision matrix:
///
/// | bare  | token                  | action                                      |
/// |-------|------------------------|---------------------------------------------|
/// | true  | `Some(non-empty)`      | inject the value into the child's env       |
/// | true  | `None` or `Some("")`   | emit loud warning, do NOT inject            |
/// | false | (any)                  | NEVER inject (keychain path must stay)      |
///
/// Pure function; no I/O. The caller is responsible for actually setting the
/// env var on the child `Command` and writing the warning to stderr.
pub fn decide_bare_auth_injection(bare: bool, token: Option<&str>) -> BareAuthDecision {
    if !bare {
        // Non-bare profiles fall through to keychain. Never inject; never warn.
        return BareAuthDecision {
            inject_value: None,
            warn: false,
        };
    }
    match token {
        Some(t) if !t.is_empty() => BareAuthDecision {
            inject_value: Some(t.to_string()),
            warn: false,
        },
        _ => BareAuthDecision {
            inject_value: None,
            warn: true,
        },
    }
}

pub enum WorkerOutput {
    Answer(String),
    Question(Prompt),
}

/// The output schema handed to `claude --json-schema`. FLAT object (no top-level
/// oneOf — the API rejects it; see spike). `kind` discriminates; the parser
/// enforces per-kind required fields.
pub const OUTPUT_SCHEMA: &str = r#"{"type":"object","required":["kind"],"properties":{"kind":{"type":"string","enum":["answer","prompt"]},"text":{"type":"string"},"multi":{"type":"boolean"},"options":{"type":"array","items":{"type":"object","required":["id","label","description"],"properties":{"id":{"type":"string"},"label":{"type":"string"},"description":{"type":"string"}}}}}}"#;

pub fn parse_worker_json(s: &str) -> Result<WorkerOutput, String> {
    let v: serde_json::Value = serde_json::from_str(s.trim())
        .map_err(|e| format!("worker output not JSON: {e}"))?;
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("answer") => {
            let t = v
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("answer missing text")?;
            Ok(WorkerOutput::Answer(t.to_string()))
        }
        Some("prompt") => {
            let text = v
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("prompt missing text")?
                .to_string();
            let multi = v
                .get("multi")
                .and_then(|m| m.as_bool())
                .ok_or("prompt missing multi")?;
            let opts_v = v
                .get("options")
                .and_then(|o| o.as_array())
                .ok_or("prompt missing options")?;
            let mut options = Vec::new();
            for o in opts_v {
                options.push(crate::harness::Opt {
                    id: o
                        .get("id")
                        .and_then(|x| x.as_str())
                        .ok_or("option missing id")?
                        .to_string(),
                    label: o
                        .get("label")
                        .and_then(|x| x.as_str())
                        .ok_or("option missing label")?
                        .to_string(),
                    description: o
                        .get("description")
                        .and_then(|x| x.as_str())
                        .ok_or("option missing description")?
                        .to_string(),
                });
            }
            Ok(WorkerOutput::Question(Prompt {
                id: crate::harness::id::mint(),
                text,
                multi,
                options,
            }))
        }
        _ => Err(format!("worker output missing/!kind: {s}")),
    }
}

/// Run the worker over an assembled prompt string. Returns the discriminated output.
///
/// TEST SEAM (`HEX_QUESTION_WORKER` env var) — makes the e2e suite deterministic
/// and CI-safe without a live LLM:
///   - unset            → live `claude --json-schema` (production)
///   - `echo`           → returns Answer(input) verbatim, so an e2e can assert the
///                        pinned option *description* actually reached the worker
///   - `<path-to-json>` → returns parse_worker_json(file contents); point it at a
///                        prompt fixture to deterministically make hex "ask".
pub fn run_worker(input: &str) -> Result<WorkerOutput, String> {
    match std::env::var("HEX_QUESTION_WORKER").ok().as_deref() {
        Some("echo") => return Ok(WorkerOutput::Answer(input.to_string())),
        Some(path) if !path.is_empty() => {
            let s = std::fs::read_to_string(path)
                .map_err(|e| format!("read HEX_QUESTION_WORKER fixture {path}: {e}"))?;
            return parse_worker_json(&s);
        }
        _ => {}
    }
    // Lean-by-default: resolve via claude_runs::resolve("harness_worker") so
    // this headless invocation does NOT inherit the workspace plugin/skill/
    // MCP/CLAUDE.md stack. See spec Sf5bj7y1d.
    let hex_dir = std::env::var("HEX_DIR").ok().map(std::path::PathBuf::from);
    let resolved = crate::claude_runs::resolve("harness_worker", hex_dir.as_deref())
        .map_err(|e| format!("claude_runs::resolve(harness_worker): {e}"))?;
    let workspace = hex_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    let mcp_cfg = crate::claude_runs::McpConfig::load(&workspace)
        .map_err(|e| format!("McpConfig::load: {e}"))?;
    let lean_flags = resolved
        .to_cli_flags(&mcp_cfg)
        .map_err(|e| format!("to_cli_flags: {e}"))?;
    let mut args: Vec<String> = lean_flags;
    args.extend([
        "-p".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--json-schema".to_string(),
        OUTPUT_SCHEMA.to_string(),
        input.to_string(),
    ]);
    // Bare-run auth injection: `--bare` skips keychain reads and ignores
    // CLAUDE_CODE_OAUTH_TOKEN. Child-scoped injection of ANTHROPIC_AUTH_TOKEN
    // (with the setup-token's value) is the verified workaround. Strictly
    // child env — never process-wide std::env::set_var, never launchctl. See
    // decision daemon-token-scoped-not-session-wide-2026-06-10.
    let oauth_token = std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok();
    let decision = decide_bare_auth_injection(resolved.bare, oauth_token.as_deref());
    if decision.warn {
        eprintln!(
            "hex: WARNING: bare claude run has no auth path — CLAUDE_CODE_OAUTH_TOKEN is unset/empty and --bare ignores keychain. Spawn will likely fail with 'Not logged in'."
        );
    }
    let mut cmd = Command::new("claude");
    cmd.args(&args);
    if let Some(value) = &decision.inject_value {
        cmd.env("ANTHROPIC_AUTH_TOKEN", value);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("spawn claude failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "claude exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    // The schema'd object is the envelope's `.structured_output` (NOT raw stdout).
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("claude envelope not JSON: {e}"))?;
    // OBS-024: claude envelope usage (same shape as memory/claude_cli.rs).
    if let Some(usage) = envelope.get("usage") {
        let in_tok = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let out_tok = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cost = envelope.get("total_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
        crate::llm_cost::record_llm_cost("worker-run", "question", in_tok, out_tok, cost, None);
    }
    let so = envelope
        .get("structured_output")
        .ok_or("claude envelope missing structured_output")?;
    parse_worker_json(&so.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_answer() {
        let o = parse_worker_json(r#"{"kind":"answer","text":"hello"}"#).unwrap();
        match o {
            WorkerOutput::Answer(t) => assert_eq!(t, "hello"),
            _ => panic!("want answer"),
        }
    }
    #[test]
    fn parses_prompt() {
        let j = r#"{"kind":"prompt","text":"pick","multi":false,"options":[{"id":"a","label":"A","description":"da"}]}"#;
        let o = parse_worker_json(j).unwrap();
        match o {
            WorkerOutput::Question(p) => assert_eq!(p.options[0].id, "a"),
            _ => panic!("want prompt"),
        }
    }
    #[test]
    fn malformed_fails_loud() {
        assert!(parse_worker_json(r#"{"kind":"prompt"}"#).is_err()); // missing options
        assert!(parse_worker_json(r#"not json"#).is_err());
    }

    // ----- Bare-run auth injection (deploy gate fix; task Tzxv4h994) -----
    //
    // When the harness spawns headless `claude` with a claude_runs profile
    // resolved to bare=true, `--bare` skips keychain reads AND ignores
    // CLAUDE_CODE_OAUTH_TOKEN. The verified workaround is to inject
    // ANTHROPIC_AUTH_TOKEN=<oauth-token-value> into THAT CHILD's env only.
    //
    // Decision matrix:
    //   bare=true,  token=Some(non-empty)  -> inject the token value
    //   bare=true,  token=None or empty    -> emit loud warning, do NOT inject
    //   bare=false, token=*                -> NEVER inject (non-bare uses keychain)
    //
    // The injection must be child-scoped (no process-wide std::env::set_var,
    // no launchctl setenv). See decision daemon-token-scoped-not-session-wide-2026-06-10.

    #[test]
    fn bare_with_token_injects_anthropic_auth_token_value() {
        let d = decide_bare_auth_injection(true, Some("sk-test-token-12345"));
        assert_eq!(
            d.inject_value.as_deref(),
            Some("sk-test-token-12345"),
            "bare profile + non-empty token must inject that exact value"
        );
        assert!(!d.warn, "no warning when injection succeeds");
    }

    #[test]
    fn bare_without_token_warns_and_does_not_inject() {
        let d = decide_bare_auth_injection(true, None);
        assert!(
            d.inject_value.is_none(),
            "no inject value when token is absent"
        );
        assert!(
            d.warn,
            "must emit loud warning that bare run has no auth path"
        );
    }

    #[test]
    fn bare_with_empty_token_warns_and_does_not_inject() {
        let d = decide_bare_auth_injection(true, Some(""));
        assert!(
            d.inject_value.is_none(),
            "empty token is not a usable auth value"
        );
        assert!(d.warn, "empty token still warrants the loud warning");
    }

    #[test]
    fn non_bare_with_token_never_injects() {
        let d = decide_bare_auth_injection(false, Some("sk-test-token-12345"));
        assert!(
            d.inject_value.is_none(),
            "non-bare profile MUST NOT inject ANTHROPIC_AUTH_TOKEN — keychain auth path must remain intact"
        );
        assert!(!d.warn, "non-bare path is fine; no warning");
    }

    #[test]
    fn non_bare_without_token_does_nothing() {
        let d = decide_bare_auth_injection(false, None);
        assert!(d.inject_value.is_none());
        assert!(
            !d.warn,
            "non-bare profiles never warn about missing oauth token"
        );
    }
}
