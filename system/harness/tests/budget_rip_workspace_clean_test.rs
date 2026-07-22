// delete after 2026-09-01 — migration-window scaffolding, not permanent coverage
//
// Red test for budget-rip task Tpp0wy712 — the final cleanup gate.
//
// Asserts the workspace-level post-condition for the whole budget rip:
// no budget machinery substring may appear anywhere under
// `system/harness/src/`. Earlier tasks targeted wake.rs / charter.rs /
// types.rs / cost.rs / budget_reset.rs explicitly; this test catches the
// stragglers (e.g. agent_spawn.rs's own `struct Budget`, main.rs's
// `state.pointer("/cost/current_period/...")` accessors, and any
// `usd_per_day` references that survive in agent_evolution.rs or
// elsewhere).
//
// Banned substrings come straight from the verifier in the task contract:
//
//   CostPeriod
//   current_period
//   struct Budget
//   usd_per_shift
//   usd_per_day
//   budget_usd
//   spent_usd
//   shift_budget
//   period_budget
//
// Like the other budget_rip_*_test.rs files, this inspects source on disk
// rather than importing symbols — the whole point is that the symbols are
// gone, so a test that referenced them would fail to compile instead of
// asserting cleanly.

use std::fs;
use std::path::{Path, PathBuf};

const BANNED: &[&str] = &[
    "CostPeriod",
    "current_period",
    "struct Budget",
    "usd_per_shift",
    "usd_per_day",
    "budget_usd",
    "spent_usd",
    "shift_budget",
    "period_budget",
];

fn harness_src_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("src");
    path
}

fn walk_rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => panic!("could not read dir {}: {e}", root.display()),
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_rust_files(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_budget_symbols_anywhere_in_harness_src() {
    let root = harness_src_dir();
    let mut files = Vec::new();
    walk_rust_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "expected to find Rust source files under {}",
        root.display()
    );

    let mut offenders: Vec<(PathBuf, &'static str, usize)> = Vec::new();
    for file in &files {
        let src = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => panic!("could not read {}: {e}", file.display()),
        };
        for (lineno, line) in src.lines().enumerate() {
            for needle in BANNED {
                if line.contains(needle) {
                    offenders.push((file.clone(), *needle, lineno + 1));
                }
            }
        }
    }

    if !offenders.is_empty() {
        let mut msg = String::from(
            "Budget rip incomplete — these substrings must not appear under system/harness/src/:\n",
        );
        for (path, needle, lineno) in &offenders {
            let rel = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path);
            msg.push_str(&format!(
                "  {}:{lineno}  contains `{needle}`\n",
                rel.display()
            ));
        }
        panic!("{msg}");
    }
}
