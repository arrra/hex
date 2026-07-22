use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

pub struct ParseArgs {
    pub file: Option<String>,
    pub dry_run: bool,
    pub force: bool,
}

struct Message {
    role: Role,
    timestamp: DateTime<Utc>,
    text: String,
}

enum Role {
    User,
    Assistant,
}

struct Session {
    id: String,
    messages: Vec<Message>,
}

pub fn run(hex_root: &Path, args: &ParseArgs) -> i32 {
    let transcript_dir = hex_root.join("raw/transcripts");
    if !transcript_dir.exists() {
        eprintln!(
            "hex memory parse-transcripts: transcript dir not found: {}",
            transcript_dir.display()
        );
        return 1;
    }

    let tracking_path = transcript_dir.join(".parsed_transcripts");
    let already_parsed = load_tracking(&tracking_path);

    let files_to_parse: Vec<PathBuf> = if let Some(ref f) = args.file {
        let p = PathBuf::from(f);
        if !p.exists() {
            // Try relative to transcript dir
            let alt = transcript_dir.join(f);
            if alt.exists() {
                vec![alt]
            } else {
                eprintln!("hex memory parse-transcripts: file not found: {}", f);
                return 1;
            }
        } else {
            vec![p]
        }
    } else {
        collect_jsonl_files(&transcript_dir)
    };

    if files_to_parse.is_empty() {
        println!("No JSONL transcript files found.");
        return 0;
    }

    // Group sessions by date for multi-append
    // date -> Vec<Session>
    let mut by_date: BTreeMap<NaiveDate, Vec<Session>> = BTreeMap::new();
    let mut newly_parsed: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for path in &files_to_parse {
        let fname = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        if !args.force && already_parsed.contains(&fname) {
            skipped += 1;
            continue;
        }

        match parse_jsonl(path) {
            Ok(session) => {
                if session.messages.is_empty() {
                    // Nothing to write; still mark as parsed
                    newly_parsed.push(fname);
                    continue;
                }
                let date = session.messages[0].timestamp.date_naive();
                by_date.entry(date).or_default().push(session);
                newly_parsed.push(fname);
            }
            Err(e) => {
                eprintln!(
                    "hex memory parse-transcripts: error parsing {}: {}",
                    path.display(),
                    e
                );
                errors += 1;
            }
        }
    }

    // Write / append to per-date markdown files
    let mut written = 0usize;
    for (date, sessions) in &by_date {
        let md_path = transcript_dir.join(format!("{}.md", date));
        let header_line = format!("# Transcript — {}", date);

        // Build content to append
        let mut content = String::new();
        let file_exists = md_path.exists();

        if !file_exists {
            content.push_str(&header_line);
            content.push('\n');
        }

        for session in sessions {
            content.push('\n');
            let first_ts = &session.messages[0].timestamp;
            let time_str = first_ts.format("%H:%M").to_string();
            // TEMP-REVERT-FOR-RED-TEST-PROOF (restored immediately after)
            let id_prefix = &session.id[..session.id.len().min(8)];
            content.push_str(&format!("### Session {}... — {}\n", id_prefix, time_str));
            content.push('\n');

            let mut msg_num = 0usize;
            for msg in &session.messages {
                let ts = msg.timestamp.format("%H:%M").to_string();
                match msg.role {
                    Role::User => {
                        msg_num += 1;
                        content.push_str(&format!("**{}. User `{}`:**\n", msg_num, ts));
                        for line in msg.text.lines() {
                            content.push_str(&format!("> {}\n", line));
                        }
                        content.push('\n');
                    }
                    Role::Assistant => {
                        if msg.text.is_empty() {
                            continue;
                        }
                        msg_num += 1;
                        content.push_str(&format!("**{}. Assistant `{}`:**\n", msg_num, ts));
                        content.push_str(&msg.text);
                        if !msg.text.ends_with('\n') {
                            content.push('\n');
                        }
                        content.push('\n');
                    }
                }
            }
        }

        if args.dry_run {
            println!(
                "[dry-run] would write {} session(s) to {}",
                sessions.len(),
                md_path.display()
            );
        } else {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&md_path)
                .unwrap_or_else(|e| {
                    eprintln!(
                        "hex memory parse-transcripts: cannot open {}: {}",
                        md_path.display(),
                        e
                    );
                    std::process::exit(1);
                });
            f.write_all(content.as_bytes()).unwrap_or_else(|e| {
                eprintln!(
                    "hex memory parse-transcripts: write failed {}: {}",
                    md_path.display(),
                    e
                );
                std::process::exit(1);
            });
            written += sessions.len();
        }
    }

    // Update tracking file
    if !args.dry_run && !newly_parsed.is_empty() {
        append_tracking(&tracking_path, &newly_parsed);
    }

    println!(
        "parse-transcripts: {} session(s) written, {} skipped (already parsed), {} error(s)",
        written, skipped, errors
    );
    if errors > 0 {
        1
    } else {
        0
    }
}

fn load_tracking(path: &Path) -> HashSet<String> {
    match fs::read_to_string(path) {
        Ok(content) => content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => HashSet::new(),
    }
}

fn append_tracking(path: &Path, entries: &[String]) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| {
            eprintln!(
                "hex memory parse-transcripts: cannot update tracking file: {}",
                e
            );
            std::process::exit(1);
        });
    for entry in entries {
        let _ = writeln!(f, "{}", entry);
    }
}

fn collect_jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return files,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn parse_jsonl(path: &Path) -> Result<Session, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;

    // `Session.id` is whatever the transcript filename stem happens to be —
    // in practice an ASCII UUID, but nothing here enforces that, so consumers
    // must not byte-index it (see the char-safe truncation in `run`).
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let mut messages: Vec<Message> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let val: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        let ts_str = val.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp: DateTime<Utc> = ts_str
            .parse()
            .unwrap_or(DateTime::<Utc>::from(std::time::UNIX_EPOCH));

        let message = match val.get("message") {
            Some(m) => m,
            None => continue,
        };

        match msg_type {
            "user" => {
                let text = extract_user_text(message);
                if !text.is_empty() {
                    messages.push(Message {
                        role: Role::User,
                        timestamp,
                        text,
                    });
                }
            }
            "assistant" => {
                let text = extract_assistant_text(message);
                if !text.is_empty() {
                    messages.push(Message {
                        role: Role::Assistant,
                        timestamp,
                        text,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(Session { id: stem, messages })
}

fn extract_user_text(message: &Value) -> String {
    let content = match message.get("content") {
        Some(c) => c,
        None => return String::new(),
    };
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        parts.push(text.to_string());
                    }
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn extract_assistant_text(message: &Value) -> String {
    let content = match message.get("content") {
        Some(c) => c,
        None => return String::new(),
    };
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                let block_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                // Only include text blocks; skip thinking, tool_use, tool_result
                if block_type == "text" {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            parts.push(trimmed.to_string());
                        }
                    }
                }
            }
            parts.join("\n\n")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the char-boundary truncation audit (spec Sxx8h8nh8,
    /// task T0c29fwfv). `Session.id` is built from `path.file_stem()` — an
    /// untyped `String` with no enforced ASCII invariant — and then sliced at
    /// `&session.id[..session.id.len().min(8)]` when rendering the markdown
    /// header. If a transcript filename's stem contains a multi-byte char
    /// spanning byte offset 8, that byte-index slice panics. Session ids are
    /// ASCII UUIDs in every real transcript today, but nothing enforces that
    /// at construction, so a specially named file can still crash
    /// `hex memory parse-transcripts`.
    #[test]
    fn session_id_with_multibyte_char_does_not_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex_root = tmp.path();
        let transcript_dir = hex_root.join("raw/transcripts");
        fs::create_dir_all(&transcript_dir).unwrap();

        // 7 ASCII bytes + one 2-byte char ('é') = 9 bytes total, 8 chars.
        // Byte offset 8 falls inside the 2-byte char, so it is not a char
        // boundary — exactly the condition that panics the current slice.
        let stem = "1234567é";
        assert_eq!(stem.len(), 9, "fixture must be 9 bytes to hit offset 8");
        assert!(
            !stem.is_char_boundary(8),
            "fixture must NOT be char-boundary-safe at byte 8"
        );

        let jsonl_path = transcript_dir.join(format!("{stem}.jsonl"));
        fs::write(
            &jsonl_path,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello"}}"#,
        )
        .unwrap();

        let args = ParseArgs {
            file: None,
            dry_run: false,
            force: false,
        };

        // Must not panic while formatting the session header from a
        // multi-byte session id.
        let code = run(hex_root, &args);
        assert_eq!(code, 0);

        // And the truncation must be char-based: the whole 8-char stem is
        // emitted intact, with no replacement chars or split code points.
        let md = fs::read_to_string(transcript_dir.join("2026-01-01.md")).unwrap();
        assert!(
            md.contains("### Session 1234567é..."),
            "expected char-safe session header, got:\n{md}"
        );
    }
}
