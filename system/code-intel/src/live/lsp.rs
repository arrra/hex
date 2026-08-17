//! Minimal hand-rolled LSP stdio plumbing (SPEC-A2 §7).
//!
//! Content-Length framing plus serde structs for EXACTLY the messages the
//! live path uses: initialize/initialized/shutdown/exit,
//! textDocument/{definition,references,prepareRename,rename},
//! textDocument/prepareCallHierarchy + callHierarchy/incomingCalls, and the
//! `experimental/serverStatus` notification.
//!
//! Deliberately NOT `lsp-types`/`tower-lsp`: dependency weight and an async
//! runtime for ~8 message shapes is a bad trade. Blocking IO only.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

/// LSP method names the live path speaks. These are the LSP-standard names;
/// the plan's shorthand "callHierarchy/prepare" / "callHierarchyItem/
/// incomingCalls" map to `textDocument/prepareCallHierarchy` /
/// `callHierarchy/incomingCalls` on the wire.
pub mod methods {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "initialized";
    pub const SHUTDOWN: &str = "shutdown";
    pub const EXIT: &str = "exit";
    pub const DEFINITION: &str = "textDocument/definition";
    pub const REFERENCES: &str = "textDocument/references";
    pub const PREPARE_RENAME: &str = "textDocument/prepareRename";
    pub const RENAME: &str = "textDocument/rename";
    pub const PREPARE_CALL_HIERARCHY: &str = "textDocument/prepareCallHierarchy";
    pub const INCOMING_CALLS: &str = "callHierarchy/incomingCalls";
    pub const SERVER_STATUS: &str = "experimental/serverStatus";
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Write one LSP frame: `Content-Length: N\r\n\r\n<body>`.
pub fn write_message(writer: &mut impl Write, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::from)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

/// Read one LSP frame. `Ok(None)` on clean EOF (stream closed between
/// frames); errors loudly on EOF mid-frame, missing Content-Length, or
/// invalid JSON. Handles split reads via `BufRead` (`read_until` /
/// `read_exact` both loop over short reads).
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    let mut saw_header = false;
    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            return if saw_header {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF mid-headers in LSP frame",
                ))
            } else {
                Ok(None)
            };
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            break; // end of headers
        }
        saw_header = true;
        if let Some((key, value)) = text.split_once(':') {
            if key.trim().eq_ignore_ascii_case("content-length") {
                let len = value.trim().parse::<usize>().map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("bad Content-Length {value:?}: {e}"),
                    )
                })?;
                content_length = Some(len);
            }
            // Other headers (Content-Type) are ignored per the LSP spec.
        }
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP frame missing Content-Length header",
        )
    })?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid JSON in LSP frame: {e}"),
        )
    })?;
    Ok(Some(value))
}

// ---------------------------------------------------------------------------
// JSON-RPC envelopes
// ---------------------------------------------------------------------------

/// Build a request envelope. Null params are omitted (e.g. `shutdown`).
pub fn request(id: i64, method: &str, params: Value) -> Value {
    if params.is_null() {
        json!({"jsonrpc": "2.0", "id": id, "method": method})
    } else {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }
}

/// Build a notification envelope. Null params are omitted (e.g. `exit`).
pub fn notification(method: &str, params: Value) -> Value {
    if params.is_null() {
        json!({"jsonrpc": "2.0", "method": method})
    } else {
        json!({"jsonrpc": "2.0", "method": method, "params": params})
    }
}

/// Build a response envelope (for answering server→client requests).
pub fn response(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Any message arriving from the server: response (id, result/error),
/// server→client request (id + method), or notification (method only).
#[derive(Debug, Deserialize)]
pub struct Incoming {
    pub id: Option<Value>,
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
    pub result: Option<Value>,
    pub error: Option<ResponseError>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Value,
}

// ---------------------------------------------------------------------------
// Message types (only what A2 uses)
// ---------------------------------------------------------------------------

/// LSP position: 0-based line, 0-based UTF-16 character. cq's 1-based
/// conversion lives in ONE place (`live/translate.rs`, Task 4) — everything
/// in this module is raw LSP coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// Servers may answer `textDocument/definition` with LocationLinks when the
/// client declares `linkSupport` — we don't, but parse them leniently anyway
/// (defense against server-side default changes).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationLink {
    pub target_uri: String,
    pub target_range: Range,
    pub target_selection_range: Range,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentIdentifier {
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentPositionParams {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
}

/// `textDocument/references` params.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceParams {
    #[serde(flatten)]
    pub position: TextDocumentPositionParams,
    pub context: ReferenceContext,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceContext {
    pub include_declaration: bool,
}

/// `textDocument/rename` params. (`textDocument/prepareRename` takes plain
/// `TextDocumentPositionParams`.)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameParams {
    #[serde(flatten)]
    pub position: TextDocumentPositionParams,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// `textDocument/rename` result. Without client `documentChanges` capability
/// the server uses the `changes` map; `document_changes` is kept as raw JSON
/// so an unexpected shape is visible, not silently dropped.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEdit {
    #[serde(default)]
    pub changes: Option<HashMap<String, Vec<TextEdit>>>,
    #[serde(default)]
    pub document_changes: Option<Value>,
}

/// Item from `textDocument/prepareCallHierarchy`. `extra` round-trips every
/// field we don't model (notably rust-analyzer's `data` token, which MUST be
/// echoed back in `callHierarchy/incomingCalls`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyItem {
    pub name: String,
    pub kind: i64,
    pub uri: String,
    pub range: Range,
    pub selection_range: Range,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallHierarchyIncomingCallsParams {
    pub item: CallHierarchyItem,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyIncomingCall {
    pub from: CallHierarchyItem,
    pub from_ranges: Vec<Range>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub process_id: Option<u32>,
    pub root_uri: String,
    pub capabilities: ClientCapabilities,
    pub initialization_options: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientCapabilities {
    pub experimental: ExperimentalCapabilities,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalCapabilities {
    pub server_status_notification: bool,
}

impl InitializeParams {
    /// The exact A2 handshake: `experimental.serverStatusNotification: true`
    /// (warm-up tracking, SPEC-A2 §2) and
    /// `initializationOptions: {"files":{"watcher":"server"}}` (live truth =
    /// disk state; rust-analyzer watches its own files).
    pub fn new(process_id: u32, worktree_root: &Path) -> Self {
        Self {
            process_id: Some(process_id),
            root_uri: path_to_uri(worktree_root),
            capabilities: ClientCapabilities {
                experimental: ExperimentalCapabilities {
                    server_status_notification: true,
                },
            },
            initialization_options: json!({"files": {"watcher": "server"}}),
        }
    }
}

/// rust-analyzer `experimental/serverStatus` notification params.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerStatusParams {
    pub health: String,
    pub quiescent: bool,
    #[serde(default)]
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Absolute path → `file://` URI with minimal percent-encoding.
pub fn path_to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.to_string_lossy().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

/// `file://` URI → path. `None` for non-file URIs or malformed escapes.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let raw = uri.strip_prefix("file://")?.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            if i + 2 >= raw.len() {
                return None;
            }
            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// Normalize a `textDocument/definition` result — the LSP allows
/// `Location | Location[] | LocationLink[] | null`. Unrecognized items are a
/// loud error, never silently skipped.
pub fn definition_locations(result: &Value) -> io::Result<Vec<Location>> {
    let items: Vec<Value> = match result {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    let mut locations = Vec::with_capacity(items.len());
    for item in items {
        if let Ok(loc) = serde_json::from_value::<Location>(item.clone()) {
            locations.push(loc);
        } else if let Ok(link) = serde_json::from_value::<LocationLink>(item.clone()) {
            locations.push(Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            });
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unrecognized definition result item: {item}"),
            ));
        }
    }
    Ok(locations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor, Read};

    /// Inner reader that yields at most `chunk` bytes per `read` call —
    /// exercises split reads through the framing layer.
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.chunk.min(buf.len()).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    fn frame(msg: &Value) -> Vec<u8> {
        let mut buf = Vec::new();
        write_message(&mut buf, msg).unwrap();
        buf
    }

    fn r(line: u32, character: u32) -> Value {
        json!({"start": {"line": line, "character": character},
               "end": {"line": line, "character": character + 1}})
    }

    #[test]
    fn framing_roundtrip_single_message() {
        let msg = request(7, methods::DEFINITION, json!({"x": 1}));
        let mut reader = BufReader::new(Cursor::new(frame(&msg)));
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), msg);
        assert!(read_message(&mut reader).unwrap().is_none()); // clean EOF
    }

    #[test]
    fn framing_split_reads_and_back_to_back_frames() {
        let a = request(1, methods::REFERENCES, json!({"a": [1, 2, 3]}));
        let b = notification(
            methods::SERVER_STATUS,
            json!({"health": "ok", "quiescent": true}),
        );
        let mut bytes = frame(&a);
        bytes.extend(frame(&b));
        // 1-byte underlying reads through a tiny BufReader: worst-case splits.
        let mut reader = BufReader::with_capacity(
            3,
            ChunkedReader {
                data: bytes,
                pos: 0,
                chunk: 1,
            },
        );
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), a);
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), b);
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn framing_missing_content_length_is_loud() {
        let mut reader = BufReader::new(Cursor::new(b"Content-Type: foo\r\n\r\n{}".to_vec()));
        let err = read_message(&mut reader).unwrap_err();
        assert!(err.to_string().contains("Content-Length"), "{err}");
    }

    #[test]
    fn framing_truncated_body_is_loud() {
        let mut reader = BufReader::new(Cursor::new(b"Content-Length: 100\r\n\r\n{}".to_vec()));
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn framing_eof_mid_headers_is_loud() {
        let mut reader = BufReader::new(Cursor::new(b"Content-Length: 2\r\n".to_vec()));
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn framing_header_name_is_case_insensitive_and_extra_headers_ignored() {
        let body = b"{}";
        let mut bytes = format!(
            "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(body);
        let v = read_message(&mut BufReader::new(Cursor::new(bytes)))
            .unwrap()
            .unwrap();
        assert_eq!(v, json!({}));
    }

    #[test]
    fn request_and_notification_omit_null_params() {
        let shutdown = request(9, methods::SHUTDOWN, Value::Null);
        assert!(shutdown.get("params").is_none());
        assert_eq!(shutdown["id"], json!(9));
        let exit = notification(methods::EXIT, Value::Null);
        assert!(exit.get("params").is_none());
        assert!(exit.get("id").is_none());
    }

    #[test]
    fn initialize_params_shape_matches_spec() {
        let p = InitializeParams::new(42, Path::new("/tmp/wt"));
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v.pointer("/processId"), Some(&json!(42)));
        assert_eq!(v.pointer("/rootUri"), Some(&json!("file:///tmp/wt")));
        assert_eq!(
            v.pointer("/capabilities/experimental/serverStatusNotification"),
            Some(&json!(true))
        );
        assert_eq!(
            v.pointer("/initializationOptions/files/watcher"),
            Some(&json!("server"))
        );
    }

    #[test]
    fn reference_and_rename_params_shapes() {
        let pos = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///a.rs".into(),
            },
            position: Position {
                line: 0,
                character: 4,
            },
        };
        let refs = serde_json::to_value(ReferenceParams {
            position: pos.clone(),
            context: ReferenceContext {
                include_declaration: true,
            },
        })
        .unwrap();
        assert_eq!(
            refs.pointer("/textDocument/uri"),
            Some(&json!("file:///a.rs"))
        );
        assert_eq!(
            refs.pointer("/context/includeDeclaration"),
            Some(&json!(true))
        );

        let ren = serde_json::to_value(RenameParams {
            position: pos,
            new_name: "twice".into(),
        })
        .unwrap();
        assert_eq!(ren.pointer("/newName"), Some(&json!("twice")));
        assert_eq!(ren.pointer("/position/line"), Some(&json!(0)));
        assert_eq!(
            ren.pointer("/textDocument/uri"),
            Some(&json!("file:///a.rs"))
        );
    }

    #[test]
    fn workspace_edit_changes_parse() {
        let v = json!({"changes": {"file:///a.rs": [
            {"range": r(0, 7), "newText": "twice"}
        ]}});
        let we: WorkspaceEdit = serde_json::from_value(v).unwrap();
        let changes = we.changes.unwrap();
        let edits = &changes["file:///a.rs"];
        assert_eq!(edits[0].new_text, "twice");
        assert_eq!(edits[0].range.start.character, 7);
        assert!(we.document_changes.is_none());
    }

    #[test]
    fn call_hierarchy_item_roundtrips_extra_fields() {
        // `data` is rust-analyzer's opaque token: it MUST survive the
        // prepare → incomingCalls round trip byte-for-byte.
        let v = json!({
            "name": "double", "kind": 12, "uri": "file:///ops.rs",
            "range": r(0, 0), "selectionRange": r(0, 7),
            "data": {"token": 99}, "detail": "fn double(x: i32) -> i32"
        });
        let item: CallHierarchyItem = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&item).unwrap(), v);

        let call: CallHierarchyIncomingCall =
            serde_json::from_value(json!({"from": v, "fromRanges": [r(4, 40)]})).unwrap();
        assert_eq!(call.from.name, "double");
        assert_eq!(call.from_ranges.len(), 1);

        let params = serde_json::to_value(CallHierarchyIncomingCallsParams { item }).unwrap();
        assert_eq!(params.pointer("/item/data/token"), Some(&json!(99)));
    }

    #[test]
    fn server_status_params_parse() {
        let s: ServerStatusParams = serde_json::from_value(
            json!({"health": "ok", "quiescent": false, "message": "indexing"}),
        )
        .unwrap();
        assert!(!s.quiescent);
        assert_eq!(s.message.as_deref(), Some("indexing"));
        // message is optional
        let s: ServerStatusParams =
            serde_json::from_value(json!({"health": "ok", "quiescent": true})).unwrap();
        assert!(s.quiescent);
    }

    #[test]
    fn definition_locations_accepts_all_response_shapes() {
        let loc = json!({"uri": "file:///x.rs", "range": r(0, 0)});
        assert_eq!(definition_locations(&loc).unwrap().len(), 1);
        assert_eq!(definition_locations(&json!([loc, loc])).unwrap().len(), 2);
        let link = json!({
            "targetUri": "file:///y.rs",
            "targetRange": r(0, 0),
            "targetSelectionRange": r(0, 7)
        });
        let locs = definition_locations(&json!([link])).unwrap();
        assert_eq!(locs[0].uri, "file:///y.rs");
        assert_eq!(locs[0].range.start.character, 7);
        assert!(definition_locations(&Value::Null).unwrap().is_empty());
        assert!(definition_locations(&json!(["nope"])).is_err());
    }

    #[test]
    fn uri_path_roundtrip_with_percent_encoding() {
        let p = Path::new("/tmp/with space/crate");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///tmp/with%20space/crate");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
        assert!(uri_to_path("untitled:foo").is_none());
        assert!(uri_to_path("file:///bad%2").is_none());
    }
}
