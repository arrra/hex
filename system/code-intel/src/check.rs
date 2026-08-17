//! `cq check [FILE]` — cargo diagnostics for the querying worktree
//! (SPEC-A2 §5).
//!
//! Runs `cargo check --message-format=json --quiet` at the worktree root
//! with `CARGO_TARGET_DIR=<worktree>/target-cq` — per-worktree, never
//! shared, so concurrent checks in different worktrees cannot contend on a
//! target-dir lock (the judges' finding). Output is one JSON object:
//! `{diagnostics:[{path,line,col,level,code,message}], checked_in_ms}`.
//!
//! Exit codes (SPEC-A2 §5): 0 clean / 1 diagnostics present / 8
//! (`CHECK_FAILED`) when cargo itself failed to run. A `FILE` argument
//! filters the *reported* diagnostics to that file; the exit code still
//! reflects the whole worktree's check (a "clean" file in a crate that does
//! not compile is not a clean check — saying otherwise would be a silent
//! failure).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;

use crate::error::CqError;

/// One normalized diagnostic (SPEC-A2 §5). Positions are 1-based, paths
/// relative to the worktree root (cargo's `--message-format=json` emits
/// them relative to the invocation directory).
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Diagnostic {
    pub path: String,
    pub line: u32,
    pub col: u32,
    /// `"error"` | `"warning"` (cargo's rendered levels).
    pub level: String,
    /// Rustc lint/error code (e.g. `E0308`), when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

/// The `cq check` stdout JSON.
#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub diagnostics: Vec<Diagnostic>,
    pub checked_in_ms: u64,
}

/// Run the check. Returns the report plus the exit code (0/1); a cargo
/// that fails to run (or fails without producing any diagnostic) is
/// [`CqError::CheckFailed`] (exit 8).
pub fn run(worktree_root: &Path, file: Option<&str>) -> Result<(CheckReport, i32)> {
    let started = Instant::now();
    let out = Command::new("cargo")
        .args(["check", "--message-format=json", "--quiet"])
        .current_dir(worktree_root)
        .env("CARGO_TARGET_DIR", worktree_root.join("target-cq"))
        .output()
        .map_err(|e| CqError::CheckFailed {
            detail: format!("spawning cargo check: {e}"),
        })?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let all = parse_diagnostics(&stdout)?;

    // cargo exited nonzero without emitting a single diagnostic: the check
    // itself broke (not a cargo workspace, broken manifest, …) — loud.
    if !out.status.success() && all.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(CqError::CheckFailed {
            detail: format!(
                "cargo check exited {} with no diagnostics; stderr tail: {}",
                out.status
                    .code()
                    .map_or("by signal".to_string(), |c| c.to_string()),
                tail.join(" | ")
            ),
        }
        .into());
    }

    let any_diagnostics = !all.is_empty();
    let diagnostics: Vec<Diagnostic> = match file {
        Some(file) => all.into_iter().filter(|d| d.path == file).collect(),
        None => all.into_iter().collect(),
    };
    let exit_code = i32::from(any_diagnostics);
    let report = CheckReport {
        diagnostics,
        checked_in_ms: started.elapsed().as_millis() as u64,
    };
    Ok((report, exit_code))
}

/// Parse cargo's newline-JSON message stream into normalized diagnostics,
/// deduplicated (cargo can re-emit the same diagnostic across compilation
/// targets) and sorted (path, line, col, level, message).
fn parse_diagnostics(stdout: &str) -> Result<Vec<Diagnostic>> {
    let mut set: BTreeSet<Diagnostic> = BTreeSet::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| CqError::CheckFailed {
                detail: format!("unparseable cargo JSON message: {e}: {line:?}"),
            })?;
        if value["reason"] != "compiler-message" {
            continue;
        }
        let message = &value["message"];
        let level = match message["level"].as_str() {
            Some(level @ ("error" | "warning")) => level.to_string(),
            // notes/helps are attached to their parent diagnostic; cargo
            // also emits non-span summary errors ("aborting due to …").
            _ => continue,
        };
        // The primary span carries the path:line:col the spec requires;
        // summary messages without spans are skipped (their root causes
        // each have their own spanned diagnostic).
        let Some(span) = message["spans"]
            .as_array()
            .and_then(|spans| spans.iter().find(|s| s["is_primary"] == true))
        else {
            continue;
        };
        set.insert(Diagnostic {
            path: span["file_name"]
                .as_str()
                .unwrap_or("<unknown>")
                .to_string(),
            line: span["line_start"].as_u64().unwrap_or(0) as u32,
            col: span["column_start"].as_u64().unwrap_or(0) as u32,
            level,
            code: message["code"]["code"].as_str().map(str::to_string),
            message: message["message"].as_str().unwrap_or("").to_string(),
        });
    }
    Ok(set.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(level: &str, file: &str, line: u32, code: Option<&str>, text: &str) -> String {
        serde_json::json!({
            "reason": "compiler-message",
            "message": {
                "level": level,
                "message": text,
                "code": code.map(|c| serde_json::json!({"code": c})),
                "spans": [{
                    "is_primary": true,
                    "file_name": file,
                    "line_start": line,
                    "column_start": 9
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn parses_normalizes_and_dedupes() {
        let stdout = [
            r#"{"reason":"compiler-artifact","target":{"name":"x"}}"#.to_string(),
            msg("error", "src/lib.rs", 4, Some("E0308"), "mismatched types"),
            // Same diagnostic re-emitted for a second compilation target.
            msg("error", "src/lib.rs", 4, Some("E0308"), "mismatched types"),
            msg(
                "warning",
                "src/ops.rs",
                2,
                Some("unused_variables"),
                "unused variable: `x`",
            ),
            r#"{"reason":"build-finished","success":false}"#.to_string(),
        ]
        .join("\n");
        let diags = parse_diagnostics(&stdout).unwrap();
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert_eq!(diags[0].path, "src/lib.rs");
        assert_eq!((diags[0].line, diags[0].col), (4, 9));
        assert_eq!(diags[0].level, "error");
        assert_eq!(diags[0].code.as_deref(), Some("E0308"));
        assert_eq!(diags[0].message, "mismatched types");
        assert_eq!(diags[1].level, "warning");
    }

    #[test]
    fn spanless_summary_messages_are_skipped() {
        let stdout = serde_json::json!({
            "reason": "compiler-message",
            "message": {
                "level": "error",
                "message": "aborting due to 1 previous error",
                "code": null,
                "spans": []
            }
        })
        .to_string();
        assert_eq!(parse_diagnostics(&stdout).unwrap(), vec![]);
    }

    #[test]
    fn garbage_line_is_loud_check_failed() {
        let err = parse_diagnostics("not json at all").unwrap_err();
        let cq = err.downcast_ref::<CqError>().expect("CqError");
        assert!(matches!(cq, CqError::CheckFailed { .. }), "{cq}");
        assert_eq!(cq.exit_code(), 8);
    }

    #[test]
    fn nonexistent_dir_is_check_failed() {
        // cargo runs but finds no manifest → nonzero exit, no diagnostics.
        let dir = tempfile::tempdir().unwrap();
        let err = run(dir.path(), None).unwrap_err();
        let cq = err.downcast_ref::<CqError>().expect("CqError");
        assert!(matches!(cq, CqError::CheckFailed { .. }), "{cq}");
    }

    #[test]
    fn report_serializes_per_spec() {
        let report = CheckReport {
            diagnostics: vec![Diagnostic {
                path: "src/lib.rs".into(),
                line: 4,
                col: 9,
                level: "error".into(),
                code: Some("E0308".into()),
                message: "mismatched types".into(),
            }],
            checked_in_ms: 1234,
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(j["diagnostics"][0]["path"], "src/lib.rs");
        assert_eq!(j["diagnostics"][0]["line"], 4);
        assert_eq!(j["diagnostics"][0]["col"], 9);
        assert_eq!(j["diagnostics"][0]["level"], "error");
        assert_eq!(j["diagnostics"][0]["code"], "E0308");
        assert_eq!(j["checked_in_ms"], 1234);
    }
}
