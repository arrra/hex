//! LSP ↔ cq translation (Task 4, SPEC-A2 §3/§5).
//!
//! `live_def` / `live_refs` / `live_callers` / `live_rename` run one live
//! query against a [`LiveBackend`] and normalize the LSP answer into the A1
//! envelope shapes: [`QueryResult`] rows (paths relative to the worktree
//! root, 1-based positions) and [`RenameEdit`] lists.
//!
//! ## Coordinate conventions — the ONE conversion pair
//!
//! cq positions are **1-based line + 1-based UTF-8 byte column** (the A1
//! convention: SCIP occurrence columns are UTF-8 code-unit offsets and the
//! envelope adds 1). LSP positions are **0-based line + 0-based UTF-16
//! code-unit column**. [`to_lsp_pos`] / [`from_lsp_pos`] are the only place
//! the conversion happens; everything else in the live path is raw LSP.
//!
//! **Multibyte is best-effort:** on pure-ASCII lines (the overwhelmingly
//! common case for Rust identifiers) byte and UTF-16 columns coincide
//! exactly. On lines with multibyte characters the pair converts correctly
//! through `char` iteration; a cq column that lands *inside* a UTF-8
//! sequence is rejected loudly, and an LSP column that lands inside a
//! surrogate pair is rounded down to the character start (logged, never
//! silent). LSP columns past the end of the line clamp to line end per the
//! LSP spec.
//!
//! ## What live results carry
//!
//! Live answers have no SCIP symbol table behind them, so:
//! - `symbol` is `""` (documented sentinel: "no SCIP symbol on the live path"),
//! - `display_name` is the source text spanned by the result range,
//! - `kind` is mapped from the LSP `SymbolKind` where one exists (callers);
//!   def/refs locations carry no kind → `"unknown"` (same fallback string A1
//!   uses for unmappable SCIP kinds).
//!
//! Errors preserve [`LiveError`] via `anyhow` source-chaining — the daemon
//! (Task 5) downcasts to map `Warming` to the immediate warming reply.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::envelope::QueryResult;
use crate::live::lsp::{self, Position};
use crate::live::LiveBackend;
use crate::proto::RenameEdit;

// ---------------------------------------------------------------------------
// The ONE position-conversion pair (SPEC-A2 §3)
// ---------------------------------------------------------------------------

/// cq (1-based line, 1-based UTF-8 byte col) → LSP (0-based, UTF-16 col),
/// given the text of that line. Loud errors on a zero coordinate, a column
/// past the end of the line, or a column inside a multibyte sequence.
pub fn to_lsp_pos(line_text: &str, line: u32, col: u32) -> Result<Position> {
    if line == 0 || col == 0 {
        bail!("cq positions are 1-based; got {line}:{col}");
    }
    let byte_target = (col - 1) as usize;
    let mut bytes = 0usize;
    let mut utf16 = 0u32;
    for ch in line_text.chars() {
        if bytes == byte_target {
            break;
        }
        if bytes > byte_target {
            bail!(
                "column {col} is inside a multibyte character on line {line} \
                 (byte offset {byte_target} is not a char boundary)"
            );
        }
        bytes += ch.len_utf8();
        utf16 += ch.len_utf16() as u32;
    }
    if bytes < byte_target {
        bail!(
            "column {col} is past the end of line {line} ({} bytes)",
            line_text.len()
        );
    }
    Ok(Position {
        line: line - 1,
        character: utf16,
    })
}

/// LSP (0-based, UTF-16 col) → cq (1-based line, 1-based UTF-8 byte col),
/// given the text of that line. Columns past line end clamp to line end (LSP
/// spec semantics); a column inside a surrogate pair rounds down to the
/// character start with a loud log.
pub fn from_lsp_pos(line_text: &str, pos: Position) -> (u32, u32) {
    (pos.line + 1, byte_col(line_text, pos) + 1)
}

/// 0-based UTF-8 byte offset of an LSP UTF-16 character offset within a line.
fn byte_col(line_text: &str, pos: Position) -> u32 {
    let mut bytes = 0u32;
    let mut utf16 = 0u32;
    for ch in line_text.chars() {
        if utf16 == pos.character {
            return bytes;
        }
        let next_utf16 = utf16 + ch.len_utf16() as u32;
        if pos.character < next_utf16 {
            eprintln!(
                "live: LSP column {} on line {} lands inside a surrogate pair — \
                 rounding down to the character start (best-effort multibyte)",
                pos.character, pos.line
            );
            return bytes;
        }
        bytes += ch.len_utf8() as u32;
        utf16 = next_utf16;
    }
    // Past end of line: clamp (LSP spec: positions beyond line length mean
    // line end).
    bytes
}

// ---------------------------------------------------------------------------
// File access (live truth = disk state of the worktree)
// ---------------------------------------------------------------------------

/// One file read from disk, with byte offsets of each line start — supports
/// both line-text lookups and exact (multi-line, newline-preserving) span
/// extraction.
struct FileText {
    content: String,
    /// Byte offset where each 0-based line starts.
    line_starts: Vec<usize>,
}

impl FileText {
    fn read(root: &Path, rel: &str) -> Result<Self> {
        let full = root.join(rel);
        let content = std::fs::read_to_string(&full)
            .with_context(|| format!("reading {} for live translation", full.display()))?;
        let mut line_starts = vec![0usize];
        for (i, b) in content.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Ok(FileText {
            content,
            line_starts,
        })
    }

    /// Text of 0-based line `idx`, without its newline.
    fn line(&self, idx: usize) -> Result<&str> {
        let start = *self
            .line_starts
            .get(idx)
            .ok_or_else(|| anyhow!("file has no line {} (live/disk mismatch)", idx + 1))?;
        let end = self
            .line_starts
            .get(idx + 1)
            .map_or(self.content.len(), |next| next - 1);
        Ok(self.content[start..end].trim_end_matches('\r'))
    }

    /// Absolute byte offset of an LSP position within the file.
    fn byte_offset(&self, pos: Position) -> Result<usize> {
        let line = self.line(pos.line as usize)?;
        let start = self.line_starts[pos.line as usize];
        Ok(start + byte_col(line, pos) as usize)
    }

    /// Exact source text spanned by an LSP range — handles multi-line ranges
    /// (newlines preserved verbatim from disk).
    fn span(&self, range: &lsp::Range) -> Result<&str> {
        let start = self.byte_offset(range.start)?;
        let end = self.byte_offset(range.end)?;
        if start > end {
            bail!(
                "inverted LSP range {}:{}..{}:{}",
                range.start.line,
                range.start.character,
                range.end.line,
                range.end.character
            );
        }
        Ok(&self.content[start..end])
    }
}

// ---------------------------------------------------------------------------
// Live query verbs
// ---------------------------------------------------------------------------

/// Live `def`: LSP `textDocument/definition`, normalized to A1 result rows
/// (role `"definition"`).
pub fn live_def(
    backend: &dyn LiveBackend,
    worktree_root: &Path,
    path: &str,
    line: u32,
    col: u32,
) -> Result<Vec<QueryResult>> {
    let params = position_params(worktree_root, path, line, col)?;
    let result = backend
        .request(lsp::methods::DEFINITION, serde_json::to_value(&params)?)
        .map_err(anyhow::Error::from)
        .context("live definition request")?;
    let locations = lsp::definition_locations(&result)?;
    let mut out = Vec::new();
    for loc in &locations {
        if let Some(row) = location_to_result(worktree_root, loc, "definition")? {
            out.push(row);
        }
    }
    sort_results(&mut out);
    Ok(out)
}

/// Live `refs`: LSP `textDocument/references` with `includeDeclaration:true`
/// (the A1 refs set includes the definition). Rows matching a definition
/// location get role `"definition"`, the rest `"reference"` — classified via
/// one extra definition request, mirroring A1's role column.
pub fn live_refs(
    backend: &dyn LiveBackend,
    worktree_root: &Path,
    path: &str,
    line: u32,
    col: u32,
) -> Result<Vec<QueryResult>> {
    let pos = position_params(worktree_root, path, line, col)?;
    let def_result = backend
        .request(lsp::methods::DEFINITION, serde_json::to_value(&pos)?)
        .map_err(anyhow::Error::from)
        .context("live definition request (for refs role classification)")?;
    let def_keys: BTreeSet<(String, u32, u32)> = lsp::definition_locations(&def_result)?
        .iter()
        .map(|loc| (loc.uri.clone(), loc.range.start.line, loc.range.start.character))
        .collect();

    let params = lsp::ReferenceParams {
        position: pos,
        context: lsp::ReferenceContext {
            include_declaration: true,
        },
    };
    let result = backend
        .request(lsp::methods::REFERENCES, serde_json::to_value(&params)?)
        .map_err(anyhow::Error::from)
        .context("live references request")?;
    let locations: Vec<lsp::Location> = match result {
        Value::Null => Vec::new(),
        other => serde_json::from_value(other).context("parsing references result")?,
    };
    let mut out = Vec::new();
    for loc in &locations {
        let key = (loc.uri.clone(), loc.range.start.line, loc.range.start.character);
        let role = if def_keys.contains(&key) {
            "definition"
        } else {
            "reference"
        };
        if let Some(row) = location_to_result(worktree_root, loc, role)? {
            out.push(row);
        }
    }
    sort_results(&mut out);
    Ok(out)
}

/// Live `callers`: `textDocument/prepareCallHierarchy` +
/// `callHierarchy/incomingCalls`, normalized the way A1 does callers — one
/// row per DISTINCT caller, located at the caller's own definition
/// (selection range), role `"definition"`. The opaque rust-analyzer `data`
/// token round-trips inside [`lsp::CallHierarchyItem::extra`].
pub fn live_callers(
    backend: &dyn LiveBackend,
    worktree_root: &Path,
    path: &str,
    line: u32,
    col: u32,
) -> Result<Vec<QueryResult>> {
    let params = position_params(worktree_root, path, line, col)?;
    let prepared = backend
        .request(
            lsp::methods::PREPARE_CALL_HIERARCHY,
            serde_json::to_value(&params)?,
        )
        .map_err(anyhow::Error::from)
        .context("live prepareCallHierarchy request")?;
    let items: Vec<lsp::CallHierarchyItem> = match prepared {
        Value::Null => Vec::new(),
        other => serde_json::from_value(other).context("parsing prepareCallHierarchy result")?,
    };

    let root = worktree_root;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        let calls_result = backend
            .request(
                lsp::methods::INCOMING_CALLS,
                serde_json::to_value(&lsp::CallHierarchyIncomingCallsParams { item })?,
            )
            .map_err(anyhow::Error::from)
            .context("live incomingCalls request")?;
        let calls: Vec<lsp::CallHierarchyIncomingCall> = match calls_result {
            Value::Null => Vec::new(),
            other => serde_json::from_value(other).context("parsing incomingCalls result")?,
        };
        for call in calls {
            let from = &call.from;
            let Some(rel) = relativize(root, &from.uri)? else {
                continue; // out-of-tree caller — logged by relativize
            };
            let file = FileText::read(root, &rel)?;
            let start = from.selection_range.start;
            let (line1, col1) = from_lsp_pos(file.line(start.line as usize)?, start);
            if !seen.insert((rel.clone(), line1, col1)) {
                continue; // same caller reached via two hierarchy items
            }
            out.push(QueryResult {
                path: rel,
                line: line1,
                col: col1,
                symbol: String::new(),
                display_name: from.name.clone(),
                kind: symbol_kind_str(from.kind),
                role: "definition".into(),
                snippet: Some(file.line(start.line as usize)?.to_string()),
            });
        }
    }
    sort_results(&mut out);
    Ok(out)
}

/// Live `rename`: LSP `textDocument/rename`, normalized to the SPEC-A2 §5
/// edit list. Every edit carries `old_text` extracted from the CURRENT file
/// content at the edit range (multi-line ranges handled — the span is sliced
/// byte-exactly, newlines preserved) — this is what makes the apply-time
/// content assertions (RENAME_ABORTED on mismatch) possible.
pub fn live_rename(
    backend: &dyn LiveBackend,
    worktree_root: &Path,
    path: &str,
    line: u32,
    col: u32,
    new_name: &str,
) -> Result<Vec<RenameEdit>> {
    let params = lsp::RenameParams {
        position: position_params(worktree_root, path, line, col)?,
        new_name: new_name.to_string(),
    };
    let result = backend
        .request(lsp::methods::RENAME, serde_json::to_value(&params)?)
        .map_err(anyhow::Error::from)
        .context("live rename request")?;
    if result.is_null() {
        bail!("rename at {path}:{line}:{col} produced no edits (not a renameable symbol?)");
    }
    let edit: lsp::WorkspaceEdit =
        serde_json::from_value(result).context("parsing rename WorkspaceEdit")?;
    workspace_edit_to_renames(worktree_root, &edit)
}

/// Normalize an LSP `WorkspaceEdit` into sorted [`RenameEdit`]s with
/// `old_text` read from disk. Split out of [`live_rename`] for direct unit
/// testing without a backend.
fn workspace_edit_to_renames(
    worktree_root: &Path,
    edit: &lsp::WorkspaceEdit,
) -> Result<Vec<RenameEdit>> {
    // Collect (uri, edits) from whichever shape the server used. Observed
    // 2026-06-11: rust-analyzer answers rename with `documentChanges`
    // (TextDocumentEdit form) even though we never declare the capability —
    // so both forms are supported. Resource operations (create/rename/
    // delete file entries, which carry a `kind` tag) are rejected loudly:
    // a symbol rename must never silently drop a file-level change.
    let mut per_file: Vec<(String, Vec<lsp::TextEdit>)> = Vec::new();
    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            per_file.push((uri.clone(), edits.clone()));
        }
    }
    if let Some(dc) = &edit.document_changes {
        let entries = dc
            .as_array()
            .ok_or_else(|| anyhow!("rename documentChanges is not an array: {dc}"))?;
        for entry in entries {
            if let Some(kind) = entry.get("kind") {
                bail!(
                    "rename produced a resource operation ({kind}) — file-level \
                     changes are not supported by cq rename: {entry}"
                );
            }
            let doc_edit: DocumentEdit = serde_json::from_value(entry.clone())
                .with_context(|| format!("parsing rename documentChanges entry: {entry}"))?;
            per_file.push((doc_edit.text_document.uri, doc_edit.edits));
        }
    }
    if per_file.is_empty() {
        bail!("rename returned a WorkspaceEdit with no changes");
    }

    let mut out = Vec::new();
    for (uri, edits) in &per_file {
        let Some(rel) = relativize(worktree_root, uri)? else {
            bail!("rename touches a file outside the worktree: {uri}");
        };
        let file = FileText::read(worktree_root, &rel)?;
        for te in edits {
            let (line1, col1) = from_lsp_pos(file.line(te.range.start.line as usize)?, te.range.start);
            let (end_line1, end_col1) =
                from_lsp_pos(file.line(te.range.end.line as usize)?, te.range.end);
            out.push(RenameEdit {
                path: rel.clone(),
                line: line1,
                col: col1,
                end_line: end_line1,
                end_col: end_col1,
                new_text: te.new_text.clone(),
                old_text: file.span(&te.range)?.to_string(),
            });
        }
    }
    out.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
    Ok(out)
}

/// `TextDocumentEdit` from a `documentChanges` array (the shape
/// rust-analyzer actually answers rename with).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentEdit {
    text_document: lsp::TextDocumentIdentifier,
    edits: Vec<lsp::TextEdit>,
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// Build LSP position params for a cq target, converting through the file's
/// actual line text (read from disk — live truth is disk state).
fn position_params(
    worktree_root: &Path,
    path: &str,
    line: u32,
    col: u32,
) -> Result<lsp::TextDocumentPositionParams> {
    let file = FileText::read(worktree_root, path)?;
    let position = to_lsp_pos(file.line((line.max(1) - 1) as usize)?, line, col)
        .with_context(|| format!("converting {path}:{line}:{col} to LSP coordinates"))?;
    Ok(lsp::TextDocumentPositionParams {
        text_document: lsp::TextDocumentIdentifier {
            uri: lsp::path_to_uri(&worktree_root.join(path)),
        },
        position,
    })
}

/// `file://` URI → worktree-relative path. `Ok(None)` (with a loud log) for
/// in-bounds-but-outside-the-worktree results — e.g. a definition resolving
/// into the standard library, which the worktree-relative A1 shape cannot
/// express. Non-file URIs are hard errors.
fn relativize(worktree_root: &Path, uri: &str) -> Result<Option<String>> {
    let abs: PathBuf = lsp::uri_to_path(uri)
        .ok_or_else(|| anyhow!("non-file URI in live result: {uri}"))?;
    match abs.strip_prefix(worktree_root) {
        Ok(rel) => Ok(Some(rel.to_string_lossy().into_owned())),
        Err(_) => {
            eprintln!(
                "live: dropping out-of-worktree result {} (root {})",
                abs.display(),
                worktree_root.display()
            );
            Ok(None)
        }
    }
}

/// One LSP `Location` → an A1 result row. `Ok(None)` when the location falls
/// outside the worktree (logged).
fn location_to_result(
    worktree_root: &Path,
    loc: &lsp::Location,
    role: &str,
) -> Result<Option<QueryResult>> {
    let Some(rel) = relativize(worktree_root, &loc.uri)? else {
        return Ok(None);
    };
    let file = FileText::read(worktree_root, &rel)?;
    let line_text = file.line(loc.range.start.line as usize)?;
    let (line1, col1) = from_lsp_pos(line_text, loc.range.start);
    Ok(Some(QueryResult {
        path: rel,
        line: line1,
        col: col1,
        symbol: String::new(),
        display_name: file.span(&loc.range)?.to_string(),
        kind: "unknown".into(),
        role: role.into(),
        snippet: Some(line_text.to_string()),
    }))
}

/// Deterministic A1 result order: (path, line, col).
fn sort_results(results: &mut [QueryResult]) {
    results.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
}

/// Human kind from the LSP `SymbolKind` numeric (subset rust-analyzer emits
/// for call-hierarchy items); unknown values → `"unknown"`, never a panic.
fn symbol_kind_str(kind: i64) -> String {
    match kind {
        2 => "module",
        5 => "class",
        6 => "method",
        8 => "field",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        22 => "enummember",
        23 => "struct",
        _ => "unknown",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::instance::{InstanceState, LiveError, LiveInstance, LiveResult};
    use serde_json::json;
    use std::collections::HashMap;
    use std::process::Command;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    // ---- the ONE conversion pair ----

    #[test]
    fn ascii_roundtrip_byte_and_utf16_coincide() {
        let line = "pub fn double(x: i32) -> i32 { x * 2 }";
        // `double` starts at byte 7 → cq col 8 → LSP character 7.
        let pos = to_lsp_pos(line, 1, 8).unwrap();
        assert_eq!(pos, Position { line: 0, character: 7 });
        assert_eq!(from_lsp_pos(line, pos), (1, 8));
        // End-of-line position (after last char) is valid both ways.
        let end = to_lsp_pos(line, 1, line.len() as u32 + 1).unwrap();
        assert_eq!(end.character, line.len() as u32);
        assert_eq!(from_lsp_pos(line, end), (1, line.len() as u32 + 1));
    }

    #[test]
    fn multibyte_two_byte_char_converts_best_effort() {
        // "é" is 2 UTF-8 bytes, 1 UTF-16 unit. Identifier after "let é_x = "
        let line = "let é_x = double(1);";
        let byte_col = line.find("double").unwrap() as u32 + 1; // 1-based byte col
        let pos = to_lsp_pos(line, 3, byte_col).unwrap();
        // bytes before "double": "let é_x = " = 11 bytes, 10 UTF-16 units.
        assert_eq!(pos, Position { line: 2, character: 10 });
        assert_eq!(from_lsp_pos(line, pos), (3, byte_col));
    }

    #[test]
    fn multibyte_supplementary_char_converts_best_effort() {
        // "🦀" is 4 UTF-8 bytes, 2 UTF-16 units.
        let line = "let 🦀 = twice(2);";
        let byte_col = line.find("twice").unwrap() as u32 + 1;
        let pos = to_lsp_pos(line, 1, byte_col).unwrap();
        assert_eq!(pos.character, "let 🦀 = ".chars().map(char::len_utf16).sum::<usize>() as u32);
        assert_eq!(from_lsp_pos(line, pos), (1, byte_col));
    }

    #[test]
    fn column_inside_multibyte_sequence_is_loud() {
        let line = "🦀x";
        // byte col 2/3/4 are inside the crab.
        let err = to_lsp_pos(line, 1, 3).unwrap_err();
        assert!(err.to_string().contains("multibyte"), "{err}");
    }

    #[test]
    fn zero_or_overflow_columns_are_loud_or_clamped_per_direction() {
        assert!(to_lsp_pos("abc", 0, 1).is_err());
        assert!(to_lsp_pos("abc", 1, 0).is_err());
        let err = to_lsp_pos("abc", 1, 10).unwrap_err();
        assert!(err.to_string().contains("past the end"), "{err}");
        // LSP → cq clamps past-end columns (LSP spec semantics).
        assert_eq!(
            from_lsp_pos("abc", Position { line: 0, character: 99 }),
            (1, 4)
        );
        // Inside a surrogate pair: round down to the char start, loudly.
        let line = "🦀x";
        assert_eq!(from_lsp_pos(line, Position { line: 0, character: 1 }), (1, 1));
    }

    // ---- old_text extraction (FileText::span), incl. multi-line ----

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    #[test]
    fn span_extracts_single_and_multi_line_ranges() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.rs", "fn double() {\n    body\n}\n");
        let file = FileText::read(dir.path(), "a.rs").unwrap();
        // single-line: "double" on line 0, chars 3..9
        let r = lsp::Range {
            start: Position { line: 0, character: 3 },
            end: Position { line: 0, character: 9 },
        };
        assert_eq!(file.span(&r).unwrap(), "double");
        // multi-line: from "{"  through the closing "}" — newlines preserved.
        let r = lsp::Range {
            start: Position { line: 0, character: 12 },
            end: Position { line: 2, character: 1 },
        };
        assert_eq!(file.span(&r).unwrap(), "{\n    body\n}");
    }

    #[test]
    fn span_past_eof_or_inverted_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.rs", "one line\n");
        let file = FileText::read(dir.path(), "a.rs").unwrap();
        let past = lsp::Range {
            start: Position { line: 5, character: 0 },
            end: Position { line: 5, character: 1 },
        };
        assert!(file.span(&past).is_err());
        let inverted = lsp::Range {
            start: Position { line: 0, character: 4 },
            end: Position { line: 0, character: 1 },
        };
        let err = file.span(&inverted).unwrap_err();
        assert!(err.to_string().contains("inverted"), "{err}");
    }

    // ---- WorkspaceEdit normalization without a backend ----

    #[test]
    fn workspace_edit_normalizes_with_old_text_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write_file(&root, "src/ops.rs", "pub fn double(x: i32) -> i32 { x * 2 }\n");
        write_file(&root, "src/lib.rs", "fn t(x: i32) -> i32 { ops::double(x) }\n");
        let ops_uri = lsp::path_to_uri(&root.join("src/ops.rs"));
        let lib_uri = lsp::path_to_uri(&root.join("src/lib.rs"));
        let edit: lsp::WorkspaceEdit = serde_json::from_value(json!({
            "changes": {
                lib_uri: [{"range": {"start": {"line": 0, "character": 27},
                                      "end": {"line": 0, "character": 33}},
                           "newText": "twice"}],
                ops_uri: [{"range": {"start": {"line": 0, "character": 7},
                                      "end": {"line": 0, "character": 13}},
                           "newText": "twice"}],
            }
        }))
        .unwrap();
        let edits = workspace_edit_to_renames(&root, &edit).unwrap();
        assert_eq!(edits.len(), 2);
        // Sorted by (path, line, col): lib.rs before ops.rs.
        assert_eq!(
            (edits[0].path.as_str(), edits[0].line, edits[0].col, edits[0].end_col),
            ("src/lib.rs", 1, 28, 34)
        );
        assert_eq!(edits[0].old_text, "double");
        assert_eq!(edits[0].new_text, "twice");
        assert_eq!(
            (edits[1].path.as_str(), edits[1].line, edits[1].col),
            ("src/ops.rs", 1, 8)
        );
        assert_eq!(edits[1].old_text, "double");
    }

    #[test]
    fn workspace_edit_document_changes_form_normalizes() {
        // The shape rust-analyzer actually answers rename with (observed
        // 2026-06-11): documentChanges + TextDocumentEdit entries.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write_file(&root, "src/ops.rs", "pub fn double(x: i32) -> i32 { x * 2 }\n");
        let uri = lsp::path_to_uri(&root.join("src/ops.rs"));
        let edit: lsp::WorkspaceEdit = serde_json::from_value(json!({
            "documentChanges": [{
                "textDocument": {"uri": uri, "version": null},
                "edits": [{"range": {"start": {"line": 0, "character": 7},
                                      "end": {"line": 0, "character": 13}},
                           "newText": "twice"}]
            }]
        }))
        .unwrap();
        let edits = workspace_edit_to_renames(&root, &edit).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!((edits[0].line, edits[0].col, edits[0].end_col), (1, 8, 14));
        assert_eq!(edits[0].old_text, "double");
    }

    #[test]
    fn workspace_edit_resource_operation_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        let edit: lsp::WorkspaceEdit = serde_json::from_value(json!({
            "documentChanges": [{"kind": "rename", "oldUri": "file:///a", "newUri": "file:///b"}]
        }))
        .unwrap();
        let err = workspace_edit_to_renames(dir.path(), &edit).unwrap_err();
        assert!(err.to_string().contains("resource operation"), "{err}");
    }

    #[test]
    fn workspace_edit_empty_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        let edit: lsp::WorkspaceEdit = serde_json::from_value(json!({})).unwrap();
        let err = workspace_edit_to_renames(dir.path(), &edit).unwrap_err();
        assert!(err.to_string().contains("no changes"), "{err}");
    }

    #[test]
    fn workspace_edit_outside_worktree_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        let edit: lsp::WorkspaceEdit = serde_json::from_value(json!({
            "changes": {"file:///somewhere/else.rs": []}
        }))
        .unwrap();
        let err = workspace_edit_to_renames(dir.path(), &edit).unwrap_err();
        assert!(err.to_string().contains("outside the worktree"), "{err}");
    }

    // ---- fake backend: error propagation incl. Warming downcast ----

    struct FakeBackend {
        responses: HashMap<String, LiveResult<Value>>,
    }

    impl LiveBackend for FakeBackend {
        fn state(&self) -> InstanceState {
            InstanceState::Ready
        }
        fn request(&self, method: &str, _params: Value) -> LiveResult<Value> {
            match self.responses.get(method) {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(LiveError::Warming { elapsed_secs })) => Err(LiveError::Warming {
                    elapsed_secs: *elapsed_secs,
                }),
                Some(Err(other)) => panic!("fake only models Ok/Warming, got {other:?}"),
                None => panic!("unexpected request {method}"),
            }
        }
        fn shutdown(&mut self) {}
        fn rss_mb(&self) -> Option<u64> {
            None
        }
        fn footprint_mb(&self) -> Option<u64> {
            None
        }
        fn last_used(&self) -> Instant {
            Instant::now()
        }
    }

    #[test]
    fn warming_error_survives_for_daemon_downcast() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write_file(&root, "src/lib.rs", "fn f() {}\n");
        let backend = FakeBackend {
            responses: HashMap::from([(
                lsp::methods::DEFINITION.to_string(),
                Err(LiveError::Warming { elapsed_secs: 42 }),
            )]),
        };
        let err = live_def(&backend, &root, "src/lib.rs", 1, 4).unwrap_err();
        match err.downcast_ref::<LiveError>() {
            Some(LiveError::Warming { elapsed_secs }) => assert_eq!(*elapsed_secs, 42),
            other => panic!("Warming must survive the anyhow chain, got {other:?}"),
        }
    }

    #[test]
    fn null_results_are_empty_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write_file(&root, "src/lib.rs", "fn f() {}\n");
        let backend = FakeBackend {
            responses: HashMap::from([
                (lsp::methods::DEFINITION.to_string(), Ok(Value::Null)),
                (lsp::methods::REFERENCES.to_string(), Ok(Value::Null)),
                (lsp::methods::PREPARE_CALL_HIERARCHY.to_string(), Ok(Value::Null)),
            ]),
        };
        assert!(live_def(&backend, &root, "src/lib.rs", 1, 4).unwrap().is_empty());
        assert!(live_refs(&backend, &root, "src/lib.rs", 1, 4).unwrap().is_empty());
        assert!(live_callers(&backend, &root, "src/lib.rs", 1, 4).unwrap().is_empty());
        // rename → null is NOT silently empty: nothing would be renamed.
        let backend = FakeBackend {
            responses: HashMap::from([(lsp::methods::RENAME.to_string(), Ok(Value::Null))]),
        };
        assert!(live_rename(&backend, &root, "src/lib.rs", 1, 4, "g").is_err());
    }

    // ---- integration: real rust-analyzer on the golden fixture ----
    // (same harness pattern as src/live/instance.rs tests)

    fn run_cmd(cwd: &Path, prog: &str, args: &[&str]) {
        let path = format!(
            "/opt/homebrew/bin:{}",
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(prog)
            .args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .output()
            .unwrap_or_else(|e| panic!("spawning {prog}: {e}"));
        assert!(
            out.status.success(),
            "{prog} {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn copy_dir(src: &Path, dst: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                copy_dir(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    fn fixture_repo() -> TempDir {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture, dir.path());
        run_cmd(dir.path(), "git", &["init", "-q", "-b", "main"]);
        run_cmd(dir.path(), "git", &["add", "-A"]);
        run_cmd(
            dir.path(),
            "git",
            &["-c", "user.email=cq@test", "-c", "user.name=cq-test", "commit", "-q", "-m", "golden"],
        );
        dir
    }

    fn ra_binary() -> String {
        let on_path = Command::new(crate::live::instance::RUST_ANALYZER_BIN)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if on_path {
            crate::live::instance::RUST_ANALYZER_BIN.to_string()
        } else {
            let fallback = "/opt/homebrew/bin/rust-analyzer";
            assert!(
                Path::new(fallback).exists(),
                "rust-analyzer not on PATH and not at {fallback}"
            );
            fallback.to_string()
        }
    }

    fn ready_instance(root: &Path) -> LiveInstance {
        let inst = LiveInstance::spawn_with_binary(&ra_binary(), root).unwrap();
        let t0 = Instant::now();
        loop {
            match inst.state() {
                InstanceState::Ready => return inst,
                InstanceState::Dead => panic!("instance died during prime"),
                InstanceState::Warming => {
                    assert!(
                        t0.elapsed() < Duration::from_secs(120),
                        "rust-analyzer not quiescent within 120s"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    fn positions(results: &[QueryResult]) -> Vec<(String, u32, u32, String)> {
        results
            .iter()
            .map(|r| (r.path.clone(), r.line, r.col, r.role.clone()))
            .collect()
    }

    /// One real-instance test covering def/refs/callers/rename against the
    /// golden fixture truths (one spawn, one prime — the fixture primes in
    /// seconds). Truth source: tests/fixtures/golden-expectations.json.
    #[test]
    fn golden_fixture_translation_def_refs_callers_rename() {
        let repo = fixture_repo();
        let root = repo.path().canonicalize().unwrap();
        let mut inst = ready_instance(&root);

        // -- def of `double` from its lib.rs call site (1-based 4:43) -------
        // src/lib.rs line 4: `pub fn top_level_fn(x: i32) -> i32 { ops::double(x) }`
        let defs = live_def(&inst, &root, "src/lib.rs", 4, 43).unwrap();
        assert_eq!(
            positions(&defs),
            vec![("src/ops.rs".to_string(), 1, 8, "definition".to_string())]
        );
        assert_eq!(defs[0].display_name, "double");
        assert_eq!(
            defs[0].snippet.as_deref(),
            Some("pub fn double(x: i32) -> i32 { x * 2 }")
        );
        assert_eq!(defs[0].symbol, "", "live results carry no SCIP symbol");

        // -- refs of `double` vs the golden truth set -----------------------
        let refs = live_refs(&inst, &root, "src/lib.rs", 4, 43).unwrap();
        let actual = positions(&refs);
        eprintln!("live refs(double): {actual:?}");
        // Golden truth (golden-expectations.json): def ops.rs 1:8,
        // refs lib.rs 4:43 (top_level_fn) + ops.rs 5:60 (fmt_user).
        // OBSERVED (rust-analyzer 0.0.0 c5d30e2331 2026-05-31, recorded
        // 2026-06-11): live references returns EXACTLY the index truth set —
        // it does NOT additionally surface the `double` token inside the
        // macro_rules! body (ops.rs:3) or the expanded call in macro_caller.
        // Pinned exactly so a rust-analyzer semantics change is loud (the
        // same purpose as A2-S3's cross-check).
        let truth: BTreeSet<(String, u32, u32, String)> = [
            ("src/ops.rs".to_string(), 1, 8, "definition".to_string()),
            ("src/lib.rs".to_string(), 4, 43, "reference".to_string()),
            ("src/ops.rs".to_string(), 5, 60, "reference".to_string()),
        ]
        .into();
        let actual_set: BTreeSet<_> = actual.iter().cloned().collect();
        assert_eq!(
            actual_set, truth,
            "live refs diverged from the golden truth set"
        );

        // -- callers of `double` via callHierarchy (def site ops.rs 1:8) ----
        let callers = live_callers(&inst, &root, "src/ops.rs", 1, 8).unwrap();
        let names: BTreeSet<&str> = callers.iter().map(|r| r.display_name.as_str()).collect();
        eprintln!("live callers(double): {names:?}");
        assert!(
            names.contains("top_level_fn") && names.contains("fmt_user"),
            "callHierarchy must see the two plain callers: {names:?}"
        );
        // FINDING (recorded per plan T4): does LIVE callHierarchy surface
        // `macro_caller`, whose call site exists only inside a macro_rules
        // expansion? The index path is structurally blind to it
        // (golden-expectations.json callers gate).
        let sees_macro_caller = names.contains("macro_caller");
        eprintln!(
            "FINDING macro_caller via live callHierarchy: {}",
            if sees_macro_caller {
                "VISIBLE — live rust-analyzer sees through macro_rules expansion"
            } else {
                "NOT visible — live callHierarchy is macro-blind here, same as the index"
            }
        );
        // OBSERVED (rust-analyzer 0.0.0 c5d30e2331 2026-05-31, recorded
        // 2026-06-11): macro_caller is NOT returned — live callHierarchy is
        // macro-blind on macro_rules-body call sites, exactly like the SCIP
        // index path (golden-expectations.json callers gate). Escalating to
        // live does NOT lift the macro limitation. Pinned so a rust-analyzer
        // upgrade changing this is loud (then update docs + callers gate).
        assert!(
            !sees_macro_caller,
            "live callHierarchy NOW surfaces macro_caller — rust-analyzer behavior \
             improved; update this pin, docs/code-intel.md, and the callers gate notes"
        );
        for c in &callers {
            assert_eq!(c.role, "definition", "callers locate at caller defs: {c:?}");
            assert_eq!(c.kind, "function", "{c:?}");
        }

        // -- rename double → twice: def + all call sites, with old_text -----
        let edits = live_rename(&inst, &root, "src/ops.rs", 1, 8, "twice").unwrap();
        eprintln!(
            "live rename(double→twice): {:?}",
            edits
                .iter()
                .map(|e| (e.path.as_str(), e.line, e.col, e.old_text.as_str()))
                .collect::<Vec<_>>()
        );
        for e in &edits {
            assert_eq!(e.old_text, "double", "{e:?}");
            assert_eq!(e.new_text, "twice", "{e:?}");
            assert_eq!((e.end_line, e.end_col - e.col), (e.line, 6), "single-token edit: {e:?}");
        }
        let edit_sites: BTreeSet<(String, u32, u32)> = edits
            .iter()
            .map(|e| (e.path.clone(), e.line, e.col))
            .collect();
        // OBSERVED (rust-analyzer 0.0.0 c5d30e2331 2026-05-31, recorded
        // 2026-06-11): rename covers the definition + both plain call sites
        // and NOTHING else — the `double` token inside the macro_rules! body
        // (ops.rs:3, `crate::ops::double($x)`) is NOT renamed. A real
        // double→twice rename of this fixture therefore leaves the macro
        // body dangling and the crate uncompilable until hand-fixed — the
        // same macro blindness as refs/callers, now on the write path.
        let required: BTreeSet<(String, u32, u32)> = [
            ("src/ops.rs".to_string(), 1, 8),  // definition
            ("src/lib.rs".to_string(), 4, 43), // top_level_fn call site
            ("src/ops.rs".to_string(), 5, 60), // fmt_user call site
        ]
        .into();
        assert_eq!(
            edit_sites, required,
            "rename edit-site set diverged from the observed truth"
        );
        eprintln!(
            "FINDING rename(double→twice): macro_rules-body call site NOT renamed \
             (3 edits: def + 2 plain call sites)"
        );

        inst.shutdown();
    }
}
