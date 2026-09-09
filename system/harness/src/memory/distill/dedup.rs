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
    // Phase 1.5: exact (subject, predicate, object) match → Noop. Superseded
    // rows are history, not current truth — an old object must not shadow a
    // clean re-assertion of today's value as a Noop (ACCEPTANCE e).
    let exact: Option<String> = conn
        .query_row(
            "SELECT id FROM facts WHERE subject=?1 AND predicate=?2 AND object=?3 \
             AND invalid_at IS NULL LIMIT 1",
            rusqlite::params![candidate.subject, candidate.predicate, candidate.object],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = exact {
        return Ok(DedupOutcome::Noop { existing_id: id });
    }

    // Phase 1.5b: same (subject, predicate), different object → Ambiguous.
    // Same live-only filter: a superseded conflict must not block a clean add.
    let conflict: Option<String> = conn
        .query_row(
            "SELECT id FROM facts WHERE subject=?1 AND predicate=?2 \
             AND invalid_at IS NULL LIMIT 1",
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
        crate::memory::schema::apply_plan3(&conn).unwrap();
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

    /// RED for closed-loop-plan-2026-09-06 §4 / ACCEPTANCE (e): dedup must
    /// classify over LIVE rows only. Re-asserting the current (live) value is
    /// a Noop; re-asserting an old, superseded value must be Ambiguous — NOT
    /// a Noop, since the row that would make it a Noop is stale history, and
    /// NOT silently swallowed either. Fails now because `classify` has no
    /// `invalid_at` column to filter on (schema_version 4) and, once that
    /// column exists, would otherwise match the superseded row via the exact
    /// (subject,predicate,object) branch and misreport it as a Noop.
    #[test]
    fn reasserting_current_value_after_supersede_is_noop_reasserting_stale_is_ambiguous() {
        let conn = fixture_conn();
        conn.execute(
            "INSERT INTO facts \
             (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by) \
             VALUES ('old1','boi','has','installed and live version 3.3.2',0.7,\
             '2026-01-01','2026-01-01','2026-01-01','2026-02-01','new1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts \
             (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by) \
             VALUES ('new1','boi','has','installed and live version 3.9.1',0.7,\
             '2026-02-01','2026-02-01','2026-02-01',NULL,NULL)",
            [],
        )
        .unwrap();

        let reassert_current = Candidate {
            subject: "boi".into(),
            predicate: "has".into(),
            object: "installed and live version 3.9.1".into(),
            importance: 0.7,
        };
        let outcome = classify(&conn, &reassert_current, None).unwrap();
        assert!(
            matches!(outcome, DedupOutcome::Noop { .. }),
            "re-asserting the live current value must be a Noop, got {outcome:?}"
        );

        let reassert_stale = Candidate {
            subject: "boi".into(),
            predicate: "has".into(),
            object: "installed and live version 3.3.2".into(),
            importance: 0.7,
        };
        let outcome2 = classify(&conn, &reassert_stale, None).unwrap();
        assert!(
            matches!(outcome2, DedupOutcome::Ambiguous { .. }),
            "re-asserting a superseded object must be Ambiguous, not Noop; got {outcome2:?}"
        );
    }
}
