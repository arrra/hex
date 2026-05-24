use chrono::Utc;
use hex::registry::{self, FunctionCapability};
use hex::types::TrailEntry;
use std::fs;
use std::io::BufRead;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_allowlist(hex_dir: &std::path::Path, agents: &[&str]) {
    let reg_dir = hex_dir.join(".hex/registry");
    fs::create_dir_all(&reg_dir).unwrap();
    let json = serde_json::to_string(agents).unwrap();
    fs::write(reg_dir.join("allowlist.json"), json).unwrap();
}

fn make_sandbox(hex_dir: &std::path::Path) -> std::path::PathBuf {
    let sandbox = hex_dir.join(".hex/containers");
    fs::create_dir_all(&sandbox).unwrap();
    let script = "#!/bin/sh\nexec \"$@\"\n";
    let script_path = sandbox.join("run-test.sh");
    fs::write(&script_path, script).unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();
    sandbox
}

fn make_trail_entry(entry_type: &str, detail: serde_json::Value) -> TrailEntry {
    TrailEntry {
        ts: Utc::now(),
        entry_type: entry_type.to_string(),
        detail,
        queue_item: None,
    }
}

// ── Test: allowlisted agent's capability_add persists ────────────────────────

#[test]
fn test_allowlisted_capability_add_persists() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    make_allowlist(hex_dir, &["agent-alpha"]);
    let sandbox = make_sandbox(hex_dir);

    let entry = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "greet-fn",
            "description": "greet the user",
            "wall_hit": "30s",
            "exec_or_event": "#!/bin/sh\necho hello\n"
        }),
    );

    let mut call_count = 0u32;
    let result = hex::wake::apply_capability_entry(
        &entry,
        "agent-alpha",
        hex_dir,
        1,
        &mut call_count,
        &sandbox,
    );

    assert!(
        result.is_ok(),
        "allowlisted agent capability_add must succeed, got: {:?}",
        result.err()
    );
    assert!(
        result.unwrap().is_none(),
        "capability_add must return None (no result entry)"
    );

    let reg_dir = hex_dir.join(".hex/registry");
    assert!(
        reg_dir.join("functions/greet-fn.json").exists(),
        "functions/greet-fn.json must be written after capability_add"
    );
    assert!(
        reg_dir.join("bin/greet-fn").exists(),
        "bin/greet-fn executable must be written after capability_add"
    );
}

// ── Test: capability_call executes and result lands as a trail entry ──────────

#[test]
fn test_capability_call_result_entry() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    make_allowlist(hex_dir, &["agent-beta"]);
    let sandbox = make_sandbox(hex_dir);

    // Pre-register a function capability directly via registry helpers
    let reg_dir = hex_dir.join(".hex/registry");
    let cap = FunctionCapability {
        id: "echo-fn".to_string(),
        kind: "function".to_string(),
        created_by: "agent-beta".to_string(),
        created_at: Utc::now().to_rfc3339(),
        created_in_wake: 1,
        unprompted: false,
        description: "echoes text".to_string(),
        exec: "#!/bin/sh\necho hello-from-fn\n".to_string(),
        input_schema: serde_json::json!({}),
        callable_by: vec!["agent-beta".to_string()],
    };
    registry::add_function(&reg_dir, &cap, b"#!/bin/sh\necho hello-from-fn\n").unwrap();

    let entry = make_trail_entry(
        "capability_call",
        serde_json::json!({
            "capability_id": "echo-fn",
            "args": {}
        }),
    );

    let mut call_count = 0u32;
    let result = hex::wake::apply_capability_entry(
        &entry,
        "agent-beta",
        hex_dir,
        1,
        &mut call_count,
        &sandbox,
    );

    assert!(
        result.is_ok(),
        "allowlisted agent capability_call must succeed, got: {:?}",
        result.err()
    );
    let maybe_entry = result.unwrap();
    assert!(
        maybe_entry.is_some(),
        "capability_call must return a result TrailEntry"
    );
    let result_entry = maybe_entry.unwrap();
    assert_eq!(
        result_entry.entry_type, "act",
        "result entry type must be 'act'"
    );

    let stdout = result_entry.detail["result"]["stdout"]
        .as_str()
        .unwrap_or("");
    assert!(
        stdout.contains("hello-from-fn"),
        "result entry stdout must contain script output, got: '{stdout}'"
    );
}

// ── Test: non-allowlisted agent's capability_add is rejected ─────────────────

#[test]
fn test_non_allowlisted_capability_add_rejected() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    // allowlist contains only "agent-alpha", NOT "rogue-agent"
    make_allowlist(hex_dir, &["agent-alpha"]);

    let entry = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "rogue-fn",
            "description": "does something nefarious",
            "wall_hit": "30s",
            "exec_or_event": "#!/bin/sh\necho rogue\n"
        }),
    );

    let sandbox = hex_dir.join(".hex/containers");
    let mut call_count = 0u32;
    let result = hex::wake::apply_capability_entry(
        &entry,
        "rogue-agent",
        hex_dir,
        1,
        &mut call_count,
        &sandbox,
    );

    assert!(
        result.is_err(),
        "non-allowlisted agent capability_add must be rejected"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("rogue-agent"),
        "rejection error must name the agent, got: '{err}'"
    );

    let reg_dir = hex_dir.join(".hex/registry");
    assert!(
        !reg_dir.join("functions/rogue-fn.json").exists(),
        "no function file must be written for a rejected add"
    );
}

// ── Test: sibling catalog is ordered after registry.capability.added event ────
//
// Wake-ordering contract: a `capability_add` MUST emit a `registry.capability.added`
// event to `.hex/registry/events.jsonl` AFTER the capability is fully persisted.
// A sibling pilot agent whose wake is triggered by `registry.capability.added`
// (rather than an unordered `timer.tick.daily` fan-out) is guaranteed to observe the
// new capability in its catalog because:
//   1. The capability files are written before the event is emitted.
//   2. The event emission is the only ordering signal — any reader that receives the
//      event and then calls build_catalog will see a consistent registry state.

#[test]
fn test_sibling_catalog_ordered_after_capability_added_event() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    make_allowlist(hex_dir, &["agent-alpha", "agent-beta"]);
    let sandbox = make_sandbox(hex_dir);

    // Agent alpha adds a function capability.
    let entry = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "shared-fn",
            "description": "shared helper function",
            "wall_hit": "30s",
            "exec_or_event": "#!/bin/sh\necho shared\n"
        }),
    );

    let mut call_count = 0u32;
    let result = hex::wake::apply_capability_entry(
        &entry,
        "agent-alpha",
        hex_dir,
        1,
        &mut call_count,
        &sandbox,
    );
    assert!(result.is_ok(), "capability_add must succeed: {:?}", result.err());

    // The `registry.capability.added` event must be recorded in events.jsonl
    // AFTER capability files are persisted. This is the ordering signal that
    // sibling agents can rely on.
    let events_path = hex_dir.join(".hex/registry/events.jsonl");
    assert!(
        events_path.exists(),
        "events.jsonl must be written after capability_add (ordering signal)"
    );
    let events_content = fs::read_to_string(&events_path).unwrap();
    assert!(
        events_content.contains("registry.capability.added"),
        "events.jsonl must contain registry.capability.added, got: '{events_content}'"
    );
    assert!(
        events_content.contains("shared-fn"),
        "registry.capability.added event must name the capability id"
    );
    assert!(
        events_content.contains("agent-alpha"),
        "registry.capability.added event must name the creating agent"
    );

    // Simulate sibling agent-beta's catalog build on receipt of the event.
    // Because events are emitted only after persistence, this catalog is
    // guaranteed to contain the newly added capability.
    let reg_dir = hex_dir.join(".hex/registry");
    let catalog = hex::registry::build_catalog(&reg_dir).expect("build_catalog must not fail");
    let found = catalog.iter().any(|e| e.id == "shared-fn" && e.created_by == "agent-alpha");
    assert!(
        found,
        "sibling catalog at wake time must contain the newly added capability"
    );
}

// ── Test (a): capability_add writes a row to audit.jsonl ─────────────────────

#[test]
fn test_capability_add_writes_audit_jsonl() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    make_allowlist(hex_dir, &["agent-alpha"]);
    let sandbox = make_sandbox(hex_dir);

    let entry = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "audit-fn",
            "description": "test audit write",
            "wall_hit": "slow log scanning",
            "exec_or_event": "#!/bin/sh\necho ok\n"
        }),
    );

    let mut call_count = 0u32;
    let result = hex::wake::apply_capability_entry(
        &entry,
        "agent-alpha",
        hex_dir,
        1,
        &mut call_count,
        &sandbox,
    );
    assert!(result.is_ok(), "capability_add must succeed: {:?}", result.err());

    let audit_path = hex_dir.join(".hex/registry/audit.jsonl");
    assert!(audit_path.exists(), "audit.jsonl must be created after capability_add");

    let content = fs::read_to_string(&audit_path).unwrap();
    let row: serde_json::Value = serde_json::from_str(content.trim()).unwrap();

    assert_eq!(row["capability_id"].as_str().unwrap(), "audit-fn");
    assert_eq!(row["capability_kind"].as_str().unwrap(), "function");
    assert_eq!(row["created_by"].as_str().unwrap(), "agent-alpha");
    assert!(row["ts"].as_str().is_some(), "audit row must have ts");
    assert!(row.get("unprompted").is_some(), "audit row must have unprompted field");
    assert!(row.get("exec_or_event").is_some(), "audit row must have exec_or_event field");
}

// ── Test (b): unprompted field is honored ─────────────────────────────────────

#[test]
fn test_capability_add_unprompted_honored() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    make_allowlist(hex_dir, &["agent-alpha"]);
    let sandbox = make_sandbox(hex_dir);

    // With unprompted: true
    let entry_unprompted = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "unprompted-fn",
            "description": "spontaneous capability",
            "wall_hit": "noticed a gap",
            "exec_or_event": "#!/bin/sh\necho unprompted\n",
            "unprompted": true
        }),
    );

    let mut call_count = 0u32;
    hex::wake::apply_capability_entry(
        &entry_unprompted, "agent-alpha", hex_dir, 1, &mut call_count, &sandbox,
    ).unwrap();

    let audit_path = hex_dir.join(".hex/registry/audit.jsonl");
    let content = fs::read_to_string(&audit_path).unwrap();
    let row: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
    assert_eq!(row["unprompted"].as_bool().unwrap(), true, "unprompted:true must be in audit row");

    // Check function JSON also has unprompted:true
    let fn_json = hex_dir.join(".hex/registry/functions/unprompted-fn.json");
    let fn_val: serde_json::Value = serde_json::from_str(&fs::read_to_string(fn_json).unwrap()).unwrap();
    assert_eq!(fn_val["unprompted"].as_bool().unwrap(), true, "function JSON must record unprompted:true");

    // Without unprompted (should default to false)
    let entry_default = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "prompted-fn",
            "description": "prompted capability",
            "wall_hit": "task required it",
            "exec_or_event": "#!/bin/sh\necho prompted\n"
        }),
    );

    hex::wake::apply_capability_entry(
        &entry_default, "agent-alpha", hex_dir, 2, &mut call_count, &sandbox,
    ).unwrap();

    let fn_json2 = hex_dir.join(".hex/registry/functions/prompted-fn.json");
    let fn_val2: serde_json::Value = serde_json::from_str(&fs::read_to_string(fn_json2).unwrap()).unwrap();
    assert_eq!(fn_val2["unprompted"].as_bool().unwrap(), false, "missing unprompted must default to false");
}

// ── Test (c): callable_by explicit vs. allowlist default ─────────────────────

#[test]
fn test_capability_add_callable_by_explicit_and_default() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    make_allowlist(hex_dir, &["scanner", "repairer", "monitor"]);
    let sandbox = make_sandbox(hex_dir);

    // Explicit callable_by
    let entry_explicit = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "restricted-fn",
            "description": "restricted to two agents",
            "wall_hit": "needs access control",
            "exec_or_event": "#!/bin/sh\necho restricted\n",
            "callable_by": ["scanner", "repairer"]
        }),
    );

    let mut call_count = 0u32;
    hex::wake::apply_capability_entry(
        &entry_explicit, "scanner", hex_dir, 1, &mut call_count, &sandbox,
    ).unwrap();

    let fn_json = hex_dir.join(".hex/registry/functions/restricted-fn.json");
    let fn_val: serde_json::Value = serde_json::from_str(&fs::read_to_string(fn_json).unwrap()).unwrap();
    let callable: Vec<String> = fn_val["callable_by"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    assert_eq!(callable, vec!["scanner".to_string(), "repairer".to_string()],
        "explicit callable_by must be stored exactly");

    // Omitted callable_by — should default to full allowlist
    let entry_default = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "open-fn",
            "description": "open to all pilots",
            "wall_hit": "general utility",
            "exec_or_event": "#!/bin/sh\necho open\n"
        }),
    );

    hex::wake::apply_capability_entry(
        &entry_default, "scanner", hex_dir, 2, &mut call_count, &sandbox,
    ).unwrap();

    let fn_json2 = hex_dir.join(".hex/registry/functions/open-fn.json");
    let fn_val2: serde_json::Value = serde_json::from_str(&fs::read_to_string(fn_json2).unwrap()).unwrap();
    let callable2: Vec<String> = fn_val2["callable_by"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    assert!(callable2.contains(&"scanner".to_string()), "default callable_by must include all allowlist members");
    assert!(callable2.contains(&"repairer".to_string()), "default callable_by must include all allowlist members");
    assert!(callable2.contains(&"monitor".to_string()), "default callable_by must include all allowlist members");
}

// ── Test (d): callable_by gate enforced during capability_call ───────────────

#[test]
fn test_capability_call_callable_by_gate() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    // Both agents in global allowlist; only allowed-agent in capability callable_by
    make_allowlist(hex_dir, &["allowed-agent", "blocked-agent"]);
    let sandbox = make_sandbox(hex_dir);

    // Register capability with explicit callable_by = ["allowed-agent"] only
    let add_entry = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "guarded-fn",
            "description": "only allowed-agent can call",
            "wall_hit": "access control needed",
            "exec_or_event": "#!/bin/sh\necho guarded\n",
            "callable_by": ["allowed-agent"]
        }),
    );

    let mut call_count = 0u32;
    hex::wake::apply_capability_entry(
        &add_entry, "allowed-agent", hex_dir, 1, &mut call_count, &sandbox,
    ).unwrap();

    // allowed-agent can call it
    let call_allowed = make_trail_entry(
        "capability_call",
        serde_json::json!({"capability_id": "guarded-fn", "args": {}}),
    );
    let mut cc = 0u32;
    let ok = hex::wake::apply_capability_entry(
        &call_allowed, "allowed-agent", hex_dir, 2, &mut cc, &sandbox,
    );
    assert!(ok.is_ok(), "allowed-agent must be able to call guarded-fn, got: {:?}", ok.err());
    let result_entry = ok.unwrap().expect("capability_call must return a result entry");
    assert_eq!(result_entry.detail["result"]["exit_code"].as_i64().unwrap(), 0,
        "exit_code must be 0 for a successful call");

    // blocked-agent is in global allowlist but NOT in callable_by — must be rejected
    let call_blocked = make_trail_entry(
        "capability_call",
        serde_json::json!({"capability_id": "guarded-fn", "args": {}}),
    );
    let mut cc2 = 0u32;
    let err = hex::wake::apply_capability_entry(
        &call_blocked, "blocked-agent", hex_dir, 2, &mut cc2, &sandbox,
    );
    assert!(err.is_err(), "blocked-agent must be rejected by callable_by gate");
    let msg = err.unwrap_err();
    assert!(
        msg.contains("blocked-agent") && msg.contains("callable_by"),
        "rejection message must name the agent and callable_by, got: '{msg}'"
    );
}

// ── End-to-end: agent A adds a function, agent B calls it ────────────────────
//
// Verifies a cross-agent row (caller != created_by) lands in calls.jsonl.

#[test]
fn test_cross_agent_call_logged_in_calls_jsonl() {
    let dir = TempDir::new().unwrap();
    let hex_dir = dir.path();

    // Both agents are allowlisted
    make_allowlist(hex_dir, &["agent-a", "agent-b"]);
    let sandbox = make_sandbox(hex_dir);

    // ── Step 1: agent-a adds the function capability ──────────────────────────
    let add_entry = make_trail_entry(
        "capability_add",
        serde_json::json!({
            "capability_kind": "function",
            "capability_id": "cross-fn",
            "description": "shared function created by agent-a",
            "wall_hit": "30s",
            "exec_or_event": "#!/bin/sh\necho cross-agent-output\n"
        }),
    );

    let mut call_count = 0u32;
    let add_result = hex::wake::apply_capability_entry(
        &add_entry,
        "agent-a",
        hex_dir,
        1,
        &mut call_count,
        &sandbox,
    );
    assert!(add_result.is_ok(), "agent-a capability_add must succeed: {:?}", add_result.err());

    // Sanity: capability files written
    let reg_dir = hex_dir.join(".hex/registry");
    assert!(reg_dir.join("functions/cross-fn.json").exists(), "functions/cross-fn.json must exist");
    assert!(reg_dir.join("bin/cross-fn").exists(), "bin/cross-fn must exist");

    // ── Step 2: agent-b calls the function ───────────────────────────────────
    let call_entry = make_trail_entry(
        "capability_call",
        serde_json::json!({
            "capability_id": "cross-fn",
            "args": {}
        }),
    );

    let mut call_count2 = 0u32;
    let call_result = hex::wake::apply_capability_entry(
        &call_entry,
        "agent-b",
        hex_dir,
        2,
        &mut call_count2,
        &sandbox,
    );
    assert!(call_result.is_ok(), "agent-b capability_call must succeed: {:?}", call_result.err());
    let trail_entry = call_result.unwrap();
    assert!(trail_entry.is_some(), "capability_call must return a result trail entry");

    // ── Step 3: verify calls.jsonl has a cross-agent row ─────────────────────
    let calls_path = reg_dir.join("calls.jsonl");
    assert!(calls_path.exists(), "calls.jsonl must exist after capability_call");

    let file = fs::File::open(&calls_path).unwrap();
    let mut found_cross_agent = false;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.unwrap();
        if line.is_empty() { continue; }
        let val: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or(serde_json::Value::Null);
        let caller = val["caller"].as_str().unwrap_or("");
        let created_by = val["created_by"].as_str().unwrap_or("");
        let fn_id = val["fn_id"].as_str().unwrap_or("");
        if fn_id == "cross-fn" && caller != created_by && caller == "agent-b" && created_by == "agent-a" {
            found_cross_agent = true;
            break;
        }
    }
    assert!(
        found_cross_agent,
        "calls.jsonl must contain a cross-agent row where caller='agent-b' and created_by='agent-a'"
    );
}
