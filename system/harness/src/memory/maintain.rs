//! `hex memory maintain` — scheduled self-repair for memory.db.
//! Weekly cron (modules/memory_maintenance.worker.rs) + on-demand CLI.
//! One-off corruption must never be permanent: orphan vectors, FTS bloat,
//! foreign transcript_files rows, and dead pages all get swept here.

use rusqlite::Connection;
use std::path::Path;

/// Conn-level maintenance core: orphan-vector sweep, FTS5 optimize,
/// transcript_files hygiene, optional facts backfill. Extracted from [`run`]
/// so tests can drive it against a tempdir DB. `hex_dir` resolves the
/// embedder cache and is only touched when `backfill_facts` is true with
/// pending facts — tests passing `backfill_facts = false` can hand in any
/// path.
///
/// Every step is loud-but-continue (Standing Order S6): a failed sweep must
/// not abort the remaining repairs, so this returns the failed-step count
/// instead of a `Result` (an early `?` would lose the tally `run` reports).
pub fn run_maintain(conn: &Connection, hex_dir: &Path, backfill_facts: bool) -> usize {
    let mut failures = 0;

    // 1. Orphan vector sweep (vec rows whose chunk was deleted pre-fix).
    match conn.execute(
        "DELETE FROM vec_chunks WHERE rowid NOT IN (SELECT rowid FROM chunks)",
        [],
    ) {
        Ok(n) => println!("maintain: swept {n} orphan vector(s)"),
        Err(e) => {
            eprintln!("maintain: orphan sweep FAILED: {e}");
            failures += 1;
        }
    }

    // 2. FTS5 segment optimize (assessment: ~52MB segment bloat).
    for fts in ["chunks", "facts_fts"] {
        match conn.execute(&format!("INSERT INTO {fts}({fts}) VALUES('optimize')"), []) {
            Ok(_) => println!("maintain: optimized {fts}"),
            Err(e) => {
                eprintln!("maintain: optimize {fts} FAILED: {e}");
                failures += 1;
            }
        }
    }

    // 3. transcript_files hygiene: only relative raw/transcripts/*.md rows are
    //    legitimate. Foreign rows (me/*.md etc.) and absolute-path duplicates
    //    polluted the table (assessment, medium): fold dupes into the relative
    //    row keeping the furthest watermark, then purge everything foreign.
    let fold = conn.execute_batch(
        "UPDATE transcript_files AS rel
           SET last_offset = MAX(rel.last_offset,
               COALESCE((SELECT MAX(abs.last_offset) FROM transcript_files abs
                          WHERE abs.path LIKE '%/' || rel.path
                            AND abs.path != rel.path), 0))
         WHERE rel.path LIKE 'raw/transcripts/%.md';
         DELETE FROM transcript_files
          WHERE path NOT LIKE 'raw/transcripts/%.md'
             OR path LIKE '/%';",
    );
    match fold {
        Ok(()) => println!("maintain: transcript_files canonicalized"),
        Err(e) => {
            eprintln!("maintain: transcript_files purge FAILED: {e}");
            failures += 1;
        }
    }

    if backfill_facts {
        match super::maintain_facts::backfill(conn, hex_dir) {
            Ok(n) => println!("maintain: embedded {n} fact(s)"),
            Err(e) => {
                eprintln!("maintain: facts backfill FAILED: {e}");
                failures += 1;
            }
        }
    }

    failures
}

/// CLI entry: open memory.db, run the conn-level sweeps, optionally VACUUM,
/// record telemetry. Exit code 0 only when every step succeeded.
pub fn run(hex_dir: &Path, vacuum: bool, backfill_facts: bool) -> i32 {
    let db_path = super::db_path(hex_dir);
    let conn = match super::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex memory maintain: cannot open {}: {e}", db_path.display());
            return 1;
        }
    };
    let mut failures = run_maintain(&conn, hex_dir, backfill_facts);

    // 4. VACUUM last (rebuilds the file: dead vec slots + freelist reclaimed;
    //    assessment: 305MB file, ~100MB live).
    if vacuum {
        let before = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
        match conn.execute("VACUUM", []) {
            Ok(_) => {
                let after = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
                println!("maintain: VACUUM {before} -> {after} bytes");
            }
            Err(e) => {
                eprintln!("maintain: VACUUM FAILED: {e}");
                failures += 1;
            }
        }
    }

    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "memory::maintain".into(),
        event: "maintain".into(),
        status: if failures == 0 { "ok".into() } else { "error".into() },
        duration_ms: None,
        exit_code: Some(if failures == 0 { 0 } else { 1 }),
        detail: Some(format!(
            "vacuum={vacuum} backfill_facts={backfill_facts} failures={failures}"
        )),
    });
    if failures == 0 {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Plan Task 10 Step 1: maintenance must (a) sweep vec rows whose chunk is
    /// gone and (b) canonicalize transcript_files — fold absolute-path dupes
    /// into the relative row keeping the furthest watermark, purge foreign rows.
    #[test]
    fn maintain_sweeps_orphan_vectors_and_canonicalizes_transcript_files() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        // Same fixture shape as index.rs tests: open_db (vec0 + Plan 2 schema)
        // then init_db (files/chunks/vec_chunks).
        let db_path = crate::memory::db_path(hex_root);
        let conn = crate::memory::open_db(&db_path).unwrap();
        crate::memory::index::init_db(&conn).unwrap();

        // (a) one orphan vector — a vec_chunks row with no chunks row.
        let v = vec![0.25f32; crate::memory::vector::EMBED_DIM];
        crate::memory::vector::insert_vec(&conn, 999, &v).unwrap();

        // (b) transcript_files pollution: a foreign row, the legitimate
        // relative row, and an absolute-path duplicate with a further offset.
        conn.execute_batch(
            "INSERT INTO transcript_files (path, last_offset) VALUES ('me/learnings.md', 7);
             INSERT INTO transcript_files (path, last_offset) VALUES ('raw/transcripts/a.md', 10);
             INSERT INTO transcript_files (path, last_offset) VALUES ('/abs/prefix/raw/transcripts/a.md', 99);",
        )
        .unwrap();

        let failures = run_maintain(&conn, hex_root, false);
        assert_eq!(failures, 0, "no maintenance step may fail on this fixture");

        let orphans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vec_chunks WHERE rowid NOT IN (SELECT rowid FROM chunks)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "orphan vectors must be swept");

        let rows: Vec<(String, i64)> = conn
            .prepare("SELECT path, last_offset FROM transcript_files")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "exactly one canonical transcript_files row must remain, got {rows:?}"
        );
        assert_eq!(rows[0].0, "raw/transcripts/a.md");
        assert_eq!(rows[0].1, 99, "watermark must fold to the furthest offset");
    }
}
