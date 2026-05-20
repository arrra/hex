use crate::types::{AgentResponse, AssessmentResponse, ClaudeOutput, Message, QueueUpdates, TrailEntry};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn parse_output(raw: &str) -> Result<ClaudeOutput, Box<dyn std::error::Error>> {
    let output: ClaudeOutput = serde_json::from_str(raw)?;
    Ok(output)
}

pub fn parse_agent_response(
    result_text: &str,
) -> Result<AgentResponse, Box<dyn std::error::Error>> {
    let cleaned = extract_json(result_text);
    match serde_json::from_str::<AgentResponse>(&cleaned) {
        Ok(response) => Ok(response),
        Err(strict_err) => {
            eprintln!("[harness] parse_agent_response: strict parse failed ({strict_err}), attempting partial recovery");
            // Try to parse as raw JSON value and reconstruct field-by-field
            let val: serde_json::Value = match serde_json::from_str(&cleaned) {
                Ok(v) => v,
                Err(json_err) => {
                    eprintln!("[harness] parse_agent_response: not valid JSON ({json_err}), attempting element-wise salvage");
                    // Even without valid JSON, try to salvage trail and outbound_messages from raw text
                    let trail = salvage_typed_array::<TrailEntry>(&cleaned, "trail");
                    let outbound_messages = salvage_typed_array::<Message>(&cleaned, "outbound_messages");
                    if trail.is_empty() && outbound_messages.is_empty() {
                        eprintln!("[harness] parse_agent_response: not valid JSON ({json_err}), discarding response");
                        return Err(json_err.into());
                    }
                    eprintln!(
                        "[harness] parse_agent_response: salvaged {}/{} trail entries and {}/{} messages from unparseable response",
                        trail.len(), trail.len(),
                        outbound_messages.len(), outbound_messages.len()
                    );
                    return Ok(AgentResponse {
                        trail,
                        queue_updates: QueueUpdates::default(),
                        memory_updates: None,
                        outbound_messages,
                        active_drained: false,
                    });
                }
            };

            // Partial recovery: JSON value parsed but strict struct deserialization failed
            let trail = val
                .get("trail")
                .and_then(|t| serde_json::from_value::<Vec<TrailEntry>>(t.clone()).ok())
                .unwrap_or_else(|| {
                    // Whole-array parse failed (likely truncated mid-element) — salvage complete elements
                    let salvaged = salvage_typed_array::<TrailEntry>(&cleaned, "trail");
                    let count = salvaged.len();
                    if count > 0 {
                        eprintln!("[harness] parse_agent_response: salvaged {count} trail entries from truncated response");
                    } else {
                        eprintln!("[harness] parse_agent_response: trail field unrecoverable");
                    }
                    salvaged
                });

            let queue_updates = val
                .get("queue_updates")
                .and_then(|q| serde_json::from_value::<QueueUpdates>(q.clone()).ok())
                .unwrap_or_default();
            let memory_updates = val.get("memory_updates").cloned();

            let outbound_messages = val
                .get("outbound_messages")
                .and_then(|m| serde_json::from_value::<Vec<Message>>(m.clone()).ok())
                .unwrap_or_else(|| {
                    let salvaged = salvage_typed_array::<Message>(&cleaned, "outbound_messages");
                    let count = salvaged.len();
                    if count > 0 {
                        eprintln!("[harness] parse_agent_response: salvaged {count} outbound_messages from truncated response");
                    }
                    salvaged
                });

            let active_drained = val
                .get("active_drained")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            eprintln!(
                "[harness] parse_agent_response: partial recovery ({} trail entries, {} messages)",
                trail.len(),
                outbound_messages.len()
            );
            Ok(AgentResponse {
                trail,
                queue_updates,
                memory_updates,
                outbound_messages,
                active_drained,
            })
        }
    }
}

/// Given a JSON fragment that should be an array of objects but may be truncated
/// mid-element, return all complete leading elements. Walks the string tracking
/// brace/bracket depth and string state; collects each balanced top-level `{...}`.
fn salvage_array_elements(fragment: &str) -> Vec<serde_json::Value> {
    let bytes = fragment.as_bytes();
    let n = bytes.len();
    let mut i = 0;

    // Skip whitespace and find opening `[`
    while i < n && bytes[i] != b'[' {
        i += 1;
    }
    if i >= n {
        return vec![];
    }
    i += 1; // consume `[`

    let mut results = Vec::new();
    let mut in_string = false;
    let mut in_escape = false;

    while i < n {
        // Skip whitespace and commas between elements at array top level
        while i < n && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n' || bytes[i] == b'\r' || bytes[i] == b',') {
            i += 1;
        }
        if i >= n || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            // Not an object element — skip to next comma or end
            break;
        }

        // Found start of an object element
        let element_start = i;
        let mut depth: i32 = 0;
        in_string = false;
        in_escape = false;

        while i < n {
            let c = bytes[i];
            if in_escape {
                in_escape = false;
            } else if in_string {
                match c {
                    b'\\' => in_escape = true,
                    b'"' => in_string = false,
                    _ => {}
                }
            } else {
                match c {
                    b'"' => in_string = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            // Complete object from element_start..=i
                            let element_str = &fragment[element_start..=i];
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(element_str) {
                                results.push(val);
                            }
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }

        // If we exited the inner loop without completing (depth > 0), object was truncated — stop
        if depth > 0 {
            break;
        }
    }

    results
}

/// Find the array span for a given JSON key in the raw text, then salvage complete elements.
fn salvage_typed_array<T: serde::de::DeserializeOwned>(
    cleaned: &str,
    key: &str,
) -> Vec<T> {
    // Find `"key"` in the string
    let key_needle = format!("\"{key}\"");
    let key_pos = match cleaned.find(&key_needle) {
        Some(p) => p,
        None => return vec![],
    };

    // Find `:` then `[` after the key
    let after_key = &cleaned[key_pos + key_needle.len()..];
    let colon_pos = match after_key.find(':') {
        Some(p) => p,
        None => return vec![],
    };
    let after_colon = &after_key[colon_pos + 1..];
    // Skip whitespace
    let bracket_offset = after_colon.find('[').unwrap_or(usize::MAX);
    if bracket_offset == usize::MAX {
        return vec![];
    }

    // The span starts at `[`
    let start = key_pos + key_needle.len() + colon_pos + 1 + bracket_offset;
    let array_span = &cleaned[start..];

    salvage_array_elements(array_span)
        .into_iter()
        .filter_map(|v| serde_json::from_value::<T>(v).ok())
        .collect()
}

pub fn parse_assessment_response(
    result_text: &str,
) -> Result<AssessmentResponse, Box<dyn std::error::Error>> {
    let cleaned = extract_json(result_text);
    let response: AssessmentResponse = serde_json::from_str(&cleaned)?;
    Ok(response)
}

fn extract_json(text: &str) -> String {
    let trimmed = text.trim();

    // Try direct parse first
    if trimmed.starts_with('{') {
        return trimmed.to_string();
    }

    // Strip markdown code fences: ```json ... ``` or ``` ... ```
    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        // Skip optional language tag (e.g., "json")
        let content_start = after_fence.find('\n').map(|i| i + 1).unwrap_or(0);
        let content = &after_fence[content_start..];
        if let Some(end) = content.find("```") {
            let inner = content[..end].trim();
            if inner.starts_with('{') {
                return inner.to_string();
            }
        }
    }

    // Find first { and last } as fallback
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            return trimmed[start..=end].to_string();
        }
    }

    trimmed.to_string()
}

pub fn build_args(model: &str, allowed_tools: &[&str]) -> Vec<String> {
    let tools_str = allowed_tools.join(",");
    vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--allowedTools".to_string(),
        tools_str,
        "--dangerously-skip-permissions".to_string(),
    ]
}

fn resolve_claude_bin() -> String {
    // Prefer explicit env override, then well-known absolute paths, then PATH lookup
    if let Ok(p) = std::env::var("CLAUDE_BIN") {
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = format!("{}/.local/bin/claude", home);
        if std::path::Path::new(&candidate).exists() {
            return candidate;
        }
    }
    "claude".to_string()
}

pub fn invoke(
    prompt: &str,
    model: &str,
    allowed_tools: &[&str],
) -> Result<ClaudeOutput, Box<dyn std::error::Error>> {
    let args = build_args(model, allowed_tools);
    let claude_bin = resolve_claude_bin();
    let mut child = Command::new(&claude_bin)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn claude: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("claude exited with {}: {stderr}", output.status).into());
    }

    let stdout = String::from_utf8(output.stdout)?;
    parse_output(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── salvage_array_elements ────────────────────────────────────────────────

    #[test]
    fn salvage_array_elements_clean_3_objects() {
        let input = r#"[{"a":1},{"a":2},{"a":3}]"#;
        let result = salvage_array_elements(input);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn salvage_array_elements_truncated_mid_3rd_object() {
        // 3rd object is cut off mid-way
        let input = r#"[{"a":1},{"a":2},{"a":3,"b":"truncat"#;
        let result = salvage_array_elements(input);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn salvage_array_elements_truncated_mid_string_value() {
        // 3rd object has a string that's truncated — only first two complete ones kept
        let input = r#"[{"a":"hello"},{"a":"world"},{"a":"trunc"#;
        let result = salvage_array_elements(input);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn salvage_array_elements_nested_braces() {
        // Objects with nested braces — must count depth correctly
        let input = r#"[{"a":{"b":1}},{"a":{"b":2}},{"a":{"b":3"#;
        let result = salvage_array_elements(input);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn salvage_array_elements_braces_in_string() {
        // Braces inside string values must not affect depth counting
        let input = r#"[{"a":"{not a brace}"},{"a":"ok"},{"truncat"#;
        let result = salvage_array_elements(input);
        assert_eq!(result.len(), 2);
    }

    // ── parse_agent_response ─────────────────────────────────────────────────

    fn make_trail_entry_json(ts: &str, entry_type: &str) -> String {
        format!(r#"{{"ts":"{ts}","type":"{entry_type}","detail":null,"queue_item":null}}"#)
    }

    #[test]
    fn parse_agent_response_well_formed_no_regression() {
        let ts = "2024-01-01T00:00:00Z";
        let response = format!(
            r#"{{"trail":[{e},{e}],"queue_updates":{{"completed":[],"added_active":[],"moved_to_blocked":[],"parked":[]}},"memory_updates":null,"outbound_messages":[],"active_drained":false}}"#,
            e = make_trail_entry_json(ts, "act")
        );
        let result = parse_agent_response(&response).unwrap();
        assert_eq!(result.trail.len(), 2);
        assert_eq!(result.outbound_messages.len(), 0);
        assert!(!result.active_drained);
    }

    #[test]
    fn parse_agent_response_trail_truncated_recovers_2_entries() {
        let ts = "2024-01-01T00:00:00Z";
        let e1 = make_trail_entry_json(ts, "act");
        let e2 = make_trail_entry_json(ts, "observe");
        // Truncate mid-3rd object: the JSON value will be invalid (no closing `}}`),
        // but we can still salvage the first two complete trail entries via raw text salvage
        let response = format!(
            r#"{{"trail":[{e1},{e2},{{"ts":"{ts}","type":"truncated"#
        );
        let result = parse_agent_response(&response).unwrap();
        assert_eq!(result.trail.len(), 2, "should salvage 2 complete trail entries");
    }

    #[test]
    fn parse_agent_response_messages_before_truncated_trail() {
        // outbound_messages appears BEFORE trail in the JSON object.
        // Trail is truncated, but messages should still be recovered.
        let ts = "2024-01-01T00:00:00Z";
        let msg = r#"{"id":"m1","msg_type":"agent","from":"a","to":["b"],"content":"hi","status":"new","created_at":"2024-01-01T00:00:00Z","subject":"hi","body":"hi","response_requested":false}"#;
        let e1 = make_trail_entry_json(ts, "act");
        // Construct: outbound_messages is intact, trail is truncated
        let response = format!(
            r#"{{"outbound_messages":[{msg}],"trail":[{e1},{{"ts":"{ts}","type":"truncated"#
        );
        let result = parse_agent_response(&response).unwrap();
        assert_eq!(result.outbound_messages.len(), 1, "intact outbound_messages should be recovered");
        // trail may be 0 or 1 depending on parse path — what matters is messages survived
        assert!(result.trail.len() <= 1);
    }
}
