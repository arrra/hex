/// Port of .hex/scripts/boi-web/serve.sh
/// Launches the BOI live status web view via server.py.
/// Respects PORT, CERT, and KEY env vars (passed through from caller env).
use std::path::PathBuf;
use std::process::Command;

const SERVER_REL: &str = ".hex/scripts/boi-web/server.py";
const DEFAULT_PORT: &str = "8891";

pub fn run_serve(hex_dir: &PathBuf) {
    let server_py = hex_dir.join(SERVER_REL);
    let server_dir = server_py
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    if !server_py.exists() {
        eprintln!("ERROR: boi-web server not found at {}", server_py.display());
        std::process::exit(1);
    }

    let port = std::env::var("PORT").unwrap_or_else(|_| DEFAULT_PORT.to_string());
    let cert = std::env::var("CERT").unwrap_or_default();
    let key = std::env::var("KEY").unwrap_or_default();

    let status = Command::new("/usr/bin/env")
        .arg("python3")
        .arg(&server_py)
        .current_dir(&server_dir)
        .env("PORT", &port)
        .env("CERT", &cert)
        .env("KEY", &key)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("hex boi-web serve: failed to launch server.py: {e}");
            std::process::exit(1);
        });
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_py_path_construction() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let expected = hex_dir.join(SERVER_REL);
        assert_eq!(
            expected.to_str().unwrap(),
            "/Users/test/hex/.hex/scripts/boi-web/server.py"
        );
    }

    #[test]
    fn server_dir_is_parent_of_server_py() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let server_py = hex_dir.join(SERVER_REL);
        let server_dir = server_py.parent().unwrap().to_path_buf();
        assert_eq!(
            server_dir.to_str().unwrap(),
            "/Users/test/hex/.hex/scripts/boi-web"
        );
    }

    #[test]
    fn default_port_is_8891() {
        assert_eq!(DEFAULT_PORT, "8891");
    }
}
