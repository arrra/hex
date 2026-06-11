pub mod claude_cli;
pub mod consolidate;
pub mod distill;
pub mod embed;
pub mod index;
pub mod maintain;
pub mod maintain_facts;
pub mod parse_transcripts;
pub mod predicates;
pub mod provider;
pub mod recent;
pub mod assemble;
pub mod recall;
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
    // Best-effort migration — log but don't fail if a DDL piece is unhappy
    // (e.g. older sqlite-vec without FLOAT[768]); the facts CLI commands will
    // surface a clearer error.
    if let Err(e) = schema::apply_plan2(&conn) {
        eprintln!("[memory] Plan 2 schema migration warning: {e}");
    }
    if let Err(e) = schema::apply_messages_schema(&conn) {
        eprintln!("[memory] messages schema migration warning: {e}");
    }
    Ok(conn)
}
