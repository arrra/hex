use rusqlite::{Connection, OptionalExtension};

pub fn last_offset(conn: &Connection, path: &str) -> anyhow::Result<i64> {
    let offset: Option<i64> = conn
        .query_row(
            "SELECT last_offset FROM transcript_files WHERE path=?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .optional()?;
    Ok(offset.unwrap_or(0))
}

/// Advance the per-file watermark. Monotonic: an attempt to regress
/// `last_offset` (concurrent write or stale caller) is rejected with a loud
/// stderr warning and the higher value is preserved.
pub fn advance_offset(conn: &Connection, path: &str, new_offset: i64) -> anyhow::Result<()> {
    // Detect regression up-front so we can warn loudly. The ON CONFLICT clause
    // below enforces the monotonic property at SQL level too.
    let prev: Option<i64> = conn
        .query_row(
            "SELECT last_offset FROM transcript_files WHERE path=?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(p) = prev {
        if new_offset < p {
            eprintln!(
                "[watermark] REGRESSION REJECTED for {}: attempted last_offset={} \
                 < stored={} — keeping stored value",
                path, new_offset, p
            );
        }
    }
    conn.execute(
        "INSERT INTO transcript_files (path, last_offset, last_distilled_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(path) DO UPDATE SET
             last_offset = MAX(transcript_files.last_offset, excluded.last_offset),
             last_distilled_at = excluded.last_distilled_at",
        rusqlite::params![path, new_offset],
    )?;
    Ok(())
}

/// Read the consecutive-failure strike counter for a file. Returns 0 when the
/// file row does not yet exist.
pub fn strikes(conn: &Connection, path: &str) -> anyhow::Result<u32> {
    let n: Option<i64> = conn
        .query_row(
            "SELECT consecutive_failures FROM transcript_files WHERE path=?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .optional()?;
    Ok(n.unwrap_or(0).max(0) as u32)
}

/// Upsert the strike counter without touching `last_offset`.
pub fn set_strikes(conn: &Connection, path: &str, n: u32) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO transcript_files (path, last_offset, consecutive_failures, last_distilled_at)
         VALUES (?1, 0, ?2, datetime('now'))
         ON CONFLICT(path) DO UPDATE SET consecutive_failures = excluded.consecutive_failures",
        rusqlite::params![path, n as i64],
    )?;
    Ok(())
}

/// The current watermark for a single file, or `None` when no row exists.
/// Unlike `last_offset` (which folds "missing" into 0), this distinguishes a
/// genuinely-absent path from a row parked at offset 0 — the distill-rewind
/// CLI needs that distinction to treat a mistargeted `--file` as a loud
/// zero-match error (S6) rather than a quiet no-op.
pub fn offset_of(conn: &Connection, path: &str) -> anyhow::Result<Option<i64>> {
    let offset: Option<i64> = conn
        .query_row(
            "SELECT last_offset FROM transcript_files WHERE path=?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .optional()?;
    Ok(offset)
}

/// Every transcript_files row as `(path, last_offset)`, path-ordered. Feeds the
/// `distill-rewind --all` preview so the CLI can print old->new per row and
/// detect the zero-row case loudly.
pub fn all_rows(conn: &Connection) -> anyhow::Result<Vec<(String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT path, last_offset FROM transcript_files ORDER BY path")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Rewind ONE file's watermark: regress `last_offset` back to 0 AND clear the
/// `consecutive_failures` strike counter, so the next quick tick reprocesses
/// the transcript from the top. Returns the number of rows affected (0 when the
/// path is not present — the caller treats that as a loud zero-match, never a
/// silent success).
///
/// Deliberate regression: `advance_offset`'s public contract is monotonic (its
/// `ON CONFLICT ... MAX()` clause structurally refuses to lower the watermark),
/// which is why rewind cannot go through it. This is a separate, explicit
/// UPDATE — the ONLY sanctioned way to move a watermark backwards — used by the
/// operator recovery path after a distill outage discarded slices. It is a
/// plain UPDATE, not an upsert: a missing path affects 0 rows and creates
/// nothing, so a mistargeted `--file` surfaces as a miss instead of a phantom
/// row parked at offset 0.
pub fn rewind_file(conn: &Connection, path: &str) -> anyhow::Result<usize> {
    let n = conn.execute(
        "UPDATE transcript_files SET last_offset = 0, consecutive_failures = 0 WHERE path = ?1",
        rusqlite::params![path],
    )?;
    Ok(n)
}

/// Rewind EVERY file's watermark (see `rewind_file` for the deliberate-regression
/// rationale). Returns the number of rows affected.
pub fn rewind_all(conn: &Connection) -> anyhow::Result<usize> {
    let n = conn.execute(
        "UPDATE transcript_files SET last_offset = 0, consecutive_failures = 0",
        [],
    )?;
    Ok(n)
}

/// Which watermark(s) `rewind` should target. `One` carries the exact
/// `transcript_files.path`; `All` means every row.
#[derive(Clone, Copy, Debug)]
pub enum RewindTarget<'a> {
    One(&'a str),
    All,
}

/// What `rewind` resolved and whether it mutated the DB. `rows` is the
/// pre-rewind `(path, last_offset)` for every targeted row (so the CLI can print
/// old->new); `applied` is false under `dry_run` (nothing was written).
#[derive(Debug)]
pub struct RewindPlan {
    pub rows: Vec<(String, i64)>,
    pub applied: bool,
}

/// Resolve and (unless `dry_run`) apply a distill-rewind. This is the testable
/// decision core the `hex memory distill-rewind` CLI is a thin printer over:
/// the zero-match S6 guard and the dry-run gate live HERE, not in the binary
/// crate's match arm, so they can be exercised against `fixture_conn`.
///
/// Zero matches is a loud `Err`, never an empty `Ok` — a mistargeted `--file`
/// (or `--all` on an empty corpus) must surface as a failure, not a quiet
/// success (S6). `dry_run` resolves and reports the same rows but writes
/// nothing, so an operator can preview the regression before committing it.
pub fn rewind(conn: &Connection, target: RewindTarget, dry_run: bool) -> anyhow::Result<RewindPlan> {
    let rows = match target {
        RewindTarget::One(path) => match offset_of(conn, path)? {
            Some(off) => vec![(path.to_string(), off)],
            None => Vec::new(),
        },
        RewindTarget::All => all_rows(conn)?,
    };
    if rows.is_empty() {
        // S6: loud, never a quiet success.
        let what = match target {
            RewindTarget::One(path) => {
                format!("no transcript_files row matches --file {path}")
            }
            RewindTarget::All => {
                "--all matched zero transcript_files rows (nothing to rewind)".to_string()
            }
        };
        anyhow::bail!("distill-rewind: {what}");
    }
    if !dry_run {
        match target {
            RewindTarget::One(path) => {
                rewind_file(conn, path)?;
            }
            RewindTarget::All => {
                rewind_all(conn)?;
            }
        }
    }
    Ok(RewindPlan {
        rows,
        applied: !dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture_conn() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        conn
    }

    #[test]
    fn defaults_to_zero_when_missing() {
        let conn = fixture_conn();
        let offset = last_offset(&conn, "/some/path.md").unwrap();
        assert_eq!(offset, 0);
    }

    #[test]
    fn advance_and_retrieve() {
        let conn = fixture_conn();
        advance_offset(&conn, "/some/path.md", 1024).unwrap();
        let offset = last_offset(&conn, "/some/path.md").unwrap();
        assert_eq!(offset, 1024);
    }

    #[test]
    fn advance_is_idempotent_upsert() {
        let conn = fixture_conn();
        advance_offset(&conn, "/some/path.md", 512).unwrap();
        advance_offset(&conn, "/some/path.md", 2048).unwrap();
        let offset = last_offset(&conn, "/some/path.md").unwrap();
        assert_eq!(offset, 2048);
    }

    #[test]
    fn advance_offset_is_monotonic() {
        let conn = fixture_conn();
        advance_offset(&conn, "/some/path.md", 2048).unwrap();
        // Attempt to regress.
        advance_offset(&conn, "/some/path.md", 100).unwrap();
        let offset = last_offset(&conn, "/some/path.md").unwrap();
        assert_eq!(offset, 2048, "monotonic guard must reject regression");
    }

    #[test]
    fn strikes_default_zero_and_set_get() {
        let conn = fixture_conn();
        assert_eq!(strikes(&conn, "/x.md").unwrap(), 0);
        set_strikes(&conn, "/x.md", 2).unwrap();
        assert_eq!(strikes(&conn, "/x.md").unwrap(), 2);
    }

    #[test]
    fn set_strikes_preserves_last_offset() {
        let conn = fixture_conn();
        advance_offset(&conn, "/x.md", 500).unwrap();
        set_strikes(&conn, "/x.md", 1).unwrap();
        assert_eq!(last_offset(&conn, "/x.md").unwrap(), 500);
        assert_eq!(strikes(&conn, "/x.md").unwrap(), 1);
    }

    // --- distill-rewind helpers (task Tmk5e03yr) -----------------------------
    // RED TESTS (written before implementation): these reference `rewind_file`
    // and `rewind_all`, which do not exist yet, so the crate fails to compile
    // until the helpers land — that compile failure is the red signal.
    //
    // They pin behavior `advance_offset` cannot express: a DELIBERATE regression
    // of last_offset back to 0 AND a reset of consecutive_failures to 0. The
    // seeded offsets are non-zero (2048/100/200) so any implementation built on
    // advance_offset's MAX() upsert would fail these — rewind must be a separate
    // UPDATE. Return values are intentionally discarded (`.unwrap()`/`let _ =`)
    // so the helper is free to return `()` or an affected-row count.

    #[test]
    fn rewind_file_resets_offset_and_strikes() {
        let conn = fixture_conn();
        advance_offset(&conn, "/x.md", 2048).unwrap();
        set_strikes(&conn, "/x.md", 3).unwrap();
        rewind_file(&conn, "/x.md").unwrap();
        assert_eq!(
            last_offset(&conn, "/x.md").unwrap(),
            0,
            "rewind must regress last_offset back to 0"
        );
        assert_eq!(
            strikes(&conn, "/x.md").unwrap(),
            0,
            "rewind must clear the consecutive-failure strike counter"
        );
    }

    #[test]
    fn rewind_all_resets_every_row() {
        let conn = fixture_conn();
        advance_offset(&conn, "/a.md", 100).unwrap();
        set_strikes(&conn, "/a.md", 2).unwrap();
        advance_offset(&conn, "/b.md", 200).unwrap();
        set_strikes(&conn, "/b.md", 1).unwrap();
        rewind_all(&conn).unwrap();
        for p in ["/a.md", "/b.md"] {
            assert_eq!(last_offset(&conn, p).unwrap(), 0, "rewind_all must zero {p}");
            assert_eq!(strikes(&conn, p).unwrap(), 0, "rewind_all must clear strikes for {p}");
        }
    }

    #[test]
    fn rewind_target_offset_of_distinguishes_missing_from_zero() {
        // The loud zero-match guard in the CLI rests on this: a row parked at
        // offset 0 must read as Some(0), NOT be folded into "missing". If it
        // were folded, `distill-rewind --file <valid-but-at-0>` would look like
        // a zero-match and error, and a truly-missing path could look present.
        let conn = fixture_conn();
        set_strikes(&conn, "/z.md", 0).unwrap(); // seeds a row with last_offset=0
        assert_eq!(
            offset_of(&conn, "/z.md").unwrap(),
            Some(0),
            "an existing row at offset 0 must read as Some(0)"
        );
        assert_eq!(
            offset_of(&conn, "/never-seen.md").unwrap(),
            None,
            "an absent path must read as None, not Some(0)"
        );
    }

    #[test]
    fn rewind_all_rows_lists_every_row_path_ordered() {
        let conn = fixture_conn();
        advance_offset(&conn, "/b.md", 200).unwrap();
        advance_offset(&conn, "/a.md", 100).unwrap();
        let rows = all_rows(&conn).unwrap();
        assert_eq!(
            rows,
            vec![("/a.md".to_string(), 100), ("/b.md".to_string(), 200)],
            "all_rows must return every (path, last_offset) path-ordered"
        );
    }

    #[test]
    fn rewind_dry_run_reports_without_mutating() {
        // --dry-run must resolve and report the target rows but write NOTHING:
        // the watermark and strikes stay put so an operator can preview first.
        let conn = fixture_conn();
        advance_offset(&conn, "/x.md", 2048).unwrap();
        set_strikes(&conn, "/x.md", 3).unwrap();
        let plan = rewind(&conn, RewindTarget::One("/x.md"), true).unwrap();
        assert_eq!(
            plan.rows,
            vec![("/x.md".to_string(), 2048)],
            "dry-run must still report the old offset"
        );
        assert!(!plan.applied, "dry-run must not mutate (applied=false)");
        assert_eq!(
            last_offset(&conn, "/x.md").unwrap(),
            2048,
            "dry-run must leave last_offset untouched"
        );
        assert_eq!(
            strikes(&conn, "/x.md").unwrap(),
            3,
            "dry-run must leave strikes untouched"
        );
    }

    #[test]
    fn rewind_applies_and_resets_when_not_dry_run() {
        let conn = fixture_conn();
        advance_offset(&conn, "/x.md", 2048).unwrap();
        set_strikes(&conn, "/x.md", 3).unwrap();
        let plan = rewind(&conn, RewindTarget::One("/x.md"), false).unwrap();
        assert!(plan.applied, "a real rewind must report applied=true");
        assert_eq!(last_offset(&conn, "/x.md").unwrap(), 0);
        assert_eq!(strikes(&conn, "/x.md").unwrap(), 0);
    }

    #[test]
    fn rewind_zero_match_file_is_loud_error() {
        // S6: a mistargeted --file must fail loudly, never quietly succeed.
        let conn = fixture_conn();
        assert!(
            rewind(&conn, RewindTarget::One("/never-seen.md"), false).is_err(),
            "zero-match --file must be a loud error"
        );
    }

    #[test]
    fn rewind_zero_match_all_is_loud_error() {
        // S6: --all on an empty corpus is a loud error, not a silent no-op.
        let conn = fixture_conn();
        assert!(
            rewind(&conn, RewindTarget::All, false).is_err(),
            "zero-match --all must be a loud error"
        );
    }

    #[test]
    fn rewind_file_missing_row_is_not_a_silent_upsert() {
        let conn = fixture_conn();
        // Zero-match must NOT silently create a fresh row at offset 0 — that
        // would mask a mistargeted --file path (S6: no quiet success). The
        // helper is a deliberate UPDATE, not an upsert; whether it signals the
        // miss via Err or a 0-row count is left to the implementation, so the
        // return value is discarded here.
        let _ = rewind_file(&conn, "/never-seen.md");
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transcript_files WHERE path=?1",
                rusqlite::params!["/never-seen.md"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0, "rewind of a missing path must not create a row");
    }
}
