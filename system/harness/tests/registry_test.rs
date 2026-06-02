use hex::registry::{
    add_function, add_trigger, append_audit, append_call, build_catalog, check_reentrancy,
    is_allowed, load_allowlist, remove_capability, FunctionCapability, TriggerCapability,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

/// Test helper: write a registry policy file directly. The old `emit_trigger_policy`
/// function was removed in the fleet teardown (it shelled out to the now-defunct
/// `hex agent wake`), but `remove_capability` still reconciles these policy files,
/// so the lifecycle and re-entrancy tests below need one present on disk.
fn write_registry_policy(registry_dir: &std::path::Path, cap_id: &str) {
    let policies_dir = registry_dir.join("policies");
    fs::create_dir_all(&policies_dir).unwrap();
    fs::write(
        policies_dir.join(format!("registry-{cap_id}.yaml")),
        format!("name: registry-{cap_id}\n"),
    )
    .unwrap();
}

fn make_fn_cap(id: &str, created_by: &str) -> FunctionCapability {
    FunctionCapability {
        id: id.to_string(),
        kind: "function".to_string(),
        created_by: created_by.to_string(),
        created_at: "2026-05-22T00:00:00Z".to_string(),
        created_in_wake: 1,
        unprompted: false,
        description: "A test function".to_string(),
        exec: format!("bin/{}", id),
        input_schema: serde_json::json!({"type": "object"}),
        callable_by: vec!["agent-a".to_string()],
    }
}

fn make_trig_cap(id: &str, created_by: &str) -> TriggerCapability {
    TriggerCapability {
        id: id.to_string(),
        kind: "trigger".to_string(),
        created_by: created_by.to_string(),
        created_at: "2026-05-22T00:00:00Z".to_string(),
        created_in_wake: 1,
        unprompted: false,
        description: "A test trigger".to_string(),
        event: "timer.tick.daily".to_string(),
        input_schema: serde_json::json!({}),
        callable_by: vec![],
    }
}

// ── Serde round-trip ─────────────────────────────────────────────────────────

#[test]
fn test_serde_roundtrip_function() {
    let cap = make_fn_cap("fn-001", "agent-a");
    let json = serde_json::to_string(&cap).unwrap();
    let back: FunctionCapability = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, "fn-001");
    assert_eq!(back.kind, "function");
    assert_eq!(back.created_by, "agent-a");
    assert_eq!(back.created_in_wake, 1);
    assert!(!back.unprompted);
}

#[test]
fn test_serde_roundtrip_trigger() {
    let cap = make_trig_cap("trig-001", "agent-b");
    let json = serde_json::to_string(&cap).unwrap();
    let back: TriggerCapability = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, "trig-001");
    assert_eq!(back.kind, "trigger");
    assert_eq!(back.event, "timer.tick.daily");
    assert_eq!(back.created_by, "agent-b");
}

// ── Atomic write ordering (bin first, JSON last) ──────────────────────────────

#[test]
fn test_atomic_write_both_exist_after_add() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    fs::create_dir_all(&registry_dir).unwrap();

    let cap = make_fn_cap("fn-atomic", "agent-a");
    add_function(&registry_dir, &cap, b"#!/bin/sh\necho hello").unwrap();

    assert!(
        registry_dir.join("bin/fn-atomic").exists(),
        "bin must exist after add"
    );
    assert!(
        registry_dir.join("functions/fn-atomic.json").exists(),
        "json must exist after add"
    );
}

#[test]
fn test_bin_is_executable_after_add() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    let cap = make_fn_cap("fn-exec", "agent-a");
    add_function(&registry_dir, &cap, b"#!/bin/sh\necho hi").unwrap();

    let bin_path = registry_dir.join("bin/fn-exec");
    let perms = fs::metadata(&bin_path).unwrap().permissions();
    assert!(
        perms.mode() & 0o111 != 0,
        "bin must have execute permission"
    );
}

// ── Reader never sees a half-written capability ───────────────────────────────

#[test]
fn test_catalog_excludes_bin_without_json() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    let bin_dir = registry_dir.join("bin");
    let fn_dir = registry_dir.join("functions");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&fn_dir).unwrap();

    // Only write the bin — simulate crash before JSON commit
    fs::write(bin_dir.join("fn-partial"), b"#!/bin/sh\necho partial").unwrap();

    let catalog = build_catalog(&registry_dir).unwrap();
    assert!(
        catalog.is_empty(),
        "catalog must be empty when only bin exists (no json commit barrier)"
    );
}

// ── Append-only behavior ──────────────────────────────────────────────────────

#[test]
fn test_append_call_creates_and_grows() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    fs::create_dir_all(&registry_dir).unwrap();

    let r1 = serde_json::json!({"ts": "t1", "fn_id": "fn-1"});
    let r2 = serde_json::json!({"ts": "t2", "fn_id": "fn-2"});

    append_call(&registry_dir, &r1).unwrap();
    append_call(&registry_dir, &r2).unwrap();

    let content = fs::read_to_string(registry_dir.join("calls.jsonl")).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "two records must be appended");
    assert!(lines[0].contains("fn-1"), "first record correct");
    assert!(lines[1].contains("fn-2"), "second record correct");
}

#[test]
fn test_append_call_does_not_overwrite() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    fs::create_dir_all(&registry_dir).unwrap();

    for i in 0..5 {
        append_call(
            &registry_dir,
            &serde_json::json!({"ts": format!("t{i}"), "fn_id": format!("fn-{i}")}),
        )
        .unwrap();
    }

    let content = fs::read_to_string(registry_dir.join("calls.jsonl")).unwrap();
    assert_eq!(
        content.lines().count(),
        5,
        "all 5 records must be present (append-only)"
    );
}

#[test]
fn test_append_audit_creates_and_appends() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    fs::create_dir_all(&registry_dir).unwrap();

    append_audit(
        &registry_dir,
        &serde_json::json!({"ts": "t1", "action": "capability_add", "agent_id": "agent-a"}),
    )
    .unwrap();

    let content = fs::read_to_string(registry_dir.join("audit.jsonl")).unwrap();
    assert!(content.contains("capability_add"), "audit entry written");
}

// ── Allowlist ─────────────────────────────────────────────────────────────────

#[test]
fn test_load_allowlist_missing_returns_empty() {
    let dir = TempDir::new().unwrap();
    let list = load_allowlist(dir.path()).unwrap();
    assert!(list.is_empty(), "missing allowlist.json must return empty list");
}

#[test]
fn test_load_allowlist_reads_pilot_agents() {
    let dir = TempDir::new().unwrap();
    let reg_dir = dir.path().join(".hex/registry");
    fs::create_dir_all(&reg_dir).unwrap();
    fs::write(reg_dir.join("allowlist.json"), r#"["agent-a","agent-b"]"#).unwrap();

    let list = load_allowlist(dir.path()).unwrap();
    assert_eq!(list, vec!["agent-a", "agent-b"]);
}

#[test]
fn test_is_allowed_pilot_agent_returns_true() {
    let dir = TempDir::new().unwrap();
    let reg_dir = dir.path().join(".hex/registry");
    fs::create_dir_all(&reg_dir).unwrap();
    fs::write(reg_dir.join("allowlist.json"), r#"["agent-a"]"#).unwrap();

    assert!(is_allowed(dir.path(), "agent-a"), "pilot agent must be allowed");
}

#[test]
fn test_is_allowed_non_pilot_agent_rejected() {
    let dir = TempDir::new().unwrap();
    let reg_dir = dir.path().join(".hex/registry");
    fs::create_dir_all(&reg_dir).unwrap();
    fs::write(reg_dir.join("allowlist.json"), r#"["agent-a"]"#).unwrap();

    assert!(
        !is_allowed(dir.path(), "not-a-pilot"),
        "non-pilot agent must not be allowed"
    );
}

#[test]
fn test_is_allowed_no_allowlist_rejects_all() {
    let dir = TempDir::new().unwrap();
    assert!(
        !is_allowed(dir.path(), "agent-x"),
        "with no allowlist, all agents rejected"
    );
}

// ── build_catalog ─────────────────────────────────────────────────────────────

#[test]
fn test_build_catalog_includes_functions_and_triggers() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    let fn_cap = make_fn_cap("fn-001", "agent-a");
    add_function(&registry_dir, &fn_cap, b"#!/bin/sh\necho hi").unwrap();

    let trig_cap = make_trig_cap("trig-001", "agent-b");
    add_trigger(&registry_dir, &trig_cap).unwrap();

    let catalog = build_catalog(&registry_dir).unwrap();
    assert_eq!(catalog.len(), 2, "catalog must include both function and trigger");

    let ids: Vec<&str> = catalog.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"fn-001"), "function in catalog");
    assert!(ids.contains(&"trig-001"), "trigger in catalog");
}

#[test]
fn test_catalog_entry_stripped_fields() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    let cap = make_fn_cap("fn-fields", "agent-a");
    add_function(&registry_dir, &cap, b"#!/bin/sh").unwrap();

    let catalog = build_catalog(&registry_dir).unwrap();
    assert_eq!(catalog.len(), 1);
    let entry = &catalog[0];
    assert_eq!(entry.id, "fn-fields");
    assert_eq!(entry.kind, "function");
    assert_eq!(entry.created_by, "agent-a");
    assert!(!entry.description.is_empty());
}

#[test]
fn test_catalog_empty_when_no_files() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");
    fs::create_dir_all(&registry_dir).unwrap();

    let catalog = build_catalog(&registry_dir).unwrap();
    assert!(catalog.is_empty());
}

// ── lifecycle removal ─────────────────────────────────────────────────────────

#[test]
fn test_remove_capability_removes_trigger_and_policy() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    let cap = make_trig_cap("trig-remove", "agent-a");
    add_trigger(&registry_dir, &cap).unwrap();
    write_registry_policy(&registry_dir, "trig-remove");

    // Confirm both exist before removal
    assert!(registry_dir.join("triggers/trig-remove.json").exists());
    assert!(registry_dir
        .join("policies/registry-trig-remove.yaml")
        .exists());

    remove_capability(&registry_dir, "trig-remove").unwrap();

    assert!(
        !registry_dir.join("triggers/trig-remove.json").exists(),
        "trigger JSON must be removed"
    );
    assert!(
        !registry_dir
            .join("policies/registry-trig-remove.yaml")
            .exists(),
        "policy file must be removed on lifecycle cleanup"
    );
}

#[test]
fn test_remove_capability_removes_function() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    let cap = make_fn_cap("fn-remove", "agent-a");
    add_function(&registry_dir, &cap, b"#!/bin/sh\necho hi").unwrap();

    assert!(registry_dir.join("functions/fn-remove.json").exists());
    assert!(registry_dir.join("bin/fn-remove").exists());

    remove_capability(&registry_dir, "fn-remove").unwrap();

    assert!(
        !registry_dir.join("functions/fn-remove.json").exists(),
        "function JSON must be removed"
    );
}

// ── re-entrancy guard ─────────────────────────────────────────────────────────

#[test]
fn test_reentrancy_guard_blocks_same_wake() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    // agent-a created trig-001 in wake 5 — agent-a must not be woken by it in wake 5
    let mut cap = make_trig_cap("trig-001", "agent-a");
    cap.created_in_wake = 5;
    add_trigger(&registry_dir, &cap).unwrap();
    write_registry_policy(&registry_dir, "trig-001");

    let result = check_reentrancy(&registry_dir, "agent-a", 5);
    assert!(
        result.is_err(),
        "re-entrancy guard must block agent-a from being woken by its own policy in wake 5"
    );
}

#[test]
fn test_reentrancy_guard_allows_different_wake() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    // agent-a created trig in wake 5, but we are now in wake 6 — should be allowed
    let mut cap = make_trig_cap("trig-diff-wake", "agent-a");
    cap.created_in_wake = 5;
    add_trigger(&registry_dir, &cap).unwrap();
    write_registry_policy(&registry_dir, "trig-diff-wake");

    let result = check_reentrancy(&registry_dir, "agent-a", 6);
    assert!(
        result.is_ok(),
        "re-entrancy guard must allow agent-a in wake 6 (policy was created in wake 5)"
    );
}

#[test]
fn test_reentrancy_guard_allows_different_agent() {
    let dir = TempDir::new().unwrap();
    let registry_dir = dir.path().join("registry");

    // agent-a created trig in wake 5 — agent-b is unrelated, must be allowed
    let mut cap = make_trig_cap("trig-other-agent", "agent-a");
    cap.created_in_wake = 5;
    add_trigger(&registry_dir, &cap).unwrap();
    write_registry_policy(&registry_dir, "trig-other-agent");

    let result = check_reentrancy(&registry_dir, "agent-b", 5);
    assert!(
        result.is_ok(),
        "re-entrancy guard must allow agent-b (policy was created by agent-a)"
    );
}
