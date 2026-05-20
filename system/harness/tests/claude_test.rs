use hex::claude;

#[test]
fn test_parse_claude_json_output() {
    let raw = r#"{
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "duration_ms": 37360,
        "duration_api_ms": 2358,
        "num_turns": 1,
        "result": "{\"trail\":[],\"queue_updates\":{\"completed\":[],\"added_active\":[],\"moved_to_blocked\":[],\"parked\":[]},\"memory_updates\":null,\"outbound_messages\":[],\"active_drained\":true}",
        "stop_reason": "end_turn",
        "total_cost_usd": 0.036,
        "usage": {
            "input_tokens": 5000,
            "output_tokens": 200,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 4500
        },
        "session_id": "test-session"
    }"#;

    let output = claude::parse_output(raw).unwrap();
    assert_eq!(output.total_cost_usd, 0.036);
    assert_eq!(output.usage.input_tokens, 5000);
    assert_eq!(output.usage.output_tokens, 200);
    assert_eq!(output.duration_ms, 37360);
}

#[test]
fn test_parse_agent_response_from_result() {
    let result_json = r#"{"trail":[{"ts":"2026-04-22T12:00:00Z","type":"observe","detail":{"what":"log.jsonl","noted":"healthy"},"queue_item":"t-1"}],"queue_updates":{"completed":["t-1"],"added_active":[],"moved_to_blocked":[],"parked":[]},"memory_updates":{"last_pattern":"all healthy"},"outbound_messages":[],"active_drained":true}"#;
    let (response, quality) = claude::parse_agent_response(result_json);
    assert_eq!(response.trail.len(), 1);
    assert_eq!(response.trail[0].entry_type, "observe");
    assert_eq!(response.queue_updates.completed, vec!["t-1"]);
    assert!(response.active_drained);
    assert_eq!(quality, claude::ResponseParseQuality::Clean);
}

#[test]
fn test_parse_malformed_output() {
    let result = claude::parse_output("not json at all");
    assert!(result.is_err());
}

#[test]
fn test_build_invocation_args() {
    let args = claude::build_args("sonnet", &["Bash", "Read", "Write"]);
    assert!(args.contains(&"--output-format".to_string()));
    assert!(args.contains(&"json".to_string()));
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"sonnet".to_string()));
}

// ── Lenient parsing red/green tests ─────────────────────────────────────────

#[test]
fn test_active_item_tolerates_missing_id() {
    // RED: before fix this would fail — missing `id` field causes serde error
    // GREEN: after fix, `id` defaults to ""
    let json = r#"{"trail":[],"queue_updates":{"completed":[],"added_active":[{"summary":"orphan task","priority":0,"created":"2026-04-23T00:00:00Z","source":"test"}],"moved_to_blocked":[],"parked":[]},"memory_updates":null,"outbound_messages":[],"active_drained":false}"#;
    let (response, _) = claude::parse_agent_response(json);
    assert_eq!(response.queue_updates.added_active.len(), 1);
    assert_eq!(response.queue_updates.added_active[0].id, "");
    assert_eq!(response.queue_updates.added_active[0].summary, "orphan task");
}

#[test]
fn test_string_in_moved_to_blocked_is_kept() {
    // BlockedItem now accepts bare strings — "s-initiative-loop" parses to BlockedItem{id, ..Default}
    let json = r#"{"trail":[],"queue_updates":{"completed":[],"added_active":[],"moved_to_blocked":["s-initiative-loop"],"parked":[]},"memory_updates":null,"outbound_messages":[],"active_drained":true}"#;
    let (response, _) = claude::parse_agent_response(json);
    assert_eq!(response.queue_updates.moved_to_blocked.len(), 1, "bare string should parse as BlockedItem");
    assert_eq!(response.queue_updates.moved_to_blocked[0].id, "s-initiative-loop");
    assert!(response.active_drained);
}

#[test]
fn test_partial_recovery_when_one_field_corrupt() {
    // RED: before fix — any field error drops entire response, losing trail
    // GREEN: after fix — trail and active_drained recovered even if added_active is malformed
    let json = r#"{"trail":[{"ts":"2026-04-23T00:00:00Z","type":"observe","detail":{"what":"test","noted":"ok"},"queue_item":"s-1"}],"queue_updates":{"completed":[],"added_active":"not_an_array","moved_to_blocked":[],"parked":[]},"memory_updates":null,"outbound_messages":[],"active_drained":true}"#;
    let (response, _) = claude::parse_agent_response(json);
    assert_eq!(response.trail.len(), 1, "trail should be recovered");
    assert!(response.active_drained, "active_drained should be recovered");
}

#[test]
fn test_not_json_returns_empty_quality() {
    // Confirms truly unparseable output returns Empty quality (no longer Err — callers always get a result)
    let (result, quality) = claude::parse_agent_response("complete garbage not json at all");
    assert_eq!(quality, claude::ResponseParseQuality::Empty, "non-JSON should be Empty quality");
    assert!(result.trail.is_empty());
}
