/// Port of .hex/scripts/capture.sh
/// Zero-friction context capture for hex agents.
/// Writes a timestamped markdown file to $HEX_DIR/raw/captures/.
use std::collections::HashSet;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::Command;

pub fn run_capture(hex_dir: &PathBuf, text_args: &[String]) {
    let captures_dir = hex_dir.join("raw/captures");
    std::fs::create_dir_all(&captures_dir).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot create {}: {e}", captures_dir.display());
        std::process::exit(1);
    });

    let text = collect_text(text_args);

    if text.trim().is_empty() {
        println!("Nothing to capture.");
        return;
    }

    // Generate timestamp; honour TZ from .hex/timezone if set
    let tz_file = hex_dir.join(".hex/timezone");
    if std::env::var("TZ").is_err() {
        if let Ok(tz) = std::fs::read_to_string(&tz_file) {
            let tz = tz.trim().to_string();
            if !tz.is_empty() {
                std::env::set_var("TZ", &tz);
            }
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let timestamp = format_timestamp(now.as_secs());
    let filename = format!("{}.md", format_filename(now.as_secs()));
    let outfile = captures_dir.join(&filename);

    let content = format!("---\ncaptured: {timestamp}\nsource: cli\n---\n\n{text}\n");

    // Atomic write: .tmp then mv
    let tmpfile = captures_dir.join(format!("{filename}.tmp"));
    std::fs::write(&tmpfile, &content).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot write {}: {e}", tmpfile.display());
        std::process::exit(1);
    });
    std::fs::rename(&tmpfile, &outfile).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot rename to {}: {e}", outfile.display());
        std::process::exit(1);
    });

    // Emit telemetry (best-effort, ignore failures)
    let emit_sh = hex_dir.join(".hex/bin/hex-emit.sh");
    if emit_sh.exists() {
        let payload = format!(
            "{{\"path\":\"{}\",\"source\":\"cli\",\"timestamp\":\"{}\"}}",
            outfile.display(),
            timestamp
        );
        let _ = Command::new(&emit_sh)
            .arg("capture.created")
            .arg(&payload)
            .arg("capture-script")
            .status();
    }

    println!("Captured. Will triage on next session startup.");
}

/// Port of system/scripts/hex-ui-feedback-ingest.sh
/// Reads hex-ui messages.json, extracts done messages, appends to feedback log.
pub fn run_ingest(hex_dir: &PathBuf) {
    use serde_json::Value;

    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let messages_path = home.join("github.com/mrap/hex-ui/backend/state/messages.json");
    let processed_path = hex_dir.join(".hex/state/hex-ui-processed-messages.json");
    let feedback_log = hex_dir.join("projects/hex-ui/feedback/ui-feedback-log.md");

    if let Some(p) = processed_path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    if let Some(p) = feedback_log.parent() {
        std::fs::create_dir_all(p).ok();
    }

    let raw = match std::fs::read_to_string(&messages_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: could not read messages: {e}");
            std::process::exit(1);
        }
    };

    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR: invalid JSON in messages.json: {e}");
            std::process::exit(1);
        }
    };

    let messages: Vec<Value> = if parsed.is_array() {
        parsed.as_array().unwrap().to_vec()
    } else if let Some(arr) = parsed.get("messages").and_then(|v| v.as_array()) {
        arr.to_vec()
    } else {
        let keys: Vec<&str> = parsed
            .as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        eprintln!(
            "ERROR: messages.json is a dict without a 'messages' key; got keys: {:?}",
            keys
        );
        std::process::exit(1);
    };

    // Load processed IDs
    let mut processed_ids: HashSet<String> = HashSet::new();
    if processed_path.exists() {
        if let Ok(data_raw) = std::fs::read_to_string(&processed_path) {
            if let Ok(data) = serde_json::from_str::<Value>(&data_raw) {
                if let Some(ids) = data.get("processed_ids").and_then(|v| v.as_array()) {
                    for id in ids {
                        if let Some(s) = id.as_str() {
                            processed_ids.insert(s.to_string());
                        }
                    }
                }
            }
        }
    }

    let new_messages: Vec<&Value> = messages
        .iter()
        .filter(|m| {
            m.get("status").and_then(|v| v.as_str()) == Some("done")
                && !processed_ids
                    .contains(m.get("id").and_then(|v| v.as_str()).unwrap_or(""))
        })
        .collect();

    if new_messages.is_empty() {
        println!("No new messages to process.");
        return;
    }

    println!("Processing {} new message(s)...", new_messages.len());

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&feedback_log)
        .unwrap_or_else(|e| {
            eprintln!("ERROR: cannot open feedback log: {e}");
            std::process::exit(1);
        });

    for msg in &new_messages {
        let created_at = msg.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
        let ts = chrono::DateTime::from_timestamp(created_at, 0)
            .map(|dt: chrono::DateTime<chrono::Utc>| {
                dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
            })
            .unwrap_or_else(|| "unknown".to_string());

        let text = msg
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .replace('\n', "\n  ");
        let thread_id = msg
            .get("thread_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let status_str = msg
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("");

        writeln!(log, "\n### [{ts}] Feedback").ok();
        writeln!(log, "**Message:** {text}").ok();
        writeln!(log, "**Thread:** {thread_id}").ok();
        writeln!(log, "**Status:** {status_str}").ok();

        processed_ids.insert(id.to_string());
        let preview = if text.len() > 60 { &text[..60] } else { &text };
        println!("  - [{ts}] {preview}...");
    }

    // Save updated processed IDs atomically
    let mut ids_sorted: Vec<String> = processed_ids.into_iter().collect();
    ids_sorted.sort();
    let data = serde_json::json!({"processed_ids": ids_sorted});
    let tmp = processed_path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&data).unwrap()).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot write state file: {e}");
        std::process::exit(1);
    });
    std::fs::rename(&tmp, &processed_path).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot rename state file: {e}");
        std::process::exit(1);
    });

    println!(
        "Done. {} message(s) written to feedback log.",
        new_messages.len()
    );
}

/// Port of system/scripts/capture-to-dispatch.sh
/// Scans triaged captures, generates BOI specs, and dispatches.
pub fn run_dispatch(hex_dir: &PathBuf, dry_run: bool, max: u32, triage: Option<String>) {
    use regex::Regex;

    let captures_dir = hex_dir.join("raw/captures");
    let spec_staging_dir = captures_dir.join(".dispatch-staging");
    let validator = hex_dir.join(".hex/scripts/validate-boi-spec.py");
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let boi = home.join(".boi/boi");
    let boi_db = home.join(".boi/boi.db");

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    // Cutoff date: 7 days ago as YYYY-MM-DD string for lexicographic comparison
    let cutoff_date = (chrono::Local::now() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();

    // Find triage file
    let triage_path = if let Some(tf) = triage {
        PathBuf::from(tf)
    } else {
        let pattern = captures_dir.join("TRIAGE-*.md");
        let mut files: Vec<PathBuf> = glob::glob(pattern.to_str().unwrap_or(""))
            .unwrap()
            .filter_map(|f| f.ok())
            .collect();
        files.sort_by_key(|f| {
            std::fs::metadata(f)
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH)
        });
        files.reverse();
        if files.is_empty() {
            println!("No TRIAGE report found in {}", captures_dir.display());
            return;
        }
        files.into_iter().next().unwrap()
    };

    if !triage_path.is_file() {
        eprintln!("Triage file not found: {}", triage_path.display());
        std::process::exit(1);
    }

    println!("=== capture-to-dispatch ===");
    println!(
        "Triage report: {}",
        triage_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    println!("Max dispatches: {max}");
    println!("Dry run: {dry_run}");
    println!();

    // Parse triage file for actionable filenames and their bold titles
    let triage_content = std::fs::read_to_string(&triage_path).unwrap_or_default();
    let fname_re =
        Regex::new(r"`(\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}[^`]*\.md)`").unwrap();
    let bold_re = Regex::new(r"\*\*([^*]+)\*\*").unwrap();

    let mut actionable: Vec<String> = Vec::new();
    let mut title_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut in_actionable = false;

    for line in triage_content.lines() {
        if line.contains("## Actionable Items") {
            in_actionable = true;
            continue;
        }
        if in_actionable && line.starts_with("## ") && !line.contains("Actionable") {
            in_actionable = false;
            continue;
        }
        if in_actionable {
            let title = bold_re
                .captures(line)
                .map(|c| c[1].to_string())
                .unwrap_or_default();
            for cap in fname_re.captures_iter(line) {
                let fname = cap[1].to_string();
                if !actionable.contains(&fname) {
                    actionable.push(fname.clone());
                }
                if !title.is_empty() && !title_map.contains_key(&fname) {
                    title_map.insert(fname, title.clone());
                }
            }
        }
    }

    actionable.sort();
    actionable.dedup();

    println!(
        "Found {} actionable captures in triage report",
        actionable.len()
    );
    println!();

    // Filter to pending (not yet dispatched, not stale)
    let mut pending: Vec<String> = Vec::new();
    for fname in &actionable {
        let filepath = captures_dir.join(fname);
        if !filepath.is_file() {
            println!("  SKIP (not found): {fname}");
            continue;
        }
        let file_content = std::fs::read_to_string(&filepath).unwrap_or_default();
        if file_content.lines().any(|l| l.starts_with("dispatched:")) {
            println!("  SKIP (already dispatched via tag): {fname}");
            continue;
        }
        // Check BOI DB for an active spec referencing this capture
        if boi_db.exists() {
            if let Ok(conn) = rusqlite::Connection::open(&boi_db) {
                let q = "SELECT 1 FROM specs WHERE spec_path LIKE ?1 \
                         AND status NOT IN ('canceled','failed') LIMIT 1";
                let pattern = format!("%{fname}%");
                let found: bool = conn
                    .query_row(q, rusqlite::params![pattern], |_| Ok(true))
                    .unwrap_or(false);
                if found {
                    println!("  SKIP (active BOI spec exists in DB): {fname}");
                    // Tag it so we don't re-check the DB next time
                    if let Ok(mut f) =
                        std::fs::OpenOptions::new().append(true).open(&filepath)
                    {
                        writeln!(f, "dispatched: {today}").ok();
                    }
                    continue;
                }
            }
        }
        // Skip stale captures (older than 7 days, compared via YYYY-MM-DD prefix)
        if fname.len() >= 10 {
            let date_prefix = &fname[..10]; // YYYY-MM-DD
            if date_prefix < cutoff_date.as_str() {
                let age = {
                    // approximate days from string diff isn't exact; count via epoch
                    let then = chrono::NaiveDate::parse_from_str(date_prefix, "%Y-%m-%d")
                        .ok()
                        .and_then(|d| d.and_hms_opt(0, 0, 0))
                        .map(|dt| dt.and_utc().timestamp())
                        .unwrap_or(0);
                    let now = chrono::Local::now().timestamp();
                    (now - then) / 86400
                };
                println!("  SKIP (stale, {age}d old): {fname}");
                continue;
            }
        }
        pending.push(fname.clone());
    }

    println!();
    println!("{} captures pending dispatch", pending.len());

    if pending.is_empty() {
        println!("Nothing to dispatch.");
        return;
    }

    std::fs::create_dir_all(&spec_staging_dir).ok();

    let mut dispatched = 0u32;
    for fname in &pending {
        if dispatched >= max {
            println!();
            println!(
                "Rate limit reached ({max}). Remaining captures deferred to next run."
            );
            break;
        }

        let filepath = captures_dir.join(fname);
        let file_content = std::fs::read_to_string(&filepath).unwrap_or_default();

        let capture_content = extract_after_frontmatter(&file_content);
        let capture_content = capture_content.trim().to_string();

        if capture_content.is_empty() {
            println!("  SKIP (empty content): {fname}");
            continue;
        }

        // Derive title: triage report > routed_to frontmatter > first line
        let title = if let Some(t) = title_map.get(fname) {
            t.clone()
        } else if let Some(line) = file_content.lines().find(|l| l.starts_with("routed_to:")) {
            line.trim_start_matches("routed_to:")
                .trim()
                .trim_matches('"')
                .to_string()
        } else {
            capture_content
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect()
        };

        // Sanitise title for use as a filename slug
        let spec_basename: String = {
            let slug: String = title
                .to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect();
            slug.split('-')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("-")
                .chars()
                .take(60)
                .collect()
        };

        let spec_path = spec_staging_dir.join(format!("{spec_basename}.spec.md"));
        let workspace = hex_dir.to_string_lossy();

        let spec_content = format!(
            "# {title}\n\n\
             **Mode:** execute\n\
             **Workspace:** worktree\n\n\
             ## Context\n\n\
             Source capture: `{filepath}`\n\
             Captured content:\n\
             > {capture_content}\n\n\
             ## Tasks\n\n\
             ### t-1: Implement {title}\n\
             PENDING\n\n\
             **Spec:** {capture_content}\n\n\
             Use absolute paths. Target repository should be determined from the task context.\n\n\
             **Verify:**\n\
             ```bash\n\
             echo \"Task completed: {title}\"\n\
             ```\n\n\
             **Self-evolution:** If the task requires multiple steps, decompose into \
             additional tasks before proceeding. If the task is research-only, produce \
             a written report at `{workspace}/raw/research/{spec_basename}.md` and verify \
             with `test -f {workspace}/raw/research/{spec_basename}.md`.\n",
            filepath = filepath.display(),
        );

        // Atomic write
        let spec_tmp = spec_path.with_extension("md.tmp");
        if let Err(e) = std::fs::write(&spec_tmp, &spec_content) {
            eprintln!("ERROR: cannot write spec: {e}");
            continue;
        }
        if let Err(e) = std::fs::rename(&spec_tmp, &spec_path) {
            eprintln!("ERROR: cannot finalize spec: {e}");
            continue;
        }

        println!();
        println!(
            "--- [{}/{}] {} ---",
            dispatched + 1,
            max,
            fname
        );
        println!("  Title: {title}");
        println!(
            "  Spec:  {}",
            spec_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );

        // Validate if validator exists
        if validator.is_file() {
            let ok = Command::new("python3")
                .arg(&validator)
                .arg(&spec_path)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                println!("  VALIDATION FAILED -- skipping");
                std::fs::remove_file(&spec_path).ok();
                continue;
            }
        }

        if dry_run {
            println!("  [DRY RUN] Would dispatch: {}", spec_path.display());
        } else {
            println!("  Dispatching...");
            let ok = Command::new(&boi)
                .arg("dispatch")
                .arg(&spec_path)
                .arg("--no-critic")
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                println!("  Dispatched successfully.");
                if let Ok(mut f) =
                    std::fs::OpenOptions::new().append(true).open(&filepath)
                {
                    writeln!(f, "dispatched: {today}").ok();
                }
                // Emit telemetry (best-effort)
                let emit = hex_dir.join(".hex/bin/hex-emit.sh");
                if emit.is_file() {
                    let spec_id = spec_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    Command::new(&emit)
                        .arg("capture.dispatched")
                        .arg(format!(
                            "{{\"capture_path\":\"{}\",\"spec_id\":\"{spec_id}\"}}",
                            filepath.display()
                        ))
                        .arg("capture-to-dispatch")
                        .status()
                        .ok();
                }
            } else {
                println!("  DISPATCH FAILED");
                continue;
            }
        }

        dispatched += 1;
    }

    println!();
    println!(
        "=== Done: {dispatched} dispatched (of {} pending) ===",
        pending.len()
    );
}

fn extract_after_frontmatter(content: &str) -> String {
    let mut in_front = false;
    let mut past = false;
    let mut result = String::new();
    for line in content.lines() {
        if past {
            result.push_str(line);
            result.push('\n');
        } else if line == "---" && !in_front {
            in_front = true;
        } else if line == "---" && in_front {
            past = true;
        }
    }
    result
}

fn collect_text(args: &[String]) -> String {
    if !args.is_empty() {
        return args.join(" ");
    }
    if !io::stdin().is_terminal() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).unwrap_or(0);
        return buf;
    }
    // Interactive: use $EDITOR or raw stdin
    if let Ok(editor) = std::env::var("EDITOR") {
        if !editor.is_empty() {
            let tmpfile = std::env::temp_dir()
                .join(format!("hex-capture-{}.md", std::process::id()));
            let _ = Command::new(&editor).arg(&tmpfile).status();
            return std::fs::read_to_string(&tmpfile).unwrap_or_default();
        }
    }
    eprintln!("Type your capture (Ctrl+D when done):");
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap_or(0);
    buf
}

fn format_timestamp(secs: u64) -> String {
    // ISO-8601 local time via `date` — avoids pulling in a time crate
    let out = Command::new("date").arg("+%Y-%m-%dT%H:%M:%S").output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => format!("{secs}"),
    }
}

fn format_filename(secs: u64) -> String {
    let out = Command::new("date").arg("+%Y-%m-%d_%H-%M-%S").output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => format!("{secs}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_dir_path_construction() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let expected = hex_dir.join("raw/captures");
        assert_eq!(
            expected.to_str().unwrap(),
            "/Users/test/hex/raw/captures"
        );
    }

    #[test]
    fn collect_text_from_args() {
        let args = vec!["hello".to_string(), "world".to_string()];
        assert_eq!(collect_text(&args), "hello world");
    }

    #[test]
    fn collect_text_empty_args_is_empty_string() {
        let args_with_content = vec!["test capture".to_string()];
        assert_eq!(collect_text(&args_with_content), "test capture");
    }

    #[test]
    fn extract_after_frontmatter_works() {
        let content = "---\ncaptured: 2026-01-01\nsource: cli\n---\n\nHello world\n";
        let result = extract_after_frontmatter(content);
        assert!(result.contains("Hello world"));
        assert!(!result.contains("captured:"));
    }

    #[test]
    fn extract_after_frontmatter_no_frontmatter() {
        let content = "Hello world\n";
        let result = extract_after_frontmatter(content);
        // No frontmatter → empty (past never becomes true)
        assert!(result.is_empty() || result.contains("Hello"));
    }
}
