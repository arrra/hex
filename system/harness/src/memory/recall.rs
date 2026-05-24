//! `hex memory recall` — fast, FTS5-only contextual retrieval for per-prompt
//! injection. No embedding model is loaded (keeps the UserPromptSubmit hook
//! inside its latency budget — spec §8). Emits a `memory.recall` event and
//! appends a line to `.hex/memory/recall-log.jsonl` for the nightly eval.

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
    // Drop stopwords and OR the remaining alphanumerics so "who is whitney" hits
    // facts mentioning whitney.
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
            "SELECT f.subject, f.predicate, f.object, f.importance
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
            })
        })?
        .filter_map(Result::ok)
        .collect()
    };

    // Slug boost: for each token in the query, surface facts whose subject
    // contains `:<token>` after the type prefix (e.g. "whitney" → subject
    // LIKE '%:whitney%' matches both `person:whitney` and `person:whitney-chew`).
    for tok in query.to_lowercase().split_whitespace() {
        if tok.len() < 3 { continue; }
        let pattern = format!("%:{tok}%");
        let mut q = conn.prepare(
            "SELECT subject, predicate, object, importance FROM facts
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
/// (fleet agents get non-private chunks only — spec §7).
pub fn recall(hex_root: &Path, query: &str, for_agent: bool) -> RecallOutcome {
    let t0 = std::time::Instant::now();

    if is_trivial(query) {
        let outcome = RecallOutcome {
            injected: false, gated: true, result_count: 0,
            facts_injected: 0, chunks_injected: 0,
            latency_ms: t0.elapsed().as_millis() as u64, context: String::new(),
        };
        log_and_emit(hex_root, &outcome);
        return outcome;
    }

    let db = super::db_path(hex_root);
    let (results, facts) = match super::open_db(&db) {
        Ok(conn) => {
            let chunks = super::search::search_fts_public(&conn, query, TOP_K * 3, None)
                .unwrap_or_default();
            let f = facts_recall(&conn, query, TOP_K).unwrap_or_default();
            (chunks, f)
        }
        Err(e) => {
            // Fail-fast: dependency unreachable → inject nothing, loudly.
            eprintln!("[memory recall] cannot open {}: {e}", db.display());
            (vec![], vec![])
        }
    };

    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| !(for_agent && r.private))
        .take(TOP_K)
        .collect();

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
    log_and_emit(hex_root, &outcome);
    outcome
}

fn format_context(results: &[super::search::SearchResult]) -> String {
    format_context_v2(results, &[])
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

/// Append a JSONL line and emit a `memory.recall` event. The JSONL is the
/// eval's data source (no telemetry-schema coupling); the event feeds
/// hex-events. Both are best-effort and never panic.
fn log_and_emit(hex_root: &Path, o: &RecallOutcome) {
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

    // Gate EventEngine on injected — the hot path runs on every prompt; building
    // EventEngine (reads config files) is only worth it when we actually injected.
    // The JSONL append above is the eval's complete data source for both injected
    // and non-injected recalls — do NOT gate that write on o.injected.
    if o.injected {
        let bus = crate::sse::SseBus::new();
        let telemetry = std::sync::Arc::new(crate::telemetry::Telemetry::new(hex_root));
        match crate::events::EventEngine::new(hex_root, telemetry, bus) {
            Ok(engine) => {
                engine.ingest(
                    "memory.recall",
                    &json!({
                        "injected": o.injected, "gated": o.gated,
                        "result_count": o.result_count, "latency_ms": o.latency_ms,
                    }),
                    "hex:memory",
                );
            }
            Err(e) => eprintln!("[memory recall] could not emit recall event: {e}"),
        }
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

    #[test]
    fn recall_returns_facts_alongside_chunks() {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('f1','person:whitney','is','Mike''s wife',0.95,'2026-05-23','2026-05-23')",
            [],
        )
        .unwrap();
        let recall = recall_with_facts(&c, "who is whitney").unwrap();
        assert!(
            recall.facts.iter().any(|f| f.subject == "person:whitney"),
            "expected person:whitney fact in recall results"
        );
    }
}
