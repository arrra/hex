use serde_json::json;
use std::path::{Path, PathBuf};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Truncate a UTF-8 string to at most `max_bytes` without splitting a char.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Emit hookSpecificOutput JSON to stdout.
fn emit_hook_output(context: &str) {
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    });
    println!("{}", out);
}

// ── topic resolution ──────────────────────────────────────────────────────────

/// Resolve CC_SESSION_KEY to a sanitised topic slug, or None.
///
/// Rules (mirrors session-start.sh):
///   - Empty / "local-dev" / "hex-main" / "#hex-main" → None
///   - "#hex-<slug>" or "hex-<slug>" → Some(slug) if slug is [a-zA-Z0-9_-]+
///   - Anything else → None
pub fn resolve_topic(session_key: &str) -> Option<String> {
    match session_key {
        "" | "local-dev" | "hex-main" | "#hex-main" => return None,
        _ => {}
    }
    let without_hash = session_key.strip_prefix('#').unwrap_or(session_key);
    let slug = without_hash.strip_prefix("hex-")?;
    if slug.is_empty() {
        return None;
    }
    // Reject anything that isn't a clean slug (blocks path traversal).
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    Some(slug.to_string())
}

// ── blocker primitive ─────────────────────────────────────────────────────────

struct BlockerEntry {
    path: PathBuf,
    headline: String,
    detail: String,
}

fn read_blocker(path: &Path) -> BlockerEntry {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let content = raw.trim();
    let mut lines = content.lines();
    let headline = lines.next().unwrap_or("(empty)").to_string();
    let detail_lines: Vec<&str> = lines.collect();
    let detail = detail_lines.join("\n").trim().to_string();
    BlockerEntry { path: path.to_path_buf(), headline, detail }
}

/// Collect all blocker flag paths, sorted.
///
/// Sources:
///   1. `<hex_dir>/.hex/state/blockers/*.flag` (sorted by filename)
///   2. `<hex_dir>/okrs/_state/overdue.flag` (legacy slot)
pub fn collect_blockers(hex_dir: &Path) -> Vec<PathBuf> {
    let blocker_dir = hex_dir.join(".hex/state/blockers");
    let overdue_flag = hex_dir.join("okrs/_state/overdue.flag");

    let mut paths: Vec<PathBuf> = Vec::new();

    if blocker_dir.is_dir() {
        let mut flag_paths: Vec<PathBuf> = std::fs::read_dir(&blocker_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.ends_with(".flag") && e.path().is_file()
            })
            .map(|e| e.path())
            .collect();
        flag_paths.sort();
        paths.extend(flag_paths);
    }

    if overdue_flag.is_file() {
        paths.push(overdue_flag);
    }

    paths
}

/// Build the blocker context string from a list of flag paths.
pub fn format_blocker_output(flag_paths: &[PathBuf], hex_dir: &Path) -> String {
    const LIMIT: usize = 5;

    let entries: Vec<BlockerEntry> = flag_paths.iter().map(|p| read_blocker(p)).collect();
    let shown = &entries[..entries.len().min(LIMIT)];
    let overflow = entries.len().saturating_sub(LIMIT);

    let mut parts = vec![
        "*** SESSION-START BLOCKERS — ADDRESS BEFORE PROCEEDING ***".to_string(),
        String::new(),
    ];

    for (i, e) in shown.iter().enumerate() {
        let rel = e
            .path
            .strip_prefix(hex_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| e.path.display().to_string());
        parts.push(format!("### Blocker {}: {}", i + 1, e.headline));
        parts.push(format!("  source: {}", rel));
        if !e.detail.is_empty() {
            parts.push(String::new());
            parts.push(truncate_str(&e.detail, 1500).to_string());
        }
        parts.push(String::new());
    }

    if overflow > 0 {
        let dir_rel = shown
            .last()
            .and_then(|e| e.path.parent())
            .map(|p| {
                p.strip_prefix(hex_dir)
                    .map(|r| r.display().to_string())
                    .unwrap_or_else(|_| p.display().to_string())
            })
            .unwrap_or_default();
        parts.push(format!("+ {} more blocker(s) — see {}/", overflow, dir_rel));
    }

    parts.push(String::new());
    parts.push(
        "Resolve or explicitly defer each blocker before normal session startup. \
         Each producer documents its own clearance protocol."
            .to_string(),
    );

    parts.join("\n")
}

// ── checkpoint resume ─────────────────────────────────────────────────────────

/// Build the checkpoint context string, capping at ~4 KB.
pub fn format_checkpoint_output(topic: &str, content: &str) -> String {
    const CAP: usize = 4096;
    let preview = truncate_str(content, CAP);
    let truncated = if content.len() > CAP {
        format!(
            "\n\n[…truncated, full file at projects/{}/checkpoint.md]",
            topic
        )
    } else {
        String::new()
    };
    format!(
        "*** Topic-scoped session: projects/{topic}/checkpoint.md ***\n\n\
         Picking up where the prior session left off in this topic. Below is the \
         checkpoint content; review it before responding to the user.\n\n\
         {preview}{truncated}"
    )
}

// ── event emission ────────────────────────────────────────────────────────────

fn emit_session_start_event(hex_dir: &Path) {
    let bus = hex::sse::SseBus::new();
    let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(hex_dir));
    let engine = match hex::events::EventEngine::new(hex_dir, telemetry, bus) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[hook/session-start] event engine init failed: {e}");
            return;
        }
    };

    let channel = std::env::var("CC_SESSION_KEY").unwrap_or_else(|_| "local-dev".to_string());
    let pid = std::process::id();
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let payload = json!({
        "channel": channel,
        "agent": "claude-code",
        "pid": pid,
        "start_ts": ts,
    });

    engine.ingest("session.start", &payload, "claude-code");
}

// ── entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    let hex_dir = match std::env::var("HEX_DIR")
        .ok()
        .or_else(|| std::env::var("CLAUDE_PROJECT_DIR").ok())
        .map(PathBuf::from)
    {
        Some(d) => d,
        None => {
            // No hex dir configured — exit 0 silently; hook must never block.
            return;
        }
    };

    // Emit session.start event (best-effort; failures are logged but don't abort).
    emit_session_start_event(&hex_dir);

    // ── Blocker primitive ─────────────────────────────────────────────────────
    let blocker_paths = collect_blockers(&hex_dir);
    if !blocker_paths.is_empty() {
        let context = format_blocker_output(&blocker_paths, &hex_dir);
        emit_hook_output(&context);
        return;
    }

    // ── Channel → topic checkpoint resume ─────────────────────────────────────
    let session_key = std::env::var("CC_SESSION_KEY").unwrap_or_default();
    if let Some(topic) = resolve_topic(&session_key) {
        let ckpt = hex_dir.join("projects").join(&topic).join("checkpoint.md");
        if ckpt.is_file() {
            match std::fs::read_to_string(&ckpt) {
                Ok(raw) => {
                    let context = format_checkpoint_output(&topic, raw.trim());
                    emit_hook_output(&context);
                }
                Err(e) => {
                    let msg = format!(
                        "[session-start hook] failed to read checkpoint at {}: {}",
                        ckpt.display(),
                        e
                    );
                    emit_hook_output(&msg);
                }
            }
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // ── topic resolution ──────────────────────────────────────────────────────

    #[test]
    fn topic_empty_returns_none() {
        assert_eq!(resolve_topic(""), None);
    }

    #[test]
    fn topic_local_dev_returns_none() {
        assert_eq!(resolve_topic("local-dev"), None);
    }

    #[test]
    fn topic_hex_main_returns_none() {
        assert_eq!(resolve_topic("hex-main"), None);
        assert_eq!(resolve_topic("#hex-main"), None);
    }

    #[test]
    fn topic_hex_foo_resolves() {
        assert_eq!(resolve_topic("hex-foo"), Some("foo".to_string()));
        assert_eq!(resolve_topic("#hex-foo"), Some("foo".to_string()));
    }

    #[test]
    fn topic_hex_with_hyphens_and_underscores() {
        assert_eq!(
            resolve_topic("hex-my-project_v2"),
            Some("my-project_v2".to_string())
        );
    }

    #[test]
    fn topic_path_traversal_rejected() {
        assert_eq!(resolve_topic("hex-../etc/passwd"), None);
        assert_eq!(resolve_topic("hex-../../secrets"), None);
        assert_eq!(resolve_topic("#hex-../etc"), None);
    }

    #[test]
    fn topic_with_slash_rejected() {
        assert_eq!(resolve_topic("hex-foo/bar"), None);
    }

    // ── blocker primitive ─────────────────────────────────────────────────────

    fn write_flag(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn blockers_zero() {
        let tmp = TempDir::new().unwrap();
        let hex_dir = tmp.path();
        std::fs::create_dir_all(hex_dir.join(".hex/state/blockers")).unwrap();
        let paths = collect_blockers(hex_dir);
        assert!(paths.is_empty());
    }

    #[test]
    fn blockers_one() {
        let tmp = TempDir::new().unwrap();
        let hex_dir = tmp.path();
        let bdir = hex_dir.join(".hex/state/blockers");
        std::fs::create_dir_all(&bdir).unwrap();
        write_flag(&bdir, "01-foo.flag", "OKR review overdue\nDue last Monday.");

        let paths = collect_blockers(hex_dir);
        assert_eq!(paths.len(), 1);

        let output = format_blocker_output(&paths, hex_dir);
        assert!(output.contains("SESSION-START BLOCKERS"));
        assert!(output.contains("Blocker 1: OKR review overdue"));
        assert!(output.contains("Due last Monday."));
        assert!(!output.contains("more blocker"));
    }

    #[test]
    fn blockers_six_overflows() {
        let tmp = TempDir::new().unwrap();
        let hex_dir = tmp.path();
        let bdir = hex_dir.join(".hex/state/blockers");
        std::fs::create_dir_all(&bdir).unwrap();
        for i in 1..=6 {
            write_flag(&bdir, &format!("0{i}-item.flag"), &format!("Blocker {i}\nDetails."));
        }

        let paths = collect_blockers(hex_dir);
        assert_eq!(paths.len(), 6);

        let output = format_blocker_output(&paths, hex_dir);
        assert!(output.contains("Blocker 1:"));
        assert!(output.contains("Blocker 5:"));
        assert!(!output.contains("Blocker 6:"));
        assert!(output.contains("+ 1 more blocker(s)"));
    }

    #[test]
    fn blocker_overdue_flag_included() {
        let tmp = TempDir::new().unwrap();
        let hex_dir = tmp.path();
        // No .hex/state/blockers dir — only legacy overdue.flag
        std::fs::create_dir_all(hex_dir.join("okrs/_state")).unwrap();
        write_flag(
            &hex_dir.join("okrs/_state"),
            "overdue.flag",
            "OKR review overdue",
        );

        let paths = collect_blockers(hex_dir);
        assert_eq!(paths.len(), 1);
    }

    // ── checkpoint preview ────────────────────────────────────────────────────

    #[test]
    fn checkpoint_no_truncation_when_short() {
        let content = "Short content.";
        let out = format_checkpoint_output("my-topic", content);
        assert!(out.contains("projects/my-topic/checkpoint.md"));
        assert!(out.contains("Short content."));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn checkpoint_truncates_at_4kb() {
        let content: String = "x".repeat(8000);
        let out = format_checkpoint_output("my-topic", &content);
        // Preview should be ≤4096 bytes of 'x' plus the message
        assert!(out.contains("truncated"));
        // The preview itself must not exceed 4096 x's
        let x_count = out.chars().filter(|&c| c == 'x').count();
        assert!(x_count <= 4096, "preview has {x_count} chars, expected ≤4096");
    }
}
