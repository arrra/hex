//! Response envelope every query verb emits on stdout, per SPEC-A1 §5.
//!
//! Lines/cols in the envelope are 1-based (the CLI convention); internal
//! SCIP/SQLite storage is 0-based and converted exactly once at assembly.

use serde::{Deserialize, Serialize};

/// One result row inside the envelope. Positions are 1-based here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryResult {
    /// Path relative to the workspace root.
    pub path: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub col: u32,
    /// Full SCIP symbol string.
    pub symbol: String,
    pub display_name: String,
    /// Human kind, e.g. "function", "struct", "trait".
    pub kind: String,
    /// "definition" or "reference".
    pub role: String,
    /// The source line, read from the worktree when the file is fresh.
    /// Omitted (and the file listed in `stale_files`) when stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// The single JSON object every query verb writes to stdout (SPEC-A1 §5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// Always "index" in A1 (daemon/live sources arrive in A2).
    pub source: String,
    pub workspace_id: String,
    pub indexed_commit: String,
    pub index_age_secs: u64,
    /// Result files whose worktree blob OID no longer matches the index.
    pub stale_files: Vec<String>,
    pub latency_ms: u64,
    /// Set to "best-effort" by `cq callers` when the callers gate is closed
    /// (spec §8). Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    pub results: Vec<QueryResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serializes_per_spec() {
        let env = Envelope {
            source: "index".into(),
            workspace_id: "ab12cd34ef56".into(),
            indexed_commit: "deadbeef".into(),
            index_age_secs: 10,
            stale_files: vec!["src/a.rs".into()],
            latency_ms: 3,
            quality: None,
            results: vec![QueryResult {
                path: "src/a.rs".into(),
                line: 12,
                col: 4,
                symbol: "scip …".into(),
                display_name: "foo".into(),
                kind: "function".into(),
                role: "definition".into(),
                snippet: Some("fn foo() {}".into()),
            }],
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(j["source"], "index");
        assert_eq!(j["workspace_id"], "ab12cd34ef56");
        assert_eq!(j["indexed_commit"], "deadbeef");
        assert_eq!(j["index_age_secs"], 10);
        assert_eq!(j["stale_files"][0], "src/a.rs");
        assert_eq!(j["latency_ms"], 3);
        assert_eq!(j["results"][0]["line"], 12);
        assert_eq!(j["results"][0]["col"], 4);
        assert_eq!(j["results"][0]["role"], "definition");
        assert_eq!(j["results"][0]["snippet"], "fn foo() {}");
        // `quality` only appears when set (callers best-effort gate, spec §8)
        assert!(j.get("quality").is_none() || j["quality"].is_null());
    }

    #[test]
    fn snippet_omitted_when_stale() {
        let env = Envelope {
            source: "index".into(),
            workspace_id: "ab12cd34ef56".into(),
            indexed_commit: "deadbeef".into(),
            index_age_secs: 0,
            stale_files: vec!["src/a.rs".into()],
            latency_ms: 1,
            quality: None,
            results: vec![QueryResult {
                path: "src/a.rs".into(),
                line: 1,
                col: 1,
                symbol: "s".into(),
                display_name: "d".into(),
                kind: "function".into(),
                role: "reference".into(),
                snippet: None,
            }],
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert!(j["results"][0].get("snippet").is_none());
    }

    #[test]
    fn quality_field_emitted_when_set() {
        let env = Envelope {
            source: "index".into(),
            workspace_id: "ab12cd34ef56".into(),
            indexed_commit: "deadbeef".into(),
            index_age_secs: 0,
            stale_files: vec![],
            latency_ms: 1,
            quality: Some("best-effort".into()),
            results: vec![],
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(j["quality"], "best-effort");
    }
}
