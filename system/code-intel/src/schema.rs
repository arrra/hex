//! Generation SQLite schema (spec §4, verbatim DDL).
//!
//! The index is a write-once artifact: created empty, populated by
//! [`crate::ingest`], then published read-only. `symbols_fts` is an external
//! content FTS5 table kept in sync manually during ingest (no triggers — the
//! index is never mutated after publish).

use rusqlite::Connection;

/// DDL exactly as specified in `docs/code-intel/SPEC-A1.md` §4.
const DDL: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE files (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  blob_oid TEXT NOT NULL,
  language TEXT NOT NULL
);

CREATE TABLE symbols (
  id INTEGER PRIMARY KEY,
  scip_symbol TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  kind INTEGER NOT NULL DEFAULT 0,
  documentation TEXT
);

CREATE TABLE occurrences (
  file_id INTEGER NOT NULL REFERENCES files(id),
  symbol_id INTEGER NOT NULL REFERENCES symbols(id),
  start_line INTEGER NOT NULL,
  start_col INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  end_col INTEGER NOT NULL,
  roles INTEGER NOT NULL,
  enclosing_symbol_id INTEGER REFERENCES symbols(id)
);
CREATE INDEX idx_occ_symbol ON occurrences(symbol_id, roles);
CREATE INDEX idx_occ_file_pos ON occurrences(file_id, start_line);

CREATE VIRTUAL TABLE symbols_fts USING fts5(display_name, content='symbols', content_rowid='id');
";

/// Create all tables and indexes for a fresh generation database.
pub fn create(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(DDL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_all_tables() {
        let conn = Connection::open_in_memory().unwrap();
        create(&conn).unwrap();
        for table in ["meta", "files", "symbols", "occurrences", "symbols_fts"] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {table}");
        }
    }

    #[test]
    fn schema_create_is_not_idempotent_by_design() {
        // Generations are write-once: creating twice on the same DB is a bug
        // upstream and must fail loudly, not silently no-op.
        let conn = Connection::open_in_memory().unwrap();
        create(&conn).unwrap();
        assert!(create(&conn).is_err());
    }
}
