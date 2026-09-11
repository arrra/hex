//! sqlite-vec integration: extension registration, the `vec_chunks` vec0
//! table, and vector insert / delete / KNN.

use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::{params, Connection, OptionalExtension};
use sqlite_vec::sqlite3_vec_init;
use std::os::raw::{c_char, c_int};
use std::sync::Once;

/// nomic-embed-text-v1.5 native output dimension (verified by the §16 spike).
pub const EMBED_DIM: usize = 768;

static VEC_INIT: Once = Once::new();

/// Register sqlite-vec as a SQLite auto-extension. Process-global and
/// idempotent: every `Connection` opened afterwards has vec0 available.
pub fn register_sqlite_vec() {
    VEC_INIT.call_once(|| unsafe {
        // Explicit transmute annotations (clippy::missing_transmute_annotations):
        // reinterpret the extension entry point as the C ABI fn pointer that
        // `sqlite3_auto_extension` expects. The error-message arg must be
        // `*mut *const c_char`, not a hardcoded i8: c_char is u8 on aarch64
        // Linux (the docker-e2e container) and i8 on Apple targets, so i8
        // compiles on the host but fails E0308 in the container.
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *const c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> c_int,
        >(sqlite3_vec_init as *const ())));
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

/// Insert (or replace) the vector for `rowid`. A vec0 INSERT on an existing
/// rowid ERRORS (UNIQUE primary key) rather than replacing, so we DELETE any
/// prior row first. This makes the chunk↔vector binding self-correcting: if a
/// stale orphan vector ever occupies this rowid — legacy pre-sweep residue, or
/// vec0 rowid reuse after a future global `chunks` rebuild — the chunk binds to
/// its OWN fresh embedding instead of silently retaining the unrelated stale
/// vector. That stale-bind is invisible to BOTH the `maintain` orphan sweep and
/// `backfill_missing_vectors` (each keys on rowid presence, not content), so it
/// would be a permanent, self-healing-proof retrieval corruption. The DELETE is
/// almost always a no-op (the rowid is absent in normal insert/backfill flow).
pub fn insert_vec(conn: &Connection, rowid: i64, embedding: &[f32]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM vec_chunks WHERE rowid = ?1", params![rowid])?;
    conn.execute(
        "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
        params![rowid, f32s_to_le_bytes(embedding)],
    )?;
    Ok(())
}

/// Insert a fact embedding into `facts_vec` (vec0: `fact_id TEXT PRIMARY KEY,
/// embedding FLOAT[768], is_live BOOLEAN`, schema.rs). Same blob serialization
/// as [`insert_vec`]. DELETE-before-insert for the same self-correcting reason
/// as [`insert_vec`]: a vec0 INSERT on an existing key ERRORs rather than
/// replacing. Currently a no-op for fresh facts (facts are insert-once —
/// backfill selects only `id NOT IN facts_vec`), but symmetry keeps a future
/// fact re-embed from silently retaining a stale vector.
///
/// `is_live` is looked up from `facts.invalid_at` AND `facts.tombstone` at
/// insert time (decision `hex-knn-is-live-metadata-filter-2026-09-10.md` §3,
/// widened by PR#9 r2 review_b G1 to also cover tombstoning: synced in CODE,
/// never triggers — a `facts` trigger corrupted the FTS5 external-content
/// shadow tables in v1's F3). A fact with no matching `facts` row yet
/// (embedded before its row is committed) defaults to live. [`knn_facts`]
/// filters on this column inside the vec0 KNN query itself. Every code path
/// that sets `facts.invalid_at` / `superseded_by` (supersede-not-overwrite)
/// OR `facts.tombstone = 1` MUST call [`mark_fact_vec_dead`] in the same
/// transaction to keep this column in sync — see that function's doc
/// comment.
pub fn insert_fact_vec(conn: &Connection, fact_id: &str, vec: &[f32]) -> rusqlite::Result<()> {
    let is_live: i64 = conn
        .query_row(
            "SELECT CASE WHEN invalid_at IS NULL AND tombstone = 0 THEN 1 ELSE 0 END \
             FROM facts WHERE id = ?1",
            params![fact_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(1);
    conn.execute("DELETE FROM facts_vec WHERE fact_id = ?1", params![fact_id])?;
    conn.execute(
        "INSERT INTO facts_vec(fact_id, embedding, is_live) VALUES (?1, ?2, ?3)",
        params![fact_id, f32s_to_le_bytes(vec), is_live],
    )?;
    Ok(())
}

/// Flip a fact's `facts_vec` row to non-live. Decision
/// `hex-knn-is-live-metadata-filter-2026-09-10.md` §3 (widened by PR#9 r2
/// review_b G1): every code path that sets `facts.invalid_at` /
/// `superseded_by` (supersede-not-overwrite) OR `facts.tombstone = 1` MUST
/// call this in the SAME transaction as that UPDATE, so `knn_facts`'s
/// `is_live = 1` filter never drifts from either column. No `facts` trigger
/// may do this instead — the v1 F3 valid_from trigger corrupted the FTS5
/// external-content shadow tables ("database disk image is malformed").
/// Named generically (`_dead`, not `_superseded`) because it now serves both
/// the supersede path and the tombstone path (`memory/consolidate.rs`'s
/// `tombstone_duplicate_fact`, the one production writer today).
pub fn mark_fact_vec_dead(conn: &Connection, fact_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE facts_vec SET is_live = 0 WHERE fact_id = ?1",
        params![fact_id],
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
/// arm. Same relevance floor as [`knn`].
///
/// Superseded AND tombstoned facts are excluded by the `is_live = 1`
/// metadata constraint evaluated INSIDE the vec0 KNN query itself (decision
/// `hex-knn-is-live-metadata-filter-2026-09-10.md`, closing PR#9 R2 G1 and,
/// for the tombstone axis, PR#9 r2 review_b G1): `facts_vec`'s `is_live`
/// column (schema.rs) is kept in sync with `facts.invalid_at` AND
/// `facts.tombstone` in code, not by a trigger (see [`mark_fact_vec_dead`]).
/// This replaces the PR#9 r1 F2 / R2 F5 adaptive-overfetch-and-clamp design,
/// which fetched extra rows past `k` before join-filtering and
/// widened/clamped that window to stay under sqlite-vec's `VEC0_K_MAX` hard
/// cap on a vec0 KNN `LIMIT` (4096): a fixed overfetch window can never see
/// past a wall of MORE than 4096 dead neighbors ranked nearer than the
/// query's live matches, no matter how far it widens. Filtering `is_live`
/// inside the vec0 MATCH itself has no such wall — `k` passes straight
/// through with no overfetch or clamp.
///
/// The outer `f.tombstone = 0` join is belt-and-braces, not load-bearing:
/// `is_live` already folds in `tombstone`, so this only matters for a row
/// tombstoned between a stale `facts_vec` write and a periodic
/// `maintain_facts::backfill` sweep — the same narrow race the supersede
/// path already tolerates.
pub fn knn_facts(conn: &Connection, query: &[f32], k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT f.rowid, v.distance
           FROM (SELECT fact_id, distance FROM facts_vec
                  WHERE embedding MATCH ?1 AND k = ?2 AND is_live = 1
                  ORDER BY distance) v
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
        assert_eq!(
            filter_by_distance(hits, KNN_MAX_DISTANCE),
            vec![(1, 0.4), (2, 0.9)]
        );
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
            let v: Vec<f32> = (0..EMBED_DIM)
                .map(|i| (id as f32 + i as f32) * 0.001)
                .collect();
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
    fn insert_vec_replaces_stale_vector_at_reused_rowid() {
        // Orphan-collision guard (adversarial review 2026-06-13): a vec0 INSERT
        // on an existing rowid ERRORS instead of replacing, so a chunk whose
        // rowid collides with a pre-existing orphan vector must still bind to
        // its OWN embedding. insert_vec DELETEs any stale row first.
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_vec_table(&conn).unwrap();

        let stale = vec![0.9f32; EMBED_DIM];
        let fresh = vec![0.1f32; EMBED_DIM];
        insert_vec(&conn, 42, &stale).unwrap();
        // Re-insert at the SAME rowid (the collision case): must not error and
        // must overwrite, leaving exactly one row bound to the fresh vector.
        insert_vec(&conn, 42, &fresh).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vec_chunks WHERE rowid = 42",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "exactly one vector at the rowid — no duplicate, no error"
        );

        // The stored vector is the FRESH one, not the stale one.
        let blob: Vec<u8> = conn
            .query_row(
                "SELECT embedding FROM vec_chunks WHERE rowid = 42",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let first = f32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
        assert!(
            (first - 0.1).abs() < 1e-6,
            "expected the fresh 0.1 vector, got {first} (stale vector silently retained!)"
        );
    }

    #[test]
    fn fact_vec_insert_and_knn_facts_joins_to_rowid() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        // ULID-style TEXT ids — deliberately NOT parseable as integers.
        for (i, id) in ["01HFACT-A", "01HFACT-B", "01HFACT-C"].iter().enumerate() {
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
                 VALUES (?1,'project:hex','uses',?2,0.5,'2026-06-11','2026-06-11')",
                params![id, format!("object {i}")],
            )
            .unwrap();
            let v: Vec<f32> = (0..EMBED_DIM)
                .map(|d| (i as f32 + d as f32) * 0.001)
                .collect();
            insert_fact_vec(&conn, id, &v).unwrap();
        }
        let query: Vec<f32> = (0..EMBED_DIM).map(|d| (1.0 + d as f32) * 0.001).collect();
        let hits = knn_facts(&conn, &query, 3).unwrap();
        assert!(!hits.is_empty(), "knn_facts must return neighbours");
        // Nearest is fact B (index 1); knn_facts returns its facts.rowid.
        let nearest_rowid = hits[0].0;
        let nearest_id: String = conn
            .query_row(
                "SELECT id FROM facts WHERE rowid = ?1",
                [nearest_rowid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            nearest_id, "01HFACT-B",
            "join must map fact_id back to the facts rowid"
        );

        // Tombstoned facts drop out of the KNN arm.
        conn.execute("UPDATE facts SET tombstone = 1 WHERE id = '01HFACT-B'", [])
            .unwrap();
        let hits = knn_facts(&conn, &query, 3).unwrap();
        assert!(
            !hits.iter().any(|(rowid, _)| *rowid == nearest_rowid),
            "tombstoned fact must be excluded"
        );
    }

    /// RED for FIX item 4 (Twbqe1c12) — a superseded row (`invalid_at` set,
    /// `superseded_by` pointing at its replacement) is never deleted or
    /// tombstoned, so it must be excluded from `knn_facts` by its OWN
    /// column, not by piggybacking on the tombstone check.
    #[test]
    fn knn_facts_excludes_superseded() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by)
             VALUES ('01HFACT-SUP','project:hex','uses','old superseded object',0.5,'2026-06-11','2026-06-11','2026-06-11','2026-09-05','01HFACT-NEW')",
            [],
        )
        .unwrap();
        let v: Vec<f32> = (0..EMBED_DIM).map(|d| d as f32 * 0.001).collect();
        insert_fact_vec(&conn, "01HFACT-SUP", &v).unwrap();

        let hits = knn_facts(&conn, &v, 3).unwrap();
        assert!(
            hits.is_empty(),
            "superseded fact must be excluded from the KNN join, got {:?}",
            hits
        );
    }

    /// Regression pin for PR#9 r1 F2 (major), now closed by G1's
    /// metadata-filtered KNN: `knn_facts` filters `is_live = 1` INSIDE the
    /// vec0 MATCH itself (see [`knn_facts`]'s doc comment), so a live fact
    /// is found regardless of how many superseded neighbors rank nearer —
    /// there is no overfetch window past which they could hide it. 20 stale
    /// facts is a small case; [`knn_facts_returns_live_neighbor_behind_a_wall_exceeding_the_vec0_k_cap`]
    /// below pins the same contract at a much larger scale (F2's original
    /// fixed-overfetch-window design could pass this smaller case but not
    /// that one).
    #[test]
    fn knn_facts_returns_live_neighbor_behind_a_wall_of_superseded_ones() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        // 20 superseded facts, each strictly nearer to the query than the
        // live fact below — more than the k=1 overfetch window of 17.
        for i in 0..20 {
            let id = format!("01HFACT-STALE-{i:02}");
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by)
                 VALUES (?1,'project:hex','uses','stale object',0.5,'2026-06-11','2026-06-11','2026-06-11','2026-09-05','01HFACT-NEW')",
                params![id],
            )
            .unwrap();
            let v: Vec<f32> = (0..EMBED_DIM)
                .map(|d| (i as f32 + d as f32) * 0.0001)
                .collect();
            insert_fact_vec(&conn, &id, &v).unwrap();
        }

        // The one live, eligible fact — farther from the query than all 20
        // stale facts above, so it ranks 21st by distance.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('01HFACT-LIVE','project:hex','uses','live object',0.5,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        let live_v: Vec<f32> = (0..EMBED_DIM).map(|d| (20.0 + d as f32) * 0.0001).collect();
        insert_fact_vec(&conn, "01HFACT-LIVE", &live_v).unwrap();

        let query: Vec<f32> = (0..EMBED_DIM).map(|d| d as f32 * 0.0001).collect();
        let hits = knn_facts(&conn, &query, 1).unwrap();

        let found_live = hits.iter().any(|(rowid, _)| {
            let id: String = conn
                .query_row("SELECT id FROM facts WHERE rowid = ?1", [*rowid], |r| {
                    r.get(0)
                })
                .unwrap();
            id == "01HFACT-LIVE"
        });
        assert!(
            found_live,
            "the live fact must be returned even though 20 superseded facts (more than the k=1 overfetch window of 17) rank nearer — got {:?}",
            hits
        );
    }

    /// Regression pin for R2 review F5 (major, regression introduced by the
    /// original F2 overfetch fix): F5 clamped the overfetch window to stay
    /// under sqlite-vec's hard cap on a vec0 KNN `LIMIT`
    /// (`SQLITE_VEC_VEC0_K_MAX = 4096`, sqlite-vec.c:7111) instead of
    /// erroring past it. G1's metadata-filtered KNN (see [`knn_facts`])
    /// removes the overfetch window entirely — `k` passes straight through
    /// with no clamp needed — so this table of 4097 all-superseded vectors
    /// with `k = 1` exercises the same "no live match anywhere" case purely
    /// through the `is_live = 1` filter: it must return an empty result,
    /// not an error, with no window-widening logic left to clamp.
    #[test]
    fn knn_facts_returns_empty_when_every_candidate_up_to_the_vec0_k_cap_is_superseded() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        // 4097 superseded facts — one more than sqlite-vec's vec0 KNN LIMIT
        // cap of 4096, and no live fact at all: the is_live = 1 filter
        // inside the vec0 MATCH itself must exclude every one of them
        // without erroring past the cap.
        for i in 0..4097 {
            let id = format!("01HFACT-WALL-{i:04}");
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by)
                 VALUES (?1,'project:hex','uses','stale object',0.5,'2026-06-11','2026-06-11','2026-06-11','2026-09-05','01HFACT-NEW')",
                params![id],
            )
            .unwrap();
            let v: Vec<f32> = (0..EMBED_DIM)
                .map(|d| (i as f32 + d as f32) * 0.0001)
                .collect();
            insert_fact_vec(&conn, &id, &v).unwrap();
        }

        let query: Vec<f32> = (0..EMBED_DIM).map(|d| d as f32 * 0.0001).collect();
        let hits = knn_facts(&conn, &query, 1);

        assert!(
            hits.is_ok(),
            "knn_facts must filter is_live = 0 rows inside the vec0 MATCH itself, up to and past the 4096 KNN cap, without erroring, got {:?}",
            hits
        );
        assert!(
            hits.unwrap().is_empty(),
            "with no live facts at all, the metadata-filtered result must be empty, not partial"
        );
    }

    /// RED for G1 (major, Codex R2) / decision
    /// `hex-knn-is-live-metadata-filter-2026-09-10.md`: the real bug the
    /// F2/F5 adaptive-overfetch-and-clamp design cannot close. sqlite-vec
    /// hard-caps a vec0 KNN `LIMIT` at `VEC0_K_MAX` (4096, "R2 review F5"
    /// above), so once exactly 4096 superseded facts all rank nearer to the
    /// query than the one live, eligible fact, the overfetch window clamps
    /// at 4096 and the live fact — ranked 4097th — can never enter the
    /// subquery's candidate window no matter how far the widening loop
    /// runs. `knn_facts` returns empty instead of the live fact.
    ///
    /// (The decision doc's illustrative wall of N=64 is deliberately NOT
    /// used here: at N=64 the current adaptive-overfetch loop already
    /// widens past the whole candidate set and succeeds today — it is only
    /// once N reaches sqlite-vec's own hard k-cap that the design
    /// structurally cannot see past the wall, which is exactly Codex R2's
    /// finding. Only a metadata-filtered KNN — `is_live = 1` evaluated
    /// INSIDE the vec0 MATCH, not a post-hoc join over an overfetched
    /// window — closes this.)
    #[test]
    fn knn_facts_returns_live_neighbor_behind_a_wall_exceeding_the_vec0_k_cap() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        // 4096 superseded facts — sqlite-vec's own vec0 KNN LIMIT cap —
        // each strictly nearer to the query than the live fact below. Scale
        // is 1e-6 (not the 1e-4 other fixtures in this module use): at 4096
        // facts the per-dimension offset needed to keep every stale point
        // strictly nearer than the live one (offset = fact index, up to
        // 4095) pushes the live point's raw L2 distance
        // (sqrt(EMBED_DIM) * 4096 * scale) past KNN_MAX_DISTANCE at the
        // larger scale — that's a fixture-construction ceiling on the
        // relevance floor, not the bug under test, so it must stay clear of
        // it here.
        const SCALE: f32 = 0.000001;
        for i in 0..4096 {
            let id = format!("01HFACT-WALL-{i:04}");
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,valid_from,invalid_at,superseded_by)
                 VALUES (?1,'project:hex','uses','stale object',0.5,'2026-06-11','2026-06-11','2026-06-11','2026-09-05','01HFACT-NEW')",
                params![id],
            )
            .unwrap();
            let v: Vec<f32> = (0..EMBED_DIM)
                .map(|d| (i as f32 + d as f32) * SCALE)
                .collect();
            insert_fact_vec(&conn, &id, &v).unwrap();
        }

        // The one live, eligible fact — farther from the query than all
        // 4096 stale facts above, so it ranks 4097th by distance.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('01HFACT-LIVE','project:hex','uses','live object',0.5,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        let live_v: Vec<f32> = (0..EMBED_DIM)
            .map(|d| (4096.0 + d as f32) * SCALE)
            .collect();
        insert_fact_vec(&conn, "01HFACT-LIVE", &live_v).unwrap();

        let query: Vec<f32> = (0..EMBED_DIM).map(|d| d as f32 * SCALE).collect();

        let hits = knn_facts(&conn, &query, 1).unwrap();

        let found_live = hits.iter().any(|(rowid, _)| {
            let id: String = conn
                .query_row("SELECT id FROM facts WHERE rowid = ?1", [*rowid], |r| {
                    r.get(0)
                })
                .unwrap();
            id == "01HFACT-LIVE"
        });
        assert!(
            found_live,
            "the live fact must be returned even though 4096 superseded facts (sqlite-vec's own vec0 KNN cap) rank nearer — a fixed overfetch window structurally cannot see past this wall; only a metadata-filtered KNN can, got {:?}",
            hits
        );
    }

    /// RED for G1 item 3 (decision doc §3, "sync in CODE, never triggers"):
    /// the `facts_vec` insert path must write `is_live = (invalid_at IS
    /// NULL)` looked up from `facts` at insert time, and `knn_facts` must
    /// filter on that column. Fails now on two counts: `facts_vec` has no
    /// `is_live` column at all (the raw SELECT below errors), and
    /// `insert_fact_vec` never looks at `facts.invalid_at`.
    #[test]
    fn insert_fact_vec_writes_is_live_from_facts_invalid_at() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('01HFACT-LIVE','project:hex','uses','a live object',0.5,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        let v: Vec<f32> = (0..EMBED_DIM).map(|d| d as f32 * 0.001).collect();
        insert_fact_vec(&conn, "01HFACT-LIVE", &v).unwrap();

        let is_live: i64 = conn
            .query_row(
                "SELECT is_live FROM facts_vec WHERE fact_id = '01HFACT-LIVE'",
                [],
                |r| r.get(0),
            )
            .expect(
                "facts_vec must carry an is_live metadata column and insert_fact_vec must populate it from facts.invalid_at",
            );
        assert_eq!(
            is_live, 1,
            "a fresh live fact's facts_vec row must have is_live = 1"
        );

        // Supersede it directly on `facts` and re-embed: the insert path
        // must recompute is_live from the CURRENT invalid_at, not default
        // to always-live.
        conn.execute(
            "UPDATE facts SET invalid_at = '2026-09-05', superseded_by = '01HFACT-NEW' WHERE id = '01HFACT-LIVE'",
            [],
        )
        .unwrap();
        insert_fact_vec(&conn, "01HFACT-LIVE", &v).unwrap();
        let is_live_after: i64 = conn
            .query_row(
                "SELECT is_live FROM facts_vec WHERE fact_id = '01HFACT-LIVE'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            is_live_after, 0,
            "re-embedding a now-superseded fact must write is_live = 0"
        );

        let hits = knn_facts(&conn, &v, 1).unwrap();
        assert!(
            hits.is_empty(),
            "knn_facts must filter is_live = 0 rows out of the metadata-constrained KNN, got {:?}",
            hits
        );
    }

    /// RED for the R1 review redo (`review-redo`, F2 major): `knn_facts`'s
    /// doc comment says every supersede-not-overwrite writer MUST call
    /// [`mark_fact_vec_dead`] in the same transaction as the
    /// `facts.invalid_at`/`superseded_by` UPDATE, but the helper itself had
    /// zero coverage — the only path pinning "invalidating a fact flips
    /// is_live" was the unrelated re-embed path in
    /// `insert_fact_vec_writes_is_live_from_facts_invalid_at`. Pins the
    /// actual supersede contract: a live fact is found by `knn_facts`, then
    /// once its `facts` row is invalidated AND `mark_fact_vec_dead` is
    /// called (the exact pairing every real writer must do), `is_live` flips
    /// to 0 and `knn_facts` excludes it.
    #[test]
    fn mark_fact_vec_superseded_flips_is_live_and_excludes_from_knn_facts() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('01HFACT-SUPERSEDE-ME','project:hex','uses','a live object',0.5,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        let v: Vec<f32> = (0..EMBED_DIM).map(|d| d as f32 * 0.001).collect();
        insert_fact_vec(&conn, "01HFACT-SUPERSEDE-ME", &v).unwrap();

        let hits = knn_facts(&conn, &v, 1).unwrap();
        assert!(
            hits.iter().any(|(rowid, _)| {
                let id: String = conn
                    .query_row("SELECT id FROM facts WHERE rowid = ?1", [*rowid], |r| {
                        r.get(0)
                    })
                    .unwrap();
                id == "01HFACT-SUPERSEDE-ME"
            }),
            "the live fact must be found by knn_facts before it is superseded, got {:?}",
            hits
        );

        // The pairing every supersede-not-overwrite writer must perform in
        // the same transaction.
        conn.execute(
            "UPDATE facts SET invalid_at = '2026-09-05', superseded_by = '01HFACT-NEW' WHERE id = '01HFACT-SUPERSEDE-ME'",
            [],
        )
        .unwrap();
        mark_fact_vec_dead(&conn, "01HFACT-SUPERSEDE-ME").unwrap();

        let is_live: i64 = conn
            .query_row(
                "SELECT is_live FROM facts_vec WHERE fact_id = '01HFACT-SUPERSEDE-ME'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(is_live, 0, "mark_fact_vec_dead must flip is_live to 0");

        let hits_after = knn_facts(&conn, &v, 1).unwrap();
        assert!(
            hits_after.is_empty(),
            "knn_facts must exclude a fact right after mark_fact_vec_dead flips its is_live, got {:?}",
            hits_after
        );
    }

    /// RED for G1 (reviewer-B redo): `is_live` currently encodes ONLY
    /// `invalid_at IS NULL` (see `insert_fact_vec`'s doc comment), so a
    /// tombstoned-but-not-superseded fact (`facts.tombstone = 1`, set by
    /// consolidate canonicalization, never deleted) still has `is_live = 1`
    /// inside `facts_vec` and is only excluded by the OUTER `f.tombstone = 0`
    /// join AFTER the vec0 `k` limit has already been applied. A wall of
    /// tombstoned rows ranked nearer than the query consumes that `k` window
    /// before the outer join ever runs, so a live match behind the wall is
    /// lost — the exact F2 regression the overfetch-removal fix was supposed
    /// to close, on the tombstone axis instead of the supersede axis. Fails
    /// now: with k = 1, the inner `is_live = 1` subquery returns only the
    /// single nearest TOMBSTONED row, and the outer join then filters it out,
    /// leaving zero hits — the live fact never gets a chance to surface.
    #[test]
    fn knn_facts_returns_live_neighbor_behind_a_wall_of_tombstoned_ones() {
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::schema::apply_plan3(&conn).unwrap();

        // 64 tombstoned facts, each strictly nearer to the query than the
        // live fact below.
        for i in 0..64 {
            let id = format!("01HFACT-TOMB-{i:02}");
            conn.execute(
                "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,tombstone)
                 VALUES (?1,'project:hex','uses','tombstoned duplicate',0.5,'2026-06-11','2026-06-11',1)",
                params![id],
            )
            .unwrap();
            let v: Vec<f32> = (0..EMBED_DIM)
                .map(|d| (i as f32 + d as f32) * 0.0001)
                .collect();
            insert_fact_vec(&conn, &id, &v).unwrap();
        }

        // The one live, eligible fact — farther from the query than all 64
        // tombstoned facts above, so it ranks 65th by distance.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('01HFACT-LIVE','project:hex','uses','live object',0.5,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        let live_v: Vec<f32> = (0..EMBED_DIM).map(|d| (64.0 + d as f32) * 0.0001).collect();
        insert_fact_vec(&conn, "01HFACT-LIVE", &live_v).unwrap();

        let query: Vec<f32> = (0..EMBED_DIM).map(|d| d as f32 * 0.0001).collect();
        let hits = knn_facts(&conn, &query, 1).unwrap();

        let found_live = hits.iter().any(|(rowid, _)| {
            let id: String = conn
                .query_row("SELECT id FROM facts WHERE rowid = ?1", [*rowid], |r| {
                    r.get(0)
                })
                .unwrap();
            id == "01HFACT-LIVE"
        });
        assert!(
            found_live,
            "the live fact must be returned even though 64 tombstoned facts rank nearer with k = 1 — is_live must fold in tombstone, not rely on the outer join, got {:?}",
            hits
        );
    }
}
