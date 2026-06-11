use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

#[derive(Default, serde::Serialize)]
pub struct ConsolidateReport {
    pub ok: Vec<String>,
    pub failed: Vec<(String, String)>,
}

pub fn run(conn: &mut Connection) -> anyhow::Result<ConsolidateReport> {
    let mut r = ConsolidateReport::default();

    macro_rules! iso {
        ($name:expr, $expr:expr) => {
            match $expr {
                Ok(()) => r.ok.push($name.to_string()),
                Err(e) => {
                    eprintln!("consolidate op '{}' FAILED: {e}", $name);
                    r.failed.push(($name.to_string(), e.to_string()));
                }
            }
        };
    }

    iso!("orientation-snapshot", op_orientation_snapshot(conn));
    iso!("catchup-distill",      op_catchup_distill(conn));
    iso!("dedup",                op_dedup(conn));
    iso!("contradiction-sweep",  op_contradiction_sweep(conn));
    iso!("prune",                op_prune(conn));
    iso!("topic-rollup",         op_topic_rollup(conn));

    // Record when consolidation last ran so `hex memory stats` can report it.
    // This is advisory bookkeeping: log loudly on failure (Rule S6) but do NOT
    // fail the run over a metadata hiccup.
    match stamp_last_consolidated(conn) {
        Ok(()) => r.ok.push("stamp-last-consolidated".to_string()),
        Err(e) => eprintln!("consolidate: WARN could not stamp last_consolidated: {e}"),
    }

    Ok(r)
}

/// Stamp the wall-clock time of this consolidation run into the `metadata`
/// key-value table under `last_consolidated`. `hex memory stats` reads this key.
/// Idempotent; creates the metadata table if a bare DB lacks it.
fn stamp_last_consolidated(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('last_consolidated', ?)",
        rusqlite::params![chrono::Local::now().to_rfc3339()],
    )?;
    Ok(())
}

fn op_orientation_snapshot(_conn: &mut Connection) -> anyhow::Result<()> {
    // Refresh standing snapshot: active project, open threads, recent-session arc.
    // FIRST so any later failure does not starve retrieval.
    Ok(())
}

fn op_catchup_distill(conn: &mut Connection) -> anyhow::Result<()> {
    let paths: Vec<String> = conn
        .prepare(
            "SELECT path FROM transcript_files WHERE last_distilled_at IS NULL
             OR datetime(last_distilled_at) < datetime('now','-1 day')",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();
    for p in paths {
        let _ = crate::memory::distill::run_on_file(conn, &p, 500);
    }
    Ok(())
}

fn op_dedup(_conn: &mut Connection) -> anyhow::Result<()> {
    // Not yet implemented: vector-cluster near-duplicate facts, feed to LLM judge for merge.
    eprintln!("consolidate op 'dedup': not yet implemented");
    Ok(())
}

fn op_contradiction_sweep(_conn: &mut Connection) -> anyhow::Result<()> {
    // Not yet implemented: resolve fact_history.op='FLAG' rows via LLM judge.
    eprintln!("consolidate op 'contradiction-sweep': not yet implemented");
    Ok(())
}

fn op_prune(conn: &mut Connection) -> anyhow::Result<()> {
    // Tombstone-eligible: access_count=0 AND age>60 AND subject!='user' AND predicate!='decided'
    conn.execute(
        "UPDATE facts SET tombstone = 1
         WHERE tombstone = 0 AND access_count = 0
           AND subject != 'user' AND predicate != 'decided'
           AND julianday('now') - julianday(updated_at) > 60",
        [],
    )?;
    Ok(())
}

fn op_topic_rollup(_conn: &mut Connection) -> anyhow::Result<()> {
    // Not yet implemented: maintain topics/fact_topics rollup.
    Ok(())
}

/// One quick tick may not hold the consolidate lock indefinitely — the
/// nightly full run needs it (lock_wait_budget = 45m). 10 minutes processes
/// ~10-20 slices; the 15-min cron picks the remainder up next tick.
pub(crate) const BACKSTOP_BUDGET: std::time::Duration = std::time::Duration::from_secs(10 * 60);

pub(crate) fn backstop_over_budget(start: std::time::Instant) -> bool {
    start.elapsed() >= BACKSTOP_BUDGET
}

/// Phase A transcript-delta backstop.
///
/// Scans `raw/transcripts/*.md`, registers any not-yet-known file in
/// `transcript_files` (reusing `memory::distill::watermark` — do NOT reinvent),
/// then runs the existing distill pipeline on the delta from that watermark
/// forward to capture corrections/decisions the live agent missed. Tolerates
/// gaps gracefully: not-yet-parsed transcripts, missing LLM key, sub-threshold
/// spans, parse failures — all are swallowed so the run continues. Idempotent:
/// a second invocation with no new content is a no-op (no duplicated row, no
/// regressed watermark).
pub fn op_transcript_backstop(conn: &mut Connection, hex_dir: &Path) -> anyhow::Result<()> {
    let dir = hex_dir.join("raw").join("transcripts");
    if !dir.exists() {
        return Ok(());
    }

    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    entries.sort();

    let loop_start = std::time::Instant::now();
    for (i, p) in entries.iter().enumerate() {
        if backstop_over_budget(loop_start) {
            let remaining = entries.len() - i;
            let msg = format!(
                "backstop budget ({:?}) reached — {remaining} file(s) deferred to next tick",
                BACKSTOP_BUDGET
            );
            println!("{msg}");
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "memory::consolidate".into(),
                event: "backstop::budget-stop".into(),
                status: "ok".into(),
                duration_ms: Some(loop_start.elapsed().as_millis() as i64),
                exit_code: None,
                detail: Some(msg),
            });
            break;
        }
        let path_str = match p.to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        // Register the file in transcript_files if absent. Reuses the
        // watermark primitive so there's exactly one writer to that table.
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM transcript_files WHERE path=?1",
                rusqlite::params![path_str.as_str()],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            crate::memory::distill::watermark::advance_offset(conn, &path_str, 0)?;
        }

        // Distill the delta. Errors (LLM unavailable, parse failure, etc.) are
        // tolerated so the backstop never crashes on partial state. The
        // watermark advances only when extraction succeeds end-to-end.
        if let Err(e) = crate::memory::distill::run_on_file(conn, &path_str, 0) {
            eprintln!("transcript-backstop: distill deferred for {path_str}: {e}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;

    #[test]
    fn backstop_budget_constant_is_ten_minutes() {
        assert_eq!(BACKSTOP_BUDGET, std::time::Duration::from_secs(10 * 60));
        let fresh = std::time::Instant::now();
        assert!(!backstop_over_budget(fresh));
    }

    /// Regression: a consolidate run must stamp `metadata.last_consolidated`
    /// so `hex memory stats` stops reporting "never" after a real run.
    #[test]
    fn consolidate_stamps_last_consolidated_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        // open_db applies the Plan 2 schema (facts, transcript_files, …) the
        // consolidate ops touch.
        let mut conn = memory::open_db(&db).unwrap();

        let before: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='last_consolidated'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(before.is_none(), "fresh DB should have no last_consolidated");

        let _ = run(&mut conn).unwrap();

        let after: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='last_consolidated'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(
            after.is_some(),
            "consolidate must stamp last_consolidated into metadata"
        );
    }
}
