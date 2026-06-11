//! Response envelope every query verb emits on stdout, per SPEC-A1 §5 and
//! SPEC-A2 §5 (`source:"index"|"live"`, optional `escalated`).
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

/// Structured escalation notice (SPEC-A2 §5): the live path was warranted
/// (stale target/results) but could not answer, so the index answer is
/// served WITH this notice — never silence, never a hang.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Escalated {
    /// `"warming"` | `"daemon-unavailable"` (SPEC-A2 §5), or `"live-error"`
    /// for a reachable daemon that answered with a structured error.
    pub reason: String,
    /// Seconds the live instance has been priming (`warming` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<u64>,
    /// Worktree of the warming instance (`warming` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Human-readable failure detail (`daemon-unavailable`/`live-error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The single JSON object every query verb writes to stdout (SPEC-A1 §5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// `"index"` (A1 fast path) or `"live"` (A2 escalation, SPEC-A2 §5).
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
    /// Present when a warranted live escalation could not answer
    /// (SPEC-A2 §5). Omitted on pure-index and successful-live envelopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated: Option<Escalated>,
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
            escalated: None,
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
            escalated: None,
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
            escalated: None,
            results: vec![],
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(j["quality"], "best-effort");
    }

    #[test]
    fn escalated_omitted_when_none_and_shaped_when_set() {
        let mut env = Envelope {
            source: "index".into(),
            workspace_id: "ab12cd34ef56".into(),
            indexed_commit: "deadbeef".into(),
            index_age_secs: 0,
            stale_files: vec!["src/a.rs".into()],
            latency_ms: 1,
            quality: None,
            escalated: None,
            results: vec![],
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert!(j.get("escalated").is_none(), "None escalated must be omitted: {j}");

        env.escalated = Some(Escalated {
            reason: "warming".into(),
            elapsed_secs: Some(42),
            workspace: Some("/w".into()),
            detail: None,
        });
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(j["escalated"]["reason"], "warming");
        assert_eq!(j["escalated"]["elapsed_secs"], 42);
        assert_eq!(j["escalated"]["workspace"], "/w");
        assert!(j["escalated"].get("detail").is_none());
        // Round-trips (cq tests deserialize envelopes).
        let back: Envelope = serde_json::from_value(j).unwrap();
        assert_eq!(back, env);

        env.escalated = Some(Escalated {
            reason: "daemon-unavailable".into(),
            elapsed_secs: None,
            workspace: None,
            detail: Some("connect refused".into()),
        });
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(j["escalated"]["reason"], "daemon-unavailable");
        assert_eq!(j["escalated"]["detail"], "connect refused");
        assert!(j["escalated"].get("elapsed_secs").is_none());
    }
}
