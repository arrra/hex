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
}
