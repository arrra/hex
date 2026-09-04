//! Git-blob-OID freshness layer (SPEC-A1 §6, plan Task 7).
//!
//! Every query response is checked per-file against the querying worktree's
//! actual git state. A result file is **stale** when any of:
//!
//! 1. Its *staged* blob OID in the worktree (`git ls-files -s`) differs from
//!    `files.blob_oid` recorded at index time — catches staged edits, and
//!    commits made after indexing (including a worktree checked out at a
//!    different commit).
//! 2. It appears in `git diff --name-only` — catches **unstaged** edits,
//!    which `ls-files -s` cannot see (it reports the staged OID).
//! 3. It is absent from the worktree's git index entirely (e.g. `git rm`).
//!
//! The union of all three is returned, sorted and deduplicated. Stale files
//! are flagged loudly, never silently served (Standing Order S6).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension as _};

/// Compute the stale subset of `files_in_results` for a query running from
/// `query_root`, against the generation database `conn` (its `files` table
/// holds the blob OIDs recorded at index time).
///
/// Paths are workspace-relative (the form stored in `files.path` and emitted
/// in results). Returns a sorted, deduplicated list.
pub fn check(
    query_root: &Path,
    files_in_results: &[String],
    conn: &Connection,
) -> Result<Vec<String>> {
    let paths: BTreeSet<&str> = files_in_results.iter().map(String::as_str).collect();
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Indexed blob OIDs. Every result path comes from the index, so a miss
    // here is an internal inconsistency — fail loudly, don't guess.
    let mut indexed: HashMap<&str, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT blob_oid FROM files WHERE path = ?1")?;
        for path in &paths {
            let oid: Option<String> = stmt
                .query_row([path], |r| r.get(0))
                .optional()
                .with_context(|| format!("looking up indexed blob OID for {path}"))?;
            let oid = oid.with_context(|| {
                format!("result file {path} has no row in the index `files` table")
            })?;
            indexed.insert(path, oid);
        }
    }

    // One subprocess each: staged OIDs, then unstaged-dirty paths.
    let worktree_oids = ls_files_oids(query_root, &paths)?;
    let dirty = diff_dirty_paths(query_root, &paths)?;

    let mut stale = Vec::new();
    for path in &paths {
        let is_stale = match worktree_oids.get(*path) {
            // Absent from the worktree's git index → stale (spec §6 rule 3).
            None => true,
            // Staged OID drifted from index-time OID → stale.
            Some(oid) => oid != &indexed[path],
        } || dirty.contains(*path);
        if is_stale {
            stale.push((*path).to_string());
        }
    }
    Ok(stale)
}

/// `git -C <root> ls-files -s -z -- <paths>` → path → staged blob OID.
fn ls_files_oids(root: &Path, paths: &BTreeSet<&str>) -> Result<HashMap<String, String>> {
    let stdout = run_git(root, &["ls-files", "-s", "-z"], paths)?;
    let mut map = HashMap::new();
    for entry in stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = std::str::from_utf8(entry).context("non-UTF8 entry in git ls-files output")?;
        // Format: "<mode> <oid> <stage>\t<path>"
        let (meta, path) = entry
            .split_once('\t')
            .with_context(|| format!("unparseable git ls-files entry: {entry:?}"))?;
        let oid = meta
            .split(' ')
            .nth(1)
            .with_context(|| format!("unparseable git ls-files entry: {entry:?}"))?;
        map.insert(path.to_string(), oid.to_string());
    }
    Ok(map)
}

/// `git -C <root> diff --name-only -z -- <paths>` → paths with unstaged
/// modifications (worktree differs from the staged index).
fn diff_dirty_paths(root: &Path, paths: &BTreeSet<&str>) -> Result<HashSet<String>> {
    let stdout = run_git(root, &["diff", "--name-only", "-z"], paths)?;
    let mut set = HashSet::new();
    for entry in stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let path = std::str::from_utf8(entry).context("non-UTF8 path in git diff output")?;
        set.insert(path.to_string());
    }
    Ok(set)
}

/// Run a git subcommand in `root` with an explicit `--` pathspec list.
/// Any git failure is a loud error, never an empty result.
fn run_git(root: &Path, args: &[&str], paths: &BTreeSet<&str>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args).arg("--");
    for path in paths {
        cmd.arg(path);
    }
    let out = cmd
        .output()
        .with_context(|| format!("spawning git {} in {}", args.join(" "), root.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use tempfile::TempDir;

    fn run(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Repo with two committed files, plus a generation DB whose `files`
    /// table records the blob OIDs as of that commit.
    fn fixture() -> (TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {}\n").unwrap();
        run(dir.path(), &["init", "-q", "-b", "main"]);
        run(dir.path(), &["config", "user.email", "t@t"]);
        run(dir.path(), &["config", "user.name", "t"]);
        run(dir.path(), &["add", "-A"]);
        run(dir.path(), &["commit", "-q", "-m", "init"]);

        let conn = Connection::open_in_memory().unwrap();
        schema::create(&conn).unwrap();
        let all: BTreeSet<&str> = ["a.rs", "src/b.rs"].into();
        for (path, oid) in ls_files_oids(dir.path(), &all).unwrap() {
            conn.execute(
                "INSERT INTO files (path, blob_oid, language) VALUES (?1, ?2, 'rust')",
                rusqlite::params![path, oid],
            )
            .unwrap();
        }
        (dir, conn)
    }

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn untouched_worktree_is_all_fresh() {
        let (dir, conn) = fixture();
        let stale = check(dir.path(), &paths(&["a.rs", "src/b.rs"]), &conn).unwrap();
        assert_eq!(stale, Vec::<String>::new());
    }

    #[test]
    fn empty_input_is_empty_output() {
        let (dir, conn) = fixture();
        assert_eq!(check(dir.path(), &[], &conn).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn unstaged_edit_is_stale() {
        // ls-files -s still shows the committed OID; only the diff sees this.
        let (dir, conn) = fixture();
        std::fs::write(dir.path().join("a.rs"), "fn a() {} // edited\n").unwrap();
        let stale = check(dir.path(), &paths(&["a.rs", "src/b.rs"]), &conn).unwrap();
        assert_eq!(stale, paths(&["a.rs"]));
    }

    #[test]
    fn staged_edit_is_stale() {
        // Staged content: the diff (index vs worktree) is empty; only the
        // ls-files OID comparison sees this.
        let (dir, conn) = fixture();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {} // staged\n").unwrap();
        run(dir.path(), &["add", "src/b.rs"]);
        let stale = check(dir.path(), &paths(&["a.rs", "src/b.rs"]), &conn).unwrap();
        assert_eq!(stale, paths(&["src/b.rs"]));
    }

    #[test]
    fn commit_after_indexing_is_stale() {
        let (dir, conn) = fixture();
        std::fs::write(dir.path().join("a.rs"), "fn a() {} // v2\n").unwrap();
        run(dir.path(), &["add", "-A"]);
        run(dir.path(), &["commit", "-q", "-m", "v2"]);
        let stale = check(dir.path(), &paths(&["a.rs"]), &conn).unwrap();
        assert_eq!(stale, paths(&["a.rs"]));
    }

    #[test]
    fn file_removed_from_git_is_stale() {
        let (dir, conn) = fixture();
        run(dir.path(), &["rm", "-q", "a.rs"]);
        let stale = check(dir.path(), &paths(&["a.rs", "src/b.rs"]), &conn).unwrap();
        assert_eq!(stale, paths(&["a.rs"]));
    }

    #[test]
    fn duplicate_input_paths_are_deduplicated() {
        let (dir, conn) = fixture();
        std::fs::write(dir.path().join("a.rs"), "fn a() {} // edited\n").unwrap();
        let stale = check(dir.path(), &paths(&["a.rs", "a.rs", "a.rs"]), &conn).unwrap();
        assert_eq!(stale, paths(&["a.rs"]));
    }

    #[test]
    fn result_file_missing_from_index_table_is_loud_error() {
        let (dir, conn) = fixture();
        let err = check(dir.path(), &paths(&["not-indexed.rs"]), &conn).unwrap_err();
        assert!(err.to_string().contains("no row in the index"), "{err}");
    }

    #[test]
    fn non_git_query_root_is_loud_error() {
        let (_dir, conn) = fixture();
        let plain = tempfile::tempdir().unwrap();
        assert!(check(plain.path(), &paths(&["a.rs"]), &conn).is_err());
    }
}
