// Red test for budget-rip task T7yrwatra.
//
// Asserts the post-conditions on system/harness/src/wake.rs after the
// shift-budget and period-budget gates are ripped out:
//
//   - No `shift-budget-hit` or `period-budget-hit` audit event types.
//   - No `charter_data.budget` field access (the `Budget` struct is gone).
//   - No `shift_budget` binding (was: `let shift_budget = charter_data.budget.usd_per_shift;`).
//   - No `cost::shift_budget_remaining` / `cost::period_budget_exhausted` callsites.
//   - No `budget_ok_for_retry` computation feeding `retry_if_empty`.
//   - No late-loop `budget_remaining` / `has_budget` bindings.
//   - No `state::initialize(..., charter_data.budget.usd_per_day)` call.
//
// This file inspects the source on disk (CARGO_MANIFEST_DIR-relative) rather
// than importing the Rust symbols, because the whole point of the task is
// that those symbols no longer exist — a test that referenced them would
// fail to compile after the rip rather than asserting cleanly.

use std::fs;
use std::path::PathBuf;

fn read_wake_src() -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src/wake.rs");
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn wake_rs_has_no_shift_budget_hit_audit() {
    let src = read_wake_src();
    assert!(
        !src.contains("shift-budget-hit"),
        "wake.rs must not emit `shift-budget-hit` audit events after budget rip"
    );
}

#[test]
fn wake_rs_has_no_period_budget_hit_audit() {
    let src = read_wake_src();
    assert!(
        !src.contains("period-budget-hit"),
        "wake.rs must not emit `period-budget-hit` audit events after budget rip"
    );
}

#[test]
fn wake_rs_has_no_charter_data_budget_access() {
    let src = read_wake_src();
    assert!(
        !src.contains("charter_data.budget"),
        "wake.rs must not access `charter_data.budget` after budget rip (Budget struct is gone)"
    );
}

#[test]
fn wake_rs_has_no_shift_budget_binding() {
    let src = read_wake_src();
    // The `let shift_budget = ...` binding at the top of the loop region must go.
    assert!(
        !src.contains("shift_budget"),
        "wake.rs must not bind or reference `shift_budget` after budget rip"
    );
}

#[test]
fn wake_rs_has_no_shift_or_period_budget_helper_calls() {
    let src = read_wake_src();
    assert!(
        !src.contains("shift_budget_remaining"),
        "wake.rs must not call `cost::shift_budget_remaining` after budget rip"
    );
    assert!(
        !src.contains("period_budget_exhausted"),
        "wake.rs must not call `cost::period_budget_exhausted` after budget rip"
    );
}

#[test]
fn wake_rs_has_no_budget_ok_for_retry() {
    let src = read_wake_src();
    assert!(
        !src.contains("budget_ok_for_retry"),
        "wake.rs must not compute `budget_ok_for_retry` after budget rip — pass `true` to retry_if_empty"
    );
}

#[test]
fn wake_rs_has_no_late_loop_budget_remaining_or_has_budget() {
    let src = read_wake_src();
    assert!(
        !src.contains("budget_remaining"),
        "wake.rs must not bind `budget_remaining` in the assessment tail after budget rip"
    );
    assert!(
        !src.contains("has_budget"),
        "wake.rs must not bind `has_budget` in the assessment tail after budget rip"
    );
}
