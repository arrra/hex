//! `cq rename --apply` — all-or-nothing, content-asserted application of a
//! live rename plan to the worktree (SPEC-A2 §5).
//!
//! Every [`RenameEdit`] carries the `old_text` the daemon read at plan
//! time. Apply runs in three strict phases:
//!
//! 1. **Assert** every edit's span still equals its `old_text` — across ALL
//!    files — before anything is written. Any mismatch aborts the whole
//!    rename ([`CqError::RenameAborted`], exit 7) with ZERO files modified.
//! 2. **Build** each file's new content in memory (edits applied
//!    last-to-first so earlier offsets stay valid; overlaps are loud
//!    errors).
//! 3. **Write** per file via temp-file + rename in the same directory
//!    (never a partially-written file on disk).
//!
//! Coordinates follow the envelope convention: 1-based lines, 1-based BYTE
//! columns (`end_col` exclusive), exactly what `live/translate` emits.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::error::CqError;
use crate::proto::RenameEdit;

/// Apply `edits` to the worktree. Returns the sorted list of modified
/// files. Content mismatches are [`CqError::RenameAborted`] with nothing
/// written; IO failures during the write phase are loud errors (phase 3
/// only starts after every assertion passed).
pub fn apply(worktree_root: &Path, edits: &[RenameEdit]) -> Result<Vec<String>> {
    if edits.is_empty() {
        bail!("rename plan is empty — nothing to apply");
    }

    let mut by_file: BTreeMap<&str, Vec<&RenameEdit>> = BTreeMap::new();
    for edit in edits {
        by_file.entry(edit.path.as_str()).or_default().push(edit);
    }

    // Phases 1+2: assert and build everything in memory first.
    let mut planned: Vec<(&str, String)> = Vec::new();
    for (path, file_edits) in &by_file {
        let full = worktree_root.join(path);
        let content = std::fs::read_to_string(&full)
            .with_context(|| format!("reading {} for rename apply", full.display()))?;
        let new_content = apply_to_content(path, &content, file_edits)?;
        planned.push((path, new_content));
    }

    // Phase 3: write each file atomically (temp + rename, same directory).
    let mut modified = Vec::new();
    for (path, new_content) in planned {
        let full = worktree_root.join(path);
        let tmp = full.with_extension("cq-rename-tmp");
        std::fs::write(&tmp, &new_content).with_context(|| format!("writing {}", tmp.display()))?;
        // Preserve the original file's permissions across the rename. The
        // file is known to exist (phase 1 read it), so a metadata failure
        // here is a real error — never silently skip preservation (S6).
        let meta = std::fs::metadata(&full)
            .with_context(|| format!("reading permissions of {}", full.display()))?;
        std::fs::set_permissions(&tmp, meta.permissions())
            .with_context(|| format!("setting permissions on {}", tmp.display()))?;
        std::fs::rename(&tmp, &full)
            .with_context(|| format!("renaming {} into place", tmp.display()))?;
        modified.push(path.to_string());
    }
    Ok(modified)
}

/// Assert + apply one file's edits against its current content. Pure (no
/// IO) so the all-or-nothing guarantee is unit-testable directly.
fn apply_to_content(path: &str, content: &str, edits: &[&RenameEdit]) -> Result<String> {
    let line_starts = line_starts(content);

    // Resolve every edit to a byte span and assert its old_text.
    let mut spans: Vec<(usize, usize, &RenameEdit)> = Vec::new();
    for edit in edits {
        let start = offset(path, content, &line_starts, edit.line, edit.col)?;
        let end = offset(path, content, &line_starts, edit.end_line, edit.end_col)?;
        if end < start {
            bail!(
                "malformed rename edit in {path}: end {}:{} precedes start {}:{}",
                edit.end_line,
                edit.end_col,
                edit.line,
                edit.col
            );
        }
        // Byte columns come from the rename PLAN, computed against plan-time
        // content; if the file drifted (e.g. a multi-byte char inserted) the
        // offsets can land inside a UTF-8 sequence. `get` returns `None` there
        // instead of panicking, and we abort the whole plan loudly.
        let found = content
            .get(start..end)
            .ok_or_else(|| CqError::RenameAborted {
                path: path.to_string(),
                detail: format!(
                    "edit at {}:{} spans a non-char-boundary byte range {start}..{end} \
                 (stale or malformed plan)",
                    edit.line, edit.col
                ),
            })?;
        if found != edit.old_text {
            return Err(CqError::RenameAborted {
                path: path.to_string(),
                detail: format!(
                    "at {}:{} expected {:?}, found {:?}",
                    edit.line, edit.col, edit.old_text, found
                ),
            }
            .into());
        }
        spans.push((start, end, edit));
    }

    // Last-to-first so earlier byte offsets stay valid; overlaps are bugs
    // in the plan and abort loudly.
    spans.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for pair in spans.windows(2) {
        let (later_start, _, _) = pair[0];
        let (_, earlier_end, e) = pair[1];
        if earlier_end > later_start {
            bail!(
                "overlapping rename edits in {path} (at {}:{}) — refusing to apply",
                e.line,
                e.col
            );
        }
    }

    let mut new_content = content.to_string();
    for (start, end, edit) in spans {
        new_content.replace_range(start..end, &edit.new_text);
    }
    Ok(new_content)
}

/// Byte offsets of each line start.
fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 1-based (line, byte-col) → byte offset, with loud bounds checks.
fn offset(path: &str, content: &str, line_starts: &[usize], line: u32, col: u32) -> Result<usize> {
    if line < 1 || col < 1 {
        bail!("rename edit in {path} has non-1-based position {line}:{col}");
    }
    let line_idx = (line - 1) as usize;
    let Some(&line_start) = line_starts.get(line_idx) else {
        bail!("rename edit in {path} targets line {line}, past the end of the file");
    };
    let line_end = line_starts
        .get(line_idx + 1)
        .copied()
        .unwrap_or(content.len());
    let off = line_start + (col - 1) as usize;
    // A position one past the line's content (exclusive range end) is valid.
    if off > line_end {
        bail!("rename edit in {path} targets column {col} past the end of line {line}");
    }
    Ok(off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn edit(path: &str, line: u32, col: u32, end_col: u32, old: &str, new: &str) -> RenameEdit {
        RenameEdit {
            path: path.into(),
            line,
            col,
            end_line: line,
            end_col,
            new_text: new.into(),
            old_text: old.into(),
        }
    }

    fn worktree(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        dir
    }

    const OPS: &str = "pub fn double(x: i32) -> i32 { x * 2 }\n";
    const LIB: &str =
        "pub fn top(x: i32) -> i32 { double(x) }\npub fn other() -> i32 { double(3) }\n";

    #[test]
    fn applies_multi_file_multi_edit_plan() {
        let wt = worktree(&[("src/ops.rs", OPS), ("src/lib.rs", LIB)]);
        let edits = vec![
            edit("src/ops.rs", 1, 8, 14, "double", "twice"),
            edit("src/lib.rs", 1, 29, 35, "double", "twice"),
            edit("src/lib.rs", 2, 25, 31, "double", "twice"),
        ];
        let modified = apply(wt.path(), &edits).unwrap();
        assert_eq!(
            modified,
            vec!["src/lib.rs".to_string(), "src/ops.rs".to_string()]
        );
        assert_eq!(
            std::fs::read_to_string(wt.path().join("src/ops.rs")).unwrap(),
            "pub fn twice(x: i32) -> i32 { x * 2 }\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt.path().join("src/lib.rs")).unwrap(),
            "pub fn top(x: i32) -> i32 { twice(x) }\npub fn other() -> i32 { twice(3) }\n"
        );
    }

    #[test]
    fn multiple_edits_on_one_line_apply_correctly() {
        let wt = worktree(&[("a.rs", "double(double(1));\n")]);
        let edits = vec![
            edit("a.rs", 1, 1, 7, "double", "twice"),
            edit("a.rs", 1, 8, 14, "double", "twice"),
        ];
        apply(wt.path(), &edits).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.path().join("a.rs")).unwrap(),
            "twice(twice(1));\n"
        );
    }

    #[test]
    fn content_mismatch_aborts_with_zero_files_modified() {
        // The mismatch is in the SECOND file: the first file's edits passed
        // their assertions, yet nothing may be written (all-or-nothing).
        let wt = worktree(&[("src/ops.rs", OPS), ("src/lib.rs", LIB)]);
        let edits = vec![
            edit("src/ops.rs", 1, 8, 14, "double", "twice"),
            edit("src/lib.rs", 1, 29, 35, "CHANGED", "twice"), // stale plan
        ];
        let err = apply(wt.path(), &edits).unwrap_err();
        let cq = err.downcast_ref::<CqError>().expect("CqError");
        assert!(matches!(cq, CqError::RenameAborted { .. }), "{cq}");
        assert_eq!(cq.exit_code(), 7);
        // Zero files modified — including the one whose assertions passed.
        assert_eq!(
            std::fs::read_to_string(wt.path().join("src/ops.rs")).unwrap(),
            OPS
        );
        assert_eq!(
            std::fs::read_to_string(wt.path().join("src/lib.rs")).unwrap(),
            LIB
        );
        // No temp droppings either.
        let leftovers: Vec<_> = walk(wt.path())
            .into_iter()
            .filter(|p| p.to_string_lossy().contains("cq-rename-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                out.extend(walk(&entry.path()));
            } else {
                out.push(entry.path());
            }
        }
        out
    }

    #[test]
    fn out_of_bounds_positions_are_loud_errors_nothing_written() {
        let wt = worktree(&[("a.rs", "fn f() {}\n")]);
        for bad in [
            edit("a.rs", 99, 1, 3, "fn", "x"),  // line past EOF
            edit("a.rs", 1, 50, 55, "fn", "x"), // col past line end
            edit("a.rs", 0, 1, 3, "fn", "x"),   // not 1-based
        ] {
            assert!(apply(wt.path(), &[bad]).is_err());
            assert_eq!(
                std::fs::read_to_string(wt.path().join("a.rs")).unwrap(),
                "fn f() {}\n"
            );
        }
    }

    #[test]
    fn overlapping_edits_are_refused() {
        let wt = worktree(&[("a.rs", "abcdef\n")]);
        let edits = vec![
            edit("a.rs", 1, 1, 5, "abcd", "x"),
            edit("a.rs", 1, 3, 7, "cdef", "y"),
        ];
        let err = apply(wt.path(), &edits).unwrap_err();
        assert!(err.to_string().contains("overlapping"), "{err}");
        assert_eq!(
            std::fs::read_to_string(wt.path().join("a.rs")).unwrap(),
            "abcdef\n"
        );
    }

    #[test]
    fn missing_file_is_loud_error() {
        let wt = worktree(&[]);
        let err = apply(wt.path(), &[edit("gone.rs", 1, 1, 3, "fn", "x")]).unwrap_err();
        assert!(err.to_string().contains("gone.rs"), "{err}");
    }

    #[test]
    fn empty_plan_is_refused() {
        let wt = worktree(&[]);
        assert!(apply(wt.path(), &[]).is_err());
    }

    #[test]
    fn multi_line_span_applies_byte_exactly() {
        let wt = worktree(&[("a.rs", "alpha\nbeta\ngamma\n")]);
        // Span from line 1 col 4 to line 2 col 3 covers "ha\nbe".
        let edits = vec![RenameEdit {
            path: "a.rs".into(),
            line: 1,
            col: 4,
            end_line: 2,
            end_col: 3,
            new_text: "XY".into(),
            old_text: "ha\nbe".into(),
        }];
        apply(wt.path(), &edits).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.path().join("a.rs")).unwrap(),
            "alpXYta\ngamma\n"
        );
    }

    #[test]
    fn multibyte_offset_landing_inside_a_char_is_loud_not_a_panic() {
        // Regression (string_slice class-kill, code-intel site rename_apply.rs:90):
        // a rename plan's byte columns are computed against the content that
        // existed at PLAN time. If the file changes upstream of the edit
        // before APPLY time — e.g. a multibyte char gets inserted — the same
        // byte columns can land inside a multi-byte UTF-8 sequence instead of
        // on a char boundary. `offset`/`apply_to_content` must report this as
        // a loud error (malformed/stale edit), never index into the middle of
        // a UTF-8 sequence and panic.
        let wt = worktree(&[("a.rs", "abc\u{20ac}def\n")]); // '€' is 3 bytes: 3..6
                                                            // 1-based col 5 -> byte offset 4, which is INSIDE the 3-byte '€'
                                                            // (bytes 3..6) -- not a char boundary.
        let bad = edit("a.rs", 1, 5, 6, "x", "y");
        let err = apply(wt.path(), &[bad]).unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "must return a loud error, not panic, on a non-char-boundary edit offset"
        );
        // Nothing written.
        assert_eq!(
            std::fs::read_to_string(wt.path().join("a.rs")).unwrap(),
            "abc\u{20ac}def\n"
        );
    }
}
