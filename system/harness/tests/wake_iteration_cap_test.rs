// Red test for wake-iteration-cap task Teqjb6aeb.
//
// The 2026-05-29 budget rip removed the per-wake shift-budget gate that
// previously broke the wake loop on `cost::shift_budget_remaining(...) <= 0`.
// The loop is now UNBOUNDED — an agent doing varied novel work each
// invocation (different action hashes, queue keeps growing) can loop forever.
// `check_and_handle_loop` catches same-action-3x, NOT same-pattern-varied-cost.
//
// This task adds a HARD iteration cap with a loud break:
//   const MAX_INVOCATIONS_PER_WAKE: usize = 50;
// emitting an `eprintln!` WARN line and an `audit::append` with kind
// `wake-iteration-cap-hit` when the loop hits the cap.
//
// The constant is private to wake.rs (no public accessor), so this test
// reads the source file at CARGO_MANIFEST_DIR/src/wake.rs and asserts the
// declaration appears verbatim. The single-line check is intentionally
// strict — its sole purpose is to guard against an off-by-one future edit
// that silently raises or lowers the cap. Full behavioral coverage of the
// wake loop is out of scope for this test.

use std::fs;
use std::path::PathBuf;

fn read_wake_src() -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src/wake.rs");
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

/// The hard wake-iteration cap must be declared as a `usize` constant set to
/// exactly 50. Any other value (or a missing declaration) silently changes
/// loop bounding behavior and must fail loudly here.
#[test]
fn cap_is_50() {
    let src = read_wake_src();
    let needle = "const MAX_INVOCATIONS_PER_WAKE: usize = 50;";
    assert!(
        src.contains(needle),
        "wake.rs must declare `{needle}` — the hard per-wake iteration cap. \
         Found wake.rs at {}/src/wake.rs but the exact constant declaration \
         was not present. If the value was intentionally changed, update \
         this test deliberately — do not silently raise or lower the cap.",
        env!("CARGO_MANIFEST_DIR")
    );
}
