use std::env;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ProviderError {
    /// Configuration problem (no key, no config file). Recoverable: ops fix the config.
    Deferred(String),
    /// Network or API error. Same handling — defer.
    Upstream(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Deferred(msg) => write!(f, "provider DEFERRED: {msg}"),
            ProviderError::Upstream(msg) => write!(f, "provider upstream error: {msg}"),
        }
    }
}

impl std::error::Error for ProviderError {}

pub fn hex_root() -> PathBuf {
    env::var("HEX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join("hex")).unwrap_or_else(|_| PathBuf::from("/tmp/hex")))
}

pub fn load_openrouter_key() -> Option<String> {
    if let Ok(k) = env::var("OPENROUTER_API_KEY") {
        if !k.is_empty() {
            return Some(k);
        }
    }
    // Only check the secrets file if HEX_DIR is explicitly set — no hardcoded fallback
    // so that test/verify environments without HEX_DIR correctly return None (Deferred).
    if let Ok(hex_dir) = env::var("HEX_DIR") {
        let env_file = PathBuf::from(&hex_dir).join(".hex/secrets/openrouter.env");
        if let Ok(content) = std::fs::read_to_string(env_file) {
            for line in content.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    if k.trim() == "OPENROUTER_API_KEY" {
                        let val = v.trim().trim_matches('"').to_string();
                        if !val.is_empty() {
                            return Some(val);
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn generate(prompt: &str, model: &str, max_tokens: u32) -> Result<String, ProviderError> {
    let key = load_openrouter_key().ok_or_else(|| {
        ProviderError::Deferred(
            "OPENROUTER_API_KEY not set and .hex/secrets/openrouter.env missing or empty".into(),
        )
    })?;

    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
    });

    let resp = ureq::post("https://openrouter.ai/api/v1/chat/completions")
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| ProviderError::Upstream(format!("openrouter: {e}")))?;

    let json: serde_json::Value = resp
        .into_json()
        .map_err(|e| ProviderError::Upstream(format!("response parse: {e}")))?;

    json["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ProviderError::Upstream(format!("no content in response: {json}")))
}

pub fn health_check() -> Result<String, ProviderError> {
    generate("Respond with a single word: OK", "anthropic/claude-haiku-4.5", 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defer_when_no_key_present() {
        std::env::remove_var("OPENROUTER_API_KEY");
        // Point HEX_DIR at a directory with no secrets file so the file
        // fallback also finds nothing.
        std::env::set_var("HEX_DIR", "/tmp/hex-provider-test-empty-dir");
        let result = generate("hi", "anthropic/claude-sonnet-4.5", 10);
        std::env::remove_var("HEX_DIR");
        match result {
            Err(ProviderError::Deferred(_)) => {}
            _ => panic!("expected Deferred"),
        }
    }
}
