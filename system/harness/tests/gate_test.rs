use chrono::Utc;
use hex::gate;
use hex::types::TrailEntry;

fn make_entry(entry_type: &str, detail: serde_json::Value) -> TrailEntry {
    TrailEntry {
        ts: Utc::now(),
        entry_type: entry_type.to_string(),
        detail,
        queue_item: None,
    }
}

#[test]
fn test_valid_observe() {
    let entry = make_entry(
        "observe",
        serde_json::json!({"what": "log.jsonl", "noted": "healthy"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_find() {
    let entry = make_entry(
        "find",
        serde_json::json!({"finding": "Path is wrong", "evidence": "log shows errors"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_decide() {
    let entry = make_entry(
        "decide",
        serde_json::json!({"decision": "Add breaker", "alternatives": ["alert"], "reasoning": "Need prevention"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_act() {
    let entry = make_entry(
        "act",
        serde_json::json!({"action": "Wrote function", "result": "Test passes"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_valid_verify() {
    let entry = make_entry(
        "verify",
        serde_json::json!({"check": "infra test", "evidence": "35/35", "status": "unconfirmed"}),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_reject_find_missing_evidence() {
    let entry = make_entry("find", serde_json::json!({"finding": "Something wrong"}));
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("evidence"));
}

#[test]
fn test_reject_decide_missing_reasoning() {
    let entry = make_entry(
        "decide",
        serde_json::json!({"decision": "Do something", "alternatives": []}),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("reasoning"));
}

#[test]
fn test_reject_unknown_type() {
    let entry = make_entry("hallucinate", serde_json::json!({"stuff": "things"}));
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown"));
}

#[test]
fn test_reject_empty_required_field() {
    let entry = make_entry(
        "find",
        serde_json::json!({"finding": "", "evidence": "some evidence"}),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("finding"));
}

// capability_add tests
#[test]
fn test_valid_capability_add() {
    let entry = make_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "my-fn",
            "description": "does a thing",
            "wall_hit": "60s",
            "exec_or_event": "#!/bin/sh\necho hello"
        }),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_capability_add_missing_wall_hit() {
    let entry = make_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "my-fn",
            "description": "does a thing",
            "exec_or_event": "#!/bin/sh\necho hello"
        }),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("wall_hit"));
}

#[test]
fn test_capability_add_missing_capability_kind() {
    let entry = make_entry(
        "capability_add",
        serde_json::json!({
            "capability_id": "my-fn",
            "description": "does a thing",
            "wall_hit": "60s",
            "exec_or_event": "#!/bin/sh\necho hello"
        }),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("capability_kind"));
}

#[test]
fn test_capability_add_missing_capability_id() {
    let entry = make_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "description": "does a thing",
            "wall_hit": "60s",
            "exec_or_event": "#!/bin/sh\necho hello"
        }),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("capability_id"));
}

#[test]
fn test_capability_add_missing_description() {
    let entry = make_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "my-fn",
            "wall_hit": "60s",
            "exec_or_event": "#!/bin/sh\necho hello"
        }),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("description"));
}

#[test]
fn test_capability_add_missing_exec_or_event() {
    let entry = make_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "my-fn",
            "description": "does a thing",
            "wall_hit": "60s"
        }),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("exec_or_event"));
}

// capability_call tests
#[test]
fn test_valid_capability_call() {
    let entry = make_entry(
        "capability_call",
        serde_json::json!({
            "capability_id": "my-fn",
            "args": {"key": "value"}
        }),
    );
    assert!(gate::validate(&entry).is_ok());
}

#[test]
fn test_capability_call_missing_capability_id() {
    let entry = make_entry(
        "capability_call",
        serde_json::json!({
            "args": {"key": "value"}
        }),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("capability_id"));
}

#[test]
fn test_capability_call_missing_args() {
    let entry = make_entry(
        "capability_call",
        serde_json::json!({
            "capability_id": "my-fn"
        }),
    );
    let result = gate::validate(&entry);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("args"));
}
