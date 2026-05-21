pub mod embed;
pub mod index;
pub mod parse_transcripts;
pub mod search;
pub mod vector;

use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub fn db_path(hex_root: &Path) -> PathBuf {
    hex_root.join(".hex/memory.db")
}

/// Open the memory DB with sqlite-vec registered. ALL memory code must open
/// connections through this — `Connection::open` directly would miss vec0.
pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    vector::register_sqlite_vec();
    Connection::open(path)
}
