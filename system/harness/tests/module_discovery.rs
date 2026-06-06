//! Module-discovery behavior against the real generated registry.

#[test]
fn core_modules_are_discovered_with_source_paths() {
    let paths = hex::workers::hex_modules::module_paths();
    // memory_maintenance + backup were migrated into src/modules/ and must show up.
    let names: Vec<&str> = paths.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"hex-memory-maintenance"), "got: {names:?}");
    assert!(names.contains(&"hex-backup"), "got: {names:?}");
    // Their source paths point at *.worker.rs under src/modules/.
    for (name, path) in &paths {
        if name == "hex-memory-maintenance" || name == "hex-backup" {
            assert!(
                path.contains("/src/modules/") && path.ends_with(".worker.rs"),
                "{name} source should be a src/modules/*.worker.rs file, got {path}"
            );
        }
    }
}

#[test]
fn registry_matches_module_registry_plus_optional_e2e() {
    // Without the e2e env flag, registry() == module_registry().
    let reg = hex::workers::registry();
    let gen = hex::workers::hex_modules::module_registry();
    assert_eq!(
        reg.len(),
        gen.len(),
        "registry() should equal module_registry() when HEX_HARNESS_E2E is unset"
    );
}
