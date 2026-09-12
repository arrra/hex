pub mod assemble;
pub mod claude_cli;
pub mod consolidate;
pub mod distill;
pub mod embed;
pub mod embed_client;
pub mod eval;
pub mod index;
pub mod maintain;
pub mod maintain_facts;
pub mod parse_transcripts;
pub mod predicates;
pub mod provider;
pub mod recall;
pub mod recall_config;
pub mod recent;
pub mod rrf;
pub mod schema;
pub mod search;
pub mod stats;
pub mod vector;

use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub fn db_path(hex_root: &Path) -> PathBuf {
    hex_root.join(".hex/memory.db")
}

/// Open the memory DB with sqlite-vec registered. ALL memory code must open
/// connections through this — `Connection::open` directly would miss vec0.
/// Also ensures the Plan 2 schema (facts, fact_history, sessions, topics,
/// transcript_files, facts_vec, facts_fts) is applied — DDL is idempotent.
pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    vector::register_sqlite_vec();
    let conn = Connection::open(path)?;
    // Be friendly under concurrent writers (quick + long cron tick, etc.):
    // wait up to 5s for a competing writer to release before erroring.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // Best-effort migration — log but don't fail if a DDL piece errors
    // (e.g. older sqlite-vec without FLOAT[768]); the facts CLI commands will
    // surface a clearer error.
    if let Err(e) = schema::apply_plan2(&conn) {
        eprintln!("[memory] Plan 2 schema migration warning: {e}");
    }
    // Required — every bi-temporal reader (recall, KNN) unconditionally
    // depends on valid_from/invalid_at/superseded_by existing on `facts`.
    // A connection missing them is not a degraded-but-usable connection, so
    // this one propagates instead of warn-and-continue.
    schema::apply_plan3(&conn).map_err(|e| {
        eprintln!("[memory] Plan 3 schema migration failed: {e}");
        e
    })?;
    if let Err(e) = schema::apply_messages_schema(&conn) {
        eprintln!("[memory] messages schema migration warning: {e}");
    }
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RED for PR#9 r1 F1 (blocker): `open_db` currently logs
    /// `schema::apply_plan3` errors and continues (warn-and-continue),
    /// returning `Ok` with a connection that is missing `valid_from` /
    /// `invalid_at` / `superseded_by`. Every bi-temporal reader added in
    /// this port unconditionally requires those columns, so a failed
    /// required migration must fail `open_db` loudly instead of handing
    /// back an incompatible connection.
    ///
    /// F9 (minor, arrra/hex PR #9 round 2): forces the failure with a
    /// deterministic, privilege- and platform-independent schema conflict
    /// instead of a read-only file permission — mode 0444 does not stop a
    /// write by root or a process with DAC-override, and is a no-op on
    /// non-Unix platforms, so the original version of this test could pass
    /// even against a broken production fix in those environments. Pre-adds
    /// `valid_from` with a CHECK that only allows NULL: `apply_plan3`'s own
    /// re-`ALTER TABLE` harmlessly hits its already-handled "duplicate
    /// column" path, but its unconditional backfill
    /// `UPDATE ... SET valid_from = created_at` then writes a real value and
    /// trips a genuine SQLITE_CONSTRAINT failure — a real Plan 3 migration
    /// error on any platform, at any privilege level.
    #[test]
    fn open_db_fails_when_required_plan3_migration_fails() {
        vector::register_sqlite_vec();
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("memory.db");

        {
            let conn = Connection::open(&db_path).unwrap();
            schema::apply_plan1_baseline_for_test(&conn).unwrap();
            schema::apply_plan2(&conn).unwrap();
            conn.execute(
                "ALTER TABLE facts ADD COLUMN valid_from TEXT CHECK (valid_from IS NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,created_at,updated_at) \
                 VALUES ('f1','s','p','o','2026-01-01','2026-01-01')",
                [],
            )
            .unwrap();
        }

        let result = open_db(&db_path);

        assert!(
            result.is_err(),
            "open_db must return Err when the required Plan 3 migration fails, not silently return Ok with an incompatible connection"
        );
    }
}
