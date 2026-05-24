/// Native Rust SSE server for BOI live status web view.
/// Replaces the former python3 shim; no longer delegates to any external script.
/// Serves GET / (HTML), GET /api/stream (SSE), GET /api/status.json.
/// Respects PORT (default 8891), CERT, and KEY env vars.
/// Ported from the legacy Python script (now deleted).
use std::convert::Infallible;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::State,
    response::{
        sse::{Event, Sse},
        Html, IntoResponse, Json,
    },
    routing::get,
    Router,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

const HTML: &str = include_str!("boi_web_index.html");
const DEFAULT_PORT: u16 = 8891;

type BcastTx = Arc<broadcast::Sender<String>>;

pub fn run_serve(_hex_dir: &std::path::PathBuf) {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let cert = std::env::var("CERT").unwrap_or_default();
    let key = std::env::var("KEY").unwrap_or_default();

    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(serve(port, cert, key));
}

async fn serve(port: u16, cert: String, key: String) {
    let (tx, _) = broadcast::channel::<String>(64);
    let tx = Arc::new(tx);

    let tx_bg = tx.clone();
    tokio::spawn(async move {
        loop {
            let mut payload = match timeout(Duration::from_secs(8), fetch_status()).await {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("boi_web: 'boi status' timed out (8s)");
                    serde_json::json!({ "error": "timeout", "boi_status_unavailable": true })
                }
            };
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            payload["_timestamp"] = serde_json::json!(ts);
            let _ = tx_bg.send(payload.to_string());
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    });

    let app = Router::new()
        .route("/", get(root_handler))
        .route("/api/stream", get(sse_handler))
        .route("/api/status.json", get(status_handler))
        .with_state(tx);

    let addr = format!("0.0.0.0:{}", port);
    let use_tls = !cert.is_empty() && !key.is_empty()
        && Path::new(&cert).exists()
        && Path::new(&key).exists();

    if use_tls {
        println!("BOI live status (TLS) → https://localhost:{}", port);
        serve_tls(app, &addr, &cert, &key).await;
    } else {
        println!("BOI live status (no TLS) → http://localhost:{}", port);
        let listener = TcpListener::bind(&addr).await.unwrap_or_else(|e| {
            eprintln!("boi_web: bind failed: {e} (port={port})");
            std::process::exit(1);
        });
        axum::serve(listener, app).await.unwrap_or_else(|e| {
            eprintln!("boi_web: serve error: {e} (port={port})");
            std::process::exit(1);
        });
    }
}

async fn serve_tls(app: Router, addr: &str, cert_path: &str, key_path: &str) {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::TlsAcceptor;

    let cert_file = std::fs::File::open(cert_path).unwrap_or_else(|e| {
        eprintln!("boi_web: open cert failed: {e} (path={cert_path})");
        std::process::exit(1);
    });
    let key_file = std::fs::File::open(key_path).unwrap_or_else(|e| {
        eprintln!("boi_web: open key failed: {e} (path={key_path})");
        std::process::exit(1);
    });

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| {
            eprintln!("boi_web: parse certs failed: {e} (path={cert_path})");
            std::process::exit(1);
        });
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .unwrap_or_else(|e| {
            eprintln!("boi_web: read key failed: {e} (path={key_path})");
            std::process::exit(1);
        })
        .unwrap_or_else(|| {
            eprintln!("boi_web: no private key found (path={key_path})");
            std::process::exit(1);
        });

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap_or_else(|e| {
            eprintln!("boi_web: TLS config failed: {e} (cert={cert_path} key={key_path})");
            std::process::exit(1);
        });
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = std::net::TcpListener::bind(addr).unwrap_or_else(|e| {
        eprintln!("boi_web: bind failed: {e} (addr={addr})");
        std::process::exit(1);
    });
    listener.set_nonblocking(true).unwrap_or_else(|e| {
        eprintln!("boi_web: set_nonblocking failed: {e} (addr={addr})");
        std::process::exit(1);
    });
    let listener = tokio::net::TcpListener::from_std(listener).unwrap_or_else(|e| {
        eprintln!("boi_web: async listener failed: {e} (addr={addr})");
        std::process::exit(1);
    });

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("boi_web: accept error: {e}");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("boi_web: TLS handshake failed: {e}");
                    return;
                }
            };
            let io = TokioIo::new(tls_stream);
            let svc = TowerToHyperService::new(app);
            let _ = http1::Builder::new()
                .serve_connection(io, svc)
                .with_upgrades()
                .await;
        });
    }
}

async fn root_handler() -> impl IntoResponse {
    Html(HTML)
}

async fn sse_handler(
    State(tx): State<BcastTx>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = BroadcastStream::new(tx.subscribe());
    let stream = rx.filter_map(|msg| {
        msg.ok().map(|data| Ok(Event::default().data(data)))
    });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}

async fn status_handler() -> impl IntoResponse {
    let mut payload = fetch_status().await;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    payload["_timestamp"] = serde_json::json!(ts);
    Json(payload)
}

async fn fetch_status() -> Value {
    let boi = shellexpand::tilde("~/.boi/boi").to_string();
    match Command::new("bash")
        .arg(&boi)
        .args(["status", "--json", "--all"])
        .output()
        .await
    {
        Err(e) => {
            eprintln!("boi_web: 'boi status' failed to spawn: {e}");
            serde_json::json!({ "error": e.to_string(), "boi_status_unavailable": true })
        }
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!(
                "boi_web: 'boi status' exited {}: {}",
                out.status.code().unwrap_or(-1),
                &stderr[..stderr.len().min(200)]
            );
            serde_json::json!({
                "error": format!("boi exited {}", out.status.code().unwrap_or(-1)),
                "stderr": &stderr[..stderr.len().min(500)],
                "boi_status_unavailable": true,
            })
        }
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            serde_json::from_str(&stdout).unwrap_or_else(|e| {
                eprintln!("boi_web: 'boi status' JSON parse error: {e}");
                serde_json::json!({ "error": format!("parse error: {e}"), "boi_status_unavailable": true })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_8891() {
        assert_eq!(DEFAULT_PORT, 8891);
    }

    #[test]
    fn html_contains_expected_elements() {
        assert!(HTML.contains("pulse"));
        assert!(HTML.contains("api/stream"));
    }
}
