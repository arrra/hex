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

/// Trend table for the recall eval — one row per `hex-eval-trend` cron run.
/// Columns mirror the eval's machine-readable summary so the trend is a
/// straight append: no scoring change, just a durable record of each night's
/// numbers. `baseline_present` is stored 0/1.
pub const EVAL_RUNS_DDL: &str = "
CREATE TABLE IF NOT EXISTS eval_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    ts               TEXT NOT NULL,
    cases_total      INTEGER NOT NULL,
    facts_hits       INTEGER NOT NULL,
    anywhere_hits    INTEGER NOT NULL,
    regressions      INTEGER NOT NULL,
    baseline_present INTEGER NOT NULL,
    harness_version  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_runs_ts ON eval_runs(ts);
";

/// Apply the `eval_runs` migration. A single `CREATE TABLE IF NOT EXISTS` DDL
/// batch is atomic and idempotent (same shape as `apply_messages_schema`), so
/// a partial or repeated apply can never leave a half-built table.
pub fn apply_eval_runs_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(EVAL_RUNS_DDL)
}

/// Auto-tuner ledgers (hill-climber stage 1, spec Tzxmamhr8). `win_log` records
/// every landed parameter change; `regret_log` records every rejected candidate
/// AND every later auto-revert. Both tables carry the identical column set the
/// spec fixes — `id, ts, params_json, tuning_score, heldout_score, action,
/// reverted` — so the weekly digest reads them uniformly. `params_json` is the
/// free-form payload (the winning `RecallConfig`, the archived `.prev` path, and
/// the pre-change held-out score the revert check re-measures against).
pub const TUNE_LOG_DDL: &str = "
CREATE TABLE IF NOT EXISTS win_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            TEXT NOT NULL,
    params_json   TEXT NOT NULL,
    tuning_score  INTEGER NOT NULL,
    heldout_score INTEGER NOT NULL,
    action        TEXT NOT NULL,
    reverted      INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS regret_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            TEXT NOT NULL,
    params_json   TEXT NOT NULL,
    tuning_score  INTEGER NOT NULL,
    heldout_score INTEGER NOT NULL,
    action        TEXT NOT NULL,
    reverted      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_win_log_ts    ON win_log(ts);
CREATE INDEX IF NOT EXISTS idx_regret_log_ts ON regret_log(ts);
";

/// Apply the `win_log`/`regret_log` migration. One `CREATE TABLE IF NOT EXISTS`
/// batch — atomic and idempotent, same shape as `apply_eval_runs_schema`, so a
/// partial or repeated apply can never leave a half-built ledger.
pub fn apply_tune_log_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(TUNE_LOG_DDL)
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
            Ok(did) => match conn.execute_batch("COMMIT") {
                Ok(()) => {
                    if did {
                        eprintln!(
                            "[schema] facts_fts widened to subject+predicate+object and rebuilt"
                        );
                    }
                }
                Err(commit_err) => {
                    // A-F8 (workflow wf_8e8c4033-b9f, PR #9 round-2 review):
                    // mirrors the ROLLBACK-on-failed-COMMIT pattern
                    // `rebuild_facts_vec_with_is_live` and consolidate.rs's
                    // tombstone writers already use — a failed COMMIT here
                    // left THIS connection sitting inside the widening
                    // transaction, which `open_db`'s unconditional next call
                    // to the REQUIRED `apply_plan3` on the SAME connection
                    // would then hit with no open transaction allowed.
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(commit_err);
                }
            },
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

/// Versioning columns so a judge Update can SUPERSEDE a fact instead of
/// overwriting its `object` in place (closed-loop-plan-2026-09-06 §4). A live
/// row has `invalid_at IS NULL`; a superseded row keeps its original `object`
/// text forever and points at its replacement via `superseded_by`.
pub const PLAN3_INDEX_DDL: &str = r#"
CREATE INDEX IF NOT EXISTS facts_live_idx ON facts(subject, predicate) WHERE invalid_at IS NULL;
"#;

/// vec0 tables cannot `ALTER TABLE ADD COLUMN`, so giving `facts_vec` the
/// `is_live` metadata column (G1, decision
/// `hex-knn-is-live-metadata-filter-2026-09-10.md`) needs a rebuild. The
/// vendored sqlite-vec 0.1.9 vec0 module registers `xRename = 0`
/// (sqlite-vec.c, citing upstream issue asg017/sqlite-vec#43): `ALTER TABLE
/// ... RENAME TO` on a vec0 table is unimplemented and leaves its shadow
/// tables (`..._rowids`, `..._chunks`, `..._info`) under the OLD name,
/// verified against this vendored build with a create-`facts_vec_new`/
/// copy/DROP-old/RENAME sequence — the RENAME step left `facts_vec` querying
/// a since-renamed `facts_vec_rowids` shadow table and failed with "no such
/// table: main.facts_vec_rowids". Same end state (embeddings preserved,
/// `is_live` derived from each fact's CURRENT `invalid_at` AND `tombstone`
/// (PR#9 r2 review_b G1), `facts_vec` rows with no matching `facts` row
/// dropped) reached without RENAME instead:
/// buffer every existing row into memory, drop the old table, then recreate
/// `facts_vec` directly under its final name and reinsert. Idempotent:
/// skipped once `facts_vec` already carries the column (probed with `SELECT
/// is_live FROM facts_vec LIMIT 0`, exactly as the task names it), so
/// re-running never re-buffers or re-derives `is_live` from a possibly-stale
/// `invalid_at` snapshot.
///
/// Runs inside one `BEGIN IMMEDIATE` transaction — same idiom as
/// `apply_plan2`'s `facts_fts` widening above: the write lock is taken up
/// front, the is_live probe is RE-CHECKED under that lock (two fresh-process
/// openers racing this path must have the loser see the winner's finished
/// table and no-op, not drop it again), and the drop+recreate+reinsert
/// commits atomically. Without this, a crash or injected error between the
/// DROP and the last INSERT leaves `facts_vec` with the new is_live shape
/// (so the guard's probe now passes and skips forever) but only PART of the
/// original rows — every embedding after the failure point is silently lost
/// until the weekly backfill re-embeds. On any error the transaction rolls
/// back to the old, still-complete table and the next open retries.
fn rebuild_facts_vec_with_is_live(conn: &Connection) -> Result<()> {
    if conn
        .prepare("SELECT is_live FROM facts_vec LIMIT 0")
        .is_ok()
    {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migrate = || -> Result<()> {
        if conn
            .prepare("SELECT is_live FROM facts_vec LIMIT 0")
            .is_ok()
        {
            return Ok(());
        }
        let mut stmt = conn.prepare(
            "SELECT v.fact_id, v.embedding, (f.invalid_at IS NULL AND f.tombstone = 0)
               FROM facts_vec v
               JOIN facts f ON f.id = v.fact_id",
        )?;
        let rows: Vec<(String, Vec<u8>, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_>>()?;
        drop(stmt);
        conn.execute_batch("DROP TABLE facts_vec;")?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE facts_vec USING vec0(
                fact_id TEXT PRIMARY KEY,
                embedding FLOAT[768],
                is_live BOOLEAN
            );",
        )?;
        for (fact_id, embedding, is_live) in rows {
            conn.execute(
                "INSERT INTO facts_vec(fact_id, embedding, is_live) VALUES (?1, ?2, ?3)",
                (fact_id, embedding, is_live),
            )?;
        }
        Ok(())
    };
    match migrate() {
        Ok(()) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(()),
            Err(commit_err) => {
                // F8 (major, arrra/hex PR #9 round 2): a failed COMMIT still
                // leaves the connection inside the transaction (same gap as
                // consolidate.rs's tombstone writers and vector.rs's
                // insert_fact_vec, PR#9 r2 review_b G2) — roll back so the
                // rebuilt-but-uncommitted table never lingers as something
                // this connection (and a retried apply_plan3 on it) would
                // wrongly treat as already migrated.
                let _ = conn.execute_batch("ROLLBACK");
                Err(commit_err)
            }
        },
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Migrate a Plan 2 (schema_version 4) database to Plan 3 (schema_version 5):
/// adds `valid_from`/`invalid_at`/`superseded_by` to `facts`, backfills
/// `valid_from = created_at` on pre-existing rows (which stay live —
/// `invalid_at` is left NULL), adds the live-rows partial index, and rebuilds
/// `facts_vec` with the `is_live` metadata column (see
/// `rebuild_facts_vec_with_is_live`) so `knn_facts` can filter superseded
/// facts inside the vector query itself. Uses the same guarded-ALTER idiom as
/// `apply_plan2`'s `transcript_files` backfill so re-running against an
/// already-migrated database is a no-op, not an error.
///
/// Skips the ALTER/backfill/index/facts_vec-rebuild work entirely once
/// `schema_version` already records version 5 (PR#9 r1 F4) AND `facts_vec`
/// already carries the `is_live` column (PR#9 r2 G1, review_b R1 G1): those
/// steps are safe to repeat, but every `open_db` call would otherwise re-run
/// three guarded `ALTER TABLE` attempts and a table scan on every process
/// start for no effect. The version-5 marker alone is not sufficient — a DB
/// migrated by the pre-G1 code already has that row but never rebuilt
/// `facts_vec`, so gating on the marker alone would skip the rebuild
/// forever and leave `insert_fact_vec`/`knn_facts` broken (`no such column:
/// is_live`). A DB whose migration only partially landed (columns present
/// but no version-5 row, or vice versa, or is_live still missing) still
/// fails one half of this check, so it still retries the full sequence —
/// `rebuild_facts_vec_with_is_live` and the guarded `ALTER TABLE`s are each
/// independently idempotent, so re-running the whole function is always
/// safe.
pub fn apply_plan3(conn: &Connection) -> Result<()> {
    let already_applied: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM schema_version WHERE version = 5",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let facts_vec_has_is_live = conn
        .prepare("SELECT is_live FROM facts_vec LIMIT 0")
        .is_ok();
    if already_applied > 0 && facts_vec_has_is_live {
        return Ok(());
    }
    for (col, ddl) in [
        ("valid_from", "ALTER TABLE facts ADD COLUMN valid_from TEXT"),
        ("invalid_at", "ALTER TABLE facts ADD COLUMN invalid_at TEXT"),
        (
            "superseded_by",
            "ALTER TABLE facts ADD COLUMN superseded_by TEXT",
        ),
    ] {
        if let Err(e) = conn.execute(ddl, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                eprintln!("[schema] facts.{col} backfill failed: {e}");
                return Err(e);
            }
        }
    }
    conn.execute(
        "UPDATE facts SET valid_from = created_at WHERE valid_from IS NULL",
        [],
    )?;
    conn.execute_batch(PLAN3_INDEX_DDL)?;
    rebuild_facts_vec_with_is_live(conn)?;
    conn.execute(
        "INSERT INTO schema_version (version, applied_at) VALUES (5, datetime('now'))
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

    /// RED for the R1 review redo (`review-redo`, F1 major): without a
    /// transaction, an error injected partway through the reinsert loop
    /// (DROP + CREATE already committed, only some rows re-inserted) left
    /// `facts_vec` in the NEW is_live shape with the remaining rows silently
    /// dropped — and since the guard's probe now passes, every later
    /// `apply_plan3` call would skip the rebuild forever, permanently losing
    /// those embeddings. `rebuild_facts_vec_with_is_live` must wrap the whole
    /// drop/create/reinsert in one transaction: on error, ROLLBACK must
    /// restore the OLD 2-column table with every original row intact, so the
    /// next `open_db` retries the rebuild from a consistent starting point.
    #[test]
    fn rebuild_facts_vec_rollback_preserves_old_table_on_reinsert_failure() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN valid_from TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN invalid_at TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN superseded_by TEXT", [])
            .unwrap();

        let n = 5;
        for i in 0..n {
            let id = format!("f{i}");
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from) VALUES (?1,'s','p','o',0.5,'2026-01-01','2026-01-01','2026-01-01')",
                rusqlite::params![id],
            ).unwrap();
            let v: Vec<f32> = (0..crate::memory::vector::EMBED_DIM)
                .map(|d| (i as f32 + d as f32) * 0.0001)
                .collect();
            conn.execute(
                "INSERT INTO facts_vec(fact_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![id, crate::memory::vector::f32s_to_le_bytes(&v)],
            )
            .unwrap();
        }

        // Deny the 3rd INSERT into the rebuilt facts_vec — simulates a crash
        // or error after DROP+CREATE already ran but before every row is
        // reinserted.
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
        let c2 = counter.clone();
        conn.authorizer(Some(move |ctx: AuthContext<'_>| {
            if let AuthAction::Insert { table_name } = ctx.action {
                if table_name == "facts_vec" {
                    let n = c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if n == 3 {
                        return Authorization::Deny;
                    }
                }
            }
            Authorization::Allow
        }));

        let result = rebuild_facts_vec_with_is_live(&conn);
        conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);

        assert!(
            result.is_err(),
            "an authorizer-denied reinsert must surface as an Err, not silently succeed"
        );

        let probe = conn.prepare("SELECT is_live FROM facts_vec LIMIT 0");
        assert!(
            probe.is_err(),
            "rollback must restore the OLD 2-column facts_vec (is_live probe must still fail), not leave the new shape half-built"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM facts_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, n as i64,
            "rollback must preserve every pre-existing facts_vec row, not leave a dropped/partial table"
        );
    }

    /// RED for F8 (major, arrra/hex PR #9 round 2): `rebuild_facts_vec_with_is_live`
    /// rolls back a failed `migrate()` (the test above), but its own `COMMIT`
    /// uses `?` OUTSIDE that rollback path — the exact gap `consolidate.rs`'s
    /// tombstone writers and `vector.rs`'s `insert_fact_vec` (PR#9 r2
    /// review_b G2) already closed. A failed COMMIT (SQLITE_BUSY at the
    /// RESERVED->EXCLUSIVE lock upgrade under a blocking reader in
    /// rollback-journal mode — same construction as index.rs's F1/F7
    /// regressions) leaves the connection sitting inside the just-migrated
    /// transaction: this connection sees the rebuilt 3-column `is_live`
    /// table as already present (even though it never reached disk), so a
    /// retried `apply_plan3` on it would see the version-5 marker plus that
    /// uncommitted column and wrongly report success, while a fresh
    /// connection still sees the old 2-column table.
    #[test]
    fn rebuild_facts_vec_rolls_back_on_failed_commit_and_retry_persists_durably() {
        crate::memory::vector::register_sqlite_vec();
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("memory.db");
        let conn = Connection::open(&db_path).unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN valid_from TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN invalid_at TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN superseded_by TEXT", [])
            .unwrap();

        let n = 3;
        for i in 0..n {
            let id = format!("f{i}");
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from) VALUES (?1,'s','p','o',0.5,'2026-01-01','2026-01-01','2026-01-01')",
                rusqlite::params![id],
            ).unwrap();
            let v: Vec<f32> = (0..crate::memory::vector::EMBED_DIM)
                .map(|d| (i as f32 + d as f32) * 0.0001)
                .collect();
            conn.execute(
                "INSERT INTO facts_vec(fact_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![id, crate::memory::vector::f32s_to_le_bytes(&v)],
            )
            .unwrap();
        }

        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            mode, "delete",
            "test setup: must be off WAL to reproduce a commit-time lock-upgrade failure"
        );
        conn.busy_timeout(std::time::Duration::from_millis(0))
            .unwrap();

        // A second connection holding an open read transaction: its SHARED
        // lock lets `conn`'s BEGIN IMMEDIATE (RESERVED) proceed, then blocks
        // the COMMIT's RESERVED->EXCLUSIVE upgrade.
        let conn2 = Connection::open(&db_path).unwrap();
        conn2.execute_batch("BEGIN;").unwrap();
        let _: i64 = conn2
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();

        let result = rebuild_facts_vec_with_is_live(&conn);
        assert!(
            result.is_err(),
            "a blocked COMMIT must surface as an error, not a silent success"
        );

        assert!(
            conn.is_autocommit(),
            "F8: a failed COMMIT must roll back, restoring the connection's \
             entry (autocommit) state, not leave the transaction open"
        );
        let probe = conn.prepare("SELECT is_live FROM facts_vec LIMIT 0");
        assert!(
            probe.is_err(),
            "F8: a rolled-back COMMIT must leave the OLD 2-column facts_vec \
             in place, not a table this connection sees as already migrated"
        );
        drop(probe);

        // Let the blocking reader go, then a retry on the SAME connection
        // must commit durably.
        conn2.execute_batch("COMMIT;").unwrap();
        drop(conn2);

        rebuild_facts_vec_with_is_live(&conn).unwrap();

        drop(conn);
        let reopened = Connection::open(&db_path).unwrap();
        let probe_after = reopened.prepare("SELECT is_live FROM facts_vec LIMIT 0");
        assert!(
            probe_after.is_ok(),
            "F8: the retry must persist durably — a fresh connection must see the migrated table"
        );
        let count: i64 = reopened
            .query_row("SELECT COUNT(*) FROM facts_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, n as i64,
            "F8: every original row must survive the rollback + successful retry"
        );
    }

    /// A-F8 (workflow wf_8e8c4033-b9f, PR #9 round-2 review, ledger id A-F8):
    /// the F8 "roll back on a failed COMMIT" pattern was applied to
    /// `rebuild_facts_vec_with_is_live` but NOT to the structurally
    /// identical `facts_fts`-widening COMMIT in `apply_plan2`, reachable on
    /// every `open_db()` call. Same construction as the sibling regression
    /// above: a blocking reader in rollback-journal mode makes the widening
    /// transaction's COMMIT fail with SQLITE_BUSY; the connection must roll
    /// back to autocommit with the OLD object-only `facts_fts` intact, and a
    /// retry on the same connection must then commit durably.
    #[test]
    fn facts_fts_widening_rolls_back_on_failed_commit_and_retry_persists_durably() {
        crate::memory::vector::register_sqlite_vec();
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("memory.db");
        let conn = Connection::open(&db_path).unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        conn.execute_batch(PLAN2_DDL).unwrap();
        conn.execute_batch(PLAN2_VEC_DDL).unwrap();
        // Old shape: object-only external-content fts (needs widening).
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

        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            mode, "delete",
            "test setup: must be off WAL to reproduce a commit-time lock-upgrade failure"
        );
        conn.busy_timeout(std::time::Duration::from_millis(0))
            .unwrap();

        // A second connection holding an open read transaction: its SHARED
        // lock lets `conn`'s BEGIN IMMEDIATE (RESERVED) proceed, then blocks
        // the widening COMMIT's RESERVED->EXCLUSIVE upgrade.
        let conn2 = Connection::open(&db_path).unwrap();
        conn2.execute_batch("BEGIN;").unwrap();
        let _: i64 = conn2
            .query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))
            .unwrap();

        let result = apply_plan2(&conn);
        assert!(
            result.is_err(),
            "a blocked widening COMMIT must surface as an error, not a silent success"
        );
        assert!(
            conn.is_autocommit(),
            "A-F8: a failed widening COMMIT must roll back, restoring the              connection's entry (autocommit) state, not leave the              transaction open"
        );
        assert!(
            facts_fts_needs_widening(&conn).unwrap(),
            "A-F8: a real rollback restores the OLD object-only facts_fts (no `subject` column), so needs_widening must read true again — false here would mean the DROP TABLE landed without its matching CREATE, an even worse half-migrated state"
        );

        // Let the blocking reader go, then a retry on the SAME connection
        // must commit durably.
        conn2.execute_batch("COMMIT;").unwrap();
        drop(conn2);

        apply_plan2(&conn).unwrap();
        assert!(
            !facts_fts_needs_widening(&conn).unwrap(),
            "A-F8: the retry must actually widen facts_fts"
        );

        drop(conn);
        let reopened = Connection::open(&db_path).unwrap();
        let post: i64 = reopened
            .query_row(
                "SELECT COUNT(*) FROM facts_fts WHERE facts_fts MATCH 'zwerk'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            post, 1,
            "A-F8: the retry must persist durably — a fresh connection must              see the widened, subject-searchable index with the original row"
        );
    }

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

    /// RED for closed-loop-plan-2026-09-06 §4 / FIX item 1: the versioning
    /// migration (schema_version 5) must run against a DB already shaped by
    /// `apply_plan2` (v4) with existing rows — not only a fresh DB. Existing
    /// rows must be backfilled `valid_from = created_at`, `invalid_at NULL`,
    /// and a second apply must be a no-op (idempotent: no duplicate-column
    /// error, version stays 5). Fails now because `apply_plan3` does not
    /// exist yet.
    #[test]
    fn apply_plan3_backfills_valid_from_and_is_idempotent_on_v4_db() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,created_at,updated_at)
             VALUES ('f1','boi','has','installed and live version 3.3.2','2026-01-01','2026-01-01')",
            [],
        )
        .unwrap();

        apply_plan3(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version, 5,
            "schema_version should record version=5 after apply_plan3"
        );

        let (valid_from, invalid_at): (String, Option<String>) = conn
            .query_row(
                "SELECT valid_from, invalid_at FROM facts WHERE id='f1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            valid_from, "2026-01-01",
            "existing rows must be backfilled with valid_from = created_at"
        );
        assert!(
            invalid_at.is_none(),
            "existing rows must remain live (invalid_at NULL) after migration"
        );

        // Idempotent: re-run must not error (guarded ALTER) and must leave version 5.
        apply_plan3(&conn).unwrap();
        let version2: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version2, 5,
            "re-applying apply_plan3 must stay at version 5"
        );
    }

    /// RED for PR#9 r1 F4 (minor): once `schema_version` already records
    /// version 5, `apply_plan3` must skip the ALTER/backfill/index work
    /// entirely rather than re-running it on every `open_db` call. Pinned by
    /// making the file read-only AFTER a completed migration: the guarded
    /// `ALTER TABLE`s are tolerated no-ops either way, but the backfill
    /// `UPDATE` needs a write transaction even when it matches zero rows —
    /// so a re-run that doesn't skip hits `SQLITE_READONLY` here, while a
    /// re-run that skips returns `Ok` untouched.
    #[test]
    fn apply_plan3_skips_backfill_when_version_5_marker_present() {
        crate::memory::vector::register_sqlite_vec();
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("memory.db");

        {
            let conn = Connection::open(&db_path).unwrap();
            apply_plan1_baseline_for_test(&conn).unwrap();
            apply_plan2(&conn).unwrap();
            apply_plan3(&conn).unwrap();
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o444)).unwrap();
        }

        let conn = Connection::open(&db_path).unwrap();
        let result = apply_plan3(&conn);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        assert!(
            result.is_ok(),
            "apply_plan3 must skip its write work (not error) once version 5 is already recorded: {result:?}"
        );
    }

    /// RED for G1 (major, Codex R2) / decision
    /// `hex-knn-is-live-metadata-filter-2026-09-10.md` item 1: `facts_vec`
    /// must gain an `is_live BOOLEAN` metadata column as part of reaching
    /// schema version 5 — vec0 tables cannot `ALTER TABLE ADD COLUMN`, so
    /// this requires the buffer/drop/recreate/reinsert sequence
    /// `rebuild_facts_vec_with_is_live` implements (RENAME is unavailable —
    /// see that function's doc comment). Probed exactly as the task names it:
    /// `SELECT is_live FROM facts_vec LIMIT 0`. Fails now because
    /// `apply_plan3` never touches `facts_vec` — it is still the plain
    /// `vec0(fact_id, embedding)` shape from `PLAN2_VEC_DDL`.
    #[test]
    fn apply_plan3_rebuilds_facts_vec_with_is_live_column() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap();

        apply_plan3(&conn).unwrap();

        let probe = conn.prepare("SELECT is_live FROM facts_vec LIMIT 0");
        assert!(
            probe.is_ok(),
            "facts_vec must carry an is_live BOOLEAN metadata column once schema version 5 is reached, got {:?}",
            probe.err()
        );
    }

    /// RED for G1 item 1/4: migrating a v4 DB that ALREADY has `facts_vec`
    /// rows (the real production shape — `maintain_facts::backfill` embeds
    /// live facts under the old 2-column table) must rebuild `facts_vec` to
    /// the 3-column `is_live` shape WITHOUT losing any embedding, and must
    /// set `is_live` from each row's CURRENT `facts.invalid_at`. Simulates a
    /// crash-mid-migration DB (bi-temporal columns already ALTERed onto
    /// `facts`, but no version-5 marker written yet — the exact case
    /// `apply_plan3`'s own doc comment calls out) so a live and a superseded
    /// fact both already exist with real `invalid_at` values BEFORE the
    /// facts_vec rebuild runs. Also pins idempotency (item 4: "apply_plan3
    /// twice is a no-op") by re-running and checking the rebuilt row is
    /// untouched. Fails now: `apply_plan3` never rebuilds `facts_vec`, so
    /// the `is_live` column does not exist and the SELECT below errors.
    #[test]
    fn apply_plan3_migrates_v4_facts_vec_preserving_embeddings_and_setting_is_live() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap(); // v4: facts_vec is still (fact_id, embedding)

        // Partial-migration simulation: bi-temporal columns already present
        // on `facts`, no version-5 marker yet.
        conn.execute("ALTER TABLE facts ADD COLUMN valid_from TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN invalid_at TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN superseded_by TEXT", [])
            .unwrap();

        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by)
             VALUES ('01HFACT-LIVE','project:hex','uses','a live object',0.5,'2026-06-11','2026-06-11','2026-06-11',NULL,NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by)
             VALUES ('01HFACT-SUP','project:hex','uses','a superseded object',0.5,'2026-06-11','2026-06-11','2026-06-11','2026-09-05','01HFACT-NEW')",
            [],
        )
        .unwrap();

        // facts_vec already populated under the OLD 2-column shape.
        let live_vec: Vec<f32> = (0..crate::memory::vector::EMBED_DIM)
            .map(|d| d as f32 * 0.001)
            .collect();
        let sup_vec: Vec<f32> = (0..crate::memory::vector::EMBED_DIM)
            .map(|d| (d as f32 + 1.0) * 0.001)
            .collect();
        conn.execute(
            "INSERT INTO facts_vec(fact_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![
                "01HFACT-LIVE",
                crate::memory::vector::f32s_to_le_bytes(&live_vec)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts_vec(fact_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![
                "01HFACT-SUP",
                crate::memory::vector::f32s_to_le_bytes(&sup_vec)
            ],
        )
        .unwrap();

        apply_plan3(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5, "migration must reach schema version 5");

        let is_live_map: std::collections::HashMap<String, i64> = {
            let mut stmt = conn
                .prepare("SELECT fact_id, is_live FROM facts_vec")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .unwrap()
                .filter_map(Result::ok)
                .collect()
        };
        assert_eq!(
            is_live_map.get("01HFACT-LIVE"),
            Some(&1),
            "pre-existing live fact must be rebuilt with is_live = 1"
        );
        assert_eq!(
            is_live_map.get("01HFACT-SUP"),
            Some(&0),
            "pre-existing superseded fact must be rebuilt with is_live = 0"
        );

        let stored: Vec<u8> = conn
            .query_row(
                "SELECT embedding FROM facts_vec WHERE fact_id = '01HFACT-LIVE'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored,
            crate::memory::vector::f32s_to_le_bytes(&live_vec),
            "embedding bytes must survive the facts_vec rebuild unchanged"
        );

        // Idempotent: a second apply_plan3 call must not error or corrupt
        // the rebuilt shape/values.
        apply_plan3(&conn).unwrap();
        let is_live_after: i64 = conn
            .query_row(
                "SELECT is_live FROM facts_vec WHERE fact_id = '01HFACT-LIVE'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            is_live_after, 1,
            "second apply_plan3 call must be a no-op, not corrupt is_live"
        );
    }

    /// RED for review_b R1 G1 (major): a database migrated by the PRIOR
    /// (pre-G1) code already carries a `schema_version = 5` row — that old
    /// code's F4 short-circuit wrote the marker without ever giving
    /// `facts_vec` the `is_live` column, since `is_live` did not exist yet.
    /// `apply_plan3`'s guard at the top of the function must not trust the
    /// version-5 marker alone: gating only on `schema_version` makes this
    /// case skip forever, leaving `facts_vec` in the plain 2-column shape so
    /// every later `insert_fact_vec`/`knn_facts` call breaks (`no such
    /// column: is_live`). The guard must also probe `facts_vec` for
    /// `is_live` and only skip when BOTH hold. Fails now because
    /// `apply_plan3` returns `Ok(())` immediately on the version-5 row,
    /// leaving the probe below failing forever.
    #[test]
    fn apply_plan3_rebuilds_facts_vec_when_old_v5_marker_lacks_is_live_column() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap(); // v4: facts_vec is still (fact_id, embedding)

        // Simulate the OLD pre-G1 migration's end state: bi-temporal columns
        // present, a version-5 marker already recorded, but facts_vec never
        // touched.
        conn.execute("ALTER TABLE facts ADD COLUMN valid_from TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN invalid_at TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN superseded_by TEXT", [])
            .unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (5, datetime('now'))",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by)
             VALUES ('01HFACT-OLD','project:hex','uses','a live object',0.5,'2026-06-11','2026-06-11','2026-06-11',NULL,NULL)",
            [],
        )
        .unwrap();
        let embedding: Vec<f32> = (0..crate::memory::vector::EMBED_DIM)
            .map(|d| d as f32 * 0.001)
            .collect();
        conn.execute(
            "INSERT INTO facts_vec(fact_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![
                "01HFACT-OLD",
                crate::memory::vector::f32s_to_le_bytes(&embedding)
            ],
        )
        .unwrap();

        // Precondition: this is exactly the buggy starting shape — version 5
        // already recorded, is_live not yet present.
        let version_before: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version_before, 5);
        assert!(
            conn.prepare("SELECT is_live FROM facts_vec LIMIT 0")
                .is_err(),
            "test setup must reproduce the old pre-G1 shape: no is_live column yet"
        );

        apply_plan3(&conn).unwrap();

        let probe = conn.prepare("SELECT is_live FROM facts_vec LIMIT 0");
        assert!(
            probe.is_ok(),
            "apply_plan3 must rebuild facts_vec with is_live even when a stale \
             version-5 marker is already present, got {:?}",
            probe.err()
        );
        let (stored, is_live): (Vec<u8>, i64) = conn
            .query_row(
                "SELECT embedding, is_live FROM facts_vec WHERE fact_id = '01HFACT-OLD'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            stored,
            crate::memory::vector::f32s_to_le_bytes(&embedding),
            "embedding must survive the deferred rebuild unchanged"
        );
        assert_eq!(is_live, 1, "live fact must be rebuilt with is_live = 1");
    }

    /// RED for G1 (reviewer-B redo): `rebuild_facts_vec_with_is_live` derives
    /// `is_live` from `(f.invalid_at IS NULL)` alone — a fact that is
    /// tombstoned (`facts.tombstone = 1`, set by consolidate canonicalization,
    /// never deleted) but never superseded (`invalid_at` still NULL) is a v4
    /// row this migration must NOT mark live. Simulates a v4 DB with a
    /// tombstoned fact whose `facts_vec` row predates the rebuild; after
    /// `apply_plan3`, its `is_live` must be 0. Fails now: the migration's
    /// copy expression ignores `tombstone` entirely, so this fact is rebuilt
    /// with `is_live = 1`.
    #[test]
    fn apply_plan3_sets_is_live_zero_for_tombstoned_rows_on_v4_migration() {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        apply_plan1_baseline_for_test(&conn).unwrap();
        apply_plan2(&conn).unwrap(); // v4: facts_vec is still (fact_id, embedding)

        conn.execute("ALTER TABLE facts ADD COLUMN valid_from TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN invalid_at TEXT", [])
            .unwrap();
        conn.execute("ALTER TABLE facts ADD COLUMN superseded_by TEXT", [])
            .unwrap();

        // Tombstoned, but NOT superseded: invalid_at stays NULL. This is the
        // canonicalization-collapse shape (memory/consolidate.rs
        // tombstone_duplicate_fact), distinct from the supersede-not-overwrite
        // shape the other v4-migration tests already cover.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by,tombstone)
             VALUES ('01HFACT-TOMB','project:hex','uses','a tombstoned duplicate',0.5,'2026-06-11','2026-06-11','2026-06-11',NULL,NULL,1)",
            [],
        )
        .unwrap();

        let embedding: Vec<f32> = (0..crate::memory::vector::EMBED_DIM)
            .map(|d| d as f32 * 0.001)
            .collect();
        conn.execute(
            "INSERT INTO facts_vec(fact_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![
                "01HFACT-TOMB",
                crate::memory::vector::f32s_to_le_bytes(&embedding)
            ],
        )
        .unwrap();

        apply_plan3(&conn).unwrap();

        let is_live: i64 = conn
            .query_row(
                "SELECT is_live FROM facts_vec WHERE fact_id = '01HFACT-TOMB'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            is_live, 0,
            "a tombstoned (but not superseded) fact must be rebuilt with is_live = 0, not just invalid_at IS NULL"
        );
    }
}
