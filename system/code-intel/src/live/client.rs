//! `cq`-side UDS client for the `scipd` daemon (SPEC-A2 §3, plan Task 6).
//!
//! Newline-delimited JSON over `<home>/scipd.sock`. The connect path is
//! bounded at 500ms (SPEC-A2 §5: socket failures add ≤500ms to a query) —
//! a missing socket file or refused connect fails immediately, and a
//! pathological blocking connect is cut off by the timeout. Replies are
//! read with a generous timeout: the daemon answers warming IMMEDIATELY,
//! but a Ready instance's answer is real LSP work (the daemon's per-request
//! LSP timeout is 30s, and refs/callers chain several requests).
//!
//! Every failure is a structured [`ClientError`] — the routing layer turns
//! it into `escalated.daemon-unavailable` (auto) or `LIVE_UNAVAILABLE`
//! (forced/rename). Nothing here hangs and nothing fails silently.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use crate::daemon::socket_path;
use crate::proto::{Op, QueryVerb, Reply, Request};

/// Hard bound on connect (SPEC-A2 §5/§7: daemon-down adds ≤500ms).
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// Reply read timeout: must outlast the daemon's worst-case dispatch
/// (chained 30s LSP requests for refs/callers/rename).
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Why the client could not get an answer from the daemon.
#[derive(Debug)]
pub enum ClientError {
    /// Daemon unreachable: socket missing, connect refused/timed out, or
    /// the connection died mid-exchange.
    Unavailable { reason: String },
    /// The daemon answered, but with something other than a valid reply to
    /// our request (protocol bug — loud, never ignored).
    Protocol { reason: String },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Unavailable { reason } => write!(f, "scipd unreachable: {reason}"),
            ClientError::Protocol { reason } => write!(f, "scipd protocol error: {reason}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// A connected client. One request/reply pair per call; the connection is
/// reused across calls on the same client.
#[derive(Debug)]
pub struct LiveClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl LiveClient {
    /// Connect to `<home>/scipd.sock` within [`CONNECT_TIMEOUT`].
    ///
    /// UDS connects to a live listener complete immediately; the timeout
    /// thread guards the pathological case (e.g. a full backlog) so cq can
    /// never hang on a sick daemon. A timed-out connect leaks only the
    /// detached helper thread, which dies as soon as the OS call returns.
    pub fn connect(home: &Path) -> Result<LiveClient, ClientError> {
        let socket = socket_path(home);
        if !socket.exists() {
            return Err(ClientError::Unavailable {
                reason: format!("no daemon socket at {}", socket.display()),
            });
        }
        let (tx, rx) = mpsc::channel();
        {
            let socket = socket.clone();
            std::thread::spawn(move || {
                let _ = tx.send(UnixStream::connect(&socket));
            });
        }
        let stream = match rx.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                return Err(ClientError::Unavailable {
                    reason: format!("connecting to {}: {e}", socket.display()),
                })
            }
            Err(_) => {
                return Err(ClientError::Unavailable {
                    reason: format!(
                        "connect to {} timed out after {CONNECT_TIMEOUT:?}",
                        socket.display()
                    ),
                })
            }
        };
        let io_err = |what: &str, e: std::io::Error| ClientError::Unavailable {
            reason: format!("{what}: {e}"),
        };
        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_err(|e| io_err("setting read timeout", e))?;
        stream
            .set_write_timeout(Some(WRITE_TIMEOUT))
            .map_err(|e| io_err("setting write timeout", e))?;
        let writer = stream.try_clone().map_err(|e| io_err("cloning stream", e))?;
        Ok(LiveClient { reader: BufReader::new(stream), writer, next_id: 1 })
    }

    /// `{"op":"ping"}` round-trip; `Ok(())` iff the daemon answered ok.
    pub fn ping(&mut self) -> Result<(), ClientError> {
        let reply = self.request(Op::Ping)?;
        if reply.ok {
            Ok(())
        } else {
            Err(ClientError::Protocol { reason: format!("ping answered not-ok: {reply:?}") })
        }
    }

    /// `{"op":"status"}` → the daemon's pool status section.
    pub fn status(&mut self) -> Result<crate::proto::PoolStatus, ClientError> {
        let reply = self.request(Op::Status)?;
        reply.status.ok_or_else(|| ClientError::Protocol {
            reason: "status reply carried no status section".into(),
        })
    }

    /// Live query (SPEC-A2 §3). The caller interprets the reply
    /// (ok/warming/error are all valid daemon answers, not client errors).
    pub fn query(
        &mut self,
        verb: QueryVerb,
        worktree: &Path,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Reply, ClientError> {
        self.request(Op::Query {
            verb,
            worktree: worktree.display().to_string(),
            path: path.to_string(),
            line,
            col,
            name: None,
        })
    }

    /// Live rename (SPEC-A2 §3/§5).
    pub fn rename(
        &mut self,
        worktree: &Path,
        path: &str,
        line: u32,
        col: u32,
        new_name: &str,
    ) -> Result<Reply, ClientError> {
        self.request(Op::Rename {
            worktree: worktree.display().to_string(),
            path: path.to_string(),
            line,
            col,
            new_name: new_name.to_string(),
        })
    }

    /// One request line out, one reply line in, with id correlation.
    fn request(&mut self, op: Op) -> Result<Reply, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_string(&Request { id, op })
            .map_err(|e| ClientError::Protocol { reason: format!("serializing request: {e}") })?;
        line.push('\n');
        let io_err = |what: &str, e: std::io::Error| ClientError::Unavailable {
            reason: format!("{what}: {e}"),
        };
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| io_err("writing request", e))?;
        self.writer.flush().map_err(|e| io_err("flushing request", e))?;

        let mut reply_line = String::new();
        let n = self
            .reader
            .read_line(&mut reply_line)
            .map_err(|e| io_err("reading reply", e))?;
        if n == 0 {
            return Err(ClientError::Unavailable {
                reason: "daemon closed the connection before replying".into(),
            });
        }
        let reply: Reply = serde_json::from_str(reply_line.trim()).map_err(|e| {
            ClientError::Protocol { reason: format!("unparseable reply: {e}: {reply_line:?}") }
        })?;
        if reply.id != id {
            return Err(ClientError::Protocol {
                reason: format!("reply id {} does not match request id {id}", reply.id),
            });
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    /// Minimal in-test daemon: answers each request line via `respond`.
    fn fake_daemon(
        home: &Path,
        respond: impl Fn(&str) -> String + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        let listener = UnixListener::bind(socket_path(home)).unwrap();
        std::thread::spawn(move || {
            // One connection per fake daemon is enough for tests.
            let Ok((stream, _addr)) = listener.accept() else { return };
            let mut writer = stream.try_clone().unwrap();
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let reply = respond(&line);
                if writer.write_all(reply.as_bytes()).is_err() {
                    break;
                }
                let _ = writer.write_all(b"\n");
            }
        })
    }

    fn echo_ok(line: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        format!(r#"{{"id":{},"ok":true}}"#, v["id"])
    }

    #[test]
    fn missing_socket_fails_fast_and_loud() {
        let home = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        let err = LiveClient::connect(home.path()).unwrap_err();
        assert!(t0.elapsed() < CONNECT_TIMEOUT, "{:?}", t0.elapsed());
        assert!(matches!(err, ClientError::Unavailable { .. }), "{err}");
        assert!(err.to_string().contains("no daemon socket"), "{err}");
    }

    #[test]
    fn stale_socket_file_fails_fast() {
        // A socket file with no listener behind it: connect refused, fast.
        //
        // Deliberately a DATAGRAM socket, not a dropped `UnixListener`: on
        // macOS, `UnixListener::bind`'s FD_CLOEXEC fcntl is not atomic with
        // `socket(2)`, so a concurrently `posix_spawn`ed test subprocess can
        // inherit the listener fd and keep the kernel socket accepting after
        // the drop — the same race `Daemon::bind`'s flock probe defends
        // against (see daemon.rs). A stream `connect(2)` to a datagram
        // address fails immediately regardless of any inherited fd, so the
        // "socket file exists, nothing serves it" scenario stays
        // deterministic under parallel-suite load.
        let home = tempfile::tempdir().unwrap();
        drop(std::os::unix::net::UnixDatagram::bind(socket_path(home.path())).unwrap());
        let t0 = Instant::now();
        let err = LiveClient::connect(home.path()).unwrap_err();
        assert!(
            t0.elapsed() < Duration::from_millis(600),
            "stale-socket connect took {:?}",
            t0.elapsed()
        );
        assert!(matches!(err, ClientError::Unavailable { .. }), "{err}");
    }

    #[test]
    fn ping_round_trips_with_id_correlation() {
        let home = tempfile::tempdir().unwrap();
        let _daemon = fake_daemon(home.path(), echo_ok);
        let mut client = LiveClient::connect(home.path()).unwrap();
        client.ping().unwrap();
        client.ping().unwrap(); // ids advance per request on one connection
    }

    #[test]
    fn mismatched_reply_id_is_protocol_error() {
        let home = tempfile::tempdir().unwrap();
        let _daemon = fake_daemon(home.path(), |_| r#"{"id":999,"ok":true}"#.to_string());
        let mut client = LiveClient::connect(home.path()).unwrap();
        let err = client.ping().unwrap_err();
        assert!(matches!(err, ClientError::Protocol { .. }), "{err}");
    }

    #[test]
    fn garbage_reply_is_protocol_error() {
        let home = tempfile::tempdir().unwrap();
        let _daemon = fake_daemon(home.path(), |_| "}{ not json".to_string());
        let mut client = LiveClient::connect(home.path()).unwrap();
        let err = client.ping().unwrap_err();
        assert!(matches!(err, ClientError::Protocol { .. }), "{err}");
    }

    #[test]
    fn hangup_before_reply_is_unavailable() {
        let home = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(socket_path(home.path())).unwrap();
        std::thread::spawn(move || {
            // Accept, then drop without reading or writing.
            let _ = listener.accept();
        });
        // Depending on timing the hangup surfaces during connect (socket
        // already reset) or during the ping — both must be Unavailable.
        let err = match LiveClient::connect(home.path()) {
            Err(e) => e,
            Ok(mut client) => client.ping().unwrap_err(),
        };
        assert!(matches!(err, ClientError::Unavailable { .. }), "{err}");
    }

    #[test]
    fn query_and_rename_send_spec_wire_shapes() {
        let home = tempfile::tempdir().unwrap();
        let _daemon = fake_daemon(home.path(), |line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            // Assert the request wire shape inside the fake daemon.
            match v["op"].as_str().unwrap() {
                "query" => {
                    assert_eq!(v["verb"], "refs");
                    assert_eq!(v["worktree"], "/w");
                    assert_eq!(v["path"], "src/a.rs");
                    assert_eq!(v["line"], 3);
                    assert_eq!(v["col"], 9);
                    format!(r#"{{"id":{},"ok":true,"source":"live","results":[]}}"#, v["id"])
                }
                "rename" => {
                    assert_eq!(v["new_name"], "twice");
                    format!(r#"{{"id":{},"ok":true,"edits":[]}}"#, v["id"])
                }
                other => panic!("unexpected op {other}"),
            }
        });
        let mut client = LiveClient::connect(home.path()).unwrap();
        let reply = client
            .query(QueryVerb::Refs, Path::new("/w"), "src/a.rs", 3, 9)
            .unwrap();
        assert!(reply.ok);
        assert_eq!(reply.source.as_deref(), Some("live"));
        let reply = client.rename(Path::new("/w"), "src/a.rs", 3, 9, "twice").unwrap();
        assert!(reply.ok);
        assert!(reply.edits.is_some());
    }
}
