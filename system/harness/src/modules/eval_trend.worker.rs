//! `hex-eval-trend` — daily recall-eval trend recorder (hill-climber stage 0).
//!
//! Every night at 04:30 UTC this runs the recall eval
//! (`hex::memory::eval::summarize`) in-process against the instance's
//! `$HEX_DIR/.hex/eval/recall-cases.toml` and appends one row to the
//! `eval_runs` table in `memory.db`. The row is a straight snapshot of that
//! run's numbers — `ts, cases_total, facts_hits, anywhere_hits, regressions,
//! baseline_present, harness_version` — so the digest worker and any human can
//! read the trend without re-running anything.
//!
//! Failure stance (SO S6):
//!  - Cases file ABSENT → SKIP LOUDLY: an `eval_trend.skipped` telemetry event
//!    plus a stderr line, then `Ok(())`. Foundation ships to every instance and
//!    most never opt into the eval, so an absent cases file is expected, not an
//!    error — but it is never silent.
//!  - Any REAL failure (unreadable/broken cases, DB open, migration, insert) →
//!    the handler returns `Err`, which the runtime records as a `status=error`
//!    telemetry row that `hex failures` counts, plus a stderr line. Loud.
//!
//! The `eval_runs` migration lives in `hex::memory::schema` (same const+fn
//! shape as `apply_messages_schema`) and is applied explicitly here so a
//! failure to create the table is loud, not best-effort.

use hex::memory::eval::{self, cases_path, EvalRunError, EvalSummary};
use hex::memory::provider::hex_root;
use hex::memory::{db_path, open_db, schema};
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression — 04:30 UTC daily (7-field: sec min hour dom mon dow year).
pub const CRON_DAILY_0430: &str = "0 30 4 * * * *";

/// Insert one trend row. Split out so the append is unit-testable against an
/// in-memory DB without a live memory store or embeddings.
fn insert_eval_run(
    conn: &rusqlite::Connection,
    s: &EvalSummary,
    harness_version: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO eval_runs
             (ts, cases_total, facts_hits, anywhere_hits, regressions, baseline_present, harness_version)
         VALUES (datetime('now'), ?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            s.cases_total as i64,
            s.facts_hits as i64,
            s.anywhere_hits as i64,
            s.regressions as i64,
            i64::from(s.baseline_present),
            harness_version,
        ],
    )?;
    Ok(())
}

/// Emit the skip event + stderr line for an absent cases file. Loud, per S6.
fn skip_loudly(ctx: &Ctx, cases_display: &str) -> Result<()> {
    eprintln!("[eval-trend] SKIP: recall-cases file absent at {cases_display}");
    ctx.emit(
        "eval_trend.skipped",
        serde_json::json!({
            "reason": "cases_absent",
            "cases_path": cases_display,
        }),
    )?;
    Ok(())
}

fn run_eval_trend(_e: Event, ctx: Ctx) -> Result<()> {
    let root = hex_root();
    let cases = cases_path(&root);

    // SKIP LOUDLY when the instance has not shipped a recall-cases file —
    // foundation never assumes instance data exists.
    if !cases.exists() {
        return skip_loudly(&ctx, &cases.display().to_string());
    }

    let summary = match eval::summarize(&root, None) {
        Ok(s) => s,
        // Raced away between the exists() check and the read — still a skip.
        Err(EvalRunError::CasesAbsent(p)) => {
            return skip_loudly(&ctx, &p.display().to_string());
        }
        Err(EvalRunError::Other(msg)) => {
            eprintln!("[eval-trend] ERROR: eval run failed: {msg}");
            return Err(anyhow::anyhow!("eval-trend: eval run failed: {msg}"));
        }
    };

    let dbp = db_path(&root);
    let conn = open_db(&dbp)
        .map_err(|e| anyhow::anyhow!("eval-trend: open memory.db {} failed: {e}", dbp.display()))?;
    // Atomic, idempotent migration; loud (S6) if the table can't be created.
    schema::apply_eval_runs_schema(&conn)
        .map_err(|e| anyhow::anyhow!("eval-trend: eval_runs migration failed: {e}"))?;

    let harness_version = env!("HEX_GIT_SHA");
    insert_eval_run(&conn, &summary, harness_version)
        .map_err(|e| anyhow::anyhow!("eval-trend: insert eval_runs row failed: {e}"))?;

    eprintln!(
        "[eval-trend] recorded: cases={} facts={} anywhere={} regressions={} baseline_present={}",
        summary.cases_total,
        summary.facts_hits,
        summary.anywhere_hits,
        summary.regressions,
        summary.baseline_present,
    );
    ctx.emit(
        "eval_trend.recorded",
        serde_json::json!({
            "cases_total": summary.cases_total,
            "facts_hits": summary.facts_hits,
            "anywhere_hits": summary.anywhere_hits,
            "regressions": summary.regressions,
            "baseline_present": summary.baseline_present,
            "harness_version": harness_version,
        }),
    )?;
    Ok(())
}

/// Build the `hex-eval-trend` worker.
pub fn worker() -> Worker {
    Worker::new("hex-eval-trend").on_cron_named("daily", CRON_DAILY_0430, run_eval_trend)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `eval_runs` migration creates the table atomically, is idempotent,
    /// and an appended row round-trips every column intact.
    #[test]
    fn migration_creates_eval_runs_and_row_roundtrips() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Applying twice must not error (idempotent) or drop data.
        schema::apply_eval_runs_schema(&conn).unwrap();
        schema::apply_eval_runs_schema(&conn).unwrap();

        // Table exists with the expected shape.
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(eval_runs)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        for expected in &[
            "ts",
            "cases_total",
            "facts_hits",
            "anywhere_hits",
            "regressions",
            "baseline_present",
            "harness_version",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "eval_runs missing column: {expected}"
            );
        }

        let s = EvalSummary {
            cases_total: 12,
            facts_hits: 9,
            anywhere_hits: 11,
            regressions: 2,
            baseline_present: true,
        };
        insert_eval_run(&conn, &s, "abc1234").unwrap();

        let (ct, fh, ah, reg, bp, hv, ts): (i64, i64, i64, i64, i64, String, String) = conn
            .query_row(
                "SELECT cases_total, facts_hits, anywhere_hits, regressions, \
                 baseline_present, harness_version, ts FROM eval_runs",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!((ct, fh, ah, reg, bp), (12, 9, 11, 2, 1));
        assert_eq!(hv, "abc1234");
        assert!(!ts.is_empty(), "ts must be populated by datetime('now')");
    }

    /// An absent cases file yields the `CasesAbsent` skip signal (the worker's
    /// SKIP-loud path) WITHOUT touching recall or the DB — proving foundation
    /// never assumes instance data exists.
    #[test]
    fn summarize_signals_cases_absent_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        match eval::summarize(tmp.path(), None) {
            Err(EvalRunError::CasesAbsent(_)) => {}
            other => panic!("expected CasesAbsent, got {other:?}"),
        }
    }
}
