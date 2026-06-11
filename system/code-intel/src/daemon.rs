//! `scipd` UDS server skeleton, per SPEC-A2 §2/§3 (plan Task 1).
//!
//! Single-process, multi-threaded, std blocking IO (no tokio by design):
//! an accept loop plus one thread per connection. Task 1 ships `ping` and a
//! stub `status` (empty pool); pool-backed dispatch arrives in Task 5.
//!
//! Socket lifecycle:
//! - bind at `<home>/scipd.sock`;
//! - a stale socket file (no listener behind it) is unlinked before bind;
//! - if a LIVE daemon already owns the socket, [`Daemon::bind`] refuses
//!   loudly ([`BindError::AlreadyRunning`]) — never two daemons;
//! - SIGTERM (flag set by the bin) drains the accept loop, removes the
//!   socket file, and exits cleanly.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::config::ScipdConfig;
use crate::proto::{parse_request, Op, PoolStatus, Reply, Request};

/// Per-connection socket read/write timeouts (SPEC-A2 plan Task 1 band:
/// 500ms–5s). Reads get the long end (a client may hold the connection
/// between requests); writes are local and short.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// Accept-loop poll interval while checking the shutdown flag.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// The daemon socket lives directly under the codeintel home (SPEC-A2 §2).
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("scipd.sock")
}

/// Why `bind` refused.
#[derive(Debug)]
pub enum BindError {
    /// A live daemon already answers on the socket — second daemon must
    /// refuse loudly (plan Task 1).
    AlreadyRunning { socket: PathBuf },
    Io(std::io::Error),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::AlreadyRunning { socket } => write!(
                f,
                "another scipd is already serving {}; refusing to start a second daemon",
                socket.display()
            ),
            BindError::Io(e) => write!(f, "binding scipd socket: {e}"),
        }
    }
}

impl std::error::Error for BindError {}

/// A bound, not-yet-running daemon.
#[derive(Debug)]
pub struct Daemon {
    listener: UnixListener,
    socket: PathBuf,
    config: ScipdConfig,
    shutdown: Arc<AtomicBool>,
}

impl Daemon {
    /// Bind the UDS at `<home>/scipd.sock` with stale-socket handling:
    /// try-connect first — a successful connect means a live daemon owns the
    /// socket (refuse); a refused/failed connect means the file is stale
    /// (unlink, then bind).
    pub fn bind(home: &Path, config: ScipdConfig) -> Result<Daemon, BindError> {
        let socket = socket_path(home);
        if socket.exists() {
            match UnixStream::connect(&socket) {
                Ok(_) => return Err(BindError::AlreadyRunning { socket }),
                Err(e) => {
                    eprintln!(
                        "scipd: removing stale socket {} (connect failed: {e})",
                        socket.display()
                    );
                    std::fs::remove_file(&socket).map_err(BindError::Io)?;
                }
            }
        }
        let listener = UnixListener::bind(&socket).map_err(BindError::Io)?;
        // Nonblocking accept so the loop can observe the shutdown flag.
        listener.set_nonblocking(true).map_err(BindError::Io)?;
        Ok(Daemon { listener, socket, config, shutdown: Arc::new(AtomicBool::new(false)) })
    }

    /// Flag the bin wires to SIGTERM: once set, `run` drains and returns.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    /// Accept loop. Returns after the shutdown flag is set; removes the
    /// socket file on the way out (clean shutdown, SPEC-A2 plan Task 1).
    pub fn run(self) -> std::io::Result<()> {
        eprintln!("scipd: listening on {}", self.socket.display());
        let config = Arc::new(self.config);
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                break;
            }
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    let config = Arc::clone(&config);
                    std::thread::spawn(move || {
                        if let Err(e) = serve_connection(stream, &config) {
                            // Per-connection failures are loud but never
                            // take the daemon down (Standing Order S6).
                            eprintln!("scipd: connection error: {e}");
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    eprintln!("scipd: accept error: {e}");
                    std::thread::sleep(ACCEPT_POLL);
                }
            }
        }
        eprintln!("scipd: shutdown requested; removing {}", self.socket.display());
        if let Err(e) = std::fs::remove_file(&self.socket) {
            eprintln!("scipd: removing socket on shutdown: {e}");
        }
        Ok(())
    }
}

/// Handle one connection: newline-JSON requests in, one reply line each.
fn serve_connection(stream: UnixStream, config: &ScipdConfig) -> std::io::Result<()> {
    // The accepted stream can inherit the listener's nonblocking mode
    // (platform-dependent); per-connection IO must be blocking-with-timeout.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            // Timeout or peer hangup: close the connection quietly — the
            // client owns the connection lifetime.
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = dispatch(&line, config);
        let mut payload = serde_json::to_string(&reply).expect("Reply serializes");
        payload.push('\n');
        writer.write_all(payload.as_bytes())?;
        writer.flush()?;
    }
    Ok(())
}

/// Pure request → reply dispatch (unit-tested without sockets). Task 1
/// implements `ping` + stub `status`; pool-backed ops reply UNIMPLEMENTED
/// until Task 5 wires the live pool.
pub fn dispatch(line: &str, config: &ScipdConfig) -> Reply {
    let req: Request = match parse_request(line) {
        Ok(req) => req,
        Err((id, reason)) => {
            return Reply::error(
                id,
                "BAD_REQUEST",
                format!("unparseable request: {reason}"),
                "send one JSON object per line, shaped per SPEC-A2 §3",
            );
        }
    };
    match req.op {
        Op::Ping => Reply::pong(req.id),
        Op::Status => Reply::status(
            req.id,
            PoolStatus { pool_cap: config.pool_cap, instances: vec![], notes: vec![] },
        ),
        Op::Query { .. } | Op::Rename { .. } | Op::Evict { .. } => Reply::error(
            req.id,
            "UNIMPLEMENTED",
            "live pool ops are not wired yet (A2 Task 5)".into(),
            "use ping/status; query/rename/evict arrive with the live pool",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ScipdConfig {
        ScipdConfig::default()
    }

    #[test]
    fn dispatch_ping_pongs() {
        let reply = dispatch(r#"{"id":1,"op":"ping"}"#, &config());
        assert_eq!(reply, Reply::pong(1));
    }

    #[test]
    fn dispatch_status_reports_empty_pool_with_configured_cap() {
        let reply = dispatch(r#"{"id":2,"op":"status"}"#, &config());
        assert!(reply.ok);
        let status = reply.status.expect("status section");
        assert_eq!(status.pool_cap, config().pool_cap);
        assert!(status.instances.is_empty());
    }

    #[test]
    fn dispatch_unknown_op_is_loud_error_with_echoed_id() {
        let reply = dispatch(r#"{"id":9,"op":"explode"}"#, &config());
        assert_eq!(reply.id, 9);
        assert!(!reply.ok);
        assert_eq!(reply.error.unwrap().code, "BAD_REQUEST");
    }

    #[test]
    fn dispatch_garbage_is_loud_error_never_panic() {
        let reply = dispatch("}{ total garbage", &config());
        assert_eq!(reply.id, 0);
        assert_eq!(reply.error.unwrap().code, "BAD_REQUEST");
    }

    #[test]
    fn dispatch_pool_ops_are_unimplemented_in_task1() {
        for line in [
            r#"{"id":3,"op":"query","verb":"def","worktree":"/w","path":"a.rs","line":1,"col":1}"#,
            r#"{"id":4,"op":"rename","worktree":"/w","path":"a.rs","line":1,"col":1,"new_name":"x"}"#,
            r#"{"id":5,"op":"evict","worktree":"/w"}"#,
        ] {
            let reply = dispatch(line, &config());
            assert!(!reply.ok, "{line}");
            assert_eq!(reply.error.unwrap().code, "UNIMPLEMENTED", "{line}");
        }
    }

    #[test]
    fn socket_path_is_under_home() {
        assert_eq!(socket_path(Path::new("/x")), PathBuf::from("/x/scipd.sock"));
    }

    #[test]
    fn bind_refuses_when_live_daemon_owns_socket() {
        let home = tempfile::tempdir().unwrap();
        let first = Daemon::bind(home.path(), config()).unwrap();
        let err = Daemon::bind(home.path(), config()).unwrap_err();
        assert!(matches!(err, BindError::AlreadyRunning { .. }), "{err}");
        drop(first);
    }

    #[test]
    fn bind_unlinks_stale_socket_and_succeeds() {
        let home = tempfile::tempdir().unwrap();
        // A socket file nobody listens on (listener dropped) is stale.
        {
            let d = Daemon::bind(home.path(), config()).unwrap();
            drop(d);
        }
        assert!(socket_path(home.path()).exists(), "dropped daemon leaves the file");
        let d = Daemon::bind(home.path(), config()).unwrap();
        drop(d);
    }
}
