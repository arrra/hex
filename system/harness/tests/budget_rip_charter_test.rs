// Red test for budget-rip task T8hwgdz6b.
//
// Asserts the post-conditions on system/harness/src/charter.rs after the
// Budget struct and the budget field on Charter are ripped out:
//
//   - No `struct Budget` / `pub struct Budget` definition in charter.rs.
//   - No `usd_per_shift` / `usd_per_day` field references in charter.rs
//     (these were the only fields validated against on the Budget struct).
//   - The parser must tolerate OLD charter.yaml files that still carry a
//     `budget:` block — the stale key gets ignored rather than failing
//     the parse. This lets the mrap-hex side migrate without ordering
//     hazards.
//
// The source-inspection tests intentionally read charter.rs from disk
// (CARGO_MANIFEST_DIR-relative) rather than importing Rust symbols —
// after the rip those symbols/fields no longer exist, so a symbol-level
// test would fail to compile instead of asserting cleanly.

use std::fs;
use std::path::PathBuf;

fn read_charter_src() -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src/charter.rs");
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn charter_rs_has_no_budget_struct_definition() {
    let src = read_charter_src();
    assert!(
        !src.contains("struct Budget"),
        "charter.rs must not define `struct Budget` after budget rip"
    );
    assert!(
        !src.contains("pub struct Budget"),
        "charter.rs must not define `pub struct Budget` after budget rip"
    );
}

#[test]
fn charter_rs_has_no_usd_per_shift_or_usd_per_day_refs() {
    let src = read_charter_src();
    assert!(
        !src.contains("usd_per_shift"),
        "charter.rs must not reference `usd_per_shift` after budget rip \
         (the Budget struct and validation against it are gone)"
    );
    assert!(
        !src.contains("usd_per_day"),
        "charter.rs must not reference `usd_per_day` after budget rip \
         (the Budget struct and validation against it are gone)"
    );
}

#[test]
fn charter_rs_has_no_charter_budget_field_access() {
    let src = read_charter_src();
    // `charter.budget.*` used to feed the validate() function in charter.rs.
    // After the rip, the `budget` field on Charter is gone, so there should
    // be no `charter.budget` access path in charter.rs.
    assert!(
        !src.contains("charter.budget"),
        "charter.rs must not access `charter.budget` after budget rip \
         (the Charter struct no longer has a `budget` field)"
    );
}

/// The parser MUST tolerate old charter.yaml files that still carry a
/// `budget:` block — workspaces (`~/hex`) migrate at their own pace and
/// the foundation must not break their parse during the transition.
#[test]
fn old_charter_yaml_with_budget_block_still_loads() {
    let yaml = r#"
id: legacy-agent
name: Legacy Agent
role: Stale charter that still carries a budget block
wake:
  triggers:
    - timer.tick.6h
  responsibilities:
    - name: health-check
      interval: 1800
      description: Run health checks
authority:
  green:
    - Read files
  yellow: []
  red: []
budget:
  wakes_per_hour: 6
  usd_per_day: 2.0
  usd_per_shift: 0.10
kill_switch: /tmp/.hex-legacy-agent-HALT
"#;
    let charter = hex::charter::load_from_str(yaml).expect(
        "charter parser must tolerate a stale `budget:` block on old charter.yaml files \
         — the key should be ignored, not rejected",
    );
    assert_eq!(charter.id, "legacy-agent");
    assert_eq!(charter.name, "Legacy Agent");
}
