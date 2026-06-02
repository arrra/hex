use serde_json::Value;
use std::io::Read;

// ── filter list ───────────────────────────────────────────────────────────────
// Deliberate design choice: high-volume tools are excluded to avoid log noise.
// DO NOT change this list without flagging and justification.
const FILTERED_TOOLS: &[&str] = &[
    "Read", "Edit", "Write", "Bash", "Grep", "Glob", "MultiEdit", "TodoRead", "TodoWrite",
];

fn is_filtered(tool_name: &str) -> bool {
    FILTERED_TOOLS.contains(&tool_name)
}

// ── outcome parsing ───────────────────────────────────────────────────────────

pub fn parse_outcome(tool_response: &Value) -> &'static str {
    match tool_response {
        Value::Object(map) => {
            if map.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false) {
                "error"
            } else {
                "success"
            }
        }
        Value::String(s) => {
            if s.to_lowercase().contains("error") {
                "error"
            } else {
                "success"
            }
        }
        _ => "success",
    }
}

// ── entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        std::process::exit(0);
    }

    let input: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => std::process::exit(0),
    };

    let tool_name = match input.get("tool_name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => std::process::exit(0),
    };

    if is_filtered(&tool_name) {
        std::process::exit(0);
    }

    std::process::exit(0);
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── filter list ───────────────────────────────────────────────────────────

    #[test]
    fn filtered_tools_are_skipped() {
        let filtered = [
            "Read", "Edit", "Write", "Bash", "Grep", "Glob", "MultiEdit", "TodoRead", "TodoWrite",
        ];
        for tool in &filtered {
            assert!(is_filtered(tool), "{tool} should be filtered");
        }
    }

    #[test]
    fn non_filtered_tools_pass_through() {
        let non_filtered = ["Agent", "WebSearch", "WebFetch", "LSP", "ToolSearch"];
        for tool in &non_filtered {
            assert!(!is_filtered(tool), "{tool} should NOT be filtered");
        }
    }

    // ── outcome parsing ───────────────────────────────────────────────────────

    #[test]
    fn is_error_true_gives_error() {
        let v = json!({"is_error": true});
        assert_eq!(parse_outcome(&v), "error");
    }

    #[test]
    fn is_error_false_gives_success() {
        let v = json!({"is_error": false});
        assert_eq!(parse_outcome(&v), "success");
    }

    #[test]
    fn is_error_absent_gives_success() {
        let v = json!({"output": "ok"});
        assert_eq!(parse_outcome(&v), "success");
    }

    #[test]
    fn string_containing_error_gives_error() {
        let v = json!("Tool execution error: timeout");
        assert_eq!(parse_outcome(&v), "error");
    }

    #[test]
    fn string_containing_error_case_insensitive() {
        let v = json!("ERROR: file not found");
        assert_eq!(parse_outcome(&v), "error");
    }

    #[test]
    fn clean_string_gives_success() {
        let v = json!("result: 42 lines processed");
        assert_eq!(parse_outcome(&v), "success");
    }

    #[test]
    fn null_response_gives_success() {
        assert_eq!(parse_outcome(&Value::Null), "success");
    }

    #[test]
    fn number_response_gives_success() {
        assert_eq!(parse_outcome(&json!(0)), "success");
    }
}
