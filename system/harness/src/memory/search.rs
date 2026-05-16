use rusqlite::{params, Connection};
use std::collections::BTreeSet;
use std::path::Path;

use super::db_path;

const PRIVATE_PREFIXES: &[&str] = &["me/", "people/", "raw/"];

pub struct SearchArgs {
    pub query: String,
    pub top: usize,
    pub file: Option<String>,
    pub compact: bool,
    pub context: Option<usize>,
    pub private: bool,
}

struct SearchResult {
    source_path: String,
    heading: String,
    #[allow(dead_code)]
    chunk_index: String,
    content: String,
    score: f64,
}

// Mirror Python's truncate(): trim to max_chars at a word boundary.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let slice = &text[..max_chars];
    if let Some(pos) = slice.rfind(' ') {
        format!("{}...", &slice[..pos])
    } else {
        format!("{}...", slice)
    }
}

// Case-insensitive highlight of a single term with ANSI bold-yellow.
fn highlight_term(text: &str, term: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_term = term.to_lowercase();
    if lower_term.is_empty() {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len() + 32);
    let mut start = 0;
    while let Some(pos) = lower_text[start..].find(lower_term.as_str()) {
        let abs = start + pos;
        result.push_str(&text[start..abs]);
        result.push_str("\x1b[1;33m");
        result.push_str(&text[abs..abs + lower_term.len()]);
        result.push_str("\x1b[0m");
        start = abs + lower_term.len();
    }
    result.push_str(&text[start..]);
    result
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
    let query_terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();
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

    // Build FTS5 query variants: phrase then AND (mirror Python)
    let queries_to_try: Vec<String> = if terms.len() > 1 {
        vec![
            format!("\"{}\"", terms.join(" ")),
            terms
                .iter()
                .map(|t| format!("\"{}\"", t))
                .collect::<Vec<_>>()
                .join(" "),
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
            "SELECT chunks.source_path, chunks.heading, chunks.chunk_index, chunks.content, \
             bm25(chunks) * COALESCE(cm.source_weight, 1.0) as score \
             FROM chunks \
             LEFT JOIN chunk_meta cm ON chunks.rowid = cm.chunk_rowid \
             WHERE chunks MATCH ?",
            "chunks.",
        )
    } else {
        (
            "SELECT source_path, heading, chunk_index, content, bm25(chunks) as score \
             FROM chunks \
             WHERE chunks MATCH ?",
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
                            source_path: row.get(0)?,
                            heading: row.get(1)?,
                            chunk_index: row.get(2)?,
                            content: row.get(3)?,
                            score: row.get(4)?,
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
                            source_path: row.get(0)?,
                            heading: row.get(1)?,
                            chunk_index: row.get(2)?,
                            content: row.get(3)?,
                            score: row.get(4)?,
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

pub fn run(hex_root: &Path, args: &SearchArgs) -> i32 {
    let db = db_path(hex_root);
    if !db.exists() {
        println!("No index found. Run memory_index.py first.");
        return 1;
    }

    let conn = match Connection::open(&db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open database: {}", e);
            return 1;
        }
    };

    let rows = match search_fts(&conn, &args.query, args.top, args.file.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Search failed: {}", e);
            return 1;
        }
    };

    // Privacy filter: exclude me/, people/, raw/ prefixes.
    let results: Vec<SearchResult> = if args.private {
        rows.into_iter()
            .filter(|r| {
                !PRIVATE_PREFIXES
                    .iter()
                    .any(|p| r.source_path.starts_with(p))
            })
            .collect()
    } else {
        rows
    };

    format_results(&results, args, &args.query);
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
            "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content) VALUES (1, ?, ?, 0, ?)",
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
        insert_chunk(&conn, "projects/foo.md", "Intro", "This is about Rust programming.", 1.0);
        insert_chunk(&conn, "projects/bar.md", "Overview", "Python scripting overview.", 1.2);

        let results = search_fts(&conn, "Rust programming", 10, None).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].source_path, "projects/foo.md");
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
        let score_a = results.iter().find(|r| r.source_path == "a.md").unwrap().score;
        let score_b = results.iter().find(|r| r.source_path == "b.md").unwrap().score;
        // b score should be ~2x more negative than a score.
        assert!((score_b / score_a - 2.0).abs() < 0.01, "scores: a={}, b={}", score_a, score_b);
    }

    #[test]
    fn test_privacy_filter() {
        let conn = setup_db();
        insert_chunk(&conn, "me/journal.md", "Notes", "private personal notes here", 1.0);
        insert_chunk(&conn, "projects/work.md", "Work", "private work project details", 1.0);
        insert_chunk(&conn, "people/alice.md", "Alice", "private contact information", 1.0);

        let rows = search_fts(&conn, "private", 10, None).unwrap();
        let args = SearchArgs {
            query: "private".to_string(),
            top: 10,
            file: None,
            compact: false,
            context: None,
            private: true,
        };

        let filtered: Vec<SearchResult> = rows
            .into_iter()
            .filter(|r| {
                !PRIVATE_PREFIXES
                    .iter()
                    .any(|p| r.source_path.starts_with(p))
            })
            .collect();

        // me/ and people/ filtered out, projects/ kept.
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source_path, "projects/work.md");

        // Suppress unused warning on args.
        drop(args);
    }

    #[test]
    fn test_file_filter() {
        let conn = setup_db();
        insert_chunk(&conn, "projects/alpha.md", "Alpha", "shared topic content", 1.0);
        insert_chunk(&conn, "projects/beta.md", "Beta", "shared topic content", 1.0);

        let results = search_fts(&conn, "shared topic", 10, Some("alpha")).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].source_path.contains("alpha"));
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
}
