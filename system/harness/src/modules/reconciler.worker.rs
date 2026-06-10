//! `hex-reconciler` — hourly cron worker that joins linter intent rows to
//! per-gate outcomes from `~/.boi/v2/boi.db`.
//!
//! Spec S253fety6, task Tdf23yg2y. Heavy lifting is in `hex::reconciler`
//! (unit-testable, no IO besides sqlite reads); this file is the cron stub
//! the worker registry picks up.
//!
//! Join rule (critique-v1 F1, binding): per-gate normalized-command content
//! hash from `outcome.evidence.verifications[]` — never spec-level status.
//! Label = first-attempt exit_code per (spec_id, gate hash).

use hex::ledger::{default_path as ledger_default_path, Ledger};
use hex::reconciler::{reconcile_from_boi_db, write_reconcile_to_ledger};
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};
use std::path::PathBuf;

/// Hourly cadence.
pub const CRON_HOURLY: &str = "0 0 * * * * *";

fn hex_dir() -> PathBuf {
    if let Ok(d) = std::env::var("HEX_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("hex");
    }
    PathBuf::from(".")
}

fn boi_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("HEX_BOI_DB") {
        return PathBuf::from(p);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".boi/v2/boi.db");
    }
    PathBuf::from(".boi/v2/boi.db")
}

fn run_reconcile(_e: Event, _ctx: Ctx) -> Result<()> {
    let boi = boi_db_path();
    let ledger_path = ledger_default_path(&hex_dir());

    // Source missing → loud heartbeat row noting it, never silent.
    let ledger = Ledger::open(&ledger_path)
        .map_err(|e| anyhow::anyhow!("reconciler: open ledger failed: {e}"))?;

    if !boi.exists() {
        eprintln!("reconciler: boi.db absent at {}; skipping", boi.display());
        let _ = ledger.append(
            "reconciler",
            "reconcile.run",
            "heartbeat",
            &serde_json::json!({"records": 0, "skipped_malformed": 0, "spec_runs_scanned": 0, "skipped_reason": "boi_db_absent"}),
        );
        return Ok(());
    }

    let (records, summary) =
        reconcile_from_boi_db(&boi).map_err(|e| anyhow::anyhow!("{e}"))?;
    write_reconcile_to_ledger(&ledger, &records, &summary)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    eprintln!(
        "reconciler: scanned {} phase_runs, {} records, {} malformed",
        summary.spec_runs_scanned, summary.records, summary.skipped_malformed
    );
    Ok(())
}

/// Build the `hex-reconciler` worker.
pub fn worker() -> Worker {
    Worker::new("hex-reconciler").on_cron(CRON_HOURLY, run_reconcile)
}
