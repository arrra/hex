use crate::memory::distill::extract::Candidate;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug)]
pub enum DedupOutcome {
    Noop { existing_id: String },
    Ambiguous { nearest_ids: Vec<String> },
    CleanAdd,
}

pub fn classify(
    conn: &Connection,
    candidate: &Candidate,
    _embedding: Option<&[f32]>,
) -> anyhow::Result<DedupOutcome> {
    // Phase 1.5: exact (subject, predicate, object) match → Noop
    let exact: Option<String> = conn
        .query_row(
            "SELECT id FROM facts WHERE subject=?1 AND predicate=?2 AND object=?3 LIMIT 1",
            rusqlite::params![candidate.subject, candidate.predicate, candidate.object],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = exact {
        return Ok(DedupOutcome::Noop { existing_id: id });
    }

    // Phase 1.5b: same (subject, predicate), different object → Ambiguous
    let conflict: Option<String> = conn
        .query_row(
            "SELECT id FROM facts WHERE subject=?1 AND predicate=?2 LIMIT 1",
            rusqlite::params![candidate.subject, candidate.predicate],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = conflict {
        return Ok(DedupOutcome::Ambiguous {
            nearest_ids: vec![id],
        });
    }

    // Phase 1.6: embedding similarity stub (full wire-up when embedding pipeline lands)
    let vec_ids = nearest_via_vec(conn, candidate)?;
    if !vec_ids.is_empty() {
        return Ok(DedupOutcome::Ambiguous {
            nearest_ids: vec_ids,
        });
    }

    Ok(DedupOutcome::CleanAdd)
}

fn nearest_via_vec(_conn: &Connection, _candidate: &Candidate) -> anyhow::Result<Vec<String>> {
    // Stub: returns empty until embedding pipeline is wired up
    Ok(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::distill::extract::Candidate;
    use rusqlite::Connection;

    fn fixture_conn() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        conn
    }

    #[test]
    fn exact_subject_predicate_object_match_is_noop() {
        let conn = fixture_conn();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('f1','user','prefers','concrete framing',0.8,'2026-05-01','2026-05-01')",
            [],
        )
        .unwrap();
        let cand = Candidate {
            subject: "user".into(),
            predicate: "prefers".into(),
            object: "concrete framing".into(),
            importance: 0.8,
        };
        let outcome = classify(&conn, &cand, None).unwrap();
        assert!(matches!(outcome, DedupOutcome::Noop { .. }));
    }

    #[test]
    fn same_subject_predicate_different_object_is_ambiguous() {
        let conn = fixture_conn();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('f1','user','status','actively job searching',0.9,'2026-05-01','2026-05-01')",
            [],
        )
        .unwrap();
        let cand = Candidate {
            subject: "user".into(),
            predicate: "status".into(),
            object: "not laid off".into(),
            importance: 0.9,
        };
        let outcome = classify(&conn, &cand, None).unwrap();
        assert!(matches!(outcome, DedupOutcome::Ambiguous { .. }));
    }

    #[test]
    fn no_matching_subject_predicate_is_clean_add() {
        let conn = fixture_conn();
        let cand = Candidate {
            subject: "person:whitney".into(),
            predicate: "is".into(),
            object: "Mike's wife".into(),
            importance: 0.9,
        };
        let outcome = classify(&conn, &cand, None).unwrap();
        assert!(matches!(outcome, DedupOutcome::CleanAdd));
    }
}
