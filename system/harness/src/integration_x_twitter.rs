/// Port of .hex/scripts/integrations/x-twitter.sh
/// Verifies X API is reachable with the current OAuth2 bearer token.
use std::process::Command;

const INTEGRATION: &str = "x-twitter";
const ENV_FILE: &str = "/Users/mrap/github.com/xdevplatform/xmcp/.env";
const API_URL: &str = "https://api.twitter.com/2/users/me";

fn read_access_token(env_file: &str) -> Option<String> {
    let text = std::fs::read_to_string(env_file).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("X_OAUTH2_ACCESS_TOKEN=") {
            let token = rest.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn check_api(token: &str) -> String {
    let output = Command::new("curl")
        .args([
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            "--max-time", "30",
            "-H", &format!("Authorization: Bearer {}", token),
            API_URL,
        ])
        .output();
    match output {
        Ok(o) => String::from_utf8(o.stdout).ok().unwrap_or_default().trim().to_string(),
        Err(_) => String::new(),
    }
}

pub fn run_probe() -> i32 {
    if !std::path::Path::new(ENV_FILE).exists() {
        eprintln!(
            "[{}/probe] No env file at {} — skipping bearer check",
            INTEGRATION, ENV_FILE
        );
        return 0;
    }

    let token = match read_access_token(ENV_FILE) {
        Some(t) => t,
        None => {
            eprintln!(
                "[{}/probe] X_OAUTH2_ACCESS_TOKEN not set in {}",
                INTEGRATION, ENV_FILE
            );
            return 1;
        }
    };

    let http_code = check_api(&token);
    if http_code == "200" {
        println!("[{}/probe] OK (HTTP {})", INTEGRATION, http_code);
        0
    } else {
        eprintln!("[{}/probe] FAIL (HTTP {})", INTEGRATION, http_code);
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_parsed_from_env_file() {
        let tmp = std::env::temp_dir().join("x_twitter_test.env");
        std::fs::write(&tmp, "X_OAUTH2_ACCESS_TOKEN=mytoken123\n").unwrap();
        assert_eq!(
            read_access_token(tmp.to_str().unwrap()),
            Some("mytoken123".to_string())
        );
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn token_missing_returns_none() {
        let tmp = std::env::temp_dir().join("x_twitter_empty.env");
        std::fs::write(&tmp, "FOO=bar\nBAZ=qux\n").unwrap();
        assert_eq!(read_access_token(tmp.to_str().unwrap()), None);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn token_empty_value_returns_none() {
        let tmp = std::env::temp_dir().join("x_twitter_empty_val.env");
        std::fs::write(&tmp, "X_OAUTH2_ACCESS_TOKEN=\n").unwrap();
        assert_eq!(read_access_token(tmp.to_str().unwrap()), None);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn constants_match_shell_script() {
        assert_eq!(INTEGRATION, "x-twitter");
        assert_eq!(ENV_FILE, "/Users/mrap/github.com/xdevplatform/xmcp/.env");
        assert_eq!(API_URL, "https://api.twitter.com/2/users/me");
    }
}
