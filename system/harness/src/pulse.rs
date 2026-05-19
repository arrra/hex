/// Port of .hex/scripts/pulse/start.sh
/// Loads Anthropic API key from secrets file, then exec's pulse/server.py.
use std::path::PathBuf;
use std::process::Command;

const SECRETS_REL: &str = ".hex/secrets/anthropic.env";
const SERVER_REL: &str = ".hex/scripts/pulse/server.py";

/// Parse `KEY=VALUE` lines from an env file into (key, value) pairs.
/// Skips blank lines and lines starting with `#`.
fn parse_env_file(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let l = l.trim_start_matches("export ").trim();
            let eq = l.find('=')?;
            let key = l[..eq].trim().to_string();
            let val = l[eq + 1..].trim().trim_matches('"').trim_matches('\'').to_string();
            if key.is_empty() { None } else { Some((key, val)) }
        })
        .collect()
}

pub fn run_start(hex_dir: &PathBuf, extra_args: &[String]) {
    let secrets_path = hex_dir.join(SECRETS_REL);
    let mut env_vars: Vec<(String, String)> = Vec::new();
    if secrets_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&secrets_path) {
            env_vars = parse_env_file(&content);
        }
    }

    let server_py = hex_dir.join(SERVER_REL);

    let mut cmd = Command::new("/opt/homebrew/bin/python3");
    cmd.arg(&server_py);
    for arg in extra_args {
        cmd.arg(arg);
    }
    for (k, v) in &env_vars {
        cmd.env(k, v);
    }

    let status = cmd.status().unwrap_or_else(|e| {
        eprintln!("hex pulse start: failed to launch server.py: {e}");
        std::process::exit(1);
    });
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_file_basic() {
        let content = "# comment\nANTHROPIC_API_KEY=sk-ant-123\nFOO=bar\n";
        let pairs = parse_env_file(content);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("ANTHROPIC_API_KEY".to_string(), "sk-ant-123".to_string()));
        assert_eq!(pairs[1], ("FOO".to_string(), "bar".to_string()));
    }

    #[test]
    fn parse_env_file_export_prefix() {
        let content = "export MY_KEY=\"hello world\"\n";
        let pairs = parse_env_file(content);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("MY_KEY".to_string(), "hello world".to_string()));
    }

    #[test]
    fn parse_env_file_skips_blanks() {
        let content = "\n\nKEY=val\n\n";
        let pairs = parse_env_file(content);
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn server_py_path_construction() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let expected = hex_dir.join(SERVER_REL);
        assert_eq!(expected.to_str().unwrap(), "/Users/test/hex/.hex/scripts/pulse/server.py");
    }
}
