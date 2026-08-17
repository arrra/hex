//! Source-repo layout detection for the upgrade flow.
//! Translates paths for the v2 (system/ + templates/) layout.

/// Detect the source repo layout.
/// Returns "v2" or "unknown".
pub fn detect_layout(root: &str) -> &'static str {
    let root_path = std::path::Path::new(root);
    if root_path.join("system").is_dir() && root_path.join("templates/AGENTS.md").is_file() {
        return "v2";
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_layout_unknown_for_missing_dir() {
        assert_eq!(
            detect_layout("/tmp/does-not-exist-path-map-test"),
            "unknown"
        );
    }

    #[test]
    fn detect_layout_v2_when_system_and_templates_present() {
        use std::fs;
        let dir = std::env::temp_dir().join("hex_path_map_test_v2");
        let system = dir.join("system");
        let templates = dir.join("templates");
        fs::create_dir_all(&system).unwrap();
        fs::create_dir_all(&templates).unwrap();
        fs::write(templates.join("AGENTS.md"), "# test").unwrap();
        assert_eq!(detect_layout(dir.to_str().unwrap()), "v2");
        fs::remove_dir_all(&dir).unwrap();
    }
}
