/// G1 — CI guardrail: every Rust shellout target must exist in the repo.
///
/// Checks two sets of paths:
///   1. `const *_REL: &str = "..."` constants in system/harness/src/ — these are
///      explicit module-level declarations of hard shellout dependencies.
///   2. A curated supplement list of critical shellout paths that are referenced
///      via dynamic .join() calls rather than named constants.
///
/// Path mapping: .hex/scripts/X → system/scripts/X (the install step copies
/// system/scripts/ into .hex/scripts/).
///
/// A failure means a script was renamed/deleted without updating the Rust caller
/// (the shellout-rename bug documented in D5b).
use regex::Regex;
use std::path::PathBuf;
use walkdir::WalkDir;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is system/harness/ — go up two levels to repo root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("system/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

/// Map .hex/scripts/X → system/scripts/X. Returns None for paths we don't track.
fn map_to_repo_path(raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix(".hex/scripts/") {
        Some(format!("system/scripts/{}", rest))
    } else if raw.starts_with("system/scripts/") {
        Some(raw.to_string())
    } else {
        // Skip .hex/secrets/, .hex/templates/, evolution/, etc.
        None
    }
}

/// Supplement: hard shellout paths not expressed as `const *_REL` constants.
/// These are critical runtime dependencies verified by code inspection.
fn supplement_paths() -> Vec<(&'static str, &'static str)> {
    vec![
        // integration_check_all.rs + integration_cmd.rs both shell out to this
        (
            "hex_dir.join(\"system/scripts/hex-integration-check.sh\")",
            "system/scripts/hex-integration-check.sh",
        ),
    ]
}

#[test]
fn all_shellout_targets_exist() {
    let repo = repo_root();
    let src_dir = repo.join("system/harness/src");

    // const *_REL: &str = "..." — explicit hard-shellout declarations
    let re_const = Regex::new(r#"const\s+\w+_REL\s*:\s*&str\s*=\s*"([^"]+)""#).unwrap();

    let mut failures: Vec<String> = Vec::new();
    let mut ok_count: usize = 0;

    let mut check = |label: &str, raw: &str| {
        if let Some(system_path) = map_to_repo_path(raw) {
            let full = repo.join(&system_path);
            if !full.exists() {
                failures.push(format!("MISSING  {label}: {raw} → {system_path}"));
            } else {
                ok_count += 1;
            }
        }
    };

    // Pass 1: scan source for const *_REL declarations
    for entry in WalkDir::new(&src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
    {
        let rel_src = entry
            .path()
            .strip_prefix(&repo)
            .unwrap_or(entry.path())
            .display()
            .to_string();

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for cap in re_const.captures_iter(&content) {
            check(&rel_src, &cap[1]);
        }
    }

    // Pass 2: supplement list
    for (label, path) in supplement_paths() {
        check(label, path);
    }

    eprintln!("shellout_paths: {ok_count} OK, {} missing", failures.len());

    if !failures.is_empty() {
        panic!(
            "shellout_paths: {} shellout target(s) missing from repo:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
