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

    Ok(r)
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
