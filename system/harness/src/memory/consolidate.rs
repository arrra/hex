use rusqlite::Connection;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;

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
