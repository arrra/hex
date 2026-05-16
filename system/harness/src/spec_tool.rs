/// Port of .hex/scripts/spec-tool/run.sh
/// Launches the spec-tool server.py from its own directory.
use std::path::PathBuf;
use std::process::Command;

const SERVER_REL: &str = ".hex/scripts/spec-tool/server.py";

pub fn run_run(hex_dir: &PathBuf) {
    let server_py = hex_dir.join(SERVER_REL);
    let server_dir = server_py
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    if !server_py.exists() {
        eprintln!("ERROR: spec-tool server not found at {}", server_py.display());
        std::process::exit(1);
    }

    let status = Command::new("/usr/bin/env")
        .arg("python3")
        .arg(&server_py)
        .current_dir(&server_dir)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("hex spec-tool run: failed to launch server.py: {e}");
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
            "/Users/test/hex/.hex/scripts/spec-tool/server.py"
        );
    }

    #[test]
    fn server_dir_is_parent_of_server_py() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let server_py = hex_dir.join(SERVER_REL);
        let server_dir = server_py.parent().unwrap().to_path_buf();
        assert_eq!(
            server_dir.to_str().unwrap(),
            "/Users/test/hex/.hex/scripts/spec-tool"
        );
    }
}
