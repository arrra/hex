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
