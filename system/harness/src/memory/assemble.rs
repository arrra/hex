//! v1 ContextAssembler — parallel retrieval moves merged by a simple
//! confidence score with a coverage floor. See
//! `me/decisions/context-assembly-parallel-moves-confidence-2026-06-04.md`.
//!
//! v1 is a KEYWORD-SHAPE assembler. M1's vector arm fires ONLY when the
//! caller supplies a pre-computed `query_vec` (semantic policy is a
//! caller decision, per spec Tj0b203yv). The assembler NEVER constructs an
//! `Embedder` itself — that would blow the UserPromptSubmit hook's latency
//! budget, since the hook is a fresh OS process per user message and the
//! 522 MB nomic model would load on every non-trivial recall (audit finding
//! 1, 2026-07-16). The hot path (`recall::recall`, `harness::submit`) MUST
//! pass `None`; offline CLI callers who want semantic search embed the query
//! themselves and pass `Some(&qv)`.

use rusqlite::Connection;
use std::collections::HashSet;

use super::recall::FactHit;
use super::search::{search_fts_public, SearchResult};

pub const MAX_CONTEXT_CHARS: usize = 10_000;

const TOP_K_PER_MOVE: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoveId {
    M1ContentMatch,
    M2EntityFilter,
    M3PredicateQuery,
    M4TemporalSelect,
}

pub enum CandidateKind {
    Chunk(SearchResult),
    Fact(FactHit),
}

pub struct Candidate {
    pub kind: CandidateKind,
    pub move_id: MoveId,
    pub move_fired: bool,
    pub native_score: f64,
    pub rank_in_move: usize,
    pub confidence: f32,
    pub dedup_key: String,
}

pub struct MoveStats {
    pub move_id: MoveId,
    pub fired: bool,
    pub candidate_count: usize,
    pub top_native_scores: Vec<f64>,
}

pub struct AssembledContext {
    pub candidates: Vec<Candidate>,
    pub per_move_stats: Vec<MoveStats>,
}

// ───────────────────────────── cue detection ──────────────────────────────

/// Map query words to the stored predicate vocabulary. Returns the list of
/// canonical predicate names whose cues appear in the query.
fn predicate_cues(query: &str) -> Vec<&'static str> {
    let q = query.to_lowercase();
    let toks: HashSet<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let mut out: Vec<&'static str> = Vec::new();
    let map: &[(&[&str], &str)] = &[
        (&["decide", "decided", "decision"], "decided"),
        (&["prefer", "prefers", "preference"], "prefers"),
        (&["dislike", "dislikes"], "dislikes"),
        (&["block", "blocked", "blocking"], "blocked-by"),
        (&["responsible", "owner", "owns"], "responsible-for"),
        (&["plan", "plans", "planning"], "plans-to"),
        (&["focus", "focused", "focusing"], "current-focus"),
        (&["status"], "status"),
        (&["know", "knows", "knowing"], "knows"),
        (&["learned", "learning", "learn"], "learned-that"),
        (&["commit", "committed", "committing"], "committed-to"),
        (&["values"], "values"),
        (&["avoid", "avoids", "avoiding"], "avoids"),
        (&["work", "works", "working"], "works-on"),
    ];
    for (cues, pred) in map {
        if cues.iter().any(|c| toks.contains(c)) && !out.contains(pred) {
            out.push(*pred);
        }
    }
    out
}

fn is_temporal(query: &str) -> bool {
    let q = query.to_lowercase();
    let toks: HashSet<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    ["current", "latest", "now", "today", "recent", "recently"]
        .iter()
        .any(|c| toks.contains(*c))
}

/// Build the entity gazetteer from DISTINCT facts.subject. Returns a list of
/// (full_subject, lowercase_match_token) pairs — the token is the slug after
/// the colon (e.g. "alice" for "person:alice").
fn detect_entity_subjects(conn: &Connection, query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    let toks: HashSet<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_string())
        .collect();
    if toks.is_empty() {
        return Vec::new();
    }
    let mut matched: Vec<String> = Vec::new();
    let mut stmt = match conn.prepare("SELECT DISTINCT subject FROM facts WHERE tombstone = 0") {
        Ok(s) => s,
        Err(_) => return matched,
    };
    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(r) => r,
        Err(_) => return matched,
    };
    for subj in rows.filter_map(Result::ok) {
        let lower = subj.to_lowercase();
        let mut hit = false;
        if toks.contains(&lower) {
            hit = true;
        } else if let Some(slug) = lower.split(':').nth(1) {
            for piece in slug.split(|c: char| c == '-' || c == '_' || c == '/') {
                if piece.len() >= 3 && toks.contains(piece) {
                    hit = true;
                    break;
                }
            }
        }
        if hit && !matched.contains(&subj) {
            matched.push(subj);
        }
    }
    matched
}

// ─────────────────────────────────── moves ─────────────────────────────────

fn fact_select_sql(extra_where: &str, order: &str) -> String {
    format!(
        "SELECT subject, predicate, object, importance, private, created_at \
         FROM facts \
         WHERE tombstone = 0 {} \
         ORDER BY {} LIMIT ?",
        extra_where, order
    )
}

fn fact_from_row(r: &rusqlite::Row) -> rusqlite::Result<(FactHit, f64)> {
    let importance: f32 = r.get(3)?;
    Ok((
        FactHit {
            subject: r.get(0)?,
            predicate: r.get(1)?,
            object: r.get(2)?,
            importance,
            private: r.get::<_, i64>(4)? != 0,
        },
        importance as f64,
    ))
}

/// M1 — content match. ALWAYS fires. FTS5 chunks, plus vector KNN ONLY when
/// the caller supplies a pre-computed `query_vec` (semantic policy is
/// caller-decided per spec Tj0b203yv). Returns ordered candidates.
fn m1_content(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    query_vec: Option<&[f32]>,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();

    let chunks = search_fts_public(conn, query, TOP_K_PER_MOVE * 3, None).unwrap_or_default();
    let mut rank = 0usize;
    for c in chunks
        .into_iter()
        .filter(|r| !(for_agent && r.private))
        .take(TOP_K_PER_MOVE)
    {
        let native = c.score;
        let dedup_key = format!("chunk:{}", c.rowid);
        let move_fired = true;
        let confidence = move_fired_relevance(move_fired) * (1.0 / (rank as f32 + 1.0));
        out.push(Candidate {
            kind: CandidateKind::Chunk(c),
            move_id: MoveId::M1ContentMatch,
            move_fired,
            native_score: native,
            rank_in_move: rank,
            confidence,
            dedup_key,
        });
        rank += 1;
    }

    // Vector arm — caller-decided embedder policy. `None` = FTS-only (the
    // UserPromptSubmit hot path per spec Tj0b203yv). `Some(qv)` = semantic
    // fusion. The assembler NEVER constructs an `Embedder` itself; the hook
    // process would otherwise cold-load a 522 MB model on every non-trivial
    // message.
    if let Some(qv) = query_vec {
        match super::vector::knn(conn, qv, TOP_K_PER_MOVE) {
            Ok(hits) => {
                for (i, (rowid, dist)) in hits.iter().enumerate() {
                    // Fetch the chunk row to build a SearchResult.
                    if let Ok(c) = conn.query_row(
                        "SELECT rowid, source_path, heading, chunk_index, content, private \
                         FROM chunks WHERE rowid = ?1",
                        [rowid],
                        |r| {
                            Ok(SearchResult {
                                rowid: r.get(0)?,
                                source_path: r.get(1)?,
                                heading: r.get(2)?,
                                chunk_index: r.get(3)?,
                                content: r.get(4)?,
                                private: r.get::<_, i64>(5)? != 0,
                                score: *dist,
                            })
                        },
                    ) {
                        if for_agent && c.private {
                            continue;
                        }
                        let dedup_key = format!("chunk:{}", c.rowid);
                        if out.iter().any(|x| x.dedup_key == dedup_key) {
                            continue;
                        }
                        let rank = i;
                        let confidence = 1.0 / (rank as f32 + 1.0);
                        out.push(Candidate {
                            kind: CandidateKind::Chunk(c),
                            move_id: MoveId::M1ContentMatch,
                            move_fired: true,
                            native_score: *dist,
                            rank_in_move: rank,
                            confidence,
                            dedup_key,
                        });
                    }
                }
            }
            Err(e) => {
                eprintln!("[assemble] M1 vector knn failed: {e}");
            }
        }
    }

    out
}

fn move_fired_relevance(fired: bool) -> f32 {
    if fired {
        1.0
    } else {
        0.3
    }
}

/// M2 — entity filter. Fires when at least one detected entity matches a
/// stored fact subject.
fn m2_entity(conn: &Connection, query: &str, for_agent: bool) -> (bool, Vec<Candidate>) {
    let subjects = detect_entity_subjects(conn, query);
    if subjects.is_empty() {
        return (false, Vec::new());
    }
    let mut hits: Vec<(FactHit, f64)> = Vec::new();
    for subj in &subjects {
        let extra = if for_agent {
            " AND subject = ?1 AND private = 0"
        } else {
            " AND subject = ?1"
        };
        let sql = fact_select_sql(extra, "importance DESC, created_at DESC");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let collected: Vec<(FactHit, f64)> = match stmt
            .query_map(rusqlite::params![subj, TOP_K_PER_MOVE as i64], |r| {
                fact_from_row(r)
            }) {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        };
        drop(stmt);
        hits.extend(collected);
    }
    // Sort by importance DESC for stable rank ordering across multiple subjects.
    hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let cands = facts_to_candidates(hits, MoveId::M2EntityFilter, true);
    (true, cands)
}

/// M3 — predicate query. Fires when a cue maps to a known predicate.
fn m3_predicate(conn: &Connection, query: &str, for_agent: bool) -> (bool, Vec<Candidate>) {
    let preds = predicate_cues(query);
    if preds.is_empty() {
        return (false, Vec::new());
    }
    let mut hits: Vec<(FactHit, f64)> = Vec::new();
    for pred in &preds {
        let extra = if for_agent {
            " AND predicate = ?1 AND private = 0"
        } else {
            " AND predicate = ?1"
        };
        let sql = fact_select_sql(extra, "importance DESC, created_at DESC");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let collected: Vec<(FactHit, f64)> = match stmt
            .query_map(rusqlite::params![pred, TOP_K_PER_MOVE as i64], |r| {
                fact_from_row(r)
            }) {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        };
        drop(stmt);
        hits.extend(collected);
    }
    hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let cands = facts_to_candidates(hits, MoveId::M3PredicateQuery, true);
    (true, cands)
}

/// M4 — temporal select (FACTS ONLY; chunks have no timestamp column).
fn m4_temporal(conn: &Connection, query: &str, for_agent: bool) -> (bool, Vec<Candidate>) {
    if !is_temporal(query) {
        return (false, Vec::new());
    }
    let extra = if for_agent { " AND private = 0" } else { "" };
    let sql = fact_select_sql(extra, "created_at DESC, importance DESC");
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return (true, Vec::new()),
    };
    let hits: Vec<(FactHit, f64)> = stmt
        .query_map(rusqlite::params![TOP_K_PER_MOVE as i64], |r| {
            fact_from_row(r)
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    let cands = facts_to_candidates(hits, MoveId::M4TemporalSelect, true);
    (true, cands)
}

fn facts_to_candidates(hits: Vec<(FactHit, f64)>, move_id: MoveId, fired: bool) -> Vec<Candidate> {
    let mr = move_fired_relevance(fired);
    hits.into_iter()
        .enumerate()
        .map(|(rank, (f, native))| {
            let dedup_key = format!("fact:{}|{}", f.subject, f.predicate);
            let confidence = mr * (1.0 / (rank as f32 + 1.0));
            Candidate {
                kind: CandidateKind::Fact(f),
                move_id,
                move_fired: fired,
                native_score: native,
                rank_in_move: rank,
                confidence,
                dedup_key,
            }
        })
        .collect()
}

// ───────────────────────────────── merge ───────────────────────────────────

fn cand_chars(c: &Candidate) -> usize {
    match &c.kind {
        CandidateKind::Chunk(s) => {
            let snip = s.content.chars().take(600).count();
            snip + s.source_path.len() + s.heading.len() + 16
        }
        CandidateKind::Fact(f) => f.subject.len() + f.predicate.len() + f.object.len() + 8,
    }
}

fn move_stats(move_id: MoveId, fired: bool, cands: &[Candidate]) -> MoveStats {
    let top_native_scores: Vec<f64> = cands.iter().take(3).map(|c| c.native_score).collect();
    MoveStats {
        move_id,
        fired,
        candidate_count: cands.len(),
        top_native_scores,
    }
}

/// Public entry. Runs the four moves, merges with floor + per-move quota
/// round-robin by confidence DESC, dedups, and truncates to the char budget.
///
/// `query_vec` is the caller-decided embedder policy (spec Tj0b203yv):
/// - `None` → FTS-only. The UserPromptSubmit hook path (`recall::recall`) and
///   the worker submit path MUST pass `None` so no `Embedder` is constructed
///   in a fresh OS process.
/// - `Some(qv)` → semantic fusion via `vector::knn`. Offline CLI callers that
///   want semantic M1 embed the query themselves and pass the vector.
///
/// The assembler NEVER constructs an `Embedder`.
pub fn assemble(
    conn: &Connection,
    query: &str,
    for_agent: bool,
    budget: usize,
    query_vec: Option<&[f32]>,
) -> AssembledContext {
    let budget = if budget == 0 {
        MAX_CONTEXT_CHARS
    } else {
        budget
    };

    // ── run the moves (sequential — local SQLite, the cost is dominated by
    // FTS5/index lookups; "parallel" in spec scope is logical, not threaded).
    let m1_c = m1_content(conn, query, for_agent, query_vec);
    let (m2_f, m2_c) = m2_entity(conn, query, for_agent);
    let (m3_f, m3_c) = m3_predicate(conn, query, for_agent);
    let (m4_f, m4_c) = m4_temporal(conn, query, for_agent);

    let per_move_stats = vec![
        move_stats(MoveId::M1ContentMatch, true, &m1_c),
        move_stats(MoveId::M2EntityFilter, m2_f, &m2_c),
        move_stats(MoveId::M3PredicateQuery, m3_f, &m3_c),
        move_stats(MoveId::M4TemporalSelect, m4_f, &m4_c),
    ];

    // ── merge: FLOOR — M1 top-1 first, then each fired non-M1 move's top-1.
    let mut merged: Vec<Candidate> = Vec::new();
    let mut chars = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut queues: Vec<(MoveId, std::vec::IntoIter<Candidate>)> = vec![
        (MoveId::M1ContentMatch, m1_c.into_iter()),
        (MoveId::M2EntityFilter, m2_c.into_iter()),
        (MoveId::M3PredicateQuery, m3_c.into_iter()),
        (MoveId::M4TemporalSelect, m4_c.into_iter()),
    ];

    // Floor: take the first available from each queue, M1 first.
    for i in 0..queues.len() {
        // Skip non-fired moves on the floor — they get the 0.3 demotion and
        // do not warrant a guaranteed slot. M1 always fires.
        let fired = match queues[i].0 {
            MoveId::M1ContentMatch => true,
            MoveId::M2EntityFilter => m2_f,
            MoveId::M3PredicateQuery => m3_f,
            MoveId::M4TemporalSelect => m4_f,
        };
        if !fired {
            continue;
        }
        if let Some(cand) = queues[i].1.next() {
            let cost = cand_chars(&cand);
            if seen.insert(cand.dedup_key.clone()) {
                if chars + cost > budget {
                    // Floor over-budget — still push so the facet coverage
                    // contract is honored, then stop (no further candidates
                    // are considered, so `chars` needs no update).
                    merged.push(cand);
                    return AssembledContext {
                        candidates: merged,
                        per_move_stats,
                    };
                }
                merged.push(cand);
                chars += cost;
            }
        }
    }

    // ── per-move QUOTA round-robin by confidence: at each round, each fired
    //    move offers its next-best candidate; we keep them sorted by
    //    confidence DESC across the round so highest-confidence wins ties.
    loop {
        // Gather one candidate from each non-empty queue.
        let mut round: Vec<Candidate> = Vec::new();
        for q in queues.iter_mut() {
            if let Some(c) = q.1.next() {
                round.push(c);
            }
        }
        if round.is_empty() {
            break;
        }
        round.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for cand in round {
            if !seen.insert(cand.dedup_key.clone()) {
                continue;
            }
            let cost = cand_chars(&cand);
            if chars + cost > budget {
                return AssembledContext {
                    candidates: merged,
                    per_move_stats,
                };
            }
            merged.push(cand);
            chars = chars.saturating_add(cost);
        }
    }
    let _ = chars;

    AssembledContext {
        candidates: merged,
        per_move_stats,
    }
}

/// Render assembled candidates into the worker-facing context block. This is the
/// layer above which `submit()` prepends the reply "pin".
///
/// NOTE: `Candidate` has NO `content` field and NO `Default` derive (verified
/// 2026-06-05). Text lives inside `CandidateKind`.
pub fn render_candidates(ctx: &AssembledContext) -> String {
    ctx.candidates
        .iter()
        .map(|c| match &c.kind {
            CandidateKind::Chunk(s) => s.content.clone(),
            CandidateKind::Fact(f) => format!("{} {} {}", f.subject, f.predicate, f.object),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh_db() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        // Production form: chunks IS the FTS5 vtable (see search.rs setup_db
        // and index.rs:379). search_fts_public queries `chunks MATCH ?` so
        // the column layout must match.
        c.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
                file_id UNINDEXED,
                source_path UNINDEXED,
                heading,
                chunk_index UNINDEXED,
                content,
                private UNINDEXED,
                tokenize='unicode61'
            );",
        )
        .unwrap();
        c
    }

    fn insert_chunk(c: &Connection, path: &str, content: &str, private: bool) {
        c.execute(
            "INSERT INTO chunks(file_id,source_path,heading,chunk_index,content,private)
             VALUES ('1',?1,'h','0',?2,?3)",
            rusqlite::params![path, content, private as i32],
        )
        .unwrap();
    }

    fn insert_fact(
        c: &Connection,
        id: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        private: bool,
    ) {
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES (?1,?2,?3,?4,0.8,'2026-06-04','2026-06-04',?5)",
            rusqlite::params![id, subject, predicate, object, private as i32],
        )
        .unwrap();
    }

    /// Floor: M1's top-1 is placed first, and each OTHER fired move
    /// contributes its top-1 before any further fill.
    #[test]
    fn floor_places_m1_top1_first_and_each_fired_move_top1() {
        let c = fresh_db();
        insert_chunk(&c, "docs/schema.md", "schema decision memory layer", false);
        // Predicate cue ("decided") should fire M3.
        insert_fact(&c, "f1", "project:hex", "decided", "use sqlite-vec", false);
        // Entity in gazetteer should fire M2.
        insert_fact(&c, "f2", "person:alice", "prefers", "rust", false);

        let r = assemble(
            &c,
            "what did alice decide about the schema",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        assert!(!r.candidates.is_empty(), "assembler returned no candidates");
        // First candidate must come from M1 (the floor).
        assert_eq!(
            r.candidates[0].move_id,
            MoveId::M1ContentMatch,
            "M1 top-1 must be placed first as the floor"
        );
        // Each fired non-M1 move must contribute at least one candidate.
        for m in &[MoveId::M2EntityFilter, MoveId::M3PredicateQuery] {
            let fired = r.per_move_stats.iter().any(|s| s.move_id == *m && s.fired);
            if fired {
                assert!(
                    r.candidates.iter().any(|c| c.move_id == *m),
                    "fired move {:?} contributed no candidate to the floor",
                    m
                );
            }
        }
    }

    /// Privacy: for_agent=true MUST exclude facts marked private from every
    /// facts move (M2/M3/M4).
    #[test]
    fn privacy_excludes_private_facts_when_for_agent() {
        let c = fresh_db();
        // Predicate cue "decided" → M3 will fire on this private fact.
        insert_fact(&c, "p1", "me/secret", "decided", "fire bob", true);
        insert_fact(&c, "p2", "project:hex", "decided", "use sqlite-vec", false);

        let r = assemble(
            &c,
            "what did we decide recently",
            true,
            MAX_CONTEXT_CHARS,
            None,
        );

        for cand in &r.candidates {
            if let CandidateKind::Fact(f) = &cand.kind {
                assert!(
                    !f.private,
                    "private fact {} leaked into for_agent=true result",
                    f.subject
                );
                assert_ne!(f.subject, "me/secret", "private subject leaked");
            }
        }
    }

    /// Per-move quota: M1 having a long candidate list MUST NOT crowd out a
    /// fired fact move's top candidate.
    #[test]
    fn per_move_quota_protects_fired_fact_moves_from_m1_domination() {
        let c = fresh_db();
        // Stuff M1 with many matching chunks.
        for i in 0..20 {
            insert_chunk(
                &c,
                &format!("docs/d{i}.md"),
                "schema decision memory layer schema decision",
                false,
            );
        }
        // One fact under a predicate cue.
        insert_fact(
            &c,
            "f1",
            "project:hex",
            "decided",
            "adopt the parallel-moves assembler",
            false,
        );

        let r = assemble(
            &c,
            "what did we decide about the schema",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        let m3_fired = r
            .per_move_stats
            .iter()
            .any(|s| s.move_id == MoveId::M3PredicateQuery && s.fired);
        assert!(m3_fired, "M3 should fire on the 'decide' cue");

        let m3_kept = r
            .candidates
            .iter()
            .any(|c| c.move_id == MoveId::M3PredicateQuery);
        assert!(
            m3_kept,
            "M3's fact was crowded out by M1's long list — per-move quota missing"
        );
    }

    /// Merge contract — confidence formula AND char budget truncation.
    /// confidence = move_relevance(1.0 fired) * 1/(rank+1)
    /// budget truncation must cut the merge before exceeding the char budget.
    #[test]
    fn confidence_formula_and_budget_truncation() {
        let c = fresh_db();
        // Populate several chunks (each ~30 chars of content + path) so M1
        // alone would exceed a small budget if truncation were absent.
        for i in 0..10 {
            insert_chunk(
                &c,
                &format!("docs/m{i}.md"),
                "schema schema schema schema schema schema schema schema",
                false,
            );
        }

        // 1) confidence formula at rank 0 for a fired move must equal 1.0.
        let full = assemble(&c, "schema", false, MAX_CONTEXT_CHARS, None);
        let m1_top = full
            .candidates
            .iter()
            .find(|x| x.move_id == MoveId::M1ContentMatch)
            .expect("M1 should produce at least one candidate");
        assert_eq!(m1_top.rank_in_move, 0, "M1 top should be rank 0");
        assert!(m1_top.move_fired, "M1 always fires");
        assert!(
            (m1_top.confidence - 1.0).abs() < 1e-6,
            "rank-0 fired confidence must be 1.0, got {}",
            m1_top.confidence
        );
        // native_score must be carried separately (BM25 is negative in FTS5)
        // — i.e. it should NOT equal the confidence value.
        assert!(
            (m1_top.native_score as f32 - m1_top.confidence).abs() > 1e-6
                || m1_top.native_score == 0.0,
            "native_score must be carried separately from confidence"
        );

        // 2) Budget truncation: a tiny budget must force the merged result
        //    to stay at or under a small bound. (Floor candidate is allowed
        //    to push slightly over per the facet-coverage contract, so we
        //    assert the merge stopped well short of the unbounded length.)
        let tiny = assemble(&c, "schema", false, 100, None);
        assert!(
            tiny.candidates.len() < full.candidates.len(),
            "tiny budget ({} cands) did not truncate vs full ({} cands)",
            tiny.candidates.len(),
            full.candidates.len()
        );
    }

    /// Embedder-down: assemble() must not panic on a DB with no vector data
    /// / no available embedder, and must still return FTS+facts results.
    #[test]
    fn embedder_down_returns_results_without_panic() {
        let c = fresh_db();
        insert_chunk(&c, "docs/a.md", "memory schema assembler", false);
        insert_fact(&c, "f1", "project:hex", "decided", "ship it", false);

        // Should NOT panic even though no embedder is wired up here.
        let r = assemble(
            &c,
            "what did we decide about the memory schema",
            false,
            MAX_CONTEXT_CHARS,
            None,
        );

        assert!(
            !r.candidates.is_empty(),
            "assemble returned no candidates even though FTS+facts are populated"
        );
    }

    #[test]
    fn render_candidates_joins_content() {
        let mk = |txt: &str| Candidate {
            kind: CandidateKind::Chunk(SearchResult {
                rowid: 0,
                source_path: "p".into(),
                heading: "h".into(),
                chunk_index: "0".into(),
                content: txt.into(),
                private: false,
                score: 0.0,
            }),
            move_id: MoveId::M1ContentMatch,
            move_fired: true,
            native_score: 0.0,
            rank_in_move: 0,
            confidence: 1.0,
            dedup_key: txt.into(),
        };
        let ctx = AssembledContext {
            candidates: vec![mk("alpha"), mk("beta")],
            per_move_stats: vec![],
        };
        let s = render_candidates(&ctx);
        assert!(s.contains("alpha") && s.contains("beta"));
    }
}
