pub mod dedup;
pub mod extract;
pub mod judge;
pub mod watermark;

use crate::memory::predicates;
use extract::Candidate;
use rusqlite::Connection;
use ulid::Ulid;

#[derive(Default, Debug)]
pub struct DistillReport {
    pub adds: u32,
    pub updates: u32,
    pub noops: u32,
    pub flags: u32,
}

pub fn run_on_file(
    conn: &mut Connection,
    path: &str,
    min_tokens: usize,
) -> anyhow::Result<DistillReport> {
    let mut report = DistillReport::default();
    let offset = watermark::last_offset(conn, path)?;
    let full = std::fs::read_to_string(path)?;
    if (full.len() as i64) <= offset {
        return Ok(report);
    }
    let span = &full[offset as usize..];
    if span.split_whitespace().count() < min_tokens {
        return Ok(report);
    }

    let candidates = extract::extract_from_span(span)?;
    let new_offset = full.len() as i64;

    let tx = conn.transaction()?;
    for c in candidates {
        let (pred, obj) = if predicates::validate(&c.predicate).is_ok() {
            (c.predicate.clone(), c.object.clone())
        } else {
            (
                "_unmapped".to_string(),
                format!("{}: {}", c.predicate, c.object),
            )
        };

        let effective = Candidate {
            predicate: pred.clone(),
            object: obj.clone(),
            ..c.clone()
        };
        let outcome = dedup::classify(&tx, &effective, None)?;
        match outcome {
            dedup::DedupOutcome::Noop { existing_id } => {
                report.noops += 1;
                tx.execute(
                    "UPDATE facts SET access_count=access_count+1, last_accessed=datetime('now') WHERE id=?1",
                    [existing_id],
                )?;
            }
            dedup::DedupOutcome::CleanAdd => {
                let id = Ulid::new().to_string();
                tx.execute(
                    "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,source_ref)
                     VALUES (?1,?2,?3,?4,?5,datetime('now'),datetime('now'),?6)",
                    rusqlite::params![id, c.subject, pred, obj, c.importance, path],
                )?;
                tx.execute(
                    "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'ADD',?2,datetime('now'))",
                    rusqlite::params![id, obj],
                )?;
                report.adds += 1;
            }
            dedup::DedupOutcome::Ambiguous { nearest_ids } => {
                let existing: Vec<(String, String, String, String)> = nearest_ids
                    .iter()
                    .filter_map(|nid| {
                        tx.query_row(
                            "SELECT id,subject,predicate,object FROM facts WHERE id=?1",
                            [nid],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .ok()
                    })
                    .collect();
                let decision = judge::judge(&c.subject, &pred, &obj, "", &existing)?;
                match decision.action {
                    judge::Action::Add => {
                        let id = Ulid::new().to_string();
                        tx.execute(
                            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,source_ref)
                             VALUES (?1,?2,?3,?4,?5,datetime('now'),datetime('now'),?6)",
                            rusqlite::params![id, c.subject, pred, obj, c.importance, path],
                        )?;
                        tx.execute(
                            "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'ADD',?2,datetime('now'))",
                            rusqlite::params![id, obj],
                        )?;
                        report.adds += 1;
                    }
                    judge::Action::Update => {
                        if let Some(tid) = decision.target_id {
                            let prev: String = tx.query_row(
                                "SELECT object FROM facts WHERE id=?1",
                                [&tid],
                                |r| r.get(0),
                            )?;
                            tx.execute(
                                "UPDATE facts SET object=?1, updated_at=datetime('now') WHERE id=?2",
                                rusqlite::params![obj, tid],
                            )?;
                            tx.execute(
                                "INSERT INTO fact_history (fact_id,op,prev_value,new_value,ts) VALUES (?1,'UPDATE',?2,?3,datetime('now'))",
                                rusqlite::params![tid, prev, obj],
                            )?;
                            report.updates += 1;
                        }
                    }
                    judge::Action::Noop => {
                        report.noops += 1;
                    }
                    judge::Action::Flag => {
                        let tid = decision.target_id.clone().unwrap_or_default();
                        tx.execute(
                            "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'FLAG',?2,datetime('now'))",
                            rusqlite::params![
                                tid,
                                format!(
                                    "contested by: ({},{},{}) — {}",
                                    c.subject, pred, obj, decision.reason
                                )
                            ],
                        )?;
                        report.flags += 1;
                    }
                }
            }
        }
    }
    watermark::advance_offset(&tx, path, new_offset)?;
    tx.commit()?;
    Ok(report)
}
