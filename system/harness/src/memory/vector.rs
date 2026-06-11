//! sqlite-vec integration: extension registration, the `vec_chunks` vec0
//! table, and vector insert / delete / KNN.

use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::{params, Connection};
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

/// Pack f32s as little-endian bytes — the compact BLOB form sqlite-vec wants.
pub fn f32s_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Create the `vec_chunks` vec0 table. Each row's rowid mirrors the
/// corresponding `chunks` FTS5 rowid — that is the join key.
pub fn init_vec_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(
            embedding float[{EMBED_DIM}]
        );"
    ))
}

pub fn insert_vec(conn: &Connection, rowid: i64, embedding: &[f32]) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
        params![rowid, f32s_to_le_bytes(embedding)],
    )?;
    Ok(())
}

/// Insert a fact embedding into `facts_vec` (vec0: `fact_id TEXT PRIMARY KEY,
/// embedding FLOAT[768]`, schema.rs). Same blob serialization as [`insert_vec`].
pub fn insert_fact_vec(conn: &Connection, fact_id: &str, vec: &[f32]) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO facts_vec(fact_id, embedding) VALUES (?1, ?2)",
        params![fact_id, f32s_to_le_bytes(vec)],
    )?;
    Ok(())
}

/// Delete vec rows by rowid, in batches (mirrors `delete_chunks_for_file`).
pub fn delete_vecs(conn: &Connection, rowids: &[i64]) -> rusqlite::Result<()> {
    for batch in rowids.chunks(500) {
        let ph: String = batch.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut stmt = conn.prepare(&format!("DELETE FROM vec_chunks WHERE rowid IN ({ph})"))?;
        for (i, id) in batch.iter().enumerate() {
            stmt.raw_bind_parameter(i + 1, *id)?;
        }
        stmt.raw_execute()?;
    }
    Ok(())
}

/// vec0 FLOAT[768] MATCH distance is L2; fastembed nomic vectors are
/// normalized, so d² = 2(1-cos): d=1.0 ≈ cos 0.5, d=1.15 ≈ cos 0.34.
/// Beyond 1.15 a "neighbor" shares almost nothing with the query — garbage
/// and empty-ish queries previously returned confident top-k (assessment
/// finding: no relevance floor). Tune with HEX_KNN_MAX_DISTANCE if needed.
pub const KNN_MAX_DISTANCE: f64 = 1.15;

pub fn filter_by_distance(hits: Vec<(i64, f64)>, max: f64) -> Vec<(i64, f64)> {
    hits.into_iter().filter(|(_, d)| *d <= max).collect()
}

fn max_distance() -> f64 {
    std::env::var("HEX_KNN_MAX_DISTANCE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(KNN_MAX_DISTANCE)
}

/// K-nearest-neighbour search. Returns (chunk_rowid, distance), nearest first.
/// Hits beyond the relevance floor (see [`KNN_MAX_DISTANCE`]) are dropped so
/// every caller gets the floor.
pub fn knn(conn: &Connection, query: &[f32], k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT rowid, distance FROM vec_chunks \
         WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![f32s_to_le_bytes(query), k as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    let hits: Vec<(i64, f64)> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(filter_by_distance(hits, max_distance()))
}

/// K-nearest-neighbour search over fact embeddings — mirrors [`knn`] against
/// `facts_vec`. `facts_vec` keys rows by the fact's TEXT ULID id (NOT an
/// integer — it cannot be parsed as i64), so hits join back to `facts` to
/// return the integer rowid: the RRF fusion key shared with the facts_fts
/// arm. Tombstoned facts are excluded (their vectors are swept weekly, not
/// live). Same relevance floor as [`knn`].
pub fn knn_facts(conn: &Connection, query: &[f32], k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT f.rowid, v.distance
           FROM (SELECT fact_id, distance FROM facts_vec
                  WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2) v
           JOIN facts f ON f.id = v.fact_id
          WHERE f.tombstone = 0
          ORDER BY v.distance",
    )?;
    let rows = stmt.query_map(params![f32s_to_le_bytes(query), k as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    let hits: Vec<(i64, f64)> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(filter_by_distance(hits, max_distance()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_floor_filters() {
        let hits = vec![(1i64, 0.4f64), (2, 0.9), (3, 1.4)];
        assert_eq!(filter_by_distance(hits, KNN_MAX_DISTANCE), vec![(1, 0.4), (2, 0.9)]);
    }

    #[test]
    fn sqlite_vec_loads() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        let ver: String = conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))
            .expect("vec_version() must work once sqlite-vec is registered");
        assert!(ver.starts_with('v'), "unexpected vec_version: {ver}");
    }

    #[test]
    fn vec_table_insert_and_knn() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_vec_table(&conn).unwrap();

        for id in 1..=5i64 {
            let v: Vec<f32> = (0..EMBED_DIM).map(|i| (id as f32 + i as f32) * 0.001).collect();
            insert_vec(&conn, id, &v).unwrap();
        }
        let query: Vec<f32> = (0..EMBED_DIM).map(|i| (3.0 + i as f32) * 0.001).collect();
        let hits = knn(&conn, &query, 3).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].0, 3, "row 3 is its own nearest neighbour");

        delete_vecs(&conn, &[3]).unwrap();
        let hits = knn(&conn, &query, 3).unwrap();
        assert!(!hits.iter().any(|(id, _)| *id == 3), "row 3 was deleted");
    }

    #[test]
    fn fact_vec_insert_and_knn_facts_joins_to_rowid() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();

        // ULID-style TEXT ids — deliberately NOT parseable as integers.
        for (i, id) in ["01HFACT-A", "01HFACT-B", "01HFACT-C"].iter().enumerate() {
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
                 VALUES (?1,'project:hex','uses',?2,0.5,'2026-06-11','2026-06-11')",
                params![id, format!("object {i}")],
            )
            .unwrap();
            let v: Vec<f32> = (0..EMBED_DIM).map(|d| (i as f32 + d as f32) * 0.001).collect();
            insert_fact_vec(&conn, id, &v).unwrap();
        }
        let query: Vec<f32> = (0..EMBED_DIM).map(|d| (1.0 + d as f32) * 0.001).collect();
        let hits = knn_facts(&conn, &query, 3).unwrap();
        assert!(!hits.is_empty(), "knn_facts must return neighbours");
        // Nearest is fact B (index 1); knn_facts returns its facts.rowid.
        let nearest_rowid = hits[0].0;
        let nearest_id: String = conn
            .query_row("SELECT id FROM facts WHERE rowid = ?1", [nearest_rowid], |r| r.get(0))
            .unwrap();
        assert_eq!(nearest_id, "01HFACT-B", "join must map fact_id back to the facts rowid");

        // Tombstoned facts drop out of the KNN arm.
        conn.execute("UPDATE facts SET tombstone = 1 WHERE id = '01HFACT-B'", [])
            .unwrap();
        let hits = knn_facts(&conn, &query, 3).unwrap();
        assert!(
            !hits.iter().any(|(rowid, _)| *rowid == nearest_rowid),
            "tombstoned fact must be excluded"
        );
    }
}
