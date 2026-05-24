/// Port of .hex/scripts/integrations/mcp-excalidraw.sh
/// Health probe for the Excalidraw MCP integration.
use std::process::Command;

const INTEGRATION: &str = "mcp-excalidraw";
const EXCALIDRAW_API_URL: &str = "https://api.excalidraw.com/api/v2/workspaces/me";

fn emit_event(event: &str, status: &str, msg: &str) {
    let ts = {
        let output = Command::new("date")
            .arg("-u")
            .arg("+%Y-%m-%dT%H:%M:%SZ")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        output.trim().to_string()
    };
    eprintln!(
        r#"{{"event":"{}","status":"{}","message":"{}","ts":"{}"}}"#,
        event, status, msg, ts
    );
}

fn claude_json_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".claude.json")
}

fn secrets_file_path() -> std::path::PathBuf {
    if let Ok(hex_dir) = std::env::var("HEX_DIR") {
        return std::path::PathBuf::from(hex_dir).join(".hex/secrets/excalidraw.env");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home)
        .join("hex/.hex/secrets/excalidraw.env")
}

/// Check if "excalidraw" appears (case-insensitive) anywhere in ~/.claude.json.
fn check_claude_json(claude_json: &std::path::Path) -> bool {
    let text = match std::fs::read_to_string(claude_json) {
        Ok(t) => t,
        Err(_) => return false,
    };
    text.to_lowercase().contains("excalidraw")
}

/// Read EXCALIDRAW_API_KEY from the secrets env file.
fn read_api_key(secrets_path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(secrets_path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("EXCALIDRAW_API_KEY=") {
            let key = rest.trim_matches('"').trim_matches('\'').trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// Light connectivity check to Excalidraw workspace API (optional — degraded on failure).
fn check_connectivity(api_key: &str) -> i32 {
    let output = Command::new("curl")
        .args([
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            "-H", &format!("Authorization: Bearer {}", api_key),
            "--connect-timeout", "5",
            "--max-time", "10",
            EXCALIDRAW_API_URL,
        ])
        .output();
    match output {
        Ok(o) => String::from_utf8(o.stdout)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .unwrap_or(0),
        Err(_) => 0,
    }
}

fn which_curl() -> bool {
    Command::new("which")
        .arg("curl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn run_probe() -> i32 {
    println!("[{}/probe] checking Excalidraw MCP...", INTEGRATION);
    let mut result = 0;

    // 1. Check excalidraw MCP is configured in ~/.claude.json
    let claude_json = claude_json_path();
    if claude_json.exists() {
        if check_claude_json(&claude_json) {
            println!(
                "[{}/probe] excalidraw MCP found in {}",
                INTEGRATION,
                claude_json.display()
            );
        } else {
            eprintln!(
                "[{}/probe] WARN: excalidraw not found in {}",
                INTEGRATION,
                claude_json.display()
            );
            result = 1;
        }
    } else {
        eprintln!(
            "[{}/probe] WARN: {} not found",
            INTEGRATION,
            claude_json.display()
        );
        result = 1;
    }

    // 2. Check EXCALIDRAW_API_KEY is set in secrets file
    let secrets_path = secrets_file_path();
    let api_key = if secrets_path.exists() {
        match read_api_key(&secrets_path) {
            Some(k) => {
                println!(
                    "[{}/probe] EXCALIDRAW_API_KEY present ({} chars)",
                    INTEGRATION,
                    k.len()
                );
                Some(k)
            }
            None => {
                eprintln!(
                    "[{}/probe] WARN: EXCALIDRAW_API_KEY empty in {}",
                    INTEGRATION,
                    secrets_path.display()
                );
                result = 1;
                None
            }
        }
    } else {
        eprintln!(
            "[{}/probe] WARN: secrets file not found: {}",
            INTEGRATION,
            secrets_path.display()
        );
        result = 1;
        None
    };

    // 3. Light connectivity check (optional — failure is degraded, not hard fail)
    if let Some(ref key) = api_key {
        if which_curl() {
            let http_status = check_connectivity(key);
            match http_status {
                200 => {
                    println!(
                        "[{}/probe] workspace API: HTTP {} (ok)",
                        INTEGRATION, http_status
                    );
                }
                401 | 403 => {
                    eprintln!(
                        "[{}/probe] WARN: workspace API: HTTP {} (auth error)",
                        INTEGRATION, http_status
                    );
                    result = 1;
                }
                _ => {
                    eprintln!(
                        "[{}/probe] WARN: workspace API: HTTP {} (degraded)",
                        INTEGRATION, http_status
                    );
                    result = 1;
                }
            }
        }
    }

    if result == 0 {
        emit_event(
            "hex.integration.mcp-excalidraw.probe_ok",
            "ok",
            "config+token+api ok",
        );
        println!("[{}/probe] OK", INTEGRATION);
    } else {
        emit_event(
            "hex.integration.mcp-excalidraw.probe_fail",
            "fail",
            "one or more checks degraded",
        );
        eprintln!("[{}/probe] DEGRADED (exit {})", INTEGRATION, result);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_parsed_from_env_file() {
        let tmp = std::env::temp_dir().join("mcp_excalidraw_test.env");
        std::fs::write(&tmp, "EXCALIDRAW_API_KEY=test-key-abc\n").unwrap();
        assert_eq!(read_api_key(&tmp), Some("test-key-abc".to_string()));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn api_key_missing_returns_none() {
        let tmp = std::env::temp_dir().join("mcp_excalidraw_empty.env");
        std::fs::write(&tmp, "# no key here\nFOO=bar\n").unwrap();
        assert_eq!(read_api_key(&tmp), None);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn api_key_quoted_variants() {
        let tmp = std::env::temp_dir().join("mcp_excalidraw_quoted.env");
        std::fs::write(&tmp, "EXCALIDRAW_API_KEY=\"quoted-key\"\n").unwrap();
        assert_eq!(read_api_key(&tmp), Some("quoted-key".to_string()));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn claude_json_check_detects_excalidraw() {
        let tmp = std::env::temp_dir().join("mcp_excalidraw_claude.json");
        std::fs::write(
            &tmp,
            r#"{"mcpServers":{"excalidraw":{"command":"npx"}}}"#,
        )
        .unwrap();
        assert!(check_claude_json(&tmp));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn claude_json_check_case_insensitive() {
        let tmp = std::env::temp_dir().join("mcp_excalidraw_claude2.json");
        std::fs::write(&tmp, r#"{"mcpServers":{"Excalidraw":{}}}"#).unwrap();
        assert!(check_claude_json(&tmp));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn claude_json_check_returns_false_when_absent() {
        let tmp = std::env::temp_dir().join("mcp_excalidraw_claude3.json");
        std::fs::write(&tmp, r#"{"mcpServers":{"exa":{}}}"#).unwrap();
        assert!(!check_claude_json(&tmp));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn constants_match_shell_script() {
        assert_eq!(INTEGRATION, "mcp-excalidraw");
        assert_eq!(
            EXCALIDRAW_API_URL,
            "https://api.excalidraw.com/api/v2/workspaces/me"
        );
    }
}
