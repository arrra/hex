//! Unified `hex consolidate` orchestrator.
//!
//! Single entry point that runs all consolidation layers:
//!   L1   — structural (doctor::consolidate)         — deterministic, no LLM
//!   L2   — memory DB    (memory::consolidate)       — deterministic, no LLM
//!   L2.5 — learnings promotion (learnings::run_promote) — deterministic, no LLM
//!   L3   — operating-model audit (provider::generate)   — FULL mode only
//!
//! Modes:
//!   Quick — L1 + L2 + L2.5 (deterministic; safe to run nightly).
//!   Full  — everything + L3 (writes an audit file for human review; never auto-edits sources).
//!
//! Per S6 (no quiet failures): every layer's failure is surfaced loudly to
//! stderr and reflected in the exit code. L3 failure does NOT abort L1+L2.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Quick,
    Full,
}

/// Run the unified consolidation pass. Returns a process exit code.
///   0 — all requested layers succeeded with no issues
///   1 — at least one layer reported issues or hard-failed
pub fn run(mode: Mode, max: bool, hex_dir: &Path) -> i32 {
    // Self-throttle the whole process (every thread + IO) unless --max.
    crate::throttle::apply(max);

    let mut any_fail = false;

    println!("=== hex memory consolidate ({:?}) ===", mode);

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

    // Layer 2.5 — LEARNINGS PROMOTION (deterministic, no LLM)
    // Scans me/learnings.md + raw/reflections for recurring pattern clusters and
    // writes promotion candidates to evolution/suggestions.md. Folded in from the
    // former standalone `hex memory learnings promote` command — one less surface.
    // Idempotent (processed clusters are deduped), so it's safe in `quick`/nightly.
    println!("\n-- Layer 2.5: learnings promotion (me/learnings.md → evolution/suggestions.md) --");
    crate::learnings::run_promote(hex_dir, false);

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

/// Layer 3 — operating-model audit (FULL mode only).
///
/// Reads CLAUDE.md + me/learnings.md, sends them to the LLM via
/// `memory::provider::generate`, then writes the audit body to
/// `evolution/consolidation-audit-YYYY-MM-DD.md` and appends an entry to
/// `evolution/consolidation-log-YYYY-MM-DD.md`. Never edits the source
/// operating-model files (CLAUDE.md / me/learnings.md). Surfaces provider
/// failures loudly (Rule S6) but does not abort Layers 1+2 — that's enforced
/// by the caller in `run()`.
fn run_layer3(hex_dir: &Path) -> Result<()> {
    let claude_path = hex_dir.join("CLAUDE.md");
    let learnings_path = hex_dir.join("me").join("learnings.md");

    let claude = fs::read_to_string(&claude_path)
        .with_context(|| format!("read {}", claude_path.display()))?;
    let learnings = fs::read_to_string(&learnings_path)
        .with_context(|| format!("read {}", learnings_path.display()))?;

    let prompt = build_audit_prompt(&claude, &learnings);
    let model = std::env::var("HEX_CONSOLIDATE_MODEL")
        .unwrap_or_else(|_| "anthropic/claude-sonnet-4.5".to_string());

    let body = crate::memory::provider::generate(&prompt, &model, 4096)
        .map_err(|e| anyhow::anyhow!("provider::generate failed: {e}"))?;

    let path = write_audit_artifact(hex_dir, &body)?;
    println!("Layer 3 wrote audit: {}", path.display());
    Ok(())
}

fn build_audit_prompt(claude_md: &str, learnings_md: &str) -> String {
    format!(
        "You are auditing the operating model of a long-running AI agent.\n\
         Read the two documents below and report:\n\
           - Contradictions (rules that conflict)\n\
           - Duplicate rules (semantic overlap)\n\
           - Aspirational/unenforceable rules\n\
           - Stale references (files/commands/skills that no longer exist)\n\
         Categorize each finding as one of: REMOVE | MERGE | UPDATE | REVIEW.\n\
         Output Markdown. Do NOT propose rewrites of the source files — list findings only.\n\
         \n\
         === CLAUDE.md ===\n{claude_md}\n\
         \n\
         === me/learnings.md ===\n{learnings_md}\n"
    )
}

/// Pure I/O helper: write the audit body to a dated file and append a log
/// entry. Never touches CLAUDE.md or me/learnings.md. Returns the path of
/// the audit file written.
pub(crate) fn write_audit_artifact(hex_dir: &Path, body: &str) -> Result<PathBuf> {
    use std::io::Write;

    let evo = hex_dir.join("evolution");
    fs::create_dir_all(&evo).with_context(|| format!("mkdir {}", evo.display()))?;

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let audit_path = evo.join(format!("consolidation-audit-{date}.md"));
    let log_path = evo.join(format!("consolidation-log-{date}.md"));

    let header = format!(
        "# Consolidation Audit — {date}\n\
         \n\
         Generated by `hex memory consolidate full`. Findings only — no source-file edits.\n\
         \n",
    );
    let mut audit = String::with_capacity(header.len() + body.len() + 1);
    audit.push_str(&header);
    audit.push_str(body);
    if !audit.ends_with('\n') {
        audit.push('\n');
    }
    fs::write(&audit_path, &audit)
        .with_context(|| format!("write {}", audit_path.display()))?;

    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    writeln!(
        log,
        "- {date}: wrote {}",
        audit_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("consolidation-audit.md")
    )
    .with_context(|| format!("append {}", log_path.display()))?;

    Ok(audit_path)
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
        let code = run(Mode::Quick, true, dir.path());
        assert!(code == 0 || code == 1, "unexpected exit code {code}");
        assert!(
            dir.path().join("evolution").join("consolidation-latest.log").exists(),
            "Layer 1 must write consolidation-latest.log"
        );
    }

    #[test]
    fn write_audit_artifact_creates_dated_file_and_appends_log_without_touching_sources() {
        // RED test for task Txjh0xxy9 (Layer-3 audit writer).
        // Behavior under test: a pure I/O helper that, given a fake LLM
        // audit body, writes evolution/consolidation-audit-YYYY-MM-DD.md
        // and appends to evolution/consolidation-log-YYYY-MM-DD.md, and
        // NEVER modifies CLAUDE.md or me/learnings.md.
        let dir = fake_hex_dir();
        let claude_before = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        let learn_before =
            fs::read_to_string(dir.path().join("me").join("learnings.md")).unwrap();

        let body = "## Audit\n- REMOVE: stale rule\n- MERGE: duplicate rule\n";
        let audit_path = super::write_audit_artifact(dir.path(), body)
            .expect("write_audit_artifact should succeed");

        // Audit file name must match consolidation-audit-YYYY-MM-DD.md
        let fname = audit_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            fname.starts_with("consolidation-audit-") && fname.ends_with(".md"),
            "expected dated audit file, got {fname}"
        );
        let date_part = fname
            .trim_start_matches("consolidation-audit-")
            .trim_end_matches(".md");
        assert_eq!(date_part.len(), 10, "expected YYYY-MM-DD, got {date_part}");
        let parts: Vec<&str> = date_part.split('-').collect();
        assert_eq!(parts.len(), 3, "expected YYYY-MM-DD, got {date_part}");
        assert!(parts[0].chars().all(|c| c.is_ascii_digit()));
        assert!(parts[1].chars().all(|c| c.is_ascii_digit()));
        assert!(parts[2].chars().all(|c| c.is_ascii_digit()));

        // Audit file contains the body we passed in.
        let written = fs::read_to_string(&audit_path).unwrap();
        assert!(
            written.contains("REMOVE: stale rule"),
            "audit file must include LLM body"
        );

        // Log file exists and was appended to for this date.
        let log_path = dir
            .path()
            .join("evolution")
            .join(format!("consolidation-log-{date_part}.md"));
        assert!(log_path.exists(), "consolidation-log-{date_part}.md must exist");
        let log = fs::read_to_string(&log_path).unwrap();
        assert!(!log.trim().is_empty(), "log entry must be appended");

        // Source operating-model files must NOT be modified.
        let claude_after = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        let learn_after =
            fs::read_to_string(dir.path().join("me").join("learnings.md")).unwrap();
        assert_eq!(claude_before, claude_after, "CLAUDE.md must not be edited");
        assert_eq!(learn_before, learn_after, "learnings.md must not be edited");
    }

    /// Regression: there must be exactly ONE consolidate orchestrator, and it
    /// lives at `hex memory consolidate` (`MemoryCommands::Consolidate`). The old
    /// `hex doctor consolidate` fragment and any top-level `hex consolidate`
    /// (`Commands::Consolidate`) must stay folded in. If anyone reintroduces a
    /// `DoctorCommands::Consolidate` or top-level `Commands::Consolidate` variant
    /// in main.rs, this guard fires.
    #[test]
    fn consolidate_lives_only_under_memory() {
        let main_rs = include_str!("main.rs");
        // The single canonical orchestrator must be present under memory.
        assert!(
            main_rs.contains("MemoryCommands::Consolidate"),
            "MemoryCommands::Consolidate must exist — `hex memory consolidate` is \
             the one canonical consolidate orchestrator"
        );
        // The doctor-side fragment must not return.
        assert!(
            !main_rs.contains("DoctorCommands::Consolidate"),
            "DoctorCommands::Consolidate must not be reintroduced — \
             use `hex memory consolidate` instead"
        );
        // No top-level `hex consolidate` dispatch arm — it now nests under memory.
        // (Match the qualified dispatch path, which is unambiguous, rather than the
        // bare enum-variant indentation that `MemoryCommands::Consolidate` shares.)
        assert!(
            !main_rs.contains("\n        Commands::Consolidate {"),
            "top-level Commands::Consolidate dispatch must not be reintroduced — \
             use `hex memory consolidate` instead"
        );
    }

    /// Regression: the unified `hex consolidate` surface must expose exactly
    /// `full` and `quick` modes. If either variant is renamed or removed, this
    /// guard fires before the help/CLI breaks for users.
    #[test]
    fn consolidate_exposes_full_and_quick_modes() {
        // The Mode enum is the single source of truth for the two modes.
        // Exhaustive match — adding a third mode without updating the contract
        // will fail to compile here, which is the desired tripwire.
        for m in [Mode::Quick, Mode::Full] {
            match m {
                Mode::Quick => {}
                Mode::Full => {}
            }
        }

        // And the clap dispatch in main.rs must still wire both variants.
        let main_rs = include_str!("main.rs");
        assert!(
            main_rs.contains("ConsolidateCommands::Quick"),
            "main.rs must dispatch ConsolidateCommands::Quick"
        );
        assert!(
            main_rs.contains("ConsolidateCommands::Full"),
            "main.rs must dispatch ConsolidateCommands::Full"
        );
    }

    #[test]
    fn quick_mode_does_not_write_audit_file() {
        let dir = fake_hex_dir();
        let _ = run(Mode::Quick, true, dir.path());
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
