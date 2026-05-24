/// Port of .hex/scripts/integrations/mcp-exa.sh
/// Health probe for the Exa MCP integration (provided via plugin:ecc).
use std::process::Command;

const INTEGRATION: &str = "mcp-exa";
const EXA_API_URL: &str = "https://api.exa.ai/search";
const EXA_PLUGIN_KEY: &str = "everything-claude-code";

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

fn claude_settings_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".claude/settings.json")
}

fn secrets_file_path() -> std::path::PathBuf {
    if let Ok(hex_dir) = std::env::var("HEX_DIR") {
        return std::path::PathBuf::from(hex_dir).join(".hex/secrets/mcp-exa.env");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home)
        .join("hex/.hex/secrets/mcp-exa.env")
}

/// Check if plugin:ecc (everything-claude-code) is enabled in Claude settings.
fn check_plugin_enabled(settings_path: &std::path::Path) -> bool {
    let text = match std::fs::read_to_string(settings_path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let plugins = match v.get("enabledPlugins").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return false,
    };
    plugins.iter().any(|(k, v)| {
        k.contains(EXA_PLUGIN_KEY) && v.as_bool().unwrap_or(false)
    })
}

/// Read EXA_API_KEY from the secrets env file.
fn read_api_key(secrets_path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(secrets_path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("EXA_API_KEY=") {
            let key = rest.trim_matches('"').trim_matches('\'').trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// Run the Exa connectivity probe (optional — degraded on failure, not hard fail).
fn check_connectivity(api_key: &str) -> i32 {
    let output = Command::new("curl")
        .args([
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            "-X", "POST", EXA_API_URL,
            "-H", "Content-Type: application/json",
            "-H", &format!("x-api-key: {}", api_key),
            "-d", r#"{"query":"hex reliability","numResults":1}"#,
            "--connect-timeout", "5",
            "--max-time", "15",
        ])
        .output();
    match output {
        Ok(o) => {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .unwrap_or(0)
        }
        Err(_) => 0,
    }
}

pub fn run_probe() -> i32 {
    println!("[{}/probe] checking Exa MCP (plugin:ecc:exa)...", INTEGRATION);
    let mut result = 0;

    // 1. Check plugin:ecc is enabled
    let settings_path = claude_settings_path();
    if settings_path.exists() {
        if check_plugin_enabled(&settings_path) {
            println!("[{}/probe] plugin:ecc (everything-claude-code) is enabled", INTEGRATION);
        } else {
            eprintln!(
                "[{}/probe] WARN: plugin:ecc not found/enabled in {}",
                INTEGRATION,
                settings_path.display()
            );
            result = 1;
        }
    } else {
        eprintln!(
            "[{}/probe] WARN: {} not found",
            INTEGRATION,
            settings_path.display()
        );
        result = 1;
    }

    // 2. Check EXA_API_KEY is set in secrets file
    let secrets_path = secrets_file_path();
    let api_key = if secrets_path.exists() {
        match read_api_key(&secrets_path) {
            Some(k) => {
                println!("[{}/probe] EXA_API_KEY present ({} chars)", INTEGRATION, k.len());
                Some(k)
            }
            None => {
                eprintln!(
                    "[{}/probe] WARN: EXA_API_KEY empty in {}",
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
                    println!("[{}/probe] Exa search API: HTTP {} (ok)", INTEGRATION, http_status);
                }
                401 | 403 => {
                    eprintln!(
                        "[{}/probe] WARN: Exa search API: HTTP {} (auth error)",
                        INTEGRATION, http_status
                    );
                    result = 1;
                }
                _ => {
                    eprintln!(
                        "[{}/probe] WARN: Exa search API: HTTP {} (degraded)",
                        INTEGRATION, http_status
                    );
                    result = 1;
                }
            }
        }
    }

    if result == 0 {
        emit_event(
            "hex.integration.mcp-exa.probe_ok",
            "ok",
            "plugin enabled + key present + api ok",
        );
        println!("[{}/probe] OK", INTEGRATION);
    } else {
        emit_event(
            "hex.integration.mcp-exa.probe_fail",
            "fail",
            "one or more checks degraded",
        );
        eprintln!("[{}/probe] DEGRADED (exit {})", INTEGRATION, result);
    }

    result
}

fn which_curl() -> bool {
    Command::new("which")
        .arg("curl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_parsed_from_env_file() {
        let tmp = std::env::temp_dir().join("mcp_exa_test.env");
        std::fs::write(&tmp, "EXA_API_KEY=test-key-123\n").unwrap();
        assert_eq!(read_api_key(&tmp), Some("test-key-123".to_string()));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn api_key_missing_returns_none() {
        let tmp = std::env::temp_dir().join("mcp_exa_empty.env");
        std::fs::write(&tmp, "# no key here\nFOO=bar\n").unwrap();
        assert_eq!(read_api_key(&tmp), None);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn api_key_quoted_variants() {
        let tmp = std::env::temp_dir().join("mcp_exa_quoted.env");
        std::fs::write(&tmp, "EXA_API_KEY=\"quoted-key\"\n").unwrap();
        assert_eq!(read_api_key(&tmp), Some("quoted-key".to_string()));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn plugin_check_returns_false_for_invalid_json() {
        let tmp = std::env::temp_dir().join("mcp_exa_bad.json");
        std::fs::write(&tmp, "not json").unwrap();
        assert!(!check_plugin_enabled(&tmp));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn plugin_check_returns_true_when_ecc_enabled() {
        let tmp = std::env::temp_dir().join("mcp_exa_settings.json");
        std::fs::write(
            &tmp,
            r#"{"enabledPlugins":{"everything-claude-code:main":true}}"#,
        )
        .unwrap();
        assert!(check_plugin_enabled(&tmp));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn plugin_check_returns_false_when_ecc_disabled() {
        let tmp = std::env::temp_dir().join("mcp_exa_settings2.json");
        std::fs::write(
            &tmp,
            r#"{"enabledPlugins":{"everything-claude-code:main":false}}"#,
        )
        .unwrap();
        assert!(!check_plugin_enabled(&tmp));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn constants_match_shell_script() {
        assert_eq!(INTEGRATION, "mcp-exa");
        assert_eq!(EXA_API_URL, "https://api.exa.ai/search");
        assert_eq!(EXA_PLUGIN_KEY, "everything-claude-code");
    }
}
