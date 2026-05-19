use chrono::{Local, NaiveDate};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Constants (mirror Python) ─────────────────────────────────────────────────

const MAX_CHUNK_WORDS: usize = 400;
const OVERLAP_WORDS: usize = 80;
const TRANSCRIPT_HOT_DAYS: i64 = 7;

const INDEX_DIRS: &[&str] = &[".", "me", "projects", "people", "evolution", "landings"];

const SKIP_PATTERNS: &[&str] = &[".hex", ".claude", ".sessions", "node_modules", ".git"];

// (subdir, strategy): "full" | "tiered" | "exclude"
const TIERED_RAW_DIRS: &[(&str, &str)] = &[
    ("raw/research", "full"),
    ("raw/captures", "full"),
    ("raw/transcripts", "tiered"),
    ("raw/reflect-runs", "exclude"),
    ("raw/handoffs", "exclude"),
    ("raw/reflections", "exclude"),
    ("raw/docs", "full"),
    ("raw/meeting-prep", "full"),
    ("raw/messages", "full"),
    ("raw/calendar", "full"),
];

// ── Chunk struct ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Chunk {
    pub heading: String,
    pub content: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn file_mtime(path: &Path) -> f64 {
    path.metadata()
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64())
        .unwrap_or(0.0)
}

fn should_skip(rel_path: &str) -> bool {
    for pat in SKIP_PATTERNS {
        if rel_path.starts_with(pat) || rel_path.contains(&format!("/{}", pat)) {
            return true;
        }
    }
    false
}

/// Detect heading: returns (level, text) if line is a markdown heading (h1-h4).
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    if !line.starts_with('#') {
        return None;
    }
    let level = line.chars().take_while(|&c| c == '#').count();
    if level > 4 {
        return None;
    }
    let rest = &line[level..];
    if rest.starts_with(' ') && rest.len() > 1 {
        let text = rest[1..].trim();
        if !text.is_empty() {
            return Some((level, text));
        }
    }
    None
}

pub fn get_source_weight(rel_path: &str, is_old_transcript: bool) -> f64 {
    if rel_path.starts_with("raw/transcripts") {
        return if is_old_transcript { 0.3 } else { 0.5 };
    }
    if rel_path.starts_with("me/decisions/") {
        return 1.5;
    }
    if rel_path.starts_with("people/") {
        return 1.5;
    }
    if rel_path.starts_with("me/") {
        return 1.2;
    }
    if rel_path.starts_with("projects/") {
        return 1.2;
    }
    if rel_path.starts_with("evolution/") {
        return 1.2;
    }
    if rel_path.starts_with("landings/") {
        return 1.0;
    }
    if rel_path.starts_with("raw/research") {
        return 1.0;
    }
    if rel_path.starts_with("raw/captures") {
        return 0.8;
    }
    1.0
}

pub fn is_old_transcript(filepath: &Path) -> bool {
    let stem = filepath.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if let Ok(date) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
        let today = Local::now().date_naive();
        let age = (today - date).num_days();
        return age > TRANSCRIPT_HOT_DAYS;
    }
    // Fall back to mtime
    let age_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        - file_mtime(filepath);
    age_secs / 86400.0 > TRANSCRIPT_HOT_DAYS as f64
}

// ── Summary extraction ────────────────────────────────────────────────────────

const SUMMARY_HEADINGS: &[&str] = &[
    "summary",
    "session summary",
    "notes for next session",
    "tasks",
    "files modified",
    "stats",
];

/// Extract ECC:SUMMARY blocks and named heading sections from a transcript.
pub fn extract_summaries(content: &str) -> String {
    let mut extracted: Vec<String> = Vec::new();

    // ECC:SUMMARY block extraction (state machine — avoids regex dep)
    let cl = content.to_lowercase();
    let start_kw = "ecc:summary:start";
    let end_kw = "ecc:summary:end";
    let mut search_from = 0;
    while search_from < cl.len() {
        let Some(s_off) = cl[search_from..].find(start_kw) else {
            break;
        };
        let abs_s = search_from + s_off;
        // Find end of the <!-- ... --> comment that contains START
        let Some(comment_end_off) = content[abs_s..].find("-->") else {
            break;
        };
        let body_start = abs_s + comment_end_off + 3;

        // Find end marker
        let Some(e_off) = cl[body_start..].find(end_kw) else {
            break;
        };
        let abs_e = body_start + e_off;
        // Walk back to find <!-- that opens the end comment
        let comment_open = content[..abs_e].rfind("<!--").unwrap_or(abs_e);
        let block_text = content[body_start..comment_open].trim();
        if !block_text.is_empty() {
            extracted.push(block_text.to_string());
        }
        search_from = abs_e + end_kw.len() + 4; // skip past -->
    }

    // Heading-based extraction
    let lines: Vec<&str> = content.split('\n').collect();
    let mut in_summary = false;
    let mut summary_lines: Vec<&str> = Vec::new();

    for line in &lines {
        if let Some((level, heading_text)) = parse_heading(line) {
            if level <= 3 {
                if in_summary && !summary_lines.is_empty() {
                    let block = summary_lines.join("\n").trim().to_string();
                    if !block.is_empty() {
                        extracted.push(block);
                    }
                    summary_lines.clear();
                }
                in_summary = SUMMARY_HEADINGS
                    .iter()
                    .any(|h| heading_text.to_lowercase() == *h);
                if in_summary {
                    summary_lines.push(line);
                }
                continue;
            }
        }
        if in_summary {
            summary_lines.push(line);
        }
    }
    if in_summary && !summary_lines.is_empty() {
        let block = summary_lines.join("\n").trim().to_string();
        if !block.is_empty() {
            extracted.push(block);
        }
    }

    extracted.retain(|s| !s.is_empty());
    extracted.join("\n\n")
}

// ── Chunking ──────────────────────────────────────────────────────────────────

/// Split markdown content into chunks by heading, then by word count.
/// Mirrors Python's chunk_by_heading().
pub fn chunk_by_heading(content: &str, deduplicate: bool) -> Vec<Chunk> {
    let mut raw_chunks: Vec<Chunk> = Vec::new();
    let mut current_heading = "(top)".to_string();
    let mut current_lines: Vec<&str> = Vec::new();

    for line in content.split('\n') {
        if let Some((_level, heading_text)) = parse_heading(line) {
            if !current_lines.is_empty() {
                let text = current_lines.join("\n").trim().to_string();
                if !text.is_empty() {
                    raw_chunks.push(Chunk {
                        heading: current_heading.clone(),
                        content: text,
                    });
                }
            }
            current_heading = heading_text.to_string();
            current_lines = vec![line];
        } else {
            current_lines.push(line);
        }
    }
    if !current_lines.is_empty() {
        let text = current_lines.join("\n").trim().to_string();
        if !text.is_empty() {
            raw_chunks.push(Chunk {
                heading: current_heading,
                content: text,
            });
        }
    }

    // Split oversized chunks with overlap
    let mut split_chunks: Vec<Chunk> = Vec::new();
    for chunk in raw_chunks {
        let words: Vec<&str> = chunk.content.split_whitespace().collect();
        if words.len() <= MAX_CHUNK_WORDS {
            split_chunks.push(chunk);
        } else {
            let mut i = 0usize;
            let mut sub_idx = 0usize;
            while i < words.len() {
                let end = (i + MAX_CHUNK_WORDS).min(words.len());
                let sub_content = words[i..end].join(" ");
                let heading = if sub_idx == 0 {
                    chunk.heading.clone()
                } else {
                    format!("{} (part {})", chunk.heading, sub_idx + 1)
                };
                split_chunks.push(Chunk {
                    heading,
                    content: sub_content,
                });
                sub_idx += 1;
                i += MAX_CHUNK_WORDS - OVERLAP_WORDS;
            }
        }
    }

    if !deduplicate {
        return split_chunks;
    }

    // Dedup by (heading.lowercase(), sha256(content))
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut deduped: Vec<Chunk> = Vec::new();
    for chunk in split_chunks {
        let key = (chunk.heading.to_lowercase(), content_hash(&chunk.content));
        if seen.insert(key) {
            deduped.push(chunk);
        }
    }
    deduped
}

// ── Schema ────────────────────────────────────────────────────────────────────

pub fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    // pragma_update fails for journal_mode because it returns a result row.
    // Use query_row instead and discard the result.
    let _ = conn.query_row::<String, _, _>("PRAGMA journal_mode=WAL", [], |r| r.get(0));


    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT UNIQUE NOT NULL,
            mtime REAL NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            indexed_at TEXT NOT NULL,
            chunk_count INTEGER DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS chunk_meta (
            chunk_rowid INTEGER PRIMARY KEY,
            source_weight REAL NOT NULL DEFAULT 1.0
        );
        ",
    )?;

    // Schema migration: chunks FTS5 v0.1.0 → v0.2.0 (add file_id column)
    let chunks_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='chunks'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if chunks_exists {
        // Check for file_id column by preparing the query; prepare validates
        // column names on FTS5 virtual tables at prepare time.
        let file_id_ok = conn
            .prepare("SELECT file_id FROM chunks LIMIT 0")
            .is_ok();
        if !file_id_ok {
            let migration_done: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM metadata WHERE key='schema_migrated_chunks_v2'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                > 0;
            if !migration_done {
                eprintln!("  NOTE: Upgrading chunks table schema (v0.1.0 → v0.2.0). Files will be re-indexed.");
                conn.execute_batch(
                    "DROP TABLE IF EXISTS chunks; DELETE FROM files; DELETE FROM chunk_meta;",
                )?;
                conn.execute(
                    "INSERT OR REPLACE INTO metadata (key, value) VALUES ('schema_migrated_chunks_v2', '1')",
                    [],
                )?;
            }
        }
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO metadata (key, value) VALUES ('schema_migrated_chunks_v2', '1')",
            [],
        )?;
    }

    conn.execute_batch(
        "
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
            file_id,
            source_path,
            heading,
            chunk_index,
            content,
            tokenize='unicode61'
        );
        ",
    )?;

    // Migration: add content_hash column if missing (old DBs)
    let has_content_hash: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .flatten()
            .collect();
        cols.iter().any(|c| c == "content_hash")
    };
    if !has_content_hash {
        conn.execute(
            "ALTER TABLE files ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }

    Ok(())
}

// ── Delete helpers ────────────────────────────────────────────────────────────

fn delete_chunks_for_file(conn: &Connection, file_id: i64) -> rusqlite::Result<()> {
    // Collect chunk rowids for chunk_meta cleanup
    let rowids: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT rowid FROM chunks WHERE file_id = ?")?;
        let rows: Vec<i64> = stmt
            .query_map(params![file_id.to_string()], |r| r.get::<_, i64>(0))?
            .flatten()
            .collect();
        rows
    };

    if !rowids.is_empty() {
        // Delete chunk_meta in batches of 500
        for batch in rowids.chunks(500) {
            let placeholders: String = batch.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM chunk_meta WHERE chunk_rowid IN ({placeholders})");
            let mut stmt = conn.prepare(&sql)?;
            for (i, id) in batch.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, *id)?;
            }
            stmt.raw_execute()?;
        }
    }

    // FTS5 delete by file_id (stored as text)
    conn.execute(
        "DELETE FROM chunks WHERE file_id = ?",
        params![file_id.to_string()],
    )?;
    Ok(())
}

// ── Index file ────────────────────────────────────────────────────────────────

/// Index a single file. Returns the number of chunks written.
pub fn index_file(
    conn: &Connection,
    filepath: &Path,
    hex_root: &Path,
    content: &str,
    mtime: f64,
    strategy: &str,
) -> rusqlite::Result<usize> {
    let rel_path = filepath
        .strip_prefix(hex_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| filepath.to_string_lossy().to_string());
    let chash = content_hash(content);

    let is_old_tr = strategy == "summary";

    // Apply summary extraction for old transcripts
    let effective_content = if strategy == "summary" {
        let s = extract_summaries(content);
        if s.trim().is_empty() {
            // No summaries found — record in files table with 0 chunks and return
            let existing_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?",
                    params![rel_path],
                    |r| r.get(0),
                )
                .ok();
            if let Some(fid) = existing_id {
                delete_chunks_for_file(conn, fid)?;
                conn.execute("DELETE FROM files WHERE id = ?", params![fid])?;
            }
            conn.execute(
                "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count) VALUES (?, ?, ?, ?, 0)",
                params![rel_path, mtime, chash, Local::now().to_rfc3339()],
            )?;
            return Ok(0);
        }
        s
    } else {
        content.to_string()
    };

    let chunks = chunk_by_heading(&effective_content, true);

    // Remove old file record + chunks
    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM files WHERE path = ?",
            params![rel_path],
            |r| r.get(0),
        )
        .ok();
    if let Some(fid) = existing_id {
        delete_chunks_for_file(conn, fid)?;
        conn.execute("DELETE FROM files WHERE id = ?", params![fid])?;
    }

    // Insert new file record
    conn.execute(
        "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count) VALUES (?, ?, ?, ?, ?)",
        params![rel_path, mtime, chash, Local::now().to_rfc3339(), chunks.len() as i64],
    )?;
    let file_id: i64 = conn.query_row("SELECT last_insert_rowid()", [], |r| r.get(0))?;

    let weight = get_source_weight(&rel_path, is_old_tr);

    for (i, chunk) in chunks.iter().enumerate() {
        conn.execute(
            "INSERT INTO chunks (file_id, source_path, heading, chunk_index, content) VALUES (?, ?, ?, ?, ?)",
            params![
                file_id.to_string(),
                rel_path,
                chunk.heading,
                i.to_string(),
                chunk.content
            ],
        )?;
        let chunk_rowid: i64 =
            conn.query_row("SELECT last_insert_rowid()", [], |r| r.get(0))?;
        conn.execute(
            "INSERT INTO chunk_meta (chunk_rowid, source_weight) VALUES (?, ?)",
            params![chunk_rowid, weight],
        )?;
    }

    Ok(chunks.len())
}

// ── File discovery ────────────────────────────────────────────────────────────

/// Collect all indexable files with their indexing strategy.
pub fn get_indexable_files(hex_root: &Path) -> Vec<(PathBuf, String)> {
    let mut files: Vec<(PathBuf, String)> = Vec::new();

    // Standard directories — full index
    for dir in INDEX_DIRS {
        let dir_path = hex_root.join(dir);
        if !dir_path.exists() {
            continue;
        }

        let exts = ["md", "txt"];
        for ext in &exts {
            let pattern = if *dir == "." {
                format!("{}/*.{}", dir_path.display(), ext)
            } else {
                format!("{}/**/*.{}", dir_path.display(), ext)
            };
            if let Ok(paths) = glob::glob(&pattern) {
                for path in paths.flatten() {
                    let rel = match path.strip_prefix(hex_root) {
                        Ok(r) => r.to_string_lossy().to_string(),
                        Err(_) => continue,
                    };
                    if !should_skip(&rel) {
                        files.push((path, "full".to_string()));
                    }
                }
            }
        }
    }

    // Tiered raw directories
    for (subdir, strategy) in TIERED_RAW_DIRS {
        if *strategy == "exclude" {
            continue;
        }
        let dir_path = hex_root.join(subdir);
        if !dir_path.exists() {
            continue;
        }
        for ext in &["md", "txt"] {
            let pattern = format!("{}/**/*.{}", dir_path.display(), ext);
            if let Ok(paths) = glob::glob(&pattern) {
                for path in paths.flatten() {
                    let rel = match path.strip_prefix(hex_root) {
                        Ok(r) => r.to_string_lossy().to_string(),
                        Err(_) => continue,
                    };
                    if should_skip(&rel) {
                        continue;
                    }
                    let file_strategy = if *strategy == "tiered" {
                        if is_old_transcript(&path) {
                            "summary".to_string()
                        } else {
                            "full".to_string()
                        }
                    } else {
                        strategy.to_string()
                    };
                    files.push((path, file_strategy));
                }
            }
        }
    }

    files
}

// ── Main indexer ──────────────────────────────────────────────────────────────

fn set_metadata(conn: &Connection, key: &str, value: &str) {
    let _ = conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES (?, ?)",
        params![key, value],
    );
}

fn get_metadata(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM metadata WHERE key = ?",
        params![key],
        |r| r.get(0),
    )
    .ok()
}

pub fn run_index(hex_root: &Path, full: bool) -> i32 {
    let t0 = std::time::Instant::now();
    let db_path = super::db_path(hex_root);

    if let Some(parent) = db_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("hex memory index: cannot create .hex dir: {e}");
            return 1;
        }
    }

    let conn = match Connection::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex memory index: cannot open {}: {e}", db_path.display());
            return 1;
        }
    };

    if let Err(e) = init_db(&conn) {
        eprintln!("hex memory index: schema init failed: {e}");
        return 1;
    }

    // Build lookup of existing records: path → (mtime, content_hash)
    let existing: std::collections::HashMap<String, (f64, String)> = {
        let mut stmt = conn
            .prepare("SELECT path, mtime, content_hash FROM files")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, String>(2).unwrap_or_default(),
            ))
        })
        .unwrap()
        .flatten()
        .map(|(p, m, h)| (p, (m, h)))
        .collect()
    };

    let file_tuples = get_indexable_files(hex_root);
    println!("Found {} files to check", file_tuples.len());

    let mut indexed = 0usize;
    let mut skipped_mtime = 0usize;
    let mut skipped_hash = 0usize;
    let mut total_chunks = 0usize;

    for (filepath, strategy) in &file_tuples {
        let rel_path = match filepath.strip_prefix(hex_root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let mtime = file_mtime(filepath);

        if !full {
            let prev = existing.get(&rel_path);

            // Stage 1: mtime pre-filter
            if let Some((prev_mtime, _)) = prev {
                if (*prev_mtime - mtime).abs() < 1e-6 {
                    skipped_mtime += 1;
                    continue;
                }
            }

            // Stage 2: content hash check
            let content = match std::fs::read_to_string(filepath) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  SKIP {rel_path}: {e}");
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }
            let chash = content_hash(&content);
            if let Some((_, prev_hash)) = prev {
                if !prev_hash.is_empty() && *prev_hash == chash {
                    // Content identical — update mtime only
                    let _ = conn.execute(
                        "UPDATE files SET mtime = ? WHERE path = ?",
                        params![mtime, rel_path],
                    );
                    skipped_hash += 1;
                    continue;
                }
            }

            // Actually re-index
            match index_file(&conn, filepath, hex_root, &content, mtime, strategy) {
                Ok(n) => {
                    if n > 0 {
                        indexed += 1;
                        total_chunks += n;
                        let tag = if strategy != "full" {
                            format!(" [{strategy}]")
                        } else {
                            String::new()
                        };
                        println!("  Indexed: {rel_path} ({n} chunks{tag})");
                    } else if strategy == "summary" {
                        println!(
                            "  Indexed: {rel_path} (0 chunks, no summaries found [summary])"
                        );
                    }
                }
                Err(e) => {
                    eprintln!("  ERROR indexing {rel_path}: {e}");
                }
            }
        } else {
            // Full mode: read unconditionally
            let content = match std::fs::read_to_string(filepath) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  SKIP {rel_path}: {e}");
                    continue;
                }
            };
            if content.trim().is_empty() {
                continue;
            }
            match index_file(&conn, filepath, hex_root, &content, mtime, strategy) {
                Ok(n) => {
                    if n > 0 {
                        indexed += 1;
                        total_chunks += n;
                        let tag = if strategy != "full" {
                            format!(" [{strategy}]")
                        } else {
                            String::new()
                        };
                        println!("  Indexed: {rel_path} ({n} chunks{tag})");
                    } else if strategy == "summary" {
                        println!(
                            "  Indexed: {rel_path} (0 chunks, no summaries found [summary])"
                        );
                    }
                }
                Err(e) => {
                    eprintln!("  ERROR indexing {rel_path}: {e}");
                }
            }
        }
    }

    // Cleanup: remove DB records for files no longer on disk
    let all_paths: HashSet<String> = file_tuples
        .iter()
        .filter_map(|(p, _)| p.strip_prefix(hex_root).ok().map(|r| r.to_string_lossy().to_string()))
        .collect();

    let mut removed = 0usize;
    for db_path_str in existing.keys() {
        if !all_paths.contains(db_path_str) {
            let existing_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?",
                    params![db_path_str],
                    |r| r.get(0),
                )
                .ok();
            if let Some(fid) = existing_id {
                let _ = delete_chunks_for_file(&conn, fid);
                let _ = conn.execute("DELETE FROM files WHERE id = ?", params![fid]);
                removed += 1;
                println!("  Removed: {db_path_str}");
            }
        }
    }

    set_metadata(&conn, "last_run", &Local::now().to_rfc3339());
    set_metadata(
        &conn,
        "last_run_mode",
        if full { "full" } else { "incremental" },
    );

    let elapsed = t0.elapsed().as_secs_f64();
    println!(
        "\nDone in {elapsed:.2}s: {indexed} indexed, \
         {skipped_mtime} unchanged (mtime), {skipped_hash} unchanged (hash), \
         {removed} removed, {total_chunks} new chunks"
    );

    0
}

// ── Stats ─────────────────────────────────────────────────────────────────────

pub fn show_stats(hex_root: &Path) -> i32 {
    let db_path = super::db_path(hex_root);
    if !db_path.exists() {
        println!("No index found. Run `hex memory index` to create one.");
        return 1;
    }

    let conn = match Connection::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex memory stats: cannot open db: {e}");
            return 1;
        }
    };

    let file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    let chunk_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap_or(0);
    let hashed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE content_hash != ''",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let db_size_kb = db_path.metadata().map(|m| m.len() as f64 / 1024.0).unwrap_or(0.0);

    println!("Database: {}", db_path.display());
    println!("Size: {db_size_kb:.1} KB");
    println!("Files indexed: {file_count} ({hashed} with content hash)");
    println!("Total chunks: {chunk_count}");
    println!("Vector embeddings: disabled (FTS5-only mode)");

    let last_run = get_metadata(&conn, "last_run");
    let last_mode = get_metadata(&conn, "last_run_mode");
    if let Some(run) = last_run {
        let mode = last_mode.as_deref().unwrap_or("unknown");
        println!("Last run: {run} ({mode})");
    }
    println!();

    println!("By directory:");
    let mut stmt = conn.prepare("
        SELECT
            CASE
                WHEN source_path LIKE '%/%'
                THEN substr(source_path, 1, instr(source_path, '/') - 1)
                ELSE '(root)'
            END as dir,
            COUNT(DISTINCT source_path) as files,
            COUNT(*) as chunks
        FROM chunks
        GROUP BY dir
        ORDER BY chunks DESC
    ").unwrap();
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    }).unwrap();
    for row in rows.flatten() {
        println!("  {}: {} files, {} chunks", row.0, row.1, row.2);
    }

    0
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(hex_root: &Path, full: bool, stats: bool) -> i32 {
    if stats {
        show_stats(hex_root)
    } else {
        let mode = if full { "Full reindex" } else { "Incremental index" };
        println!("{mode}...");
        run_index(hex_root, full)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_heading_basic() {
        assert_eq!(parse_heading("# Hello"), Some((1, "Hello")));
        assert_eq!(parse_heading("## Section"), Some((2, "Section")));
        assert_eq!(parse_heading("#### Deep"), Some((4, "Deep")));
        assert_eq!(parse_heading("##### TooDeep"), None);
        assert_eq!(parse_heading("Not a heading"), None);
        assert_eq!(parse_heading("#NoSpace"), None);
    }

    #[test]
    fn test_chunk_by_heading_simple() {
        let content = "# Title\nSome content here.\n## Section\nMore content.";
        let chunks = chunk_by_heading(content, false);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Title");
        assert!(chunks[0].content.contains("Some content"));
        assert_eq!(chunks[1].heading, "Section");
    }

    #[test]
    fn test_chunk_large_content_splits_with_overlap() {
        // Build content with 500 words under one heading.
        // The heading line ("# Big") is kept in current_lines, so the chunk content
        // includes "# Big" (2 extra tokens) → total 502 tokens.
        // first chunk:  words[0..400]       = 400 tokens; i → 320
        // second chunk: words[320..502]      = 182 tokens; i → 640 (done)
        let many_words = vec!["word"; 500].join(" ");
        let content = format!("# Big\n{}", many_words);
        let chunks = chunk_by_heading(content.as_str(), false);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Big");
        assert_eq!(chunks[1].heading, "Big (part 2)");
        assert_eq!(chunks[0].content.split_whitespace().count(), 400);
        assert_eq!(chunks[1].content.split_whitespace().count(), 182);
    }

    #[test]
    fn test_chunk_deduplication() {
        let content = "# A\nSame content.\n# A\nSame content.";
        let chunks_dedup = chunk_by_heading(content, true);
        let chunks_no_dedup = chunk_by_heading(content, false);
        assert_eq!(chunks_no_dedup.len(), 2);
        assert_eq!(chunks_dedup.len(), 1);
    }

    #[test]
    fn test_source_weight_matching() {
        assert_eq!(get_source_weight("me/decisions/foo.md", false), 1.5);
        assert_eq!(get_source_weight("people/alice.md", false), 1.5);
        assert_eq!(get_source_weight("me/journal.md", false), 1.2);
        assert_eq!(get_source_weight("projects/foo/bar.md", false), 1.2);
        assert_eq!(get_source_weight("evolution/x.md", false), 1.2);
        assert_eq!(get_source_weight("landings/today.md", false), 1.0);
        assert_eq!(get_source_weight("raw/research/paper.md", false), 1.0);
        assert_eq!(get_source_weight("raw/captures/clip.md", false), 0.8);
        assert_eq!(get_source_weight("raw/transcripts/2020-01-01.md", true), 0.3);
        assert_eq!(get_source_weight("raw/transcripts/2026-05-01.md", false), 0.5);
        assert_eq!(get_source_weight("misc/unknown.md", false), 1.0);
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        let h3 = content_hash("different");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        // SHA256 hex is 64 chars
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn test_init_db_creates_schema() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("memory.db");
        let conn = Connection::open(&db_path).unwrap();
        init_db(&conn).unwrap();

        // Verify files table
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        // Verify chunks FTS5 table
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        // Verify chunk_meta table
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunk_meta", [], |r| r.get(0))
            .unwrap();
        // Verify metadata table
        let val: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_migrated_chunks_v2'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert_eq!(val.as_deref(), Some("1"));
    }

    #[test]
    fn test_incremental_index_skips_unchanged() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        let hex_dir = hex_root.join(".hex");
        std::fs::create_dir_all(&hex_dir).unwrap();

        // Create a CLAUDE.md so it looks like a hex root
        std::fs::write(hex_root.join("CLAUDE.md"), "# Hex").unwrap();

        // Create a simple markdown file
        let me_dir = hex_root.join("me");
        std::fs::create_dir_all(&me_dir).unwrap();
        let test_file = me_dir.join("test.md");
        std::fs::write(&test_file, "# Test\nSome content here.").unwrap();

        // First index run
        let code1 = run_index(hex_root, false);
        assert_eq!(code1, 0);

        let db = Connection::open(hex_dir.join("memory.db")).unwrap();
        // test.md should have been indexed with at least 1 chunk
        let test_chunks: i64 = db
            .query_row("SELECT chunk_count FROM files WHERE path LIKE '%test.md'", [], |r| r.get(0))
            .unwrap();
        assert!(test_chunks > 0);
        let files_after_run1: i64 = db
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();

        // Second run — files unchanged, should skip at mtime stage (same file count)
        let code2 = run_index(hex_root, false);
        assert_eq!(code2, 0);
        let files_after_run2: i64 = db
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files_after_run2, files_after_run1);
    }

    #[test]
    fn test_index_file_inserts_chunks_and_meta() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        let hex_dir = hex_root.join(".hex");
        std::fs::create_dir_all(&hex_dir).unwrap();

        let db_path = hex_dir.join("memory.db");
        let conn = Connection::open(&db_path).unwrap();
        init_db(&conn).unwrap();

        let test_file = hex_root.join("test.md");
        let content = "# Hello\nWorld content here.\n## More\nExtra info.";
        std::fs::write(&test_file, content).unwrap();

        let n = index_file(&conn, &test_file, hex_root, content, 0.0, "full").unwrap();
        assert!(n >= 2);

        let chunk_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunk_count, n as i64);

        let meta_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunk_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(meta_count, n as i64);
    }
}
