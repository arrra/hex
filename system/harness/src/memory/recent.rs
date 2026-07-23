//! `hex memory recent` — fast, LLM-free pointer scan of the hex workspace.
//!
//! Scans $HEX_DIR live and prints ~10 recency-ordered pointer lines:
//!   - project dirs under `projects/*/` by mtime (excluding `boi:`-prefixed)
//!   - last ~10 decision files (me/decisions/* and projects/*/decisions/*) by mtime
//!   - top 3 items from todo.md `## Now` section
//!
//! Output is **pointers only** — relative path + human-readable age. No file
//! bodies, no LLM, target <200ms.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// One recency pointer line.
#[derive(Debug, Clone)]
struct Pointer {
    /// Relative path (or pseudo-path like `todo.md#Now`) shown to the user.
    rel: String,
    /// mtime — sort key (newer first). For todo.md `## Now` items we use the
    /// file's mtime so they slot in by recency too.
    mtime: SystemTime,
}

fn human_age(now: SystemTime, mtime: SystemTime) -> String {
    let dur = now.duration_since(mtime).unwrap_or(Duration::ZERO);
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

fn project_dir_mtimes(hex_dir: &Path) -> Vec<Pointer> {
    let projects = hex_dir.join("projects");
    let mut out = Vec::new();
    let entries = match fs::read_dir(&projects) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name_str = name.to_string_lossy().to_string();
        // Filter `boi:`-prefixed noise.
        if name_str.starts_with("boi:") {
            continue;
        }
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(meta) = e.metadata() {
            if let Ok(m) = meta.modified() {
                out.push(Pointer {
                    rel: format!("projects/{}/", name_str),
                    mtime: m,
                });
            }
        }
    }
    out
}

fn decision_files(hex_dir: &Path, limit: usize) -> Vec<Pointer> {
    let mut out: Vec<Pointer> = Vec::new();

    // me/decisions/*.md
    if let Ok(entries) = fs::read_dir(hex_dir.join("me/decisions")) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with("boi:") {
                continue;
            }
            if let Ok(meta) = e.metadata() {
                if let Ok(m) = meta.modified() {
                    out.push(Pointer {
                        rel: format!("me/decisions/{}", name),
                        mtime: m,
                    });
                }
            }
        }
    }

    // projects/*/decisions/*.md
    if let Ok(projs) = fs::read_dir(hex_dir.join("projects")) {
        for proj in projs.flatten() {
            let proj_name = proj.file_name().to_string_lossy().to_string();
            if proj_name.starts_with("boi:") {
                continue;
            }
            let dec_dir = proj.path().join("decisions");
            if let Ok(entries) = fs::read_dir(&dec_dir) {
                for e in entries.flatten() {
                    let path = e.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("md") {
                        continue;
                    }
                    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if let Ok(meta) = e.metadata() {
                        if let Ok(m) = meta.modified() {
                            out.push(Pointer {
                                rel: format!("projects/{}/decisions/{}", proj_name, name),
                                mtime: m,
                            });
                        }
                    }
                }
            }
        }
    }

    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    out.truncate(limit);
    out
}

fn todo_now_top3(hex_dir: &Path) -> Vec<Pointer> {
    let todo_path = hex_dir.join("todo.md");
    let body = match fs::read_to_string(&todo_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mtime = fs::metadata(&todo_path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::now());

    let mut in_now = false;
    let mut items: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("## ") {
            let header = trimmed.trim_start_matches("## ").trim();
            in_now = header.eq_ignore_ascii_case("Now");
            continue;
        }
        if in_now {
            if let Some(rest) = trimmed.strip_prefix("- ") {
                let item = rest.trim();
                if !item.is_empty() {
                    items.push(item.to_string());
                    if items.len() >= 3 {
                        break;
                    }
                }
            }
        }
    }

    items
        .into_iter()
        .map(|item| {
            // Truncate item text so we stay pointers-only (no big bodies).
            // Floor the cut to a char boundary — byte 80 can land mid-char.
            let snippet = if item.len() > 80 {
                let mut end = 80;
                while !item.is_char_boundary(end) {
                    end -= 1;
                }
                // SAFETY(string_slice): `end` was floored to a char boundary by
                // the is_char_boundary loop above.
                #[allow(clippy::string_slice)]
                let head = &item[..end];
                format!("{}…", head)
            } else {
                item
            };
            Pointer {
                rel: format!("todo.md#Now: {}", snippet),
                mtime,
            }
        })
        .collect()
}

/// Collect up to ~10 pointer lines as a single newline-joined string.
/// Pointers only — relative path + human-readable age. Used by both the CLI
/// (`hex memory recent`) and the SessionStart hook (recency prime).
pub fn collect_text(hex_dir: &Path) -> String {
    let mut all: Vec<Pointer> = Vec::new();
    all.extend(project_dir_mtimes(hex_dir));
    all.extend(decision_files(hex_dir, 10));
    all.extend(todo_now_top3(hex_dir));

    // Recency-ordered, newest first.
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all.truncate(10);

    let now = SystemTime::now();
    let lines: Vec<String> = all
        .iter()
        .map(|p| format!("{}  ({})", p.rel, human_age(now, p.mtime)))
        .collect();
    lines.join("\n")
}

/// Collect and print up to ~10 pointer lines. Returns exit code.
pub fn run(hex_dir: &Path) -> i32 {
    let text = collect_text(hex_dir);
    if !text.is_empty() {
        println!("{}", text);
    }
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn make_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::write(p.join("CLAUDE.md"), "").unwrap();
        fs::write(
            p.join("todo.md"),
            "## Now\n- task a\n- task b\n- task c\n- task d\n\n## Later\n- not now\n",
        )
        .unwrap();
        fs::create_dir_all(p.join("projects/older")).unwrap();
        fs::write(p.join("projects/older/context.md"), "older\n").unwrap();
        fs::create_dir_all(p.join("projects/boi:noise")).unwrap();
        fs::write(p.join("projects/boi:noise/context.md"), "noise\n").unwrap();
        sleep(Duration::from_millis(1100));
        fs::create_dir_all(p.join("projects/newer")).unwrap();
        fs::write(p.join("projects/newer/context.md"), "newer\n").unwrap();
        fs::create_dir_all(p.join("me/decisions")).unwrap();
        fs::write(
            p.join("me/decisions/foo-2026-06-05.md"),
            "# Decision: foo\nbody\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn projects_filter_boi_and_recency_order() {
        let ws = make_workspace();
        let ptrs = project_dir_mtimes(ws.path());
        assert!(ptrs.iter().any(|p| p.rel.contains("newer")));
        assert!(ptrs.iter().any(|p| p.rel.contains("older")));
        assert!(
            !ptrs.iter().any(|p| p.rel.contains("boi:noise")),
            "boi: noise must be filtered"
        );
    }

    #[test]
    fn run_produces_non_empty_recency_ordered_pointers() {
        // Capture-by-running: the integration test in tests/memory_recent.rs
        // covers stdout end-to-end. Here we just exercise the collector.
        let ws = make_workspace();
        let mut all: Vec<Pointer> = Vec::new();
        all.extend(project_dir_mtimes(ws.path()));
        all.extend(decision_files(ws.path(), 10));
        all.extend(todo_now_top3(ws.path()));
        all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        all.truncate(10);
        assert!(!all.is_empty(), "must produce pointers");

        let newer_pos = all.iter().position(|p| p.rel.contains("projects/newer/"));
        let older_pos = all.iter().position(|p| p.rel.contains("projects/older/"));
        assert!(newer_pos.is_some() && older_pos.is_some());
        assert!(
            newer_pos.unwrap() < older_pos.unwrap(),
            "newer project must appear before older (recency)"
        );

        // Pointers only — no file bodies.
        for p in &all {
            assert!(!p.rel.contains("older body"));
            assert!(!p.rel.contains("newer body"));
        }
    }

    #[test]
    fn todo_now_returns_top_3_only() {
        let ws = make_workspace();
        let items = todo_now_top3(ws.path());
        assert_eq!(items.len(), 3);
        assert!(items[0].rel.contains("task a"));
        assert!(items[2].rel.contains("task c"));
    }

    #[test]
    fn todo_now_truncates_long_items_at_char_boundary() {
        let ws = make_workspace();
        // 79 ASCII bytes followed by a 3-byte char: byte 80 falls mid-char,
        // so a naive byte slice at 80 panics.
        let long = format!("{}✅ trailing text past the truncation point", "x".repeat(79));
        fs::write(ws.path().join("todo.md"), format!("## Now\n- {}\n", long)).unwrap();
        let items = todo_now_top3(ws.path());
        assert_eq!(items.len(), 1);
        assert!(items[0].rel.ends_with('…'), "long item must be truncated");
    }
}
