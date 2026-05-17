/// Port of .hex/scripts/route-message-llm.py + context_router/
///
/// Commands:
///   hex route message <text>               - LLM-based message routing
///   hex route comment <id> <asset> <text>  - route comment to matching agents
///   hex route detect-context <message>     - heuristic fingerprint+thermal routing
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_THRESHOLD: f64 = 0.4;
const OPENROUTER_MODEL: &str = "google/gemini-2.5-flash-lite";
const OLLAMA_MODEL: &str = "gemma4:e2b";
const OLLAMA_HOST: &str = "http://localhost:11434";

const SKIP_AGENTS: &[&str] = &[
    "hex-ops",
    "hex-autonomy",
    "sentinel",
    "boi-optimizer",
    "system-arch",
];

struct Charter {
    id: String,
    role: String,
    keywords: Vec<String>,
}

fn load_charters(projects_dir: &Path) -> Vec<Charter> {
    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let mut entries: Vec<_> = match std::fs::read_dir(projects_dir) {
        Ok(e) => e.flatten().collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort_by_key(|e| e.path());

    let mut charters = Vec::new();
    for entry in entries {
        let charter_path = entry.path().join("charter.yaml");
        if !charter_path.is_file() {
            continue;
        }
        let agent_id = entry.file_name().to_string_lossy().to_string();
        if SKIP_AGENTS.contains(&agent_id.as_str()) {
            continue;
        }
        let content = match std::fs::read_to_string(&charter_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let data: serde_yaml::Value = match serde_yaml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = data["role"].as_str().unwrap_or("").to_string();
        let keywords: Vec<String> = data["input_sources"]["keywords"]
            .as_sequence()
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .take(10)
                    .collect()
            })
            .unwrap_or_default();
        charters.push(Charter { id: agent_id, role, keywords });
    }
    charters
}

fn build_prompt(charters: &[Charter], message: &str) -> String {
    let lines: Vec<String> = charters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let kw_part = if c.keywords.is_empty() {
                String::new()
            } else {
                format!(" [signals: {}]", c.keywords.join(", "))
            };
            format!("{}. {}: {}{}", i + 1, c.id, c.role, kw_part)
        })
        .collect();
    format!(
        "Score how relevant this message is to each agent (0.0-1.0). Return ONLY a JSON array.\n\n\
         AGENTS:\n{}\n\nMESSAGE: {}\n\n\
         Return: [{{\"agent\":\"id\", \"c\": 0.85, \"r\": \"reason\"}}]\n\
         Only agents with confidence >= 0.3.",
        lines.join("\n"),
        message
    )
}

fn load_openrouter_key(hex_dir: &Path) -> String {
    let env_path = hex_dir.join(".hex/secrets/openrouter.env");
    if env_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&env_path) {
            for line in content.lines() {
                if let Some(val) = line.strip_prefix("OPENROUTER_API_KEY=") {
                    return val.to_string();
                }
            }
        }
    }
    std::env::var("OPENROUTER_API_KEY").unwrap_or_default()
}

fn call_openrouter(api_key: &str, model: &str, prompt: &str) -> Result<String, String> {
    let payload = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.1,
        "max_tokens": 512,
    })
    .to_string();

    let output = Command::new("curl")
        .args([
            "-s", "-X", "POST",
            "https://openrouter.ai/api/v1/chat/completions",
            "-H", "Content-Type: application/json",
            "-H", &format!("Authorization: Bearer {api_key}"),
            "-d", &payload,
            "--max-time", "15",
        ])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;

    if !output.status.success() {
        return Err(format!("curl exit {}", output.status.code().unwrap_or(1)));
    }
    let resp: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("json parse: {e}"))?;
    Ok(resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

fn call_ollama(host: &str, model: &str, prompt: &str) -> Result<String, String> {
    let payload = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "options": {"temperature": 0.1, "num_predict": 2048},
    })
    .to_string();

    let output = Command::new("curl")
        .args([
            "-s", "-X", "POST",
            &format!("{host}/api/chat"),
            "-H", "Content-Type: application/json",
            "-d", &payload,
            "--max-time", "120",
        ])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;

    if !output.status.success() {
        return Err(format!("curl exit {}", output.status.code().unwrap_or(1)));
    }
    let resp: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("json parse: {e}"))?;
    Ok(resp["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string())
}

fn parse_response(content: &str, charters: &[Charter]) -> Vec<serde_json::Value> {
    let re = match regex::Regex::new(r"(?s)\[.*\]") {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let m = match re.find(content) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let items: Vec<serde_json::Value> = match serde_json::from_str(m.as_str()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let agent_roles: HashMap<&str, &str> =
        charters.iter().map(|c| (c.id.as_str(), c.role.as_str())).collect();

    let mut results: Vec<serde_json::Value> = items
        .iter()
        .filter_map(|item| {
            let agent_id = item["agent"].as_str()?;
            if !agent_roles.contains_key(agent_id) {
                return None;
            }
            let confidence = item
                .get("c")
                .or_else(|| item.get("confidence"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reason = item
                .get("r")
                .or_else(|| item.get("reason"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(serde_json::json!({
                "agent_id": agent_id,
                "role": agent_roles[agent_id],
                "confidence": (confidence * 100.0).round() / 100.0,
                "reason": reason,
            }))
        })
        .collect();

    results.sort_by(|a, b| {
        b["confidence"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["confidence"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

fn route_message_inner(
    hex_dir: &Path,
    message: &str,
    threshold: f64,
    provider: &str,
) -> serde_json::Value {
    let home = std::env::var("HOME").unwrap_or_default();
    let projects_dir = PathBuf::from(&home).join("mrap-hex/projects");
    let charters = load_charters(&projects_dir);

    if charters.is_empty() {
        eprintln!("Warning: no charters loaded from {}", projects_dir.display());
    }

    let prompt = build_prompt(&charters, message);
    let model_env =
        std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| OPENROUTER_MODEL.to_string());
    let ollama_model =
        std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| OLLAMA_MODEL.to_string());
    let ollama_host =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| OLLAMA_HOST.to_string());

    let (content, actual_model, actual_provider) = if provider == "openrouter" {
        let api_key = load_openrouter_key(hex_dir);
        if api_key.is_empty() {
            eprintln!("No OpenRouter API key found");
            return serde_json::json!({
                "message_hash": "",
                "threshold": threshold,
                "model": "error",
                "provider": "error",
                "matches": [],
            });
        }
        match call_openrouter(&api_key, &model_env, &prompt) {
            Ok(c) => (c, model_env, "openrouter".to_string()),
            Err(e) => {
                eprintln!("OpenRouter error: {e}. Falling back to Ollama...");
                match call_ollama(&ollama_host, &ollama_model, &prompt) {
                    Ok(c) => (c, ollama_model, "ollama (fallback)".to_string()),
                    Err(e2) => {
                        eprintln!("Ollama fallback failed: {e2}");
                        return serde_json::json!({
                            "message_hash": "",
                            "threshold": threshold,
                            "model": "error",
                            "provider": "error",
                            "matches": [],
                        });
                    }
                }
            }
        }
    } else {
        match call_ollama(&ollama_host, &ollama_model, &prompt) {
            Ok(c) => (c, ollama_model, "ollama".to_string()),
            Err(e) => {
                eprintln!("Ollama error: {e}");
                return serde_json::json!({
                    "message_hash": "",
                    "threshold": threshold,
                    "model": "error",
                    "provider": "error",
                    "matches": [],
                });
            }
        }
    };

    let all_results = parse_response(&content, &charters);
    let above: Vec<_> = all_results
        .iter()
        .filter(|r| r["confidence"].as_f64().unwrap_or(0.0) >= threshold)
        .cloned()
        .collect();

    let hash = format!("{:x}", Sha256::digest(message.as_bytes()));
    let msg_hash = &hash[..12];

    serde_json::json!({
        "message_hash": msg_hash,
        "threshold": threshold,
        "model": actual_model,
        "provider": actual_provider,
        "matches": above,
    })
}

pub fn run_message(
    hex_dir: &Path,
    message: &str,
    threshold: f64,
    all_flag: bool,
    provider: &str,
) -> i32 {
    let effective_threshold = if all_flag { 0.0 } else { threshold };
    let result = route_message_inner(hex_dir, message, effective_threshold, provider);
    println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    if result["matches"].as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        0
    } else {
        1
    }
}

fn update_comment_via_api(comment_id: &str, status: &str, action: &str, routed_to: &[String]) {
    let payload = serde_json::json!({
        "id": comment_id,
        "status": status,
        "action": action,
        "routed_to": routed_to,
    })
    .to_string();
    let _ = Command::new("curl")
        .args([
            "-s", "-X", "POST",
            "http://127.0.0.1:8901/api/comments/update",
            "-H", "Content-Type: application/json",
            "-d", &payload,
            "--max-time", "10",
        ])
        .output();
}

fn send_agent_message_via_hex(_hex_dir: &Path, agent_id: &str, asset: &str, text: &str) {
    let hex_bin = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("hex"));
    let subject = format!("Comment on {asset}");
    let _ = Command::new(&hex_bin)
        .args([
            "agent", "message", "hex-main", agent_id,
            "--subject", &subject,
            "--body", text,
        ])
        .output();
}

/// Route a comment to matching agents. Replaces subprocess call to route-comment.py.
pub fn run_comment(hex_dir: &Path, comment_id: &str, asset: &str, text: &str) -> i32 {
    let message = format!("Comment on {asset}: {text}");
    let provider =
        std::env::var("ROUTE_PROVIDER").unwrap_or_else(|_| "openrouter".to_string());
    let result = route_message_inner(hex_dir, &message, DEFAULT_THRESHOLD, &provider);
    let matches = result["matches"].as_array().cloned().unwrap_or_default();

    if !matches.is_empty() {
        let agent_ids: Vec<String> = matches
            .iter()
            .filter_map(|m| m["agent_id"].as_str().map(|s| s.to_string()))
            .collect();
        println!("Matched agents: {}", agent_ids.join(", "));
        for agent_id in &agent_ids {
            send_agent_message_via_hex(hex_dir, agent_id, asset, text);
        }
        let action = format!("Routed to: {}", agent_ids.join(", "));
        update_comment_via_api(comment_id, "seen", &action, &agent_ids);
    } else {
        println!("No agent match — updating to general inbox");
        update_comment_via_api(comment_id, "seen", "No agent match — general inbox", &[]);
    }

    0
}

/// Heuristic context routing: keyword fingerprint + correction detection.
/// Port of context_router/router.py + corrections.py.
pub fn run_detect_context(_hex_dir: &Path, message: &str) -> i32 {
    let correction = detect_correction(message);

    let home = std::env::var("HOME").unwrap_or_default();
    let projects_dir = PathBuf::from(&home).join("mrap-hex/projects");
    let charters = load_charters(&projects_dir);

    let message_lower = message.to_lowercase();
    let mut scores: Vec<(String, f64)> = charters
        .iter()
        .filter_map(|c| {
            if c.keywords.is_empty() {
                return None;
            }
            let hits = c
                .keywords
                .iter()
                .filter(|kw| message_lower.contains(kw.to_lowercase().as_str()))
                .count();
            if hits == 0 {
                return None;
            }
            let score = hits as f64 / c.keywords.len() as f64;
            Some((c.id.clone(), score))
        })
        .collect();

    scores.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    });
    scores.truncate(3);

    let result = serde_json::json!({
        "routing_path": "heuristic",
        "correction_type": correction,
        "matches": scores.iter().map(|(id, score)| serde_json::json!({
            "agent_id": id,
            "score": (score * 100.0).round() / 100.0,
        })).collect::<Vec<_>>(),
        "redirect_fired": false,
    });

    println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    if scores.is_empty() { 1 } else { 0 }
}

/// Port of context_router/corrections.py detect_correction().
fn detect_correction(message: &str) -> Option<String> {
    let rules: &[(&str, &str)] = &[
        (r"(?i)\bi\s+meant\b", "wrong_agent"),
        (r"(?i)\bno[,\s]+i\s+was\s+asking\s+about\b", "wrong_agent"),
        (r"(?i)\bnot\s+\w[\w-]*[,]?\s+i\s+meant\b", "wrong_agent"),
        (r"(?i)\bactually\s+the\s+\w[\w-]*\s+should\s+handle\b", "wrong_agent"),
        (r"(?i)\bthat'?s?\s+not\s+what\s+i\s+(said|asked|meant)\b", "wrong_agent"),
        (r"(?i)\bwrong\s+agent\b", "wrong_agent"),
        (r"(?i)\byou\s+(forgot|missed|omitted|skipped)\b", "missing_agent"),
        (r"(?i)\byou\s+didn'?t\s+include\b", "missing_agent"),
        (r"(?i)\balso\s+(ask|check|include|look\s+at)\b", "missing_agent"),
        (
            r"(?i)\bthat'?s?\s+(outdated|old|stale|no\s+longer\s+(true|accurate|correct))\b",
            "stale_context",
        ),
        (r"(?i)\bthat\s+(was|has\s+been)\s+(changed|updated)\b", "stale_context"),
        (r"(?i)\bno\s+longer\s+(correct|true|accurate|valid)\b", "stale_context"),
        (r"(?i)\byou'?re?\s+confusing\b", "domain_bleed"),
        (r"(?i)\bmixing\s+up\b", "domain_bleed"),
        (r"(?i)\bdon'?t\s+mix\b", "domain_bleed"),
        (r"(?i)\bfrom\s+the\s+wrong\s+(domain|context|agent)\b", "domain_bleed"),
    ];
    for (pattern, correction_type) in rules {
        if let Ok(re) = regex::Regex::new(pattern) {
            if re.is_match(message) {
                return Some(correction_type.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_correction_wrong_agent() {
        assert_eq!(
            detect_correction("I meant the other agent"),
            Some("wrong_agent".to_string())
        );
    }

    #[test]
    fn detect_correction_missing_agent() {
        assert_eq!(
            detect_correction("you forgot to include finance"),
            Some("missing_agent".to_string())
        );
    }

    #[test]
    fn detect_correction_none() {
        assert_eq!(detect_correction("Please route this to sales"), None);
    }

    #[test]
    fn detect_correction_domain_bleed() {
        assert_eq!(
            detect_correction("you're confusing the domains"),
            Some("domain_bleed".to_string())
        );
    }

    #[test]
    fn build_prompt_contains_agent_and_message() {
        let charters = vec![Charter {
            id: "sales".to_string(),
            role: "handles sales".to_string(),
            keywords: vec!["revenue".to_string()],
        }];
        let prompt = build_prompt(&charters, "hello world");
        assert!(prompt.contains("sales"));
        assert!(prompt.contains("hello world"));
        assert!(prompt.contains("revenue"));
    }

    #[test]
    fn build_prompt_skips_empty_keywords() {
        let charters = vec![Charter {
            id: "ops".to_string(),
            role: "ops agent".to_string(),
            keywords: vec![],
        }];
        let prompt = build_prompt(&charters, "test");
        assert!(!prompt.contains("[signals:"));
    }
}
