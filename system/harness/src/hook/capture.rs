use std::path::{Path, PathBuf};
use std::time::SystemTime;

// ── path derivation ───────────────────────────────────────────────────────────

/// Convert a CLAUDE_PROJECT_DIR value to the slug used by Claude Code.
///
/// Claude Code turns the project dir path into a storage slug by replacing
/// every '/' with '-' (no leading dash; the path always starts with '/').
fn dir_to_slug(project_dir: &str) -> String {
    project_dir.replace('/', "-")
}

/// Fast-path source resolution using env vars (O(1)).
pub fn fast_path_source(
    projects_dir: &Path,
    project_dir: &str,
    session_id: &str,
) -> PathBuf {
    let slug = dir_to_slug(project_dir);
    projects_dir.join(&slug).join(format!("{session_id}.jsonl"))
}

/// Fallback: walk `~/.claude/projects/` up to depth 2, return the
/// most-recently-modified `.jsonl` file.
pub fn find_latest_jsonl(projects_dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;

    let top = match std::fs::read_dir(projects_dir) {
        Ok(d) => d,
        Err(_) => return None,
    };

    for project_entry in top.filter_map(|e| e.ok()) {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let inner = match std::fs::read_dir(&project_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for file_entry in inner.filter_map(|e| e.ok()) {
            let path = file_entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !name.ends_with(".jsonl") {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    match &best {
                        None => best = Some((mtime, path)),
                        Some((prev_time, _)) if mtime > *prev_time => {
                            best = Some((mtime, path));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    best.map(|(_, p)| p)
}

// ── entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    let home = match std::env::var("HOME").ok().map(PathBuf::from) {
        Some(h) => h,
        None => return,
    };
    let projects_dir = home.join(".claude/projects");

    let hex_dir = match std::env::var("HEX_DIR")
        .ok()
        .or_else(|| std::env::var("CLAUDE_PROJECT_DIR").ok())
        .map(PathBuf::from)
    {
        Some(d) => d,
        None => return,
    };
    let backup_dir = hex_dir.join("raw/transcripts");

    // Determine source path.
    let source: Option<PathBuf> = {
        let session_id = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();
        let project_dir = std::env::var("CLAUDE_PROJECT_DIR").unwrap_or_default();

        if !session_id.is_empty() && !project_dir.is_empty() {
            let candidate = fast_path_source(&projects_dir, &project_dir, &session_id);
            if candidate.is_file() {
                Some(candidate)
            } else {
                find_latest_jsonl(&projects_dir)
            }
        } else {
            find_latest_jsonl(&projects_dir)
        }
    };

    let source = match source {
        Some(s) => s,
        None => return,
    };

    let basename = match source.file_name() {
        Some(n) => n.to_os_string(),
        None => return,
    };
    let dest = backup_dir.join(&basename);

    // Spawn a child `cp` process so the hook returns immediately.
    // A spawned subprocess lives independently after the parent exits,
    // faithfully mirroring the shell `(...) &` backgrounding in backup_session.sh.
    if std::fs::create_dir_all(&backup_dir).is_ok() {
        std::process::Command::new("cp")
            .arg(&source)
            .arg(&dest)
            .spawn()
            .ok();
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn slug_replaces_slashes() {
        assert_eq!(dir_to_slug("/Users/test/hex"), "-Users-test-hex");
    }

    #[test]
    fn fast_path_derives_correct_path() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();
        let project_dir = "/Users/test/hex";
        let session_id = "abc123";

        let got = fast_path_source(projects_dir, project_dir, session_id);
        let expected = projects_dir
            .join("-Users-test-hex")
            .join("abc123.jsonl");
        assert_eq!(got, expected);
    }

    #[test]
    fn find_latest_jsonl_returns_most_recent() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();

        // Create two project subdirs with .jsonl files.
        let proj_a = projects_dir.join("proj-a");
        let proj_b = projects_dir.join("proj-b");
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();

        std::fs::write(proj_a.join("old.jsonl"), b"old").unwrap();
        // Small sleep to ensure different mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(proj_b.join("new.jsonl"), b"new").unwrap();

        let result = find_latest_jsonl(projects_dir).expect("should find a file");
        assert_eq!(result.file_name().unwrap(), "new.jsonl");
    }

    #[test]
    fn find_latest_jsonl_empty_dir_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(find_latest_jsonl(tmp.path()).is_none());
    }

    #[test]
    fn find_latest_jsonl_ignores_non_jsonl() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();
        let proj = projects_dir.join("proj-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("session.log"), b"not jsonl").unwrap();

        assert!(find_latest_jsonl(projects_dir).is_none());
    }
}
