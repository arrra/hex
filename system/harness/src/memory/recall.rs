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

pub struct RecallOutcome {
    pub injected: bool,
    pub gated: bool,
    pub result_count: usize,
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
            latency_ms: t0.elapsed().as_millis() as u64, context: String::new(),
        };
        log_and_emit(hex_root, &outcome);
        return outcome;
    }

    let db = super::db_path(hex_root);
    let results = match super::open_db(&db) {
        Ok(conn) => super::search::search_fts_public(&conn, query, TOP_K * 3, None)
            .unwrap_or_default(),
        Err(e) => {
            // Fail-fast: dependency unreachable → inject nothing, loudly.
            eprintln!("[memory recall] cannot open {}: {e}", db.display());
            vec![]
        }
    };

    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| !(for_agent && r.private))
        .take(TOP_K)
        .collect();

    let outcome = if filtered.is_empty() {
        RecallOutcome {
            injected: false, gated: false, result_count: 0,
            latency_ms: t0.elapsed().as_millis() as u64, context: String::new(),
        }
    } else {
        RecallOutcome {
            injected: true,
            gated: false,
            result_count: filtered.len(),
            latency_ms: t0.elapsed().as_millis() as u64,
            context: format_context(&filtered),
        }
    };
    log_and_emit(hex_root, &outcome);
    outcome
}

fn format_context(results: &[super::search::SearchResult]) -> String {
    let mut out = String::from(
        "## Relevant workspace memory\n\nThe following may be relevant to the current request \
         (retrieved from hex's memory index — verify before relying on it):\n\n",
    );
    for r in results {
        let snippet: String = r.content.chars().take(600).collect();
        out.push_str(&format!("### {} — {}\n{}\n\n", r.source_path, r.heading, snippet.trim()));
        if out.len() >= MAX_CONTEXT_CHARS {
            break;
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
            })
        );
    }

    let bus = hex::sse::SseBus::new();
    let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(hex_root));
    if let Ok(engine) = hex::events::EventEngine::new(hex_root, telemetry, bus) {
        engine.ingest(
            "memory.recall",
            &json!({
                "injected": o.injected, "gated": o.gated,
                "result_count": o.result_count, "latency_ms": o.latency_ms,
            }),
            "hex:memory",
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
