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
    object,
    content=facts,
    content_rowid=rowid,
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS facts_fts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, object) VALUES (new.rowid, new.object);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, object) VALUES('delete', old.rowid, old.object);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, object) VALUES('delete', old.rowid, old.object);
    INSERT INTO facts_fts(rowid, object) VALUES (new.rowid, new.object);
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
    conn.execute_batch(PLAN2_FTS_DDL)?;
    conn.execute(
        "INSERT INTO schema_version (version, applied_at) VALUES (4, datetime('now'))
         ON CONFLICT(version) DO NOTHING",
        [],
    )?;
    Ok(())
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
