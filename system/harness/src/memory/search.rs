use rusqlite::{params, Connection};
use std::collections::BTreeSet;
use std::path::Path;

use super::db_path;

pub struct SearchArgs {
    pub query: String,
    pub top: usize,
    pub file: Option<String>,
    pub compact: bool,
    pub context: Option<usize>,
    pub private: bool,
}

pub struct SearchResult {
    pub rowid: i64,
    pub source_path: String,
    pub heading: String,
    #[allow(dead_code)]
    pub chunk_index: String,
    pub content: String,
    pub private: bool,
    pub score: f64,
}

// Mirror Python's truncate(): trim to max_chars at a word boundary.
// Uses char-safe slicing — text.len() is bytes, not chars; byte-slicing
// panics on any multibyte character (é, em-dash, curly quotes, CJK, …).
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    // SAFETY(string_slice): `end` is a byte index from char_indices().nth(),
    // so it is a UTF-8 char boundary — slicing there cannot split a char.
    #[allow(clippy::string_slice)]
    let slice = &text[..end];
    match slice.rfind(' ') {
        // SAFETY(string_slice): `pos` is the byte index of an ASCII space
        // (rfind(' ')), always a char boundary.
        #[allow(clippy::string_slice)]
        Some(pos) => format!("{}...", &slice[..pos]),
        None => format!("{}...", slice),
    }
}

// Case-insensitive highlight of a single term with ANSI bold-yellow.
//
// Matches against the ORIGINAL `text` (never a `to_lowercase()` copy): case
// folding can change UTF-8 byte length (e.g. 'İ' U+0130 is 2 bytes but folds to
// "i\u{307}", 3 bytes), so byte offsets found in a lowercased copy can land
// mid-character in `text` and panic. `ci_prefix_match_len` folds on the fly and
// only ever returns offsets that are char boundaries in `text` itself.
fn highlight_term(text: &str, term: &str) -> String {
    let lower_term = term.to_lowercase();
    if lower_term.is_empty() {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len() + 32);
    let mut cursor = 0usize;
    while cursor < text.len() {
        // SAFETY(string_slice): `cursor` starts at 0 and only ever advances by
        // a whole matched span (`n`, a char-boundary length) or a whole char
        // (`len_utf8()`), so it is always a UTF-8 char boundary in `text`.
        #[allow(clippy::string_slice)]
        let rest = &text[cursor..];
        match ci_prefix_match_len(rest, &lower_term) {
            Some(n) => {
                result.push_str("\x1b[1;33m");
                // SAFETY(string_slice): `n` is a char-boundary byte length
                // within `rest` returned by ci_prefix_match_len.
                #[allow(clippy::string_slice)]
                result.push_str(&rest[..n]);
                result.push_str("\x1b[0m");
                cursor += n;
            }
            None => {
                let ch = rest.chars().next().unwrap();
                result.push(ch);
                cursor += ch.len_utf8();
            }
        }
    }
    result
}

/// If `hay` case-insensitively starts with `lower_term` (already lowercased),
/// return the number of `hay` bytes the match consumes — always ending on a
/// char boundary in `hay`. Folds each source char of `hay` on the fly and
/// compares against `lower_term`'s char stream, so it never carries a byte
/// offset from a separately-lowercased copy (which could desync and panic).
fn ci_prefix_match_len(hay: &str, lower_term: &str) -> Option<usize> {
    let mut needle = lower_term.chars();
    let mut nc = needle.next()?;
    for (off, ch) in hay.char_indices() {
        for f in ch.to_lowercase() {
            if f != nc {
                return None;
            }
            match needle.next() {
                Some(n) => nc = n,
                None => return Some(off + ch.len_utf8()),
            }
        }
    }
    None
}

// Mirror Python's highlight_terms(): iterates query words, applies each.
fn highlight_terms(text: &str, query: &str) -> String {
    let mut result = text.to_string();
    for term in query.split_whitespace() {
        result = highlight_term(&result, term);
    }
    result
}

// Mirror Python's _print_context_content().
fn print_context_content(content: &str, query: &str, context_lines: usize) {
    let lines: Vec<&str> = content.split('\n').collect();
    let query_terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
    let mut matching: BTreeSet<usize> = BTreeSet::new();

    for (idx, line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        if query_terms.iter().any(|t| lower.contains(t.as_str())) {
            let lo = idx.saturating_sub(context_lines);
            let hi = (idx + context_lines + 1).min(lines.len());
            for j in lo..hi {
                matching.insert(j);
            }
        }
    }

    if !matching.is_empty() {
        let mut prev: i64 = -2;
        for &idx in &matching {
            if idx as i64 > prev + 1 {
                println!("    ...");
            }
            println!("    {}", highlight_terms(lines[idx], query));
            prev = idx as i64;
        }
    } else {
        let snippet = truncate(content, 500);
        for line in snippet.split('\n') {
            println!("    {}", line);
        }
    }
}

// Sanitize FTS5 special chars (mirror Python's re.sub(r'["*(){}^-~]', ' ', ...)).
fn sanitize_query(query: &str) -> Vec<String> {
    let s: String = query
        .trim()
        .chars()
        .map(|c| match c {
            '"' | '*' | '(' | ')' | '{' | '}' | '^' | '-' | '~' => ' ',
            other => other,
        })
        .collect();
    s.split_whitespace().map(str::to_string).collect()
}

fn search_fts(
    conn: &Connection,
    query: &str,
    top: usize,
    file_filter: Option<&str>,
) -> rusqlite::Result<Vec<SearchResult>> {
    let terms = sanitize_query(query);
    if terms.is_empty() {
        return Ok(vec![]);
    }

    // FTS5 query variants, tried in order until one yields a match (see the
    // loop below): exact phrase → all terms AND-ed → any term OR-ed. The OR
    // fallback is what lets a natural-language prompt retrieve anything — the
    // recall hook and the nightly eval depend on it. Each term is quoted, so a
    // term that looks like an FTS5 keyword stays a literal; BM25 (ORDER BY
    // score) ranks the OR matches, so chunks hitting more/rarer terms still win.
    let queries_to_try: Vec<String> = if terms.len() > 1 {
        let quoted: Vec<String> = terms.iter().map(|t| format!("\"{}\"", t)).collect();
        vec![
            format!("\"{}\"", terms.join(" ")),
            quoted.join(" "),
            quoted.join(" OR "),
        ]
    } else {
        vec![format!("\"{}\"", terms[0])]
    };

    // Detect chunk_meta for source_weight (backwards compat with old DBs).
    let has_meta: bool = conn
        .query_row(
            "SELECT COUNT(1) FROM sqlite_master WHERE type='table' AND name='chunk_meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    let (sql_base, col_prefix) = if has_meta {
        (
            "SELECT chunks.rowid, chunks.source_path, chunks.heading, chunks.chunk_index, \
             chunks.content, chunks.private, \
             bm25(chunks) * COALESCE(cm.source_weight, 1.0) as score \
             FROM chunks \
             LEFT JOIN chunk_meta cm ON chunks.rowid = cm.chunk_rowid \
             WHERE chunks MATCH ?",
            "chunks.",
        )
    } else {
        (
            "SELECT rowid, source_path, heading, chunk_index, content, private, \
             bm25(chunks) as score FROM chunks WHERE chunks MATCH ?",
            "",
        )
    };

    let filter_clause = if file_filter.is_some() {
        format!(" AND {}source_path LIKE ?", col_prefix)
    } else {
        String::new()
    };

    let full_sql = format!("{}{} ORDER BY score LIMIT ?", sql_base, filter_clause);

    let mut results: Vec<SearchResult> = vec![];
    for fts_q in &queries_to_try {
        let query_result: rusqlite::Result<Vec<SearchResult>> = match file_filter {
            Some(ff) => {
                let filter_pattern = format!("%{}%", ff);
                let mut stmt = conn.prepare(&full_sql)?;
                let rows: rusqlite::Result<Vec<SearchResult>> = stmt
                    .query_map(params![fts_q, filter_pattern, top as i64], |row| {
                        Ok(SearchResult {
                            rowid: row.get(0)?,
                            source_path: row.get(1)?,
                            heading: row.get(2)?,
                            chunk_index: row.get(3)?,
                            content: row.get(4)?,
                            private: row.get::<_, i64>(5)? != 0,
                            score: row.get(6)?,
                        })
                    })?
                    .collect();
                rows
            }
            None => {
                let mut stmt = conn.prepare(&full_sql)?;
                let rows: rusqlite::Result<Vec<SearchResult>> = stmt
                    .query_map(params![fts_q, top as i64], |row| {
                        Ok(SearchResult {
                            rowid: row.get(0)?,
                            source_path: row.get(1)?,
                            heading: row.get(2)?,
                            chunk_index: row.get(3)?,
                            content: row.get(4)?,
                            private: row.get::<_, i64>(5)? != 0,
                            score: row.get(6)?,
                        })
                    })?
                    .collect();
                rows
            }
        };

        match query_result {
            Ok(rows) if !rows.is_empty() => {
                results = rows;
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("Search error: {}", e);
                continue;
            }
        }
    }

    Ok(results)
}

/// Public entry to the FTS5 arm for other memory modules (e.g. recall).
pub fn search_fts_public(
    conn: &Connection,
    query: &str,
    top: usize,
    file_filter: Option<&str>,
) -> rusqlite::Result<Vec<SearchResult>> {
    search_fts(conn, query, top, file_filter)
}

/// Fetch chunk rows by rowid, returned in the exact order of `rowids`.
/// No `bm25()` here — it is only valid inside a `MATCH` query. Post-fusion the
/// RRF score is what matters; `run` attaches it after this call.
fn fetch_chunks_by_rowid(conn: &Connection, rowids: &[i64]) -> rusqlite::Result<Vec<SearchResult>> {
    if rowids.is_empty() {
        return Ok(vec![]);
    }
    let ph: String = rowids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT rowid, source_path, heading, chunk_index, content, private \
         FROM chunks WHERE rowid IN ({ph})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut by_rowid: std::collections::HashMap<i64, SearchResult> =
        std::collections::HashMap::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(rowids.iter()), |r| {
        Ok(SearchResult {
            rowid: r.get(0)?,
            source_path: r.get(1)?,
            heading: r.get(2)?,
            chunk_index: r.get(3)?,
            content: r.get(4)?,
            private: r.get::<_, i64>(5)? != 0,
            score: 0.0,
        })
    })?;
    for row in rows {
        let r = row?;
        by_rowid.insert(r.rowid, r);
    }
    Ok(rowids.iter().filter_map(|id| by_rowid.remove(id)).collect())
}

// Mirror Python's format_results().
fn format_results(results: &[SearchResult], args: &SearchArgs, query: &str) {
    if results.is_empty() {
        println!("No results for: {}", query);
        return;
    }

    println!();
    println!("{}", "=".repeat(60));
    println!(" Memory Search: \"{}\" — {} results", query, results.len());
    println!("{}", "=".repeat(60));
    println!();

    for (i, r) in results.iter().enumerate() {
        if args.compact {
            let snippet = truncate(&r.content.replace('\n', " "), 100);
            println!(
                "  [{}] {} > {}  (score: {:.2})",
                i + 1,
                r.source_path,
                r.heading,
                r.score
            );
            println!("      {}", snippet);
            println!();
        } else {
            println!("--- Result {} ---", i + 1);
            println!("  File:    {}", r.source_path);
            println!("  Section: {}", r.heading);
            println!("  Score:   {:.2}", r.score);
            println!("  Content:");
            if let Some(ctx) = args.context {
                print_context_content(&r.content, query, ctx);
            } else {
                let snippet = truncate(&r.content, 500);
                for line in snippet.split('\n') {
                    println!("    {}", line);
                }
            }
            println!();
        }
    }

    if results.len() == args.top {
        println!("(Showing top {}. Use --top N to see more.)", args.top);
    }
}

/// How many facts the facts arm contributes to `hex memory search` output.
const FACTS_TOP_K: usize = 5;

/// Core retrieval for `hex memory search`: the FTS+KNN chunk fusion plus the
/// facts arm (`facts_recall` — FTS, fused with KNN over `facts_vec` when a
/// query embedding is supplied). Split from [`run`] so tests can drive it
/// with a synthetic query vector.
///
/// Review-fix 2026-06-11 (findings 1/4/6): the facts KNN arm previously had
/// NO production caller passing a vector — `facts_vec` was populated weekly
/// and read by nothing. `run` embeds the query ONCE and the same vector
/// feeds both the chunk KNN arm and the facts KNN arm (plan Task 11 Step 3).
pub(crate) fn run_query(
    conn: &Connection,
    args: &SearchArgs,
    query_vec: Option<&[f32]>,
) -> (Vec<SearchResult>, Vec<super::recall::FactHit>) {
    // FTS5 arm — keeps bm25 * source_weight as its pre-RRF rank input (spec §7).
    let fts =
        search_fts(conn, &args.query, args.top.max(20), args.file.as_deref()).unwrap_or_default();
    let fts_rowids: Vec<i64> = fts.iter().map(|r| r.rowid).collect();

    // Vector arm — best-effort. If KNN fails, log loud and fall back to
    // FTS5-only (never silently degrade to nothing).
    let vec_rowids: Vec<i64> = match query_vec {
        Some(qv) => super::vector::knn(conn, qv, args.top.max(20))
            .map(|hits| hits.into_iter().map(|(id, _)| id).collect())
            .unwrap_or_else(|e| {
                eprintln!("vector arm failed: {e}");
                vec![]
            }),
        None => vec![],
    };

    let fused = super::rrf::rrf_fuse(&[fts_rowids, vec_rowids], super::rrf::RRF_K);
    let fused_rowids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
    let mut results = fetch_chunks_by_rowid(conn, &fused_rowids).unwrap_or_default();

    // Attach the RRF score to each result for display.
    let scores: std::collections::HashMap<i64, f64> = fused.iter().copied().collect();
    for r in &mut results {
        r.score = scores.get(&r.rowid).copied().unwrap_or(0.0);
    }

    // Facts arm — reuses the already-computed query vector for its KNN side.
    // Privacy is applied post-fusion below alongside the chunk arms, so the
    // retrieval itself runs unfiltered (exclude_private = false).
    let mut facts: Vec<super::recall::FactHit> =
        super::recall::facts_recall(conn, &args.query, FACTS_TOP_K, query_vec, false)
            .map(|hits| hits.into_iter().map(|(f, _)| f).collect())
            .unwrap_or_else(|e| {
                eprintln!("facts arm failed: {e}");
                vec![]
            });

    // --file path filter, applied POST-fusion so it covers every arm. The FTS
    // arm filters in SQL (search_fts), but the vector and facts arms do NOT —
    // RRF fuses filtered + unfiltered rowids, so vector/facts hits leaked past
    // `--file` (2026-06-12: `--file me/decisions` returned CLAUDE.md/AGENTS.md).
    // Retain only results whose source_path matches, case-insensitively to align
    // with FTS's ASCII-case-insensitive `LIKE '%ff%'`. (Not a full LIKE: `_`/`%`
    // are matched literally, not as wildcards — a deliberate, safe narrowing.)
    // Facts carry no path, so a path filter excludes them.
    // Limitation: each arm fetches top.max(20) BEFORE this filter, so a query
    // whose in-path hits are only vector-discoverable can return < top results
    // (acceptable vs. the leak; a tighter future fix filters inside each arm).
    if let Some(ff) = args.file.as_deref() {
        let needle = ff.to_ascii_lowercase();
        results.retain(|r| r.source_path.to_ascii_lowercase().contains(&needle));
        facts.clear();
    }

    // Privacy filter — the index-time `private` column (spec §7), applied to
    // chunks and facts alike.
    if args.private {
        results.retain(|r| !r.private);
        facts.retain(|f| !f.private);
    }
    results.truncate(args.top);

    (results, facts)
}

fn format_facts(facts: &[super::recall::FactHit]) {
    if facts.is_empty() {
        return;
    }
    println!();
    println!("Facts:");
    for f in facts {
        println!("  - {} {} {}", f.subject, f.predicate, f.object);
    }
}

pub fn run(hex_root: &Path, args: &SearchArgs) -> i32 {
    let db = db_path(hex_root);
    if !db.exists() {
        println!("No index found. Run `hex memory index` first.");
        return 1;
    }
    let conn = match super::open_db(&db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open database: {e}");
            return 1;
        }
    };

    // Embed the query ONCE — the same vector feeds the chunk KNN arm and the
    // facts KNN arm. If the model fails, log loud and fall back to FTS5-only.
    let query_vec: Option<Vec<f32>> =
        match super::embed::Embedder::new(hex_root).and_then(|e| e.embed_query(&args.query)) {
            Ok(qv) => Some(qv),
            Err(e) => {
                eprintln!("query embedding failed, FTS5-only: {e}");
                None
            }
        };

    let (results, facts) = run_query(&conn, args, query_vec.as_deref());

    format_results(&results, args, &args.query);
    format_facts(&facts);
    0
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE chunks USING fts5(
                file_id UNINDEXED,
                source_path UNINDEXED,
                heading,
                chunk_index UNINDEXED,
                content,
                private UNINDEXED,
                tokenize='unicode61'
            );
            CREATE TABLE chunk_meta (
                chunk_rowid INTEGER PRIMARY KEY,
                source_weight REAL NOT NULL DEFAULT 1.0
            );",
        )
        .unwrap();
        conn
    }

    fn insert_chunk(conn: &Connection, path: &str, heading: &str, content: &str, weight: f64) {
        conn.execute(
            "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content, private) VALUES ('1', ?, ?, '0', ?, 0)",
            params![path, heading, content],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunk_meta (chunk_rowid, source_weight) VALUES (?, ?)",
            params![rowid, weight],
        )
        .unwrap();
    }

    /// Build the full production-shaped fixture: Plan 2 schema (facts,
    /// facts_fts, facts_vec) + the chunks FTS5 vtable + vec_chunks.
    fn setup_full_db() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE chunks USING fts5(
                file_id UNINDEXED,
                source_path UNINDEXED,
                heading,
                chunk_index UNINDEXED,
                content,
                private UNINDEXED,
                tokenize='unicode61'
            );
            CREATE TABLE chunk_meta (
                chunk_rowid INTEGER PRIMARY KEY,
                source_weight REAL NOT NULL DEFAULT 1.0
            );",
        )
        .unwrap();
        crate::memory::vector::init_vec_table(&conn).unwrap();
        conn
    }

    fn search_args(query: &str, private: bool) -> SearchArgs {
        SearchArgs {
            query: query.to_string(),
            top: 5,
            file: None,
            compact: false,
            context: None,
            private,
        }
    }

    /// Review-fix 2026-06-11 (findings 1/4/6): the facts KNN arm was dead
    /// code — no production caller ever passed a query vector. `hex memory
    /// search` already embeds the query for the chunk arm; `run_query` (the
    /// core `run` delegates to) must hoist that SAME vector into
    /// `facts_recall` so facts fuse FTS + KNN (plan Task 11 Step 3).
    #[test]
    fn run_query_passes_query_vector_to_facts_arm() {
        let conn = setup_full_db();

        // Fact A: FTS-matchable by the query tokens. Fact B: shares NO token
        // with the query — only the KNN arm can surface it.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('fa','project:hex','uses','sqlite-vec for the vector store',0.9,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('fb','person:bob','prefers','zzqx qqzz',0.9,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();
        let qv = vec![0.1f32; crate::memory::vector::EMBED_DIM];
        crate::memory::vector::insert_fact_vec(&conn, "fb", &qv).unwrap();

        let args = search_args("what powers the vector store", false);

        // No query vector ⇒ FTS-only facts: the KNN-only fact stays invisible.
        let (_chunks, facts_fts_only) = run_query(&conn, &args, None);
        assert!(
            facts_fts_only.iter().any(|f| f.subject == "project:hex"),
            "FTS facts arm must work without an embedder"
        );
        assert!(
            !facts_fts_only.iter().any(|f| f.subject == "person:bob"),
            "without a query vector the KNN-only fact must not surface"
        );

        // With the (hoisted) query vector, both arms contribute.
        let (_chunks, facts) = run_query(&conn, &args, Some(&qv));
        assert!(
            facts.iter().any(|f| f.subject == "project:hex"),
            "FTS fact must survive fusion"
        );
        assert!(
            facts.iter().any(|f| f.subject == "person:bob"),
            "search must pass the query vector through to the facts KNN arm, got {:?}",
            facts.iter().map(|f| &f.subject).collect::<Vec<_>>()
        );
    }

    /// `--private` must filter private facts exactly as it filters chunks.
    #[test]
    fn run_query_private_flag_filters_private_facts() {
        let conn = setup_full_db();
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,private)
             VALUES ('fp','me:secret','prefers','the vector store stays hidden',0.9,'2026-06-11','2026-06-11',1)",
            [],
        )
        .unwrap();

        let (_c, facts) = run_query(&conn, &search_args("vector store", false), None);
        assert!(
            facts.iter().any(|f| f.subject == "me:secret"),
            "without --private the fact is visible"
        );

        let (_c, facts) = run_query(&conn, &search_args("vector store", true), None);
        assert!(
            !facts.iter().any(|f| f.subject == "me:secret"),
            "--private must filter private facts"
        );
    }

    /// `--file` must filter results from EVERY arm, not just FTS. The vector
    /// arm previously bypassed the filter, so a KNN hit outside the path leaked
    /// (2026-06-12: `--file me/decisions` returned CLAUDE.md/AGENTS.md). Facts
    /// carry no path, so a path filter excludes them entirely.
    #[test]
    fn run_query_file_filter_covers_vector_arm_and_facts() {
        let conn = setup_full_db();

        // me/decisions doc: FTS-matchable by the query.
        insert_chunk(
            &conn,
            "me/decisions/d.md",
            "decision",
            "alpha decision record",
            1.0,
        );
        // CLAUDE.md doc: shares NO query token (FTS won't surface it) but gets
        // the query vector, so ONLY the vector arm can surface it — the leak.
        insert_chunk(&conn, "CLAUDE.md", "footguns", "zzqx qqzz unrelated", 1.0);
        let claude_rowid = conn.last_insert_rowid();
        let qv = vec![0.1f32; crate::memory::vector::EMBED_DIM];
        crate::memory::vector::insert_vec(&conn, claude_rowid, &qv).unwrap();

        // A fact that FTS-matches the query — must be excluded once --file is set.
        conn.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at)
             VALUES ('fd','project:x','has','alpha decision',0.9,'2026-06-11','2026-06-11')",
            [],
        )
        .unwrap();

        let args = SearchArgs {
            query: "alpha decision".to_string(),
            top: 10,
            file: Some("me/decisions".to_string()),
            compact: false,
            context: None,
            private: false,
        };
        let (results, facts) = run_query(&conn, &args, Some(&qv));

        assert!(
            results
                .iter()
                .all(|r| r.source_path.contains("me/decisions")),
            "--file must exclude non-matching paths from ALL arms, got {:?}",
            results.iter().map(|r| &r.source_path).collect::<Vec<_>>()
        );
        assert!(
            !results.iter().any(|r| r.source_path == "CLAUDE.md"),
            "a vector-arm hit outside the path must not leak past --file"
        );
        assert!(
            facts.is_empty(),
            "--file (a path filter) must exclude path-less facts"
        );
    }

    #[test]
    fn test_sanitize_query() {
        let terms = sanitize_query("hello \"world\" (test)");
        assert_eq!(terms, vec!["hello", "world", "test"]);

        let terms = sanitize_query("foo-bar ^baz~");
        assert_eq!(terms, vec!["foo", "bar", "baz"]);

        let terms = sanitize_query("simple");
        assert_eq!(terms, vec!["simple"]);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 20), "hello world");
        let t = truncate("hello world foo bar", 12);
        assert!(t.ends_with("..."));
        assert!(t.len() <= 15);
    }

    #[test]
    fn test_highlight_terms() {
        let out = highlight_terms("The quick brown fox", "quick fox");
        assert!(out.contains("\x1b[1;33m"));
        assert!(out.contains("quick"));
        assert!(out.contains("fox"));
    }

    #[test]
    fn test_schema_query() {
        let conn = setup_db();
        let results = search_fts(&conn, "nonexistent", 10, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_returns_results() {
        let conn = setup_db();
        insert_chunk(
            &conn,
            "projects/foo.md",
            "Intro",
            "This is about Rust programming.",
            1.0,
        );
        insert_chunk(
            &conn,
            "projects/bar.md",
            "Overview",
            "Python scripting overview.",
            1.2,
        );

        let results = search_fts(&conn, "Rust programming", 10, None).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].source_path, "projects/foo.md");
    }

    #[test]
    fn search_fts_or_fallback_matches_scattered_terms() {
        let conn = setup_db();
        // No chunk contains every query term, and the query is not a verbatim
        // phrase anywhere — the exact-phrase and all-terms-AND variants both
        // miss. Only an OR fallback can surface these. This is the natural-
        // language recall case the UserPromptSubmit hook actually sees.
        insert_chunk(
            &conn,
            "a.md",
            "Deploy",
            "the deployment pipeline runs nightly",
            1.0,
        );
        insert_chunk(
            &conn,
            "b.md",
            "Schema",
            "we chose a vector schema for embeddings",
            1.0,
        );

        let results =
            search_fts(&conn, "what schema did we pick for deployment", 10, None).unwrap();
        assert!(
            !results.is_empty(),
            "OR fallback should surface chunks matching only some query terms"
        );
    }

    #[test]
    fn test_source_weight_applied() {
        let conn = setup_db();
        // Both match "memory", but bar has higher weight.
        insert_chunk(&conn, "a.md", "A", "memory recall information", 1.0);
        insert_chunk(&conn, "b.md", "B", "memory recall information", 2.0);

        let results = search_fts(&conn, "memory recall", 10, None).unwrap();
        // b.md has weight 2.0 so its score is more negative (better in BM25 ordering).
        assert_eq!(results.len(), 2);
        // Verify scores differ by weight factor.
        let score_a = results
            .iter()
            .find(|r| r.source_path == "a.md")
            .unwrap()
            .score;
        let score_b = results
            .iter()
            .find(|r| r.source_path == "b.md")
            .unwrap()
            .score;
        // b score should be ~2x more negative than a score.
        assert!(
            (score_b / score_a - 2.0).abs() < 0.01,
            "scores: a={}, b={}",
            score_a,
            score_b
        );
    }

    #[test]
    fn test_privacy_filter() {
        let conn = setup_db();
        // Insert one private row (private=1) and one public row (private=0).
        conn.execute(
            "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content, private) \
             VALUES (1, ?, ?, '0', ?, ?)",
            params![
                "me/journal.md",
                "Notes",
                "private personal notes here",
                1i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content, private) \
             VALUES (1, ?, ?, '0', ?, ?)",
            params![
                "projects/work.md",
                "Work",
                "private work project details",
                0i64
            ],
        )
        .unwrap();

        let mut results = search_fts(&conn, "private", 10, None).unwrap();
        // Apply column-based privacy filter (same logic as `run`).
        results.retain(|r| !r.private);

        // private=1 row dropped, private=0 row kept.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_path, "projects/work.md");
    }

    #[test]
    fn test_file_filter() {
        let conn = setup_db();
        insert_chunk(
            &conn,
            "projects/alpha.md",
            "Alpha",
            "shared topic content",
            1.0,
        );
        insert_chunk(
            &conn,
            "projects/beta.md",
            "Beta",
            "shared topic content",
            1.0,
        );

        let results = search_fts(&conn, "shared topic", 10, Some("alpha")).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].source_path.contains("alpha"));
    }

    #[test]
    fn test_truncate_multibyte_unicode() {
        // "café" is 4 chars but 5 bytes (é = 2 bytes, at byte indices 3-4).
        // The old code's `&text[..max_chars]` byte-slice panics when max_chars
        // lands inside a multibyte char. Verify no panic + sane output.
        let s = "café menu items";
        // max_chars=4 makes the old `&text[..4]` end inside 'é' (bytes 3-4) —
        // this subcase panics against the pre-fix code. (max_chars=5 would not:
        // byte 5 is the space, a valid boundary.)
        let t = truncate(s, 4);
        assert!(t.ends_with("..."), "expected trailing '...', got: {:?}", t);
        // Result is valid UTF-8 (round-tripping through String proves it) and
        // no longer than max_chars characters.
        let char_count = t.trim_end_matches("...").chars().count();
        assert!(char_count <= 4, "char_count={char_count}");

        // Em-dash (3 bytes): max_chars=4 puts the old byte cut mid-em-dash too.
        let em = "foo\u{2014}bar baz"; // em-dash = U+2014 = 3 bytes
        let te = truncate(em, 4); // 4 chars: 'f','o','o','—'
        assert!(te.ends_with("...") || te == "foo\u{2014}bar baz");

        // Strings short enough must be returned unchanged (no '...').
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn test_highlight_term_multibyte_casefold_mismatch() {
        // Case-folding can change UTF-8 byte length: 'İ' (U+0130, LATIN
        // CAPITAL LETTER I WITH DOT ABOVE) is 2 bytes but lowercases to
        // "i\u{307}" (3 bytes). highlight_term() searches for the match
        // position in `lower_text` (the case-folded copy) but then slices
        // into the *original* `text` at that same byte offset. Once a
        // byte-length-changing char precedes another multibyte char, the
        // offset from `lower_text` desyncs from `text`'s char boundaries and
        // can land mid-character, panicking on the old code:
        // "İÉhello" (İ=2 bytes, É=2 bytes) lowercases to "i\u{307}éhello"
        // (i\u{307}=3 bytes, é=2 bytes) — searching for "É" finds it at byte
        // offset 3 in the lowercased copy, but byte offset 3 in the original
        // text falls inside É's own 2-byte encoding (bytes 2..4).
        let text = "İÉhello";
        let out = highlight_term(text, "É");
        // No panic, valid UTF-8 (guaranteed by returning a String), and the
        // matched term is still present in the output, wrapped for highlight.
        assert!(
            out.contains('É'),
            "expected matched char preserved in output, got: {:?}",
            out
        );
        assert!(
            out.contains("hello"),
            "expected trailing text preserved, got: {:?}",
            out
        );
    }

    #[test]
    fn test_output_format_compact() {
        // Smoke test: format_results doesn't panic on empty.
        let args = SearchArgs {
            query: "test".to_string(),
            top: 10,
            file: None,
            compact: true,
            context: None,
            private: false,
        };
        format_results(&[], &args, "test");
    }

    #[test]
    fn test_fetch_chunks_by_rowid_preserves_order() {
        let conn = setup_db();
        insert_chunk(&conn, "a.md", "A", "alpha", 1.0);
        insert_chunk(&conn, "b.md", "B", "beta", 1.0);
        insert_chunk(&conn, "c.md", "C", "gamma", 1.0);
        // request rowids out of natural order
        let got = fetch_chunks_by_rowid(&conn, &[3, 1]).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].source_path, "c.md");
        assert_eq!(got[1].source_path, "a.md");
    }
}
