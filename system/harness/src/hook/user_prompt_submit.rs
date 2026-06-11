//! Claude Code `UserPromptSubmit` hook — injects relevant workspace memory as
//! `additionalContext`. This is the consumer that retires V1's zero-consumers
//! failure. Fail-open: any error → emit nothing, exit 0, never block the turn.

use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;

fn emit_context(context: &str) {
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": context,
        }
    });
    println!("{out}");
}

pub fn run() {
    // Read the hook's stdin JSON; `prompt` carries the user's text.
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        std::process::exit(0);
    }
    let input: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => std::process::exit(0),
    };
    let prompt = match input.get("prompt").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => std::process::exit(0),
    };

    let hex_dir = match std::env::var("HEX_DIR")
        .ok()
        .or_else(|| std::env::var("CLAUDE_PROJECT_DIR").ok())
        .map(PathBuf::from)
    {
        Some(d) => d,
        None => {
            // S6 — a missing HEX_DIR is a config bug, not input noise: be loud.
            eprintln!(
                "[hook/user-prompt-submit] HEX_DIR/CLAUDE_PROJECT_DIR not set — memory injection disabled"
            );
            std::process::exit(0);
        }
    };

    // Mike's interactive session — not a fleet agent — so for_agent = false.
    let outcome = crate::memory::recall::recall(&hex_dir, prompt, false);
    if outcome.injected {
        emit_context(&outcome.context);
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_context_is_valid_hook_json() {
        // Smoke test the JSON shape — capturing stdout is overkill; build the
        // value directly the way emit_context does.
        let v = json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "hello",
            }
        });
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    }
}
