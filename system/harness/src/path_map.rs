/// Source-repo layout detection for the upgrade flow.
/// Translates paths between v1 (dot-claude/) and v2 (system/ + templates/) layouts.

/// Detect the source repo layout.
/// Returns "v1", "v2", or "unknown". v1 takes priority if both coexist.
pub fn detect_layout(root: &str) -> &'static str {
    let root_path = std::path::Path::new(root);
    if root_path.join("dot-claude").is_dir() {
        return "v1";
    }
    if root_path.join("system").is_dir() && root_path.join("templates/AGENTS.md").is_file() {
        return "v2";
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    // detect_layout tests
    #[test]
    fn detect_layout_unknown_for_missing_dir() {
        assert_eq!(detect_layout("/tmp/does-not-exist-path-map-test"), "unknown");
    }

    #[test]
    fn detect_layout_v1_when_dot_claude_present() {
        use std::fs;
        let dir = std::env::temp_dir().join("hex_path_map_test_v1");
        let dot_claude = dir.join("dot-claude");
        fs::create_dir_all(&dot_claude).unwrap();
        assert_eq!(detect_layout(dir.to_str().unwrap()), "v1");
        fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn detect_layout_v1_priority_over_v2() {
        use std::fs;
        let dir = std::env::temp_dir().join("hex_path_map_test_both");
        let dot_claude = dir.join("dot-claude");
        let system = dir.join("system");
        let templates = dir.join("templates");
        fs::create_dir_all(&dot_claude).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::create_dir_all(&templates).unwrap();
        fs::write(templates.join("AGENTS.md"), "# test").unwrap();
        assert_eq!(detect_layout(dir.to_str().unwrap()), "v1");
        fs::remove_dir_all(&dir).unwrap();
    }
}
