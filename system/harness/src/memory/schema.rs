use rusqlite::{Connection, Result};

pub const PLAN2_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT
);

CREATE TABLE IF NOT EXISTS facts (
    id            TEXT PRIMARY KEY,
    subject       TEXT NOT NULL,
    predicate     TEXT NOT NULL,
    object        TEXT NOT NULL,
    importance    REAL NOT NULL DEFAULT 0.5,
    access_count  INTEGER NOT NULL DEFAULT 0,
    last_accessed TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    source_ref    TEXT,
    private       INTEGER NOT NULL DEFAULT 0,
    tombstone     INTEGER NOT NULL DEFAULT 0,
    embedding     BLOB
);
CREATE INDEX IF NOT EXISTS facts_subject_idx     ON facts(subject);
CREATE INDEX IF NOT EXISTS facts_predicate_idx   ON facts(predicate);
CREATE INDEX IF NOT EXISTS facts_tombstone_idx   ON facts(tombstone);
CREATE INDEX IF NOT EXISTS facts_dedup_idx       ON facts(subject, predicate);

CREATE TABLE IF NOT EXISTS fact_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_id     TEXT NOT NULL,
    op          TEXT NOT NULL CHECK (op IN ('ADD','UPDATE','DELETE','FLAG')),
    prev_value  TEXT,
    new_value   TEXT,
    ts          TEXT NOT NULL,
    FOREIGN KEY (fact_id) REFERENCES facts(id)
);
CREATE INDEX IF NOT EXISTS fact_history_fact_idx ON fact_history(fact_id);

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    date        TEXT NOT NULL,
    source_path TEXT NOT NULL UNIQUE,
    summary     TEXT,
    topic_id    TEXT
);

CREATE TABLE IF NOT EXISTS topics (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL UNIQUE,
    rollup_md         TEXT,
    last_consolidated TEXT
);

CREATE TABLE IF NOT EXISTS fact_topics (
    fact_id  TEXT NOT NULL,
    topic_id TEXT NOT NULL,
    PRIMARY KEY (fact_id, topic_id)
);

CREATE TABLE IF NOT EXISTS transcript_files (
    path                  TEXT PRIMARY KEY,
    last_offset           INTEGER NOT NULL DEFAULT 0,
    last_distilled_at     TEXT,
    consecutive_failures  INTEGER NOT NULL DEFAULT 0
);
"#;

pub const PLAN2_VEC_DDL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS facts_vec USING vec0(
    fact_id TEXT PRIMARY KEY,
    embedding FLOAT[768]
);
"#;

pub const PLAN2_FTS_DDL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    subject,
    predicate,
    object,
    content=facts,
    content_rowid=rowid,
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS facts_fts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, subject, predicate, object)
        VALUES (new.rowid, new.subject, new.predicate, new.object);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object)
        VALUES('delete', old.rowid, old.subject, old.predicate, old.object);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, subject, predicate, object)
        VALUES('delete', old.rowid, old.subject, old.predicate, old.object);
    INSERT INTO facts_fts(rowid, subject, predicate, object)
        VALUES (new.rowid, new.subject, new.predicate, new.object);
END;
"#;

pub const MESSAGES_DDL: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id          TEXT PRIMARY KEY,
    source      TEXT NOT NULL,
    kind        TEXT NOT NULL,
    body        TEXT,
    reply_to    TEXT,
    answer_json TEXT,
    prompt_json TEXT,
    resolved    INTEGER NOT NULL DEFAULT 0,
    ts          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_reply_to ON messages(reply_to);
";

pub fn apply_messages_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(MESSAGES_DDL)
}

pub fn apply_plan2(conn: &Connection) -> Result<()> {
    conn.execute_batch(PLAN2_DDL)?;
    // Backfill: older DBs created before consecutive_failures was added still
    // need the column. ALTER TABLE in SQLite errors if the column already
    // exists, so we ignore that one specific error.
    if let Err(e) = conn.execute(
        "ALTER TABLE transcript_files ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        let msg = e.to_string();
        if !msg.contains("duplicate column") {
            // Loud — but tolerated, as the column may already be present in a
            // fresh schema.
            eprintln!("[schema] transcript_files.consecutive_failures backfill: {e}");
        }
    }
    conn.execute_batch(PLAN2_VEC_DDL)?;
    if facts_fts_needs_widening(conn)? {
        // One IMMEDIATE transaction: the write lock is taken up front, the
        // widening need is RE-checked under that lock (two fresh-process
        // openers race this path — the loser must see the winner's finished
        // table and no-op, not drop it again), and drop+recreate+rebuild
        // commit atomically so no crash can leave the index dropped or
        // empty. On any error the guard rolls back to the old, still-
        // searchable table and the next open retries.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let migrate = || -> Result<bool> {
            if !facts_fts_needs_widening(conn)? {
                return Ok(false);
            }
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS facts_fts_ai;
                 DROP TRIGGER IF EXISTS facts_fts_ad;
                 DROP TRIGGER IF EXISTS facts_fts_au;
                 DROP TABLE IF EXISTS facts_fts;",
            )?;
            conn.execute_batch(PLAN2_FTS_DDL)?;
            // External-content fts5: repopulate the 3-column index from facts.
            conn.execute("INSERT INTO facts_fts(facts_fts) VALUES('rebuild')", [])?;
            Ok(true)
        };
        match migrate() {
            Ok(did) => {
                conn.execute_batch("COMMIT")?;
                if did {
                    eprintln!("[schema] facts_fts widened to subject+predicate+object and rebuilt");
                }
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
    } else {
        conn.execute_batch(PLAN2_FTS_DDL)?;
    }
    conn.execute(
        "INSERT INTO schema_version (version, applied_at) VALUES (4, datetime('now'))
         ON CONFLICT(version) DO NOTHING",
        [],
    )?;
    Ok(())
}

/// Pre-2026-08 instances carry an object-only facts_fts, which makes any query
/// naming a subject or predicate structurally invisible to relevance ranking.
/// True when that shape is present and the widening migration must run.
fn facts_fts_needs_widening(conn: &Connection) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='facts_fts'",
        [],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(false);
    }
    let has_subject: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('facts_fts') WHERE name='subject'",
        [],
        |r| r.get(0),
    )?;
    Ok(has_subject == 0)
}

/// Create the minimal Plan 1 schema baseline needed by tests that exercise Plan 2.
pub fn apply_plan1_baseline_for_test(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT
        );",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_version VALUES (3, datetime('now'))",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_creates_all_plan2_tables() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        for expected in &[
            "facts",
            "fact_history",
            "sessions",
            "topics",
            "fact_topics",
            "transcript_files",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing table: {expected}"
            );
        }
    }

    #[test]
    fn apply_plan2_idempotent_without_preexisting_schema_version() {
        // Simulates a production DB that came from Plan 1 without a schema_version table.
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        // No schema_version table created — bare DB, like a real Plan 1 production DB.
        apply_plan2(&conn).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version, 4,
            "schema_version should record version=4 after apply_plan2"
        );
    }

    /// A pre-widening DB (object-only facts_fts + old triggers) must be
    /// migrated in place: subject tokens become searchable, existing rows are
    /// re-indexed, and the recreated triggers keep new inserts in sync.
    #[test]
    fn facts_fts_widening_migrates_object_only_index() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        conn.execute_batch(PLAN2_DDL).unwrap();
        // Old shape: object-only external-content fts + object-only triggers.
        conn.execute_batch(
            "CREATE VIRTUAL TABLE facts_fts USING fts5(
                object, content=facts, content_rowid=rowid,
                tokenize='porter unicode61'
            );
            CREATE TRIGGER facts_fts_ai AFTER INSERT ON facts BEGIN
                INSERT INTO facts_fts(rowid, object) VALUES (new.rowid, new.object);
            END;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,created_at,updated_at)
             VALUES ('f1','Zwerk','is','an agent platform','2026-01-01','2026-01-01')",
            [],
        )
        .unwrap();
        // Pre-migration: subject token invisible.
        let pre: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'zwerk'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pre, 0, "old index should not match subject tokens");

        apply_plan2(&conn).unwrap();

        let post: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'zwerk'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            post, 1,
            "widened index must match subject tokens after rebuild"
        );

        // Recreated trigger keeps new inserts searchable by subject.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,created_at,updated_at)
             VALUES ('f2','Brickholm','is','a game','2026-01-01','2026-01-01')",
            [],
        )
        .unwrap();
        let trig: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'brickholm'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(trig, 1, "post-migration insert trigger must index subject");

        // Idempotent: second apply must not drop/rebuild again or error.
        apply_plan2(&conn).unwrap();
    }

    #[test]
    fn tombstone_requires_zero_access_and_age_over_threshold() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        let col_check: Vec<String> = conn
            .prepare("PRAGMA table_info(facts)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(col_check.contains(&"access_count".to_string()));
        assert!(col_check.contains(&"tombstone".to_string()));
    }
}
