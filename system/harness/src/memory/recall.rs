//! `hex memory recall` — fast, FTS5-only contextual retrieval for per-prompt
//! injection. No embedding model is loaded (keeps the UserPromptSubmit hook
//! inside its latency budget — spec §8). Appends a line to
//! `.hex/memory/recall-log.jsonl` for the nightly eval.

use serde_json::json;
use std::io::Write;
use std::path::Path;

const MIN_QUERY_CHARS: usize = 12;
const MAX_CONTEXT_CHARS: usize = 10_000; // spec §8 hard cap

pub type Hit = super::search::SearchResult;

#[derive(Debug)]
pub struct FactHit {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub importance: f32,
    pub private: bool,
}

pub struct RecallV2 {
    pub chunks: Vec<Hit>,
    pub facts: Vec<FactHit>,
}

pub fn recall_with_facts(
    conn: &rusqlite::Connection,
    query: &str,
) -> rusqlite::Result<RecallV2> {
    let chunks = chunks_recall(conn, query, 5).unwrap_or_default();
    // Hot-path budget: no embedding model is loaded here (module doc), so the
    // facts vector arm is off (None ⇒ exactly the FTS-only behavior).
    let facts = facts_recall(conn, query, 5, None)?;
    Ok(RecallV2 { chunks, facts })
}

fn chunks_recall(
    conn: &rusqlite::Connection,
    query: &str,
    k: usize,
) -> rusqlite::Result<Vec<Hit>> {
    super::search::search_fts_public(conn, query, k, None)
}

/// Facts retrieval: FTS keyword arm, plus a KNN arm over `facts_vec` when the
/// caller already holds a query embedding (hoist it from the chunk path — do
/// NOT cold-load the model here). `query_vec = None` ⇒ FTS-only, identical to
/// the pre-fusion behavior.
pub(crate) fn facts_recall(
    conn: &rusqlite::Connection,
    query: &str,
    k: usize,
    query_vec: Option<&[f32]>,
) -> rusqlite::Result<Vec<FactHit>> {
    // FTS5 default-ANDs tokens — for natural-language queries we want any-match.
    // Drop stopwords and OR the remaining alphanumerics so "who is alice" hits
    // facts mentioning the slug.
    let fts_query = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3 && !matches!(*t, "the" | "and" | "for" | "are" | "was" | "who" | "what" | "how" | "does" | "did" | "is"))
        .collect::<Vec<_>>()
        .join(" OR ");

    // FTS arm — ranked facts rowids (bm25, then importance). The rowid is the
    // fusion key shared with the KNN arm (facts.id is a TEXT ULID, not an
    // integer — knn_facts joins it back to the rowid).
    let fts_ids: Vec<i64> = if fts_query.is_empty() {
        Vec::new()
    } else {
        conn.prepare(
            "SELECT facts_fts.rowid
             FROM facts_fts JOIN facts f ON f.rowid = facts_fts.rowid
             WHERE facts_fts MATCH ?1 AND f.tombstone = 0
             ORDER BY bm25(facts_fts), f.importance DESC LIMIT ?2",
        )?
        .query_map(rusqlite::params![fts_query, k as i64], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect()
    };

    // Vector arm — same shape as the chunk-side fusion (search.rs run()):
    // best-effort, loud on failure, never degrades the FTS arm.
    let knn_ids: Vec<i64> = match query_vec {
        Some(qv) => super::vector::knn_facts(conn, qv, k.max(20))
            .map(|hits| hits.into_iter().map(|(id, _)| id).collect())
            .unwrap_or_else(|e| {
                eprintln!("facts vector arm failed: {e}");
                vec![]
            }),
        None => vec![],
    };

    let fused = super::rrf::rrf_fuse(&[fts_ids, knn_ids], super::rrf::RRF_K);

    // Fetch facts in fused order. Importance breaks RRF-score ties (the
    // fuse's HashMap ordering is arbitrary on equal scores); the sort is
    // stable, so the single-arm (None) path keeps exactly the FTS order.
    let mut scored: Vec<(FactHit, f64)> = Vec::new();
    for (rowid, score) in &fused {
        let row = conn.query_row(
            "SELECT subject, predicate, object, importance, private
             FROM facts WHERE rowid = ?1 AND tombstone = 0",
            [rowid],
            |r| {
                Ok(FactHit {
                    subject: r.get(0)?,
                    predicate: r.get(1)?,
                    object: r.get(2)?,
                    importance: r.get(3)?,
                    private: r.get::<_, i64>(4)? != 0,
                })
            },
        );
        if let Ok(h) = row {
            scored.push((h, *score));
        }
    }
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.0.importance
                    .partial_cmp(&a.0.importance)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let mut hits: Vec<FactHit> = scored.into_iter().map(|(h, _)| h).collect();
    hits.truncate(k);

    // Slug boost: for each token in the query, surface facts whose subject
    // contains `:<token>` after the type prefix (e.g. "alice" → subject
    // LIKE '%:alice%' matches both `person:alice` and `person:alice-chew`).
    for tok in query.to_lowercase().split_whitespace() {
        if tok.len() < 3 { continue; }
        let pattern = format!("%:{tok}%");
        let mut q = conn.prepare(
            "SELECT subject, predicate, object, importance, private FROM facts
             WHERE subject LIKE ?1 AND tombstone = 0
             ORDER BY importance DESC LIMIT 3",
        )?;
        for hit in q
            .query_map([&pattern], |r| {
                Ok(FactHit {
                    subject: r.get(0)?,
                    predicate: r.get(1)?,
                    object: r.get(2)?,
                    importance: r.get(3)?,
                    private: r.get::<_, i64>(4)? != 0,
                })
            })?
            .filter_map(Result::ok)
        {
            if !hits
                .iter()
                .any(|h| h.subject == hit.subject && h.predicate == hit.predicate)
            {
                hits.push(hit);
            }
        }
    }
    hits.truncate(k);
    Ok(hits)
}

pub struct RecallOutcome {
    pub injected: bool,
    pub gated: bool,
    pub result_count: usize,
    pub facts_injected: usize,
    pub chunks_injected: usize,
    pub latency_ms: u64,
    /// The formatted context block, ready for `additionalContext`. Empty when
    /// `injected` is false.
    pub context: String,
}

/// Trivial-prompt pre-filter (spec §8) — runs before any DB work.
fn is_trivial(query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.len() < MIN_QUERY_CHARS
        || matches!(q.as_str(), "ok" | "okay" | "thanks" | "thank you" | "yes" | "no" | "go" | "continue")
}

/// Run recall for `query`. `for_agent` = true applies the private filter
/// (BOI workers get non-private chunks only — spec §7).
pub fn recall(hex_root: &Path, query: &str, for_agent: bool) -> RecallOutcome {
    let t0 = std::time::Instant::now();

    if is_trivial(query) {
        let outcome = RecallOutcome {
            injected: false, gated: true, result_count: 0,
            facts_injected: 0, chunks_injected: 0,
            latency_ms: t0.elapsed().as_millis() as u64, context: String::new(),
        };
        log_recall(hex_root, &outcome, &LogExtras::default());
        return outcome;
    }

    let db = super::db_path(hex_root);
    let (filtered, facts, extras): (
        Vec<super::search::SearchResult>,
        Vec<FactHit>,
        LogExtras,
    ) = match super::open_db(&db) {
        Ok(conn) => {
            // Route the hot path through the v1 ContextAssembler.
            let assembled = super::assemble::assemble(
                &conn,
                query,
                for_agent,
                MAX_CONTEXT_CHARS,
            );

            // Capture per-move stats for the recall-log (calibration seam —
            // raw native scores per move; top_confidence alone is useless).
            let per_move_stats: Vec<serde_json::Value> = assembled
                .per_move_stats
                .iter()
                .map(|s| {
                    json!({
                        "move_id": move_id_str(s.move_id),
                        "fired": s.fired,
                        "candidate_count": s.candidate_count,
                        "top_native_scores": s.top_native_scores,
                        "native_score": s.top_native_scores.first().copied(),
                    })
                })
                .collect();

            // Identify M1's top-1 (first candidate from M1 in the merged
            // list — floor places it first). Used for the ablation control.
            let m1_top1_key: Option<String> = assembled
                .candidates
                .iter()
                .find(|c| c.move_id == super::assemble::MoveId::M1ContentMatch)
                .map(|c| c.dedup_key.clone());

            // Ablation dedup_keys (the merge with M1 top-1 removed).
            let ablation_dedup_keys: Vec<String> = assembled
                .candidates
                .iter()
                .filter(|c| Some(&c.dedup_key) != m1_top1_key.as_ref())
                .map(|c| c.dedup_key.clone())
                .collect();

            // Partition merged candidates by kind. Order within each kind is
            // preserved, so the first Chunk == M1's top-1 (when M1 fired).
            let mut chunks: Vec<super::search::SearchResult> = Vec::new();
            let mut fs: Vec<FactHit> = Vec::new();
            let mut m1_is_chunk = false;
            for cand in assembled.candidates {
                let is_m1_top1 =
                    Some(&cand.dedup_key) == m1_top1_key.as_ref();
                match cand.kind {
                    super::assemble::CandidateKind::Chunk(c) => {
                        if is_m1_top1 {
                            m1_is_chunk = true;
                        }
                        chunks.push(c);
                    }
                    super::assemble::CandidateKind::Fact(f) => fs.push(f),
                }
            }

            // Render ablation context block to measure total_chars. M1 only
            // produces chunks today, so dropping its top-1 = drop chunks[0].
            let ablation_chars = if m1_is_chunk && !chunks.is_empty() {
                format_context_v2(&chunks[1..], &fs).len()
            } else {
                format_context_v2(&chunks, &fs).len()
            };

            let extras = LogExtras {
                per_move_stats,
                ablation: json!({
                    "dedup_keys": ablation_dedup_keys,
                    "total_chars": ablation_chars,
                }),
            };
            (chunks, fs, extras)
        }
        Err(e) => {
            eprintln!("[memory recall] cannot open {}: {e}", db.display());
            (vec![], vec![], LogExtras::default())
        }
    };

    let injected = !filtered.is_empty() || !facts.is_empty();
    let outcome = if injected {
        RecallOutcome {
            injected: true,
            gated: false,
            result_count: filtered.len() + facts.len(),
            facts_injected: facts.len(),
            chunks_injected: filtered.len(),
            latency_ms: t0.elapsed().as_millis() as u64,
            context: format_context_v2(&filtered, &facts),
        }
    } else {
        RecallOutcome {
            injected: false, gated: false, result_count: 0,
            facts_injected: 0, chunks_injected: 0,
            latency_ms: t0.elapsed().as_millis() as u64, context: String::new(),
        }
    };
    log_recall(hex_root, &outcome, &extras);
    outcome
}

#[derive(Default)]
struct LogExtras {
    per_move_stats: Vec<serde_json::Value>,
    ablation: serde_json::Value,
}

fn move_id_str(m: super::assemble::MoveId) -> &'static str {
    use super::assemble::MoveId::*;
    match m {
        M1ContentMatch => "M1",
        M2EntityFilter => "M2",
        M3PredicateQuery => "M3",
        M4TemporalSelect => "M4",
    }
}

fn format_context_v2(results: &[super::search::SearchResult], facts: &[FactHit]) -> String {
    let mut out = String::from(
        "## Relevant workspace memory\n\nThe following may be relevant to the current request \
         (retrieved from hex's memory index — verify before relying on it):\n\n",
    );

    if !facts.is_empty() {
        out.push_str("### Facts\n\n");
        for f in facts {
            out.push_str(&format!(
                "- **{}** {} {}\n",
                f.subject, f.predicate, f.object
            ));
            if out.len() >= MAX_CONTEXT_CHARS {
                break;
            }
        }
        out.push('\n');
    }

    if !results.is_empty() {
        out.push_str("### Chunks\n\n");
        for r in results {
            let snippet: String = r.content.chars().take(600).collect();
            out.push_str(&format!(
                "#### {} — {}\n{}\n\n",
                r.source_path,
                r.heading,
                snippet.trim()
            ));
            if out.len() >= MAX_CONTEXT_CHARS {
                break;
            }
        }
    }

    // Char-safe hard cap — String::truncate panics on a non-char-boundary index.
    if out.len() > MAX_CONTEXT_CHARS {
        let mut end = MAX_CONTEXT_CHARS;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

/// Append a JSONL line to `.hex/memory/recall-log.jsonl` for the nightly eval.
/// Best-effort — never panics.
fn log_recall(hex_root: &Path, o: &RecallOutcome, extras: &LogExtras) {
    let dir = hex_root.join(".hex/memory");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open(dir.join("recall-log.jsonl"))
    {
        let _ = writeln!(
            f,
            "{}",
            json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "injected": o.injected, "gated": o.gated,
                "result_count": o.result_count, "latency_ms": o.latency_ms,
                "facts_injected": o.facts_injected, "chunks_injected": o.chunks_injected,
                "per_move_stats": extras.per_move_stats,
                "ablation_without_top1": extras.ablation,
            })
        );
    }

}

/// `hex memory recall <query>` — prints the context block to stdout.
pub fn run(hex_root: &Path, query: &str, for_agent: bool) -> i32 {
    let o = recall(hex_root, query, for_agent);
    if o.injected {
        print!("{}", o.context);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_prompts_are_gated() {
        assert!(is_trivial("ok"));
        assert!(is_trivial("thanks"));
        assert!(is_trivial("yes"));
        assert!(!is_trivial("what did we decide about the schema"));
    }

    #[test]
    fn gated_recall_does_not_inject() {
        let tmp = tempfile::TempDir::new().unwrap();
        let o = recall(tmp.path(), "ok", false);
        assert!(o.gated && !o.injected);
    }

    #[test]
    fn missing_index_fails_soft() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Non-trivial query, but no DB — must not panic, must not inject.
        let o = recall(tmp.path(), "what did we decide about the memory schema", false);
        assert!(!o.injected);
    }
}

#[cfg(test)]
mod plan2_tests {
    use super::*;
    use rusqlite::Connection;

    /// RED test for T5ffsh4b0 — `recall::recall` (hot path) MUST route
    /// through `assemble::assemble`, which adds the predicate-cue path
    /// (M3) the legacy FTS-only `facts_recall` lacks.
    ///
    /// The query word "preference" is a M3 cue mapped to the stored
    /// predicate "prefers". The fact's content shares NO tokens with the
    /// query, so the legacy FTS path returns nothing. Only an assemble-
    /// routed `recall()` surfaces the fact and reports `injected=true`.
    #[test]
    fn recall_routes_through_assemble_predicate_cue() {
        use std::path::PathBuf;
        let tmp = tempfile::TempDir::new().unwrap();
        let hex_root: PathBuf = tmp.path().to_path_buf();
        let db_path = crate::memory::db_path(&hex_root);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

        crate::memory::vector::register_sqlite_vec();
        let c = rusqlite::Connection::open(&db_path).unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES ('f1','project:hex','prefers','vim keybindings',0.9,'2026-06-04','2026-06-04',0)",
            [],
        )
        .unwrap();
        drop(c);

        // Query shares NO tokens with the stored fact text. Tokens after
        // stopword filtering: "editor", "preference" — neither appears in
        // any facts_fts column ("project", "hex", "prefers", "vim",
        // "keybindings"). The legacy facts_recall therefore returns 0,
        // recall() reports injected=false. After T5ffsh4b0 wires
        // assemble::assemble, M3 maps "preference" → predicate "prefers"
        // and the fact is surfaced.
        let o = recall(&hex_root, "what is the editor preference here", false);

        assert!(
            o.injected,
            "recall must route through assemble — predicate-cue ('preference' → 'prefers') \
             should surface the fact even when no token FTS-matches"
        );
        assert!(
            o.facts_injected >= 1,
            "expected ≥1 fact via M3 predicate cue, got {}",
            o.facts_injected
        );
        assert!(
            o.context.contains("prefers") && o.context.contains("vim keybindings"),
            "context block must contain the M3-surfaced fact; got: {:?}",
            o.context
        );
    }

    /// RED test for Tsztwz7dd — `log_recall` MUST extend the JSONL line
    /// emitted to `.hex/memory/recall-log.jsonl` with:
    ///   (a) a per-move breakdown that carries the raw `native_score`(s)
    ///       for every move (M1/M2/M3/M4), and
    ///   (b) an `ablation_without_top1` field — the merge result with M1's
    ///       top-1 removed — so lift of the top candidate is measurable
    ///       offline.
    ///
    /// Logging `top_confidence` alone is worthless (it is ~always 0.5).
    /// The native scores and the ablation are the calibration seam.
    #[test]
    fn recall_log_carries_native_score_and_ablation() {
        use std::path::PathBuf;
        let tmp = tempfile::TempDir::new().unwrap();
        let hex_root: PathBuf = tmp.path().to_path_buf();
        let db_path = crate::memory::db_path(&hex_root);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

        crate::memory::vector::register_sqlite_vec();
        let c = rusqlite::Connection::open(&db_path).unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        // A fact M3 can surface via the predicate cue ("decided").
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES ('f1','project:hex','decided','use sqlite-vec',0.9,'2026-06-04','2026-06-04',0)",
            [],
        )
        .unwrap();
        drop(c);

        let _ = recall(
            &hex_root,
            "what did we decide about the memory layer",
            false,
        );

        let log_path = hex_root.join(".hex/memory/recall-log.jsonl");
        let raw = std::fs::read_to_string(&log_path)
            .expect("recall-log.jsonl must be written by log_recall");
        let last = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
            .expect("recall-log.jsonl must contain at least one line");
        let v: serde_json::Value =
            serde_json::from_str(last).expect("recall-log line must be valid JSON");

        // (a) per-move breakdown with native_score(s).
        let stats = v
            .get("per_move_stats")
            .expect("recall-log line must include `per_move_stats`");
        let arr = stats
            .as_array()
            .expect("`per_move_stats` must be an array of move entries");
        assert!(
            arr.len() >= 4,
            "expected per_move_stats for all 4 moves (M1/M2/M3/M4), got {}",
            arr.len()
        );
        for entry in arr {
            assert!(
                entry.get("move_id").is_some(),
                "per_move_stats entry missing `move_id`: {entry}"
            );
            assert!(
                entry.get("fired").is_some(),
                "per_move_stats entry missing `fired`: {entry}"
            );
            assert!(
                entry.get("candidate_count").is_some(),
                "per_move_stats entry missing `candidate_count`: {entry}"
            );
            assert!(
                entry.get("top_native_scores").is_some()
                    || entry.get("native_scores").is_some()
                    || entry.get("native_score").is_some(),
                "per_move_stats entry missing native_score field (top_native_scores / native_scores / native_score): {entry}"
            );
        }
        // Native score must also be discoverable by raw substring — the spec
        // verification greps for it.
        assert!(
            raw.contains("native_score"),
            "recall-log line must mention `native_score` (raw: {raw})"
        );

        // (b) ablation_without_top1.
        let ablation = v
            .get("ablation_without_top1")
            .expect("recall-log line must include `ablation_without_top1`");
        assert!(
            ablation.get("dedup_keys").is_some(),
            "`ablation_without_top1` must include `dedup_keys`: {ablation}"
        );
        assert!(
            ablation.get("total_chars").is_some()
                || ablation.get("chars").is_some(),
            "`ablation_without_top1` must include a char total (`total_chars` or `chars`): {ablation}"
        );
    }

    /// RED test for Plan Task 11 Step 3 — `facts_recall` must gain a vector
    /// arm: when the caller passes a query embedding, facts that share NO
    /// token with the query but sit near it in embedding space must surface,
    /// fused (RRF) with the FTS arm. `None` keeps today's FTS-only behavior.
    #[test]
    fn facts_recall_fuses_knn_arm_with_fts() {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();

        // Fact A: FTS-matchable by the query tokens ("vector", "store").
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('fa','project:hex','uses','sqlite-vec for the vector store',0.9,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        // Fact B: shares NO token with the query — only the KNN arm can find
        // it. Synthetic embedding identical to the query vector (distance 0,
        // safely under KNN_MAX_DISTANCE).
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('fb','person:bob','prefers','zzqx qqzz',0.9,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        let qv = vec![0.1f32; crate::memory::vector::EMBED_DIM];
        crate::memory::vector::insert_fact_vec(&c, "fb", &qv).unwrap();

        // FTS-only: B is invisible.
        let fts_only = facts_recall(&c, "what powers the vector store", 5, None).unwrap();
        assert!(
            fts_only.iter().any(|f| f.subject == "project:hex"),
            "FTS arm must still surface the keyword match"
        );
        assert!(
            !fts_only.iter().any(|f| f.subject == "person:bob"),
            "without a query vector the KNN-only fact must NOT appear"
        );

        // Fused: both arms contribute.
        let fused = facts_recall(&c, "what powers the vector store", 5, Some(&qv)).unwrap();
        assert!(
            fused.iter().any(|f| f.subject == "project:hex"),
            "FTS hit must survive fusion"
        );
        assert!(
            fused.iter().any(|f| f.subject == "person:bob" && f.object == "zzqx qqzz"),
            "KNN arm must surface the semantically-near fact, got {:?}",
            fused.iter().map(|f| &f.subject).collect::<Vec<_>>()
        );
    }

    /// Tombstoned facts must not leak through the KNN arm even when their
    /// vector is still present in facts_vec (sweep happens weekly, not live).
    #[test]
    fn facts_recall_knn_arm_skips_tombstoned() {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,tombstone)
             VALUES ('fd','person:dead','was','zzqx qqzz',0.9,'2026-06-11','2026-06-11',1)",
            [],
        )
        .unwrap();
        let qv = vec![0.1f32; crate::memory::vector::EMBED_DIM];
        crate::memory::vector::insert_fact_vec(&c, "fd", &qv).unwrap();

        let fused = facts_recall(&c, "anything relevant here at all", 5, Some(&qv)).unwrap();
        assert!(
            !fused.iter().any(|f| f.subject == "person:dead"),
            "tombstoned fact must not surface via the KNN arm"
        );
    }

    #[test]
    fn recall_returns_facts_alongside_chunks() {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('f1','person:alice','is','a sample person',0.95,'2026-05-23','2026-05-23')",
            [],
        )
        .unwrap();
        let recall = recall_with_facts(&c, "who is alice").unwrap();
        assert!(
            recall.facts.iter().any(|f| f.subject == "person:alice"),
            "expected person:alice fact in recall results"
        );
    }
}
