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

    // 2b. facts_fts integrity check + self-heal. External-content fts5 can
    //     hold an index that no longer matches the facts table (lost rebuild,
    //     out-of-band writes); recall's relevance arm then degrades silently.
    //     rank=1 checks the index AGAINST the content table; on mismatch,
    //     rebuild — loud either way (SO S6).
    match conn.execute(
        "INSERT INTO facts_fts(facts_fts, rank) VALUES('integrity-check', 1)",
        [],
    ) {
        Ok(_) => println!("maintain: facts_fts integrity ok"),
        Err(e) => {
            eprintln!("maintain: facts_fts integrity check failed ({e}) — rebuilding");
            match conn.execute("INSERT INTO facts_fts(facts_fts) VALUES('rebuild')", []) {
                Ok(_) => println!("maintain: facts_fts rebuilt"),
                Err(e2) => {
                    eprintln!("maintain: facts_fts rebuild FAILED: {e2}");
                    failures += 1;
                }
            }
        }
    }

    // 3. transcript_files hygiene. Canonical rows are keyed EXACTLY as the
    //    live writer writes them: op_transcript_backstop (consolidate.rs)
    //    registers the ABSOLUTE path under `<hex_dir>/raw/transcripts/`, and
    //    distill::run_on_file opens that string directly. (Review-fix
    //    2026-06-11: the original SQL treated RELATIVE rows as canonical and
    //    deleted every absolute row — wiping the live watermarks weekly and
    //    forcing a full-corpus re-distillation. Never purge rows the live
    //    writer recreates.) Non-canonical transcript-shaped rows (relative
    //    `raw/transcripts/…`, stale absolute prefixes) fold into their
    //    canonical row keeping the furthest watermark; rows that are not
    //    transcript-shaped at all (me/*.md etc.) are purged.
    match transcript_files_hygiene(conn, hex_dir) {
        Ok((folded, purged)) => {
            println!("maintain: transcript_files canonicalized ({folded} folded, {purged} purged)")
        }
        Err(e) => {
            eprintln!("maintain: transcript_files hygiene FAILED: {e}");
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

/// Canonicalize `transcript_files` to the form the live writer uses: the
/// absolute path `<hex_dir>/raw/transcripts/<file>.md`. Returns
/// `(folded, purged)` row counts.
///
/// - Canonical rows (absolute, directly under the current hex_dir's
///   transcripts dir) are kept untouched — these are the live watermarks.
/// - Other transcript-shaped rows (`…raw/transcripts/<file>.md` under any
///   prefix: relative rows, stale absolute prefixes from an old hex_dir
///   location) fold INTO the canonical row, keeping the furthest watermark,
///   then the non-canonical row is deleted.
/// - Rows that are not transcript-shaped (me/learnings.md etc.) are purged.
fn transcript_files_hygiene(conn: &Connection, hex_dir: &Path) -> rusqlite::Result<(usize, usize)> {
    let canon_dir = hex_dir.join("raw").join("transcripts");
    let canon_prefix = format!("{}/", canon_dir.display());

    let rows: Vec<(String, i64)> = conn
        .prepare("SELECT path, last_offset FROM transcript_files")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut folded = 0usize;
    let mut purged = 0usize;
    for (path, offset) in rows {
        let is_canonical = path
            .strip_prefix(&canon_prefix)
            .is_some_and(|rest| rest.ends_with(".md") && !rest.contains('/'));
        if is_canonical {
            continue;
        }
        // Transcript-shaped under a non-canonical prefix? Extract the basename.
        let basename = path
            .rsplit_once("raw/transcripts/")
            .map(|(_, base)| base)
            .filter(|base| base.ends_with(".md") && !base.contains('/') && base.len() > 3);
        if let Some(base) = basename {
            let canon_path = canon_dir.join(base);
            conn.execute(
                "INSERT INTO transcript_files (path, last_offset)
                 VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET
                     last_offset = MAX(transcript_files.last_offset, excluded.last_offset)",
                rusqlite::params![canon_path.to_string_lossy(), offset],
            )?;
            folded += 1;
        } else {
            purged += 1;
        }
        conn.execute(
            "DELETE FROM transcript_files WHERE path = ?1",
            rusqlite::params![path],
        )?;
    }
    Ok((folded, purged))
}

/// CLI entry: open memory.db, run the conn-level sweeps, optionally VACUUM,
/// record telemetry. Exit code 0 only when every step succeeded.
pub fn run(hex_dir: &Path, vacuum: bool, backfill_facts: bool) -> i32 {
    let db_path = super::db_path(hex_dir);
    let conn = match super::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "hex memory maintain: cannot open {}: {e}",
                db_path.display()
            );
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
        status: if failures == 0 {
            "ok".into()
        } else {
            "error".into()
        },
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

    /// Review-fix 2026-06-11: hygiene must NEVER purge the rows the live
    /// writer creates. `op_transcript_backstop` (consolidate.rs) registers
    /// watermarks keyed by the ABSOLUTE path under
    /// `<hex_dir>/raw/transcripts/`, and `distill::run_on_file` opens that
    /// exact string. The original hygiene SQL deleted every absolute-path row
    /// weekly, wiping the live watermarks and forcing a full-corpus
    /// re-distillation after each maintain run.
    #[test]
    fn maintain_preserves_live_backstop_watermarks() {
        use rusqlite::OptionalExtension;

        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();
        let db_path = crate::memory::db_path(hex_root);
        let conn = crate::memory::open_db(&db_path).unwrap();
        crate::memory::index::init_db(&conn).unwrap();

        // Register the watermark with the SAME primitive the backstop uses,
        // keyed by the absolute path it derives from read_dir(hex_dir).
        let abs = hex_root.join("raw").join("transcripts").join("live.md");
        let abs_str = abs.to_str().unwrap();
        crate::memory::distill::watermark::advance_offset(&conn, abs_str, 1234).unwrap();

        let failures = run_maintain(&conn, hex_root, false);
        assert_eq!(failures, 0, "no maintenance step may fail on this fixture");

        let offset: Option<i64> = conn
            .query_row(
                "SELECT last_offset FROM transcript_files WHERE path = ?1",
                [abs_str],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(
            offset,
            Some(1234),
            "the live backstop watermark (absolute path under hex_dir) must \
             survive maintain — purging it forces full re-distillation"
        );
    }

    /// Plan Task 10 Step 1, amended by review-fix 2026-06-11: maintenance must
    /// (a) sweep vec rows whose chunk is gone and (b) canonicalize
    /// transcript_files. The canonical key is the ABSOLUTE path under the
    /// current `<hex_dir>/raw/transcripts/` (the form the live backstop
    /// writes), NOT the relative form the original plan assumed — relative
    /// rows and stale absolute prefixes fold into it keeping the furthest
    /// watermark; foreign rows are purged.
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

        // (b) transcript_files pollution: a foreign row, a relative row, and
        // a stale-absolute-prefix duplicate with a further offset. Both
        // transcript-shaped rows must fold into ONE canonical row keyed by
        // the absolute path under THIS hex_dir.
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
        let canon = hex_root
            .join("raw")
            .join("transcripts")
            .join("a.md")
            .to_string_lossy()
            .to_string();
        assert_eq!(
            rows[0].0, canon,
            "canonical key is the absolute path under this hex_dir (the form \
             the live backstop writes)"
        );
        assert_eq!(rows[0].1, 99, "watermark must fold to the furthest offset");
    }
}
