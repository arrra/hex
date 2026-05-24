/// Port of .hex/scripts/integrations/publer.sh
/// Probes the Publer API — verifies the API key is valid and the workspace endpoint is reachable.
use std::process::Command;

const INTEGRATION: &str = "publer";
const API_URL: &str = "https://app.publer.com/api/v1/workspaces";

fn secrets_file_path() -> std::path::PathBuf {
    if let Ok(hex_dir) = std::env::var("HEX_DIR") {
        return std::path::PathBuf::from(hex_dir).join(".hex/secrets/publer.env");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join("hex/.hex/secrets/publer.env")
}
const TIMEOUT: &str = "15";

fn read_api_key(secrets_file: &str) -> Option<String> {
    let text = std::fs::read_to_string(secrets_file).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PUBLER_API_KEY=") {
            let key = rest.trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

fn probe_api(api_key: Option<&str>) -> String {
    let auth_header = format!(
        "Authorization: Bearer-API {}",
        api_key.unwrap_or("")
    );
    let output = Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--max-time",
            TIMEOUT,
            "-H",
            &auth_header,
            API_URL,
        ])
        .output();
    match output {
        Ok(o) => String::from_utf8(o.stdout)
            .ok()
            .unwrap_or_default()
            .trim()
            .to_string(),
        Err(_) => "000".to_string(),
    }
}

pub fn run_probe() -> i32 {
    println!("[{}/probe] checking Publer API...", INTEGRATION);

    let secrets_path = secrets_file_path();
    let api_key = read_api_key(secrets_path.to_str().unwrap_or_default());

    let http_code = probe_api(api_key.as_deref());

    match http_code.as_str() {
        "200" => {
            println!("[{}/probe] OK (HTTP {})", INTEGRATION, http_code);
            0
        }
        "401" | "403" => {
            eprintln!(
                "[{}/probe] FAIL: HTTP {} (bad credentials)",
                INTEGRATION, http_code
            );
            1
        }
        _ => {
            eprintln!("[{}/probe] FAIL: HTTP {}", INTEGRATION, http_code);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_parsed_from_secrets_file() {
        let tmp = std::env::temp_dir().join("publer_test.env");
        std::fs::write(&tmp, "PUBLER_API_KEY=myapikey123\n").unwrap();
        assert_eq!(
            read_api_key(tmp.to_str().unwrap()),
            Some("myapikey123".to_string())
        );
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn api_key_missing_returns_none() {
        let tmp = std::env::temp_dir().join("publer_empty.env");
        std::fs::write(&tmp, "FOO=bar\nBAZ=qux\n").unwrap();
        assert_eq!(read_api_key(tmp.to_str().unwrap()), None);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn api_key_empty_value_returns_none() {
        let tmp = std::env::temp_dir().join("publer_empty_val.env");
        std::fs::write(&tmp, "PUBLER_API_KEY=\n").unwrap();
        assert_eq!(read_api_key(tmp.to_str().unwrap()), None);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn constants_match_shell_script() {
        assert_eq!(INTEGRATION, "publer");
        assert_eq!(API_URL, "https://app.publer.com/api/v1/workspaces");
    }
}
