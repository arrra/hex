pub mod index;
pub mod parse_transcripts;
pub mod search;

use std::path::{Path, PathBuf};

pub fn db_path(hex_root: &Path) -> PathBuf {
    hex_root.join(".hex/memory.db")
}
