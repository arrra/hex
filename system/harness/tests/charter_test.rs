use std::path::PathBuf;

#[test]
fn test_load_valid_charter() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid-charter.yaml");
    let charter = hex::charter::load(&path).unwrap();
    assert_eq!(charter.id, "test-agent");
    assert_eq!(charter.name, "Test Agent");
    assert_eq!(charter.wake.responsibilities.len(), 2);
    assert_eq!(charter.authority.green.len(), 2);
}

#[test]
fn test_reject_charter_missing_id() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/missing-id-charter.yaml");
    let result = hex::charter::load(&path);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("id") || err.contains("missing"),
        "Error should mention missing 'id': {err}"
    );
}

/// The parser MUST tolerate old charter.yaml files that still carry a
/// `budget:` block — workspaces (`~/hex`) migrate at their own pace and
/// the foundation must not break their parse during the transition.
#[test]
fn test_load_charter_with_stale_budget_block_ignored() {
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
