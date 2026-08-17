//! UDS wire protocol between `cq` and `scipd`, per SPEC-A2 §3.
//!
//! One JSON object per line, request/response. Every request carries an `id`
//! the reply echoes. Unknown or malformed requests NEVER panic the daemon —
//! [`parse_request`] returns a structured error the dispatcher turns into an
//! error reply (Standing Order S6: loud, structured, never silent).

use serde::{Deserialize, Serialize};

use crate::envelope::QueryResult;

/// Query verbs the live path supports (SPEC-A2 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryVerb {
    Def,
    Refs,
    Callers,
}

/// A request line from `cq` to `scipd` (SPEC-A2 §3).
///
/// Unknown EXTRA fields are tolerated (forward compatibility); an unknown
/// `op` tag is a parse error (`deny_unknown_fields` cannot combine with
/// `flatten`, and tolerance is the better trade anyway).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

/// The operation payload, tagged by `op` (SPEC-A2 §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    Ping,
    Status,
    Query {
        verb: QueryVerb,
        /// Absolute worktree root.
        worktree: String,
        /// Path relative to the worktree root.
        path: String,
        /// 1-based line (CLI convention; LSP translation happens daemon-side).
        line: u32,
        /// 1-based column.
        col: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Rename {
        worktree: String,
        path: String,
        line: u32,
        col: u32,
        new_name: String,
    },
    /// Ops hatch: drop the live instance for a worktree.
    Evict {
        worktree: String,
    },
}

/// Warming payload: the daemon never queues behind a prime (SPEC-A2 §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Warming {
    pub elapsed_secs: u64,
    /// Worktree the warming instance is rooted at (SPEC-A2 §2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Structured error payload inside an error reply — same code/message/hint
/// triple as the CLI taxonomy (SPEC-A1 §5, SPEC-A2 §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyError {
    pub code: String,
    pub message: String,
    pub hint: String,
}

/// One normalized rename edit (SPEC-A2 §5). `old_text` is the expected
/// current content of the edited span — apply-time content assertions
/// depend on it (mismatch ⇒ RENAME_ABORTED, nothing written).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameEdit {
    /// Path relative to the worktree root.
    pub path: String,
    /// 1-based start line.
    pub line: u32,
    /// 1-based start column.
    pub col: u32,
    /// 1-based end line (exclusive-col convention matches LSP ranges).
    pub end_line: u32,
    /// 1-based end column.
    pub end_col: u32,
    pub new_text: String,
    /// Expected current text of the span, for apply-time assertions.
    pub old_text: String,
}

/// Per-instance state as reported by `status` (SPEC-A2 §2/§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    Warming,
    Ready,
    Dead,
}

/// One live instance row inside a `status` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub worktree: String,
    pub state: InstanceState,
    pub rss_mb: u64,
    pub age_secs: u64,
    pub idle_secs: u64,
}

/// Pool occupancy snapshot returned by `status` (SPEC-A2 §3/§4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolStatus {
    pub pool_cap: usize,
    pub instances: Vec<InstanceStatus>,
    /// Loud notes the pool wants surfaced (mem-watchdog kills, evictions).
    #[serde(default)]
    pub notes: Vec<String>,
}

/// A reply line from `scipd` to `cq`. One struct with optional sections —
/// constructors below guarantee each wire shape from SPEC-A2 §3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<QueryResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<RenameEdit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warming: Option<Warming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReplyError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PoolStatus>,
}

impl Reply {
    /// `{"id":N,"ok":true}` — ping reply.
    pub fn pong(id: u64) -> Self {
        Reply {
            id,
            ok: true,
            source: None,
            results: None,
            edits: None,
            warming: None,
            error: None,
            status: None,
        }
    }

    /// `{"id":N,"ok":true,"status":{...}}` — status reply.
    pub fn status(id: u64, status: PoolStatus) -> Self {
        Reply {
            status: Some(status),
            ..Reply::pong(id)
        }
    }

    /// `{"id":N,"ok":true,"source":"live","results":[...]}` — query success.
    pub fn results(id: u64, results: Vec<QueryResult>) -> Self {
        Reply {
            source: Some("live".into()),
            results: Some(results),
            ..Reply::pong(id)
        }
    }

    /// `{"id":N,"ok":true,"edits":[...]}` — rename success.
    pub fn edits(id: u64, edits: Vec<RenameEdit>) -> Self {
        Reply {
            edits: Some(edits),
            ..Reply::pong(id)
        }
    }

    /// `{"id":N,"ok":false,"warming":{...}}` — instance still priming.
    pub fn warming(id: u64, warming: Warming) -> Self {
        Reply {
            ok: false,
            warming: Some(warming),
            ..Reply::pong(id)
        }
    }

    /// `{"id":N,"ok":false,"error":{...}}` — structured failure.
    pub fn error(id: u64, code: &str, message: String, hint: &str) -> Self {
        Reply {
            ok: false,
            error: Some(ReplyError {
                code: code.into(),
                message,
                hint: hint.into(),
            }),
            ..Reply::pong(id)
        }
    }
}

/// Parse one request line. On failure returns the best-effort request `id`
/// (so the daemon can address its error reply) plus a human-readable reason.
/// Never panics on malformed input.
pub fn parse_request(line: &str) -> Result<Request, (u64, String)> {
    match serde_json::from_str::<Request>(line) {
        Ok(req) => Ok(req),
        Err(e) => {
            // Best-effort id extraction so the error reply is addressable.
            let id = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
                .unwrap_or(0);
            Err((id, e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(req: &Request) -> Request {
        let line = serde_json::to_string(req).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn ping_request_matches_spec_wire_shape() {
        let req = parse_request(r#"{"id":1,"op":"ping"}"#).unwrap();
        assert_eq!(
            req,
            Request {
                id: 1,
                op: Op::Ping
            }
        );
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn status_request_roundtrips() {
        let req = parse_request(r#"{"id":2,"op":"status"}"#).unwrap();
        assert_eq!(req.op, Op::Status);
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn query_request_matches_spec_wire_shape() {
        let line = r#"{"id":3,"op":"query","verb":"def","worktree":"/abs","path":"src/a.rs","line":1,"col":1,"name":"foo"}"#;
        let req = parse_request(line).unwrap();
        match &req.op {
            Op::Query {
                verb,
                worktree,
                path,
                line,
                col,
                name,
            } => {
                assert_eq!(*verb, QueryVerb::Def);
                assert_eq!(worktree, "/abs");
                assert_eq!(path, "src/a.rs");
                assert_eq!((*line, *col), (1, 1));
                assert_eq!(name.as_deref(), Some("foo"));
            }
            other => panic!("wrong op: {other:?}"),
        }
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn query_name_is_optional_and_omitted_when_none() {
        let req = parse_request(
            r#"{"id":3,"op":"query","verb":"refs","worktree":"/abs","path":"a.rs","line":2,"col":5}"#,
        )
        .unwrap();
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains("name"), "None name must be omitted: {line}");
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn all_three_verbs_parse() {
        for verb in ["def", "refs", "callers"] {
            let line = format!(
                r#"{{"id":3,"op":"query","verb":"{verb}","worktree":"/w","path":"p.rs","line":1,"col":1}}"#
            );
            parse_request(&line).unwrap_or_else(|e| panic!("verb {verb}: {e:?}"));
        }
    }

    #[test]
    fn rename_request_matches_spec_wire_shape() {
        let line = r#"{"id":4,"op":"rename","worktree":"/abs","path":"a.rs","line":1,"col":1,"new_name":"x"}"#;
        let req = parse_request(line).unwrap();
        assert!(matches!(&req.op, Op::Rename { new_name, .. } if new_name == "x"));
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn evict_request_matches_spec_wire_shape() {
        let req = parse_request(r#"{"id":5,"op":"evict","worktree":"/abs"}"#).unwrap();
        assert!(matches!(&req.op, Op::Evict { worktree } if worktree == "/abs"));
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn unknown_op_is_error_not_panic() {
        let err = parse_request(r#"{"id":9,"op":"explode"}"#).unwrap_err();
        assert_eq!(err.0, 9, "id must be recovered for the error reply");
        assert!(err.1.contains("explode") || !err.1.is_empty());
    }

    #[test]
    fn garbage_line_is_error_with_id_zero() {
        let err = parse_request("not json at all").unwrap_err();
        assert_eq!(err.0, 0);
    }

    #[test]
    fn missing_fields_is_error_not_panic() {
        // query without worktree
        let err = parse_request(r#"{"id":7,"op":"query","verb":"def"}"#).unwrap_err();
        assert_eq!(err.0, 7);
    }

    #[test]
    fn ok_results_reply_matches_spec_wire_shape() {
        let reply = Reply::results(3, vec![]);
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(j["id"], 3);
        assert_eq!(j["ok"], true);
        assert_eq!(j["source"], "live");
        assert!(j["results"].is_array());
        assert!(j.get("warming").is_none());
        assert!(j.get("error").is_none());
    }

    #[test]
    fn warming_reply_matches_spec_wire_shape() {
        let reply = Reply::warming(
            3,
            Warming {
                elapsed_secs: 42,
                workspace: Some("/w".into()),
            },
        );
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(j["id"], 3);
        assert_eq!(j["ok"], false);
        assert_eq!(j["warming"]["elapsed_secs"], 42);
        assert_eq!(j["warming"]["workspace"], "/w");
        assert!(j.get("results").is_none());
    }

    #[test]
    fn error_reply_matches_spec_wire_shape() {
        let reply = Reply::error(3, "LIVE_UNAVAILABLE", "daemon thing".into(), "do X");
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(j["ok"], false);
        assert_eq!(j["error"]["code"], "LIVE_UNAVAILABLE");
        assert_eq!(j["error"]["message"], "daemon thing");
        assert_eq!(j["error"]["hint"], "do X");
    }

    #[test]
    fn pong_and_status_replies_roundtrip() {
        let pong = Reply::pong(1);
        let parsed: Reply = serde_json::from_str(&serde_json::to_string(&pong).unwrap()).unwrap();
        assert_eq!(parsed, pong);

        let status = Reply::status(
            2,
            PoolStatus {
                pool_cap: 2,
                instances: vec![InstanceStatus {
                    worktree: "/w".into(),
                    state: InstanceState::Ready,
                    rss_mb: 512,
                    age_secs: 60,
                    idle_secs: 5,
                }],
                notes: vec!["evicted /old (LRU)".into()],
            },
        );
        let parsed: Reply = serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
        assert_eq!(parsed, status);
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
        assert_eq!(j["status"]["pool_cap"], 2);
        assert_eq!(j["status"]["instances"][0]["state"], "ready");
    }

    #[test]
    fn rename_edits_reply_roundtrips() {
        let reply = Reply::edits(
            4,
            vec![RenameEdit {
                path: "src/a.rs".into(),
                line: 3,
                col: 8,
                end_line: 3,
                end_col: 14,
                new_text: "twice".into(),
                old_text: "double".into(),
            }],
        );
        let parsed: Reply = serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(parsed, reply);
    }
}
