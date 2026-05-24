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

pub fn advance_offset(conn: &Connection, path: &str, new_offset: i64) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO transcript_files (path, last_offset, last_distilled_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(path) DO UPDATE SET last_offset=excluded.last_offset,
                                         last_distilled_at=excluded.last_distilled_at",
        rusqlite::params![path, new_offset],
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
}
