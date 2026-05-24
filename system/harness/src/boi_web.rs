/// Native Rust SSE server for BOI live status web view.
/// Replaces the former python3 shim; no longer delegates to any external script.
/// Serves GET / (HTML), GET /api/stream (SSE), GET /api/status.json.
/// Respects PORT (default 8891), CERT, and KEY env vars.
/// Ported from the legacy Python script (now deleted).
use std::convert::Infallible;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
use tokio::sync::broadcast;
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
            let mut payload = fetch_status();
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
        let listener = TcpListener::bind(&addr).await.expect("bind");
        axum::serve(listener, app).await.expect("serve");
    }
}

async fn serve_tls(app: Router, addr: &str, cert_path: &str, key_path: &str) {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::TlsAcceptor;

    let cert_file = std::fs::File::open(cert_path).expect("open cert");
    let key_file = std::fs::File::open(key_path).expect("open key");

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .expect("parse certs");
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .expect("parse key io")
        .expect("no private key found");

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("TLS config");
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = std::net::TcpListener::bind(addr).expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let listener = tokio::net::TcpListener::from_std(listener).expect("async listener");

    loop {
        let (stream, _) = listener.accept().await.expect("accept");
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            if let Ok(tls_stream) = acceptor.accept(stream).await {
                let io = TokioIo::new(tls_stream);
                let svc = TowerToHyperService::new(app);
                let _ = http1::Builder::new()
                    .serve_connection(io, svc)
                    .with_upgrades()
                    .await;
            }
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
    let mut payload = fetch_status();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    payload["_timestamp"] = serde_json::json!(ts);
    Json(payload)
}

fn fetch_status() -> Value {
    let boi = shellexpand::tilde("~/.boi/boi").to_string();
    match Command::new("bash")
        .arg(&boi)
        .args(["status", "--json", "--all"])
        .output()
    {
        Err(e) => serde_json::json!({ "error": e.to_string() }),
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            serde_json::json!({
                "error": format!("boi exited {}", out.status.code().unwrap_or(-1)),
                "stderr": &stderr[..stderr.len().min(500)],
            })
        }
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            serde_json::from_str(&stdout).unwrap_or_else(|e| {
                serde_json::json!({ "error": format!("parse error: {e}") })
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
