/// Port of .hex/scripts/hex-router/serve.sh
/// Launches the hex-router reverse proxy (router.py) on 127.0.0.1:7000.
/// Tailscale Serve fronts this on :443 so named paths like /ui, /boi, /visions work.
use std::path::PathBuf;
use std::process::Command;

const ROUTER_REL: &str = ".hex/scripts/hex-router/router.py";
const DEFAULT_PORT: &str = "7000";

pub fn run_serve(hex_dir: &PathBuf) {
    let router_py = hex_dir.join(ROUTER_REL);
    let router_dir = router_py
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    if !router_py.exists() {
        eprintln!("ERROR: hex-router not found at {}", router_py.display());
        std::process::exit(1);
    }

    let port = std::env::var("PORT").unwrap_or_else(|_| DEFAULT_PORT.to_string());

    let status = Command::new("/usr/bin/env")
        .arg("python3")
        .arg(&router_py)
        .current_dir(&router_dir)
        .env("PORT", &port)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("hex router serve: failed to launch router.py: {e}");
            std::process::exit(1);
        });
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_py_path_construction() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let expected = hex_dir.join(ROUTER_REL);
        assert_eq!(
            expected.to_str().unwrap(),
            "/Users/test/hex/.hex/scripts/hex-router/router.py"
        );
    }

    #[test]
    fn router_dir_is_parent_of_router_py() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let router_py = hex_dir.join(ROUTER_REL);
        let router_dir = router_py.parent().unwrap().to_path_buf();
        assert_eq!(
            router_dir.to_str().unwrap(),
            "/Users/test/hex/.hex/scripts/hex-router"
        );
    }

    #[test]
    fn default_port_is_7000() {
        assert_eq!(DEFAULT_PORT, "7000");
    }
}
