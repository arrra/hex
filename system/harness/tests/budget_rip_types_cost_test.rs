// Red test for budget-rip task Thppbyn9n.
//
// Asserts the post-conditions on system/harness/src/types.rs and
// system/harness/src/cost.rs after the period-budget concept is ripped out:
//
//   - types.rs: no `CostPeriod` struct and no `current_period` field;
//     `lifetime_usd` and `last_wake_usd` are still there.
//   - cost.rs:  no `shift_budget_remaining`, no `period_budget_exhausted`,
//     and no leftover `current_period.spent_usd +=` mutation inside
//     `record_invocation`. `pub fn record_invocation` itself stays.
//
// This file inspects the source on disk (CARGO_MANIFEST_DIR-relative) rather
// than importing the Rust symbols, because the whole point of the task is
// that those symbols no longer exist — a test that referenced them would
// fail to compile after the rip rather than asserting cleanly.

use std::fs;
use std::path::PathBuf;

fn read_src(rel: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn types_rs_has_no_cost_period_or_current_period() {
    let src = read_src("src/types.rs");
    assert!(
        !src.contains("CostPeriod"),
        "types.rs must not mention `CostPeriod` after budget rip"
    );
    assert!(
        !src.contains("current_period"),
        "types.rs must not mention `current_period` after budget rip"
    );
}

#[test]
fn types_rs_keeps_lifetime_and_last_wake_fields() {
    let src = read_src("src/types.rs");
    assert!(
        src.contains("lifetime_usd"),
        "types.rs must keep `lifetime_usd` on Cost (cost recording stays)"
    );
    assert!(
        src.contains("last_wake_usd"),
        "types.rs must keep `last_wake_usd` on Cost (cost recording stays)"
    );
}

#[test]
fn cost_rs_has_no_shift_or_period_budget_helpers() {
    let src = read_src("src/cost.rs");
    assert!(
        !src.contains("shift_budget_remaining"),
        "cost.rs must not contain `shift_budget_remaining` after budget rip"
    );
    assert!(
        !src.contains("period_budget_exhausted"),
        "cost.rs must not contain `period_budget_exhausted` after budget rip"
    );
}

#[test]
fn cost_rs_keeps_record_invocation_but_drops_current_period_mutation() {
    let src = read_src("src/cost.rs");
    assert!(
        src.contains("pub fn record_invocation"),
        "cost.rs must keep `pub fn record_invocation` (cost recording stays)"
    );
    // The line `cost.current_period.spent_usd += usd;` (and any analogue)
    // must be gone — that field no longer exists on `Cost`.
    assert!(
        !src.contains("current_period"),
        "cost.rs must not reference `current_period` anywhere after budget rip"
    );
}
