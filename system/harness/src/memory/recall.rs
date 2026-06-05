//! `hex memory recall` — fast, FTS5-only contextual retrieval for per-prompt
//! injection. No embedding model is loaded (keeps the UserPromptSubmit hook
//! inside its latency budget — spec §8). Appends a line to
//! `.hex/memory/recall-log.jsonl` for the nightly eval.

use serde_json::json;
use std::io::Write;
use std::path::Path;

const MIN_QUERY_CHARS: usize = 12;
const MAX_CONTEXT_CHARS: usize = 10_000; // spec §8 hard cap
const TOP_K: usize = 6;

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
    let facts = facts_recall(conn, query, 5)?;
    Ok(RecallV2 { chunks, facts })
}

fn chunks_recall(
    conn: &rusqlite::Connection,
    query: &str,
    k: usize,
) -> rusqlite::Result<Vec<Hit>> {
    super::search::search_fts_public(conn, query, k, None)
}

fn facts_recall(
    conn: &rusqlite::Connection,
    query: &str,
    k: usize,
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

    let mut hits: Vec<FactHit> = if fts_query.is_empty() {
        Vec::new()
    } else {
        conn.prepare(
            "SELECT f.subject, f.predicate, f.object, f.importance, f.private
             FROM facts_fts JOIN facts f ON f.rowid = facts_fts.rowid
             WHERE facts_fts MATCH ?1 AND f.tombstone = 0
             ORDER BY bm25(facts_fts), f.importance DESC LIMIT ?2",
        )?
        .query_map(rusqlite::params![fts_query, k as i64], |r| {
            Ok(FactHit {
                subject: r.get(0)?,
                predicate: r.get(1)?,
                object: r.get(2)?,
                importance: r.get(3)?,
                private: r.get::<_, i64>(4)? != 0,
            })
        })?
        .filter_map(Result::ok)
        .collect()
    };

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
        log_recall(hex_root, &outcome);
        return outcome;
    }

    let db = super::db_path(hex_root);
    let (filtered, facts): (Vec<super::search::SearchResult>, Vec<FactHit>) =
        match super::open_db(&db) {
            Ok(conn) => {
                // Route the hot path through the v1 ContextAssembler. assemble()
                // applies the for_agent private gate to facts moves (M2/M3/M4)
                // and to M1's chunk results internally, runs the 4 parallel
                // moves, and returns a merged candidate list under the char
                // budget (default MAX_CONTEXT_CHARS).
                let assembled = super::assemble::assemble(
                    &conn,
                    query,
                    for_agent,
                    MAX_CONTEXT_CHARS,
                );
                let mut chunks: Vec<super::search::SearchResult> = Vec::new();
                let mut fs: Vec<FactHit> = Vec::new();
                for cand in assembled.candidates {
                    match cand.kind {
                        super::assemble::CandidateKind::Chunk(c) => chunks.push(c),
                        super::assemble::CandidateKind::Fact(f) => fs.push(f),
                    }
                }
                (chunks, fs)
            }
            Err(e) => {
                // Fail-fast: dependency unreachable → inject nothing, loudly.
                eprintln!("[memory recall] cannot open {}: {e}", db.display());
                (vec![], vec![])
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
    log_recall(hex_root, &outcome);
    outcome
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
fn log_recall(hex_root: &Path, o: &RecallOutcome) {
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
