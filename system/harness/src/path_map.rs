/// Port of .hex/scripts/path-mapping.sh
/// Translates paths between v1 (dot-claude/) and v2 (system/ + templates/) layouts.

/// Translate a v1 source-relative path to the v2 equivalent.
/// Returns Some(v2_path) on success, None if path has no v2 equivalent.
pub fn v1_to_v2(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("dot-claude/scripts/") {
        Some(format!("system/scripts/{}", rest))
    } else if let Some(rest) = path.strip_prefix("dot-claude/skills/") {
        Some(format!("system/skills/{}", rest))
    } else if let Some(rest) = path.strip_prefix("dot-claude/commands/") {
        Some(format!("system/commands/{}", rest))
    } else if path == "CLAUDE.md" {
        Some("templates/CLAUDE.md".to_string())
    } else {
        None
    }
}

/// Translate a v2 source-relative path to the v1 equivalent.
/// Returns Some(v1_path) on success, None if path has no v1 equivalent.
pub fn v2_to_v1(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("system/scripts/") {
        Some(format!("dot-claude/scripts/{}", rest))
    } else if let Some(rest) = path.strip_prefix("system/skills/") {
        Some(format!("dot-claude/skills/{}", rest))
    } else if let Some(rest) = path.strip_prefix("system/commands/") {
        Some(format!("dot-claude/commands/{}", rest))
    } else if path == "templates/CLAUDE.md" {
        Some("CLAUDE.md".to_string())
    } else {
        None
    }
}

/// Detect the source repo layout.
/// Returns "v1", "v2", or "unknown". v1 takes priority if both coexist.
pub fn detect_layout(root: &str) -> &'static str {
    let root_path = std::path::Path::new(root);
    if root_path.join("dot-claude").is_dir() {
        return "v1";
    }
    if root_path.join("system").is_dir() && root_path.join("templates/CLAUDE.md").is_file() {
        return "v2";
    }
    "unknown"
}

pub fn run_v1_to_v2(path: &str) {
    match v1_to_v2(path) {
        Some(v2) => println!("{}", v2),
        None => std::process::exit(1),
    }
}

pub fn run_v2_to_v1(path: &str) {
    match v2_to_v1(path) {
        Some(v1) => println!("{}", v1),
        None => std::process::exit(1),
    }
}

pub fn run_detect_layout(root: &str) {
    let layout = detect_layout(root);
    println!("{}", layout);
    if layout == "unknown" {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // v1_to_v2 tests
    #[test]
    fn v1_scripts_to_v2() {
        assert_eq!(
            v1_to_v2("dot-claude/scripts/foo.sh"),
            Some("system/scripts/foo.sh".to_string())
        );
    }

    #[test]
    fn v1_skills_to_v2() {
        assert_eq!(
            v1_to_v2("dot-claude/skills/bar/skill.yaml"),
            Some("system/skills/bar/skill.yaml".to_string())
        );
    }

    #[test]
    fn v1_commands_to_v2() {
        assert_eq!(
            v1_to_v2("dot-claude/commands/baz.md"),
            Some("system/commands/baz.md".to_string())
        );
    }

    #[test]
    fn v1_claude_md_to_v2() {
        assert_eq!(v1_to_v2("CLAUDE.md"), Some("templates/CLAUDE.md".to_string()));
    }

    #[test]
    fn v1_unknown_returns_none() {
        assert_eq!(v1_to_v2("some/random/path"), None);
    }

    // v2_to_v1 tests
    #[test]
    fn v2_scripts_to_v1() {
        assert_eq!(
            v2_to_v1("system/scripts/foo.sh"),
            Some("dot-claude/scripts/foo.sh".to_string())
        );
    }

    #[test]
    fn v2_skills_to_v1() {
        assert_eq!(
            v2_to_v1("system/skills/bar/skill.yaml"),
            Some("dot-claude/skills/bar/skill.yaml".to_string())
        );
    }

    #[test]
    fn v2_commands_to_v1() {
        assert_eq!(
            v2_to_v1("system/commands/baz.md"),
            Some("dot-claude/commands/baz.md".to_string())
        );
    }

    #[test]
    fn v2_claude_md_to_v1() {
        assert_eq!(v2_to_v1("templates/CLAUDE.md"), Some("CLAUDE.md".to_string()));
    }

    #[test]
    fn v2_unknown_returns_none() {
        assert_eq!(v2_to_v1("some/random/path"), None);
    }

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
        fs::write(templates.join("CLAUDE.md"), "# test").unwrap();
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
        fs::write(templates.join("CLAUDE.md"), "# test").unwrap();
        assert_eq!(detect_layout(dir.to_str().unwrap()), "v1");
        fs::remove_dir_all(&dir).unwrap();
    }

    // round-trip tests
    #[test]
    fn v1_v2_roundtrip_scripts() {
        let v1 = "dot-claude/scripts/some-script.sh";
        let v2 = v1_to_v2(v1).unwrap();
        assert_eq!(v2_to_v1(&v2).unwrap(), v1);
    }

    #[test]
    fn v1_v2_roundtrip_skills() {
        let v1 = "dot-claude/skills/memory/skill.yaml";
        let v2 = v1_to_v2(v1).unwrap();
        assert_eq!(v2_to_v1(&v2).unwrap(), v1);
    }
}
