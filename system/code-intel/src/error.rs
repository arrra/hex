//! Error taxonomy → exit codes, per SPEC-A1 §5.
//!
//! Every error is a structured JSON object on stderr with `code`, `message`,
//! `hint`. Never exit 0 with empty results due to an internal failure
//! (Standing Order S6).

use serde_json::json;

/// The full error taxonomy from SPEC-A1 §5. Each variant maps to a stable
/// `error.code` string and a CLI exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CqError {
    /// ≥1 result file stale and `--strict` refused (exit 2).
    StaleResults,
    /// No index / `CURRENT` missing / SQLite unopenable (exit 3).
    NoIndex { workspace_id: String },
    /// CWD not in a registered workspace (exit 4).
    UnregisteredWorkspace { cwd: String },
    /// Workspace registered but not a Rust workspace (exit 4).
    UnsupportedWorkspace { reason: String },
    /// Symbol/position resolves to nothing (exit 5).
    NotFound { query: String },
    /// Emit subprocess failed during `cq index` (exit 6).
    EmitFailed { stderr_tail: String },
}

impl CqError {
    /// Stable machine-readable code, per the spec §5 table.
    pub fn code_str(&self) -> &'static str {
        match self {
            CqError::StaleResults => "STALE_RESULTS",
            CqError::NoIndex { .. } => "NO_INDEX",
            CqError::UnregisteredWorkspace { .. } => "UNREGISTERED_WORKSPACE",
            CqError::UnsupportedWorkspace { .. } => "UNSUPPORTED_WORKSPACE",
            CqError::NotFound { .. } => "NOT_FOUND",
            CqError::EmitFailed { .. } => "EMIT_FAILED",
        }
    }

    /// CLI exit code, per the spec §5 table.
    pub fn exit_code(&self) -> i32 {
        match self {
            CqError::StaleResults => 2,
            CqError::NoIndex { .. } => 3,
            CqError::UnregisteredWorkspace { .. } => 4,
            CqError::UnsupportedWorkspace { .. } => 4,
            CqError::NotFound { .. } => 5,
            CqError::EmitFailed { .. } => 6,
        }
    }

    fn message(&self) -> String {
        match self {
            CqError::StaleResults => {
                "results touch files that changed since indexing; refused under --strict".into()
            }
            CqError::NoIndex { workspace_id } => {
                format!("no published index generation for workspace {workspace_id}")
            }
            CqError::UnregisteredWorkspace { cwd } => {
                format!("{cwd} is not inside a registered workspace")
            }
            CqError::UnsupportedWorkspace { reason } => {
                format!("workspace is registered but not a supported Rust workspace: {reason}")
            }
            CqError::NotFound { query } => {
                format!("no symbol or position matched: {query}")
            }
            CqError::EmitFailed { stderr_tail } => {
                format!("rust-analyzer scip emit failed; stderr tail: {stderr_tail}")
            }
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            CqError::StaleResults => {
                "run `cq index` to refresh the index, or drop --strict to get results with stale_files annotated"
            }
            CqError::NoIndex { .. } => "run `cq index` to build the first generation",
            CqError::UnregisteredWorkspace { .. } => {
                "run `cq register <PATH>` from the workspace root first"
            }
            CqError::UnsupportedWorkspace { .. } => {
                "A1 supports Rust cargo workspaces only; ensure Cargo.toml exists at the primary checkout root"
            }
            CqError::NotFound { .. } => {
                "check spelling, or run `cq index` if the symbol was added after the last index"
            }
            CqError::EmitFailed { .. } => {
                "run `cq doctor` to verify rust-analyzer is on PATH and the workspace compiles"
            }
        }
    }

    /// Structured stderr payload: `{"error":{"code","message","hint"}}`.
    pub fn to_json(&self) -> String {
        json!({
            "error": {
                "code": self.code_str(),
                "message": self.message(),
                "hint": self.hint(),
            }
        })
        .to_string()
    }
}

impl std::fmt::Display for CqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code_str(), self.message())
    }
}

impl std::error::Error for CqError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(CqError::StaleResults.exit_code(), 2);
        assert_eq!(CqError::NoIndex { workspace_id: "x".into() }.exit_code(), 3);
        assert_eq!(CqError::UnregisteredWorkspace { cwd: "/tmp".into() }.exit_code(), 4);
        assert_eq!(
            CqError::UnsupportedWorkspace { reason: "no Cargo.toml".into() }.exit_code(),
            4
        );
        assert_eq!(CqError::NotFound { query: "nope".into() }.exit_code(), 5);
        assert_eq!(CqError::EmitFailed { stderr_tail: "boom".into() }.exit_code(), 6);
    }

    #[test]
    fn error_serializes_with_code_message_hint() {
        let e = CqError::NoIndex { workspace_id: "ab12".into() };
        let j: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(j["error"]["code"], "NO_INDEX");
        assert!(j["error"]["message"].as_str().unwrap().contains("ab12"));
        assert!(j["error"]["hint"].as_str().unwrap().contains("cq index"));
    }

    #[test]
    fn code_strings_match_spec_table() {
        assert_eq!(CqError::StaleResults.code_str(), "STALE_RESULTS");
        assert_eq!(CqError::NoIndex { workspace_id: "x".into() }.code_str(), "NO_INDEX");
        assert_eq!(
            CqError::UnregisteredWorkspace { cwd: "/x".into() }.code_str(),
            "UNREGISTERED_WORKSPACE"
        );
        assert_eq!(
            CqError::UnsupportedWorkspace { reason: "r".into() }.code_str(),
            "UNSUPPORTED_WORKSPACE"
        );
        assert_eq!(CqError::NotFound { query: "q".into() }.code_str(), "NOT_FOUND");
        assert_eq!(CqError::EmitFailed { stderr_tail: "t".into() }.code_str(), "EMIT_FAILED");
    }

    #[test]
    fn every_variant_has_nonempty_hint() {
        let all = [
            CqError::StaleResults,
            CqError::NoIndex { workspace_id: "x".into() },
            CqError::UnregisteredWorkspace { cwd: "/x".into() },
            CqError::UnsupportedWorkspace { reason: "r".into() },
            CqError::NotFound { query: "q".into() },
            CqError::EmitFailed { stderr_tail: "t".into() },
        ];
        for e in all {
            let j: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
            assert!(
                !j["error"]["hint"].as_str().unwrap().is_empty(),
                "empty hint for {}",
                e.code_str()
            );
            assert!(!j["error"]["message"].as_str().unwrap().is_empty());
        }
    }
}
