// Red test for budget-rip task T2c9zef8w.
//
// Asserts the post-conditions for the deletion of `system/harness/src/budget_reset.rs`
// and removal of every module/use reference to it:
//
//   - The `budget_reset.rs` file is gone.
//   - No `pub mod budget_reset` / `mod budget_reset` declarations remain in
//     `lib.rs`, `main.rs`, or `doctor/mod.rs`.
//   - No `use ... budget_reset` import / `budget_reset::` qualified path
//     reference remains in `lib.rs` or `main.rs`.
//   - `state::initialize` no longer references `CostPeriod` or
//     `cost.current_period` — those fields are gone from `types.rs` and
//     `state.rs` must stop snapshotting them.
//
// This file scans the source on disk (CARGO_MANIFEST_DIR-relative) rather
// than importing Rust symbols — the whole point of the task is that those
// symbols are gone, so a `use hex::budget_reset` would simply fail to compile.

use std::fs;
use std::path::{Path, PathBuf};

fn src_path(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("src");
    p.push(rel);
    p
}

fn read_if_exists(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok()
}

fn read_required(p: &Path) -> String {
    fs::read_to_string(p)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", p.display()))
}

#[test]
fn budget_reset_rs_file_is_gone() {
    let p = src_path("budget_reset.rs");
    assert!(
        !p.exists(),
        "system/harness/src/budget_reset.rs must be deleted after budget rip (still present at {})",
        p.display()
    );
}

/// Scan a known list of likely parents for `mod budget_reset` /
/// `pub mod budget_reset` declarations. Hard-coded over walking because
/// `walkdir` is not a dev-dependency.
#[test]
fn no_budget_reset_module_declaration_in_likely_parents() {
    let candidates = [
        "lib.rs",
        "main.rs",
        "doctor/mod.rs",
    ];
    let mut offenders: Vec<String> = Vec::new();
    for rel in &candidates {
        let p = src_path(rel);
        let Some(src) = read_if_exists(&p) else { continue };
        for (lineno, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("mod budget_reset")
                || trimmed.starts_with("pub mod budget_reset")
            {
                offenders.push(format!("{}:{}: {}", p.display(), lineno + 1, line));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "no `mod budget_reset` declarations may remain in src/ parents after budget rip; found:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn no_budget_reset_qualified_reference_in_main_or_lib() {
    // The doctor command in main.rs used to call `budget_reset::run(...)`.
    // After the rip, no `budget_reset::` qualified path or `use ... budget_reset`
    // may remain in main.rs or lib.rs.
    let candidates = ["lib.rs", "main.rs"];
    let mut offenders: Vec<String> = Vec::new();
    for rel in &candidates {
        let p = src_path(rel);
        let Some(src) = read_if_exists(&p) else { continue };
        for (lineno, line) in src.lines().enumerate() {
            if line.contains("budget_reset::")
                || line.contains("use budget_reset")
                || line.contains("use crate::budget_reset")
            {
                offenders.push(format!("{}:{}: {}", p.display(), lineno + 1, line));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "no `budget_reset` qualified references may remain in main.rs / lib.rs after budget rip; found:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn state_rs_has_no_cost_period_handling() {
    // state.rs (which holds `state::initialize`) must no longer reference
    // `CostPeriod` or `current_period` snapshots — those fields are gone
    // from `Cost` after the rip.
    let p = src_path("state.rs");
    let src = read_required(&p);
    assert!(
        !src.contains("CostPeriod"),
        "state.rs must not import or reference `CostPeriod` after budget rip"
    );
    assert!(
        !src.contains("current_period"),
        "state.rs must not handle `cost.current_period` after budget rip"
    );
}
