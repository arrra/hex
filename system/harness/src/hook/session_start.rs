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

// ── recency prime ─────────────────────────────────────────────────────────────

/// Build the recency-prime context string by driving `memory::recent` in-process.
/// Pointers only (paths + ages); no file bodies, no LLM. Replaces the prior
/// topic-checkpoint-resume blurb (per F-08, opaque continuity → out).
pub fn format_recency_prime(hex_dir: &Path) -> String {
    let pointers = crate::memory::recent::collect_text(hex_dir);
    if pointers.is_empty() {
        return String::new();
    }
    format!(
        "*** Session recency prime — recent workspace pointers ***\n\n\
         Glance at these before responding; they are pointers only, not bodies.\n\n\
         {pointers}"
    )
}

// ── deterministic health check ────────────────────────────────────────────────

/// Run the deterministic memory-health check (no LLM) and return any FAIL/WARN
/// lines as context, or an empty string if healthy / script unavailable.
/// Relocated from the deleted `startup.rs` step_health.
pub fn format_health_check(hex_dir: &Path) -> String {
    let script = hex_dir.join(".hex/skills/memory/scripts/memory_health.py");
    if !script.is_file() {
        return String::new();
    }
    let output = std::process::Command::new("python3")
        .arg(&script)
        .arg("--quiet")
        .output();
    let out = match output {
        Ok(o) => o,
        Err(_) => return String::new(),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut surfaced: Vec<String> = Vec::new();
    for line in stdout.lines() {
        if line.contains("FAIL") || line.contains("WARN") {
            surfaced.push(line.to_string());
        }
    }
    if surfaced.is_empty() {
        return String::new();
    }
    format!(
        "*** Session-start health check — FAIL/WARN ***\n\n{}",
        surfaced.join("\n")
    )
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

    // ── Blocker primitive ─────────────────────────────────────────────────────
    let blocker_paths = collect_blockers(&hex_dir);
    if !blocker_paths.is_empty() {
        let context = format_blocker_output(&blocker_paths, &hex_dir);
        emit_hook_output(&context);
        return;
    }

    // ── Recency prime + deterministic health check ────────────────────────────
    // Replaces the prior topic-checkpoint-resume (F-08: opaque continuity → out).
    let mut sections: Vec<String> = Vec::new();

    let health = format_health_check(&hex_dir);
    if !health.is_empty() {
        sections.push(health);
    }

    let prime = format_recency_prime(&hex_dir);
    if !prime.is_empty() {
        sections.push(prime);
    }

    if !sections.is_empty() {
        emit_hook_output(&sections.join("\n\n"));
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

    // ── recency prime (replaces topic-checkpoint-resume) ──────────────────────

    /// Red test for task Texbb48j5: SessionStart must emit a recency prime
    /// built from `memory::recent`-style pointers (todo.md `## Now`, project
    /// dirs, recent decisions) — NOT a topic-scoped checkpoint preview.
    ///
    /// The implementer should add a `format_recency_prime(hex_dir: &Path) -> String`
    /// (or similarly-named) helper that drives memory::recent in-process and
    /// returns its text. This test will fail to compile until it exists.
    #[test]
    fn recency_prime_includes_todo_now_pointers() {
        let tmp = TempDir::new().unwrap();
        let hex_dir = tmp.path();
        std::fs::write(
            hex_dir.join("todo.md"),
            "## Now\n- ship the recency prime\n- delete checkpoint resume\n\n## Later\n- nope\n",
        )
        .unwrap();
        std::fs::create_dir_all(hex_dir.join("projects/example")).unwrap();
        std::fs::write(
            hex_dir.join("projects/example/context.md"),
            "example project\n",
        )
        .unwrap();

        let out = format_recency_prime(hex_dir);
        assert!(
            out.contains("ship the recency prime"),
            "recency prime must surface todo.md ## Now items; got: {out}"
        );
        assert!(
            !out.contains("projects/my-topic/checkpoint.md"),
            "recency prime must NOT be the topic-checkpoint-resume blurb; got: {out}"
        );
    }

}
