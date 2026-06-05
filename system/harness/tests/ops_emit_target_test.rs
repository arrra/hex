//! Red test for `ops::emit_target` — pure event→state-target mapping.
//!
//! Contract: `emit_target(event, &data)` returns an `EmitTarget` describing the
//! state write that `emit` will perform. It must be pure (no engine, no network),
//! deterministic, and map the event name into the (scope, key) addressing iii
//! state-triggered workers fire on, with the supplied data as the value.

use hex::ops::{emit_target, EmitTarget};
use serde_json::json;

#[test]
fn emit_target_maps_event_to_state_scope_key_value() {
    let data = json!({"spec_id": "Skt0r3dbg", "status": "ok"});
    let target = emit_target("boi.spec.complete", &data);

    // scope must be the event namespace ("events") — that's the state surface
    // hex-events triggers subscribe to. key must be the full event name so
    // distinct events fire distinct triggers. value must be the supplied data.
    assert_eq!(
        target,
        EmitTarget {
            scope: "events".to_string(),
            key: "boi.spec.complete".to_string(),
            value: data,
        }
    );
}

#[test]
fn emit_target_is_pure_and_deterministic() {
    let data = json!({"n": 1});
    let a = emit_target("x.y.z", &data);
    let b = emit_target("x.y.z", &data);
    assert_eq!(a, b);
}
