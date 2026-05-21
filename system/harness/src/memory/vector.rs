//! sqlite-vec integration: extension registration, the `vec_chunks` vec0
//! table, and vector insert / delete / KNN.

use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::Connection;
use sqlite_vec::sqlite3_vec_init;
use std::sync::Once;

/// nomic-embed-text-v1.5 native output dimension (verified by the §16 spike).
pub const EMBED_DIM: usize = 768;

static VEC_INIT: Once = Once::new();

/// Register sqlite-vec as a SQLite auto-extension. Process-global and
/// idempotent: every `Connection` opened afterwards has vec0 available.
pub fn register_sqlite_vec() {
    VEC_INIT.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite3_vec_init as *const (),
        )));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_vec_loads() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        let ver: String = conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))
            .expect("vec_version() must work once sqlite-vec is registered");
        assert!(ver.starts_with('v'), "unexpected vec_version: {ver}");
    }
}
