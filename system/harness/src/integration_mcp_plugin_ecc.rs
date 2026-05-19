/// Port of .hex/scripts/integrations/mcp-plugin-ecc.sh
/// Health probe for the ECC plugin (github/memory/context7/sequential-thinking).
use std::process::Command;

const INTEGRATION: &str = "mcp-plugin-ecc";
const ECC_PLUGIN_KEY: &str = "everything-claude-code";
const GITHUB_API_URL: &str = "https://api.github.com/user";

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
        return std::path::PathBuf::from(hex_dir).join(".hex/secrets/mcp-plugin-ecc.env");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home)
        .join("mrap-hex/.hex/secrets/mcp-plugin-ecc.env")
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
        k.contains(ECC_PLUGIN_KEY) && v.as_bool().unwrap_or(false)
    })
}

/// Read GITHUB_TOKEN from the secrets env file.
fn read_github_token(secrets_path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(secrets_path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("GITHUB_TOKEN=") {
            let token = rest.trim_matches('"').trim_matches('\'').trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Light connectivity check — verify GitHub API reachable with the token.
fn check_connectivity(token: &str) -> i32 {
    let output = Command::new("curl")
        .args([
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            GITHUB_API_URL,
            "-H", &format!("Authorization: Bearer {}", token),
            "-H", "X-GitHub-Api-Version: 2022-11-28",
            "--connect-timeout", "5",
            "--max-time", "15",
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
    println!(
        "[{}/probe] checking ECC plugin (github/memory/context7/sequential-thinking)...",
        INTEGRATION
    );
    let mut result = 0;

    // 1. Check plugin:ecc (everything-claude-code) is enabled in Claude settings
    let settings_path = claude_settings_path();
    if settings_path.exists() {
        if check_plugin_enabled(&settings_path) {
            println!(
                "[{}/probe] plugin:ecc (everything-claude-code) is enabled",
                INTEGRATION
            );
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

    // 2. Check GITHUB_TOKEN is set in secrets file
    let secrets_path = secrets_file_path();
    let github_token = if secrets_path.exists() {
        match read_github_token(&secrets_path) {
            Some(t) => {
                println!(
                    "[{}/probe] GITHUB_TOKEN present ({} chars)",
                    INTEGRATION,
                    t.len()
                );
                Some(t)
            }
            None => {
                eprintln!(
                    "[{}/probe] WARN: GITHUB_TOKEN empty in {}",
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

    // 3. Light connectivity check — verify GitHub API reachable (auth check)
    if let Some(ref token) = github_token {
        if which_curl() {
            let http_status = check_connectivity(token);
            match http_status {
                200 => {
                    println!(
                        "[{}/probe] GitHub API: HTTP {} (ok)",
                        INTEGRATION, http_status
                    );
                }
                401 | 403 => {
                    eprintln!(
                        "[{}/probe] WARN: GitHub API: HTTP {} (auth error)",
                        INTEGRATION, http_status
                    );
                    result = 1;
                }
                _ => {
                    eprintln!(
                        "[{}/probe] WARN: GitHub API: HTTP {} (degraded)",
                        INTEGRATION, http_status
                    );
                    result = 1;
                }
            }
        }
    }

    if result == 0 {
        emit_event(
            "hex.integration.mcp-plugin-ecc.probe_ok",
            "ok",
            "plugin enabled + GITHUB_TOKEN present + api ok",
        );
        println!("[{}/probe] OK", INTEGRATION);
    } else {
        emit_event(
            "hex.integration.mcp-plugin-ecc.probe_fail",
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
    fn github_token_parsed_from_env_file() {
        let tmp = std::env::temp_dir().join("mcp_plugin_ecc_test.env");
        std::fs::write(&tmp, "GITHUB_TOKEN=ghp_test123\n").unwrap();
        assert_eq!(read_github_token(&tmp), Some("ghp_test123".to_string()));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn github_token_missing_returns_none() {
        let tmp = std::env::temp_dir().join("mcp_plugin_ecc_empty.env");
        std::fs::write(&tmp, "# no token here\nFOO=bar\n").unwrap();
        assert_eq!(read_github_token(&tmp), None);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn github_token_quoted_variants() {
        let tmp = std::env::temp_dir().join("mcp_plugin_ecc_quoted.env");
        std::fs::write(&tmp, "GITHUB_TOKEN=\"quoted-token\"\n").unwrap();
        assert_eq!(read_github_token(&tmp), Some("quoted-token".to_string()));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn plugin_check_returns_false_for_invalid_json() {
        let tmp = std::env::temp_dir().join("mcp_plugin_ecc_bad.json");
        std::fs::write(&tmp, "not json").unwrap();
        assert!(!check_plugin_enabled(&tmp));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn plugin_check_returns_true_when_ecc_enabled() {
        let tmp = std::env::temp_dir().join("mcp_plugin_ecc_settings.json");
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
        let tmp = std::env::temp_dir().join("mcp_plugin_ecc_settings2.json");
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
        assert_eq!(INTEGRATION, "mcp-plugin-ecc");
        assert_eq!(ECC_PLUGIN_KEY, "everything-claude-code");
        assert_eq!(GITHUB_API_URL, "https://api.github.com/user");
    }
}
