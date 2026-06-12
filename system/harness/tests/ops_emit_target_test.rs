//! Integration test for `ops::emit_target` — pure event→state-target mapping
//! with hex-native envelope.
//!
//! Contract: `emit_target(event, producer, ts, &data)` returns an `EmitTarget`
//! describing the state write that `emit` will perform. It must be pure (no
//! engine, no network), deterministic, and map the event name into the
//! (scope, key) addressing iii state-triggered workers fire on, with a
//! `{event, producer, ts, data}` envelope as the value.

use hex::ops::{emit_target, EmitTarget};
use serde_json::json;

#[test]
fn emit_target_maps_event_to_state_scope_key_envelope() {
    let data = json!({"spec_id": "Skt0r3dbg", "status": "ok"});
    let target = emit_target("boi.spec.complete", "cli", "2026-06-04T00:00:00Z", &data);

    assert_eq!(
        target,
        EmitTarget {
            scope: "events".to_string(),
            key: "boi.spec.complete".to_string(),
            value: json!({
                "event": "boi.spec.complete",
                "producer": "cli",
                "ts": "2026-06-04T00:00:00Z",
                "data": data,
            }),
        }
    );
}

#[test]
fn emit_target_is_pure_and_deterministic() {
    let data = json!({"n": 1});
    let a = emit_target("x.y.z", "cli", "2026-06-04T00:00:00Z", &data);
    let b = emit_target("x.y.z", "cli", "2026-06-04T00:00:00Z", &data);
    assert_eq!(a, b);
}
