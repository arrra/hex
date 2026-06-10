//! Red test for spec S253fety6 / task Tdf23yg2y — `hex dial`.
//!
//! Pins the headline contract of the dial: below `min_n` confirmed outcomes
//! it returns INSUFFICIENT and NEVER a number. The dial module does not yet
//! exist; this test fails to compile until task Tdf23yg2y is implemented.
//!
//! Kept minimal on purpose — exhaustive unit tests (earn / decay / miss-reset
//! / irreversible ASK) live next to the implementation in `src/dial.rs`. This
//! file is the cross-crate API smoke that has to stay green once the task
//! lands.

use hex::dial::{self, DialOutcome, OutcomeRow};

#[test]
fn dial_below_min_n_is_insufficient_never_a_number() {
    // One confirmed end-state outcome — well below a min_n of 3.
    let rows = vec![OutcomeRow {
        agent: "agent-a".to_string(),
        action_class: "class-x".to_string(),
        success: true,
        ts: 1,
    }];

    let out = dial::compute(&rows, "agent-a", "class-x", 3, false);

    match out {
        DialOutcome::Insufficient { .. } => {}
        other => panic!(
            "below min_n must yield Insufficient, never a number. got: {:?}",
            other
        ),
    }
}
