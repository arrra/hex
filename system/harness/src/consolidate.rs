//! Unified `hex consolidate` orchestrator.
//!
//! Single entry point that runs all three consolidation layers:
//!   L1 — structural (doctor::consolidate)        — deterministic, no LLM
//!   L2 — memory DB    (memory::consolidate)      — deterministic, no LLM
//!   L3 — operating-model audit (provider::generate) — FULL mode only
//!
//! Modes:
//!   Quick — L1 + L2 only (safe to run nightly).
//!   Full  — L1 + L2 + L3 (writes an audit file for human review; never auto-edits sources).
//!
//! Per S6 (no quiet failures): every layer's failure is surfaced loudly to
//! stderr and reflected in the exit code. L3 failure does NOT abort L1+L2.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Quick,
    Full,
}

/// Run the unified consolidation pass. Returns a process exit code.
///   0 — all requested layers succeeded with no issues
///   1 — at least one layer reported issues or hard-failed
pub fn run(mode: Mode, hex_dir: &Path) -> i32 {
    let mut any_fail = false;

    println!("=== hex consolidate ({:?}) ===", mode);

    // Layer 1 — STRUCTURAL (deterministic)
    println!("\n-- Layer 1: structural (doctor::consolidate) --");
    let l1 = crate::doctor::consolidate::run(hex_dir);
    if l1 != 0 {
        eprintln!("Layer 1 reported issues (exit={l1})");
        any_fail = true;
    }

    // Layer 2 — MEMORY DB (deterministic)
    println!("\n-- Layer 2: memory db (memory::consolidate) --");
    let db_path = crate::memory::db_path(hex_dir);
    match crate::memory::open_db(&db_path) {
        Ok(mut conn) => match crate::memory::consolidate::run(&mut conn) {
            Ok(report) => {
                println!(
                    "Layer 2 ok={} failed={}",
                    report.ok.len(),
                    report.failed.len()
                );
                for (name, err) in &report.failed {
                    eprintln!("Layer 2 op '{name}' FAILED: {err}");
                }
                if !report.failed.is_empty() {
                    any_fail = true;
                }
            }
            Err(e) => {
                eprintln!("Layer 2 hard-failed: {e}");
                any_fail = true;
            }
        },
        Err(e) => {
            eprintln!("Layer 2 hard-failed: cannot open memory.db at {}: {e}", db_path.display());
            any_fail = true;
        }
    }

    // Layer 3 — OPERATING-MODEL AUDIT (LLM, FULL only)
    if matches!(mode, Mode::Full) {
        println!("\n-- Layer 3: operating-model audit (provider::generate) --");
        match run_layer3(hex_dir) {
            Ok(()) => println!("Layer 3 ok"),
            Err(e) => {
                eprintln!("Layer 3 FAILED (Layers 1+2 already ran): {e}");
                any_fail = true;
            }
        }
    }

    println!("\n=== consolidate done (exit={}) ===", if any_fail { 1 } else { 0 });
    if any_fail { 1 } else { 0 }
}

/// Stub for Layer 3. Filled in by the llm-audit task (Txjh0xxy9):
/// reads CLAUDE.md + me/learnings.md, calls memory::provider::generate,
/// writes evolution/consolidation-audit-YYYY-MM-DD.md + appends a log entry.
/// Never edits the operating-model source files.
fn run_layer3(_hex_dir: &Path) -> anyhow::Result<()> {
    eprintln!("Layer 3: not yet implemented (operating-model audit stub)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fake_hex_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::write(p.join("CLAUDE.md"), "").unwrap();
        let evo = p.join("evolution");
        fs::create_dir_all(&evo).unwrap();
        fs::write(evo.join("observations.md"), "").unwrap();
        fs::write(evo.join("suggestions.md"), "").unwrap();
        fs::write(evo.join("changelog.md"), "").unwrap();
        fs::create_dir_all(p.join("projects")).unwrap();
        let me = p.join("me");
        fs::create_dir_all(&me).unwrap();
        fs::write(me.join("learnings.md"), "").unwrap();
        dir
    }

    #[test]
    fn quick_mode_runs_l1_and_l2_and_writes_structural_log() {
        let dir = fake_hex_dir();
        let code = run(Mode::Quick, dir.path());
        assert!(code == 0 || code == 1, "unexpected exit code {code}");
        assert!(
            dir.path().join("evolution").join("consolidation-latest.log").exists(),
            "Layer 1 must write consolidation-latest.log"
        );
    }

    #[test]
    fn quick_mode_does_not_write_audit_file() {
        let dir = fake_hex_dir();
        let _ = run(Mode::Quick, dir.path());
        let evo = dir.path().join("evolution");
        for entry in fs::read_dir(&evo).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with("consolidation-audit-"),
                "quick must not write LLM audit: found {name}"
            );
        }
    }
}
