use std::collections::HashMap;
use std::io::{Read as IoRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub struct SseBus {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    manifests: Arc<Mutex<HashMap<String, TopicManifest>>>,
}

struct Subscriber {
    id: String,
    topics: Vec<String>,
    sender: std::sync::mpsc::Sender<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TopicManifest {
    pub topic: String,
    pub description: String,
    #[serde(default)]
    pub bridge: Vec<String>,
    #[serde(default)]
    pub events: Vec<EventSchema>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventSchema {
    pub r#type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub payload: HashMap<String, serde_json::Value>,
}

impl SseBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
            manifests: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn load_manifests(&self, topics_dir: &Path) {
        let pattern = topics_dir.join("*.yaml");
        let pattern_str = pattern.to_string_lossy();
        let paths = match glob::glob(&pattern_str) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SSE: failed to glob topics dir: {e}");
                return;
            }
        };
        let mut manifests = self.manifests.lock().unwrap();
        for entry in paths.flatten() {
            let content = match std::fs::read_to_string(&entry) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("SSE: failed to read {:?}: {e}", entry);
                    continue;
                }
            };
            match serde_yaml::from_str::<TopicManifest>(&content) {
                Ok(m) => {
                    manifests.insert(m.topic.clone(), m);
                }
                Err(e) => {
                    eprintln!("SSE: failed to parse {:?}: {e}", entry);
                }
            }
        }
    }

    pub fn subscribe(&self, topics: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let id = Uuid::new_v4().to_string();
        let sub = Subscriber {
            id: id.clone(),
            topics,
            sender: tx,
        };
        self.subscribers.lock().unwrap().push(sub);
        (id, rx)
    }

    pub fn unsubscribe(&self, id: &str) {
        self.subscribers.lock().unwrap().retain(|s| s.id != id);
    }

    pub fn publish(&self, topic: &str, event_type: &str, payload: &serde_json::Value) {
        let msg = serde_json::json!({
            "topic": topic,
            "type": event_type,
            "payload": payload,
        });
        let msg_str = match serde_json::to_string(&msg) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("SSE: serialize failed: {e}");
                return;
            }
        };
        let subs = self.subscribers.lock().unwrap();
        for sub in subs.iter() {
            if sub.topics.iter().any(|t| topic_matches(t, topic)) {
                let _ = sub.sender.send(msg_str.clone());
            }
        }
    }

    pub fn get_manifests(&self) -> HashMap<String, TopicManifest> {
        self.manifests.lock().unwrap().clone()
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }
}

// Returns true if filter matches the given topic.
// Exact: "content.comments" matches "content.comments"
// Wildcard: "content.*" matches anything starting with "content."
// Global: "*" matches everything
fn topic_matches(filter: &str, topic: &str) -> bool {
    if filter == "*" {
        return true;
    }
    if filter == topic {
        return true;
    }
    if let Some(prefix) = filter.strip_suffix(".*") {
        return topic.starts_with(&format!("{prefix}."));
    }
    false
}

/// Bridge a hex-event name to a topic/type using manifest data.
/// Real SSE bridge implementation — no shell-out.
pub fn bridge(hex_dir: &Path, hex_event_name: &str, raw_payload: &str) {
    let bus_url = std::env::var("SSE_BUS_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8880".to_string());

    // Load manifests; try both layout paths.
    let mapping = load_bridge_mapping(hex_dir);

    let info = resolve_bridge_entry(&mapping, hex_event_name).unwrap_or_else(|| {
        eprintln!(
            "warning: no manifest mapping for {:?}, publishing to raw topic",
            hex_event_name
        );
        let parts: Vec<&str> = hex_event_name.splitn(3, '.').collect();
        let topic = if parts.len() >= 2 {
            format!("{}.{}", parts[0], parts[1])
        } else {
            hex_event_name.to_string()
        };
        let event_type = hex_event_name.rsplit('.').next().unwrap_or("unknown").to_string();
        BridgeInfo { topic, event_type }
    });

    let payload_val: serde_json::Value = serde_json::from_str(raw_payload).unwrap_or_else(|e| {
        eprintln!("warning: invalid payload JSON: {e}");
        serde_json::Value::Object(Default::default())
    });

    let body = serde_json::json!({
        "topic": info.topic,
        "type": info.event_type,
        "payload": payload_val,
    })
    .to_string();

    if let Err(e) = http_post_json(&bus_url, "/events/publish", &body) {
        eprintln!("warning: SSE bus unreachable ({bus_url}): {e}");
    } else {
        eprintln!(
            "bridge: {} → {}/{} (ok)",
            hex_event_name, info.topic, info.event_type
        );
    }
}

struct BridgeInfo {
    topic: String,
    event_type: String,
}

fn load_bridge_mapping(hex_dir: &Path) -> HashMap<String, BridgeInfo> {
    // Try system/sse/topics first, then .hex/sse/topics as fallback.
    let candidates = [
        hex_dir.join("system/sse/topics"),
        hex_dir.join(".hex/sse/topics"),
    ];

    let mut mapping: HashMap<String, BridgeInfo> = HashMap::new();

    for dir in &candidates {
        let pattern = dir.join("*.yaml");
        let pattern_str = pattern.to_string_lossy().into_owned();
        let paths = match glob::glob(&pattern_str) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for entry in paths.flatten() {
            let content = match std::fs::read_to_string(&entry) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("warning: failed to load {:?}: {e}", entry);
                    continue;
                }
            };
            let manifest: TopicManifest = match serde_yaml::from_str(&content) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("warning: failed to parse {:?}: {e}", entry);
                    continue;
                }
            };
            let event_types: Vec<String> =
                manifest.events.iter().map(|e| e.r#type.clone()).collect();
            for bridge_entry in &manifest.bridge {
                let event_type = match_bridge_event_type(bridge_entry, &event_types);
                mapping.insert(
                    bridge_entry.clone(),
                    BridgeInfo {
                        topic: manifest.topic.clone(),
                        event_type,
                    },
                );
            }
        }
    }

    mapping
}

fn match_bridge_event_type(hex_event: &str, event_types: &[String]) -> String {
    let suffix = hex_event.rsplit('.').next().unwrap_or(hex_event);
    if event_types.contains(&suffix.to_string()) {
        return suffix.to_string();
    }
    let common: &[(&str, &str)] = &[
        ("created", "created"),
        ("updated", "status_changed"),
        ("woke", "wake_started"),
        ("failed", "wake_failed"),
        ("dispatched", "dispatched"),
        ("completed", "completed"),
        ("registered", "registered"),
        ("removed", "removed"),
    ];
    for (key, val) in common {
        if suffix.contains(key) && event_types.contains(&val.to_string()) {
            return val.to_string();
        }
    }
    event_types.first().cloned().unwrap_or_else(|| suffix.to_string())
}

fn resolve_bridge_entry(
    mapping: &HashMap<String, BridgeInfo>,
    hex_event_name: &str,
) -> Option<BridgeInfo> {
    // Exact match
    if let Some(info) = mapping.get(hex_event_name) {
        return Some(BridgeInfo {
            topic: info.topic.clone(),
            event_type: info.event_type.clone(),
        });
    }
    // Glob wildcard match
    for (pattern, info) in mapping {
        if pattern.contains('*') && glob_matches(pattern, hex_event_name) {
            return Some(BridgeInfo {
                topic: info.topic.clone(),
                event_type: info.event_type.clone(),
            });
        }
    }
    None
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    // Simple fnmatch-style: only supports '*' at end or middle of segments.
    let re_pattern = regex::escape(pattern).replace("\\*", ".*");
    regex::Regex::new(&format!("^{re_pattern}$"))
        .map(|r| r.is_match(value))
        .unwrap_or(false)
}

/// Minimal stdlib-only HTTP POST (HTTP/1.0, no chunked encoding needed).
fn http_post_json(base_url: &str, path: &str, body: &str) -> std::io::Result<()> {
    // Parse host:port from base_url.
    let url = base_url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let (host, port) = if let Some(colon) = url.rfind(':') {
        let h = &url[..colon];
        let p: u16 = url[colon + 1..]
            .trim_end_matches('/')
            .parse()
            .unwrap_or(8880);
        (h.to_string(), p)
    } else {
        (url.trim_end_matches('/').to_string(), 8880u16)
    };

    let addr = format!("{host}:{port}");
    let mut stream = std::net::TcpStream::connect(&addr)?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(4)))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(4)))?;

    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;

    // Read response status line to confirm 2xx.
    let mut resp = String::new();
    let mut buf = [0u8; 512];
    let n = stream.read(&mut buf).unwrap_or(0);
    resp.push_str(&String::from_utf8_lossy(&buf[..n]));
    let status_line = resp.lines().next().unwrap_or("");
    if !status_line.contains("200") && !status_line.contains("201") && !status_line.contains("204") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("HTTP error: {status_line}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(topic_matches("content.comments", "content.comments"));
        assert!(!topic_matches("content.comments", "content.assets"));
    }

    #[test]
    fn wildcard_match() {
        assert!(topic_matches("content.*", "content.comments"));
        assert!(topic_matches("content.*", "content.assets"));
        assert!(!topic_matches("content.*", "system.agents"));
        assert!(!topic_matches("content.*", "content"));
    }

    #[test]
    fn global_match() {
        assert!(topic_matches("*", "content.comments"));
        assert!(topic_matches("*", "system.boi"));
        assert!(topic_matches("*", "anything"));
    }
}
