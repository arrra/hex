//! Claude Code `PreToolUse` hook — lint BOI dispatches before they spawn
//! workers (agent-infra P0: the linter agent's mechanical trigger).
//!
//! Rust port of the former bash `lint-gates-predispatch.sh` (whose JSON
//! parsing shelled out to python3 — banned 2026-06-10, "All rust. There
//! should be no python"). The shipped script is now a logic-free shim that
//! execs `hex hook lint-predispatch`.
//!
//! Behavior:
//!   * stdin = PreToolUse JSON payload. Unparseable / missing
//!     `tool_input.command` → ALLOW silently (exit 0, fail-open: a broken
//!     hook must never wedge the session).
//!   * command not matching `boi dispatch <spec>` → ALLOW silently.
//!   * dispatch detected → run `hex::lint_gates` IN-PROCESS over the spec:
//!     one `intent` ledger row per command gate (shadow mode), summary to
//!     stderr, exit 0. Unreadable spec or TOML/contract parse error →
//!     reason on stderr, exit 2 (BLOCK — the dispatch would fail anyway).
//!   * ledger trouble → loud on stderr, exit 1 (NON-blocking: never block a
//!     dispatch because our own bookkeeping is broken; exit 1 ≠ 2 so Claude
//!     Code surfaces the error without denying the tool call).
//!
//! Dispatch pattern note: the old bash regex `(^|[^[:alnum:]_/])boi` excluded
//! a preceding `/`, so the canonical `~/.boi/bin/boi dispatch <spec>` form
//! NEVER matched and the hook silently linted nothing (latent P0 bug, found
//! in the 2026-06-10 E0 smoke). A preceding `/` MUST match.

use regex::Regex;
use std::io::Read;
use std::path::PathBuf;

/// What the hook decided to do with one tool call. Pure — testable without
/// stdin/exit codes.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Not a boi dispatch (or unparseable payload): allow, say nothing.
    Skip,
    /// A dispatch of this spec path: lint it.
    Lint(String),
}

pub fn run() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        std::process::exit(0); // fail-open
    }
    let spec_path = match decide(&raw) {
        Action::Skip => std::process::exit(0),
        Action::Lint(p) => p,
    };

    // Expand a leading `~/` — the command string is shell text, not a path.
    let spec_path = expand_tilde(&spec_path);

    let src = match std::fs::read_to_string(&spec_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lint-predispatch: cannot read spec {}: {e} — blocking dispatch", spec_path.display());
            std::process::exit(2);
        }
    };
    let gates = match hex::lint_gates::extract_gates_from_spec(&src) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("lint-predispatch: {e} — blocking dispatch (BOI would reject this spec)");
            std::process::exit(2);
        }
    };

    // Shadow mode: one intent row per gate, summary only, never advice.
    // (Same row shape as the `hex lint-gates` CLI arm in main.rs — keep in sync.)
    let hex_dir = lenient_hex_dir();
    let ledger_path = hex::ledger::default_path(&hex_dir);
    let ledger = match hex::ledger::Ledger::open(&ledger_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("lint-predispatch: ledger open failed ({e}) — allowing dispatch, predictions LOST");
            std::process::exit(1); // loud, non-blocking
        }
    };
    for gate in &gates {
        let v = hex::lint_gates::analyze_command(gate);
        let predicted = match v.predicted {
            hex::lint_gates::Prediction::Pass => "pass",
            hex::lint_gates::Prediction::Fail => "fail",
        };
        let payload = serde_json::json!({
            "gate_hash": v.content_hash,
            "predicted": predicted,
            "rules_fired": v.rules_fired,
            "shadow": true,
            "command": gate,
            "via": "predispatch-hook",
        });
        if let Err(e) = ledger.append("lint-gates", "verify-gate", "intent", &payload) {
            eprintln!("lint-predispatch: ledger append failed ({e}) — allowing dispatch");
            std::process::exit(1);
        }
    }
    eprintln!("lint-predispatch: {}", hex::lint_gates::shadow_summary(&gates));
    std::process::exit(0);
}

/// Pure decision over the raw PreToolUse payload.
pub fn decide(raw: &str) -> Action {
    let input: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Action::Skip,
    };
    let cmd = match input
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(serde_json::Value::as_str)
    {
        Some(c) if !c.is_empty() => c,
        _ => return Action::Skip,
    };
    match extract_spec_path(cmd) {
        Some(p) => Action::Lint(p),
        None => Action::Skip,
    }
}

/// Find `boi dispatch <spec>` in a shell command and return the spec token.
/// A preceding `/` matches deliberately (`~/.boi/bin/boi dispatch …` is the
/// canonical form). Preceding alnum/underscore does not (`myboi dispatch` is
/// not boi).
pub fn extract_spec_path(cmd: &str) -> Option<String> {
    // (^|[^A-Za-z0-9_]) boi \s+ dispatch \s+ (<token>)
    let re = Regex::new(r"(?:^|[^A-Za-z0-9_])boi\s+dispatch\s+([^\s;|&]+)").ok()?;
    let caps = re.captures(cmd)?;
    let tok = caps.get(1)?.as_str();
    // Strip a flag-looking token: `boi dispatch --help` is not a spec.
    if tok.starts_with('-') {
        return None;
    }
    Some(tok.to_string())
}

/// `~/x` → `$HOME/x`; everything else verbatim.
fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

/// HEX_DIR → $HOME/hex → "." — lenient on purpose (a hook must not hard-exit
/// over a missing workspace marker; the ledger path is derived best-effort).
fn lenient_hex_dir() -> PathBuf {
    if let Ok(d) = std::env::var("HEX_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("hex");
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_dotboi_dispatch_matches() {
        // THE bug case: the old bash regex never matched this form.
        let cmd = "~/.boi/bin/boi dispatch projects/agent-infra/specs/x.spec.toml";
        assert_eq!(
            extract_spec_path(cmd),
            Some("projects/agent-infra/specs/x.spec.toml".to_string())
        );
    }

    #[test]
    fn bare_boi_dispatch_matches() {
        assert_eq!(
            extract_spec_path("boi dispatch /tmp/a.toml && echo ok"),
            Some("/tmp/a.toml".to_string())
        );
    }

    #[test]
    fn non_dispatch_commands_skip() {
        assert_eq!(extract_spec_path("ls -la"), None);
        assert_eq!(extract_spec_path("boi dashboard"), None);
        assert_eq!(extract_spec_path("myboi dispatch x.toml"), None);
        assert_eq!(extract_spec_path("echo boi dispatcher"), None);
    }

    #[test]
    fn flag_token_after_dispatch_skips() {
        assert_eq!(extract_spec_path("boi dispatch --help"), None);
    }

    #[test]
    fn payload_decide_roundtrip() {
        let raw = r#"{"tool_name":"Bash","tool_input":{"command":"~/.boi/bin/boi dispatch /tmp/s.toml"}}"#;
        assert_eq!(decide(raw), Action::Lint("/tmp/s.toml".to_string()));
        assert_eq!(decide(r#"{"tool_input":{"command":"ls"}}"#), Action::Skip);
        assert_eq!(decide("not json"), Action::Skip);
        assert_eq!(decide(""), Action::Skip);
    }
}
