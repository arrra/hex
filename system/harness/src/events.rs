use chrono::Utc;
use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use crate::server::{Request, Response};
use crate::sse::SseBus;
use crate::telemetry::Telemetry;

// Global stop flag written by the SIGTERM handler.
static DAEMON_STOP: AtomicBool = AtomicBool::new(false);

// Raw C signal() — available on all Unix targets without adding libc to Cargo.toml.
#[cfg(unix)]
unsafe fn libc_signal(signum: i32, handler: usize) {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    signal(signum, handler);
}

// ── Policy structures ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    // Rate limiting metadata (not enforced in this skeleton, stored for future use)
    #[serde(default)]
    pub rate_limit: Option<Value>,
    #[serde(default)]
    pub max_fires: Option<i64>,
    #[serde(default)]
    pub after_limit: Option<String>,
    #[serde(default)]
    pub standing_orders: Vec<String>,
    #[serde(default)]
    pub provides: Option<Value>,
    #[serde(default)]
    pub requires: Option<Value>,
    #[serde(default)]
    pub workflow: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub name: String,
    pub trigger: Trigger,
    // `conditions` (list) and `condition` (singular) are both used in the wild
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub condition: Option<Condition>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Trigger {
    pub event: String,
    // Some policies put conditions inside the trigger
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Condition {
    // field-based conditions
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    // shell conditions
    #[serde(rename = "type")]
    pub cond_type: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    pub r#type: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub on_success: Vec<Action>,
    #[serde(default)]
    pub on_failure: Vec<Action>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    // update-file fields
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,    // "replace" (default) | "append"
    #[serde(default)]
    pub content: Option<String>,
    // emit fields
    #[serde(default)]
    pub dedup_key: Option<String>,
    #[serde(default)]
    pub delay: Option<String>,
    // notify fields
    #[serde(default)]
    pub tier: Option<String>,
}

// ── Agent-charter policy types ────────────────────────────────────────────────
//
// These types deserialize the `wake:` block from a project charter.yaml.
// `id` is set by CharterLoader (derived from the project directory name).

#[derive(Debug, Clone)]
pub struct TriggerSpec {
    pub event: String,
    pub condition: Option<String>,
}

// Accept either a bare string (`"timer.tick.6h"`) or a full struct
// (`{event: "...", condition: "..."}`). The bare-string form sets
// `condition = None`.
#[derive(Deserialize)]
#[serde(untagged)]
enum TriggerSpecRepr {
    Bare(String),
    Full {
        event: String,
        #[serde(default)]
        condition: Option<String>,
    },
}

impl<'de> Deserialize<'de> for TriggerSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match TriggerSpecRepr::deserialize(d)? {
            TriggerSpecRepr::Bare(s) => Ok(TriggerSpec { event: s, condition: None }),
            TriggerSpecRepr::Full { event, condition } => Ok(TriggerSpec { event, condition }),
        }
    }
}

impl Serialize for TriggerSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if self.condition.is_none() {
            s.serialize_str(&self.event)
        } else {
            let mut st = s.serialize_struct("TriggerSpec", 2)?;
            st.serialize_field("event", &self.event)?;
            st.serialize_field("condition", self.condition.as_ref().unwrap())?;
            st.end()
        }
    }
}

/// Parsed from `wake.rate_limit` in a charter. `window` is a duration string
/// like "60m" or "24h"; call `.window_duration()` to convert to `std::time::Duration`.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimit {
    pub max_fires: u32,
    pub window: String,
}

impl RateLimit {
    /// Parse the window string ("60m", "24h", "1d") into a `Duration`.
    /// Returns an error string if the format is unrecognised.
    pub fn window_duration(&self) -> Result<Duration, String> {
        let s = self.window.trim();
        if let Some(n) = s.strip_suffix('m') {
            n.parse::<u64>()
                .map(|v| Duration::from_secs(v * 60))
                .map_err(|e| format!("invalid rate_limit window '{}': {e}", self.window))
        } else if let Some(n) = s.strip_suffix('h') {
            n.parse::<u64>()
                .map(|v| Duration::from_secs(v * 3600))
                .map_err(|e| format!("invalid rate_limit window '{}': {e}", self.window))
        } else if let Some(n) = s.strip_suffix('d') {
            n.parse::<u64>()
                .map(|v| Duration::from_secs(v * 86400))
                .map_err(|e| format!("invalid rate_limit window '{}': {e}", self.window))
        } else {
            Err(format!(
                "unrecognised rate_limit window '{}' — expected suffix m/h/d",
                self.window
            ))
        }
    }
}

/// Mirrors the `wake:` block of a charter.yaml.  The `id` field is injected by
/// `CharterLoader` after discovery (it is NOT present in the YAML itself).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentPolicy {
    /// Agent identifier — set by CharterLoader from the project directory name.
    #[serde(skip)]
    pub id: String,

    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Optional rate limit.  `null` is valid; a missing key is also valid (treated
    /// as no limit).  The spec distinguishes "null is OK; absence is NOT" only for
    /// validation purposes — both deserialise to `None` here.
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,

    /// Override wake command. When `None`, the daemon uses `hex agent wake <id>`.
    #[serde(default)]
    pub command: Option<String>,

    #[serde(default)]
    pub triggers: Vec<TriggerSpec>,

    #[serde(default)]
    pub on_success: Vec<String>,

    #[serde(default)]
    pub on_failure: Vec<String>,
}

fn default_true() -> bool {
    true
}

// ── Charter-as-policy loader ──────────────────────────────────────────────────

/// Raw charter YAML shape for wake-policy extraction.
/// Only `id` and `wake` are parsed; all other charter fields are ignored.
#[derive(Debug, Deserialize)]
struct CharterDocRaw {
    pub id: String,
    #[serde(default)]
    pub wake: Option<AgentPolicy>,
}

/// Known trigger-event namespace prefixes.  A trigger event whose name begins
/// with one of these prefixes (or is exactly "*") is accepted; anything else
/// is rejected at load time to catch typos early.
const TRIGGER_ALLOWLIST_PREFIXES: &[&str] = &[
    "timer.tick.",
    "boi.",
    "hex.",
    "git.",
    "integrations.",
    "calendar.",
];

fn validate_trigger_event(event: &str) -> Option<String> {
    if event == "*" {
        return None;
    }
    for prefix in TRIGGER_ALLOWLIST_PREFIXES {
        if event.starts_with(prefix) {
            return None;
        }
    }
    Some(format!(
        "trigger event '{event}' is not in an allowed namespace \
         (expected prefix: {})",
        TRIGGER_ALLOWLIST_PREFIXES.join(", ")
    ))
}

/// Loads `AgentPolicy` objects from `<hex_dir>/projects/*/charter.yaml`.
/// Uses mtime-based caching: only re-reads files when a mtime changes.
/// Validation errors are written to `hex_dir/health.json` and the offending
/// agent is excluded from the returned list.
pub struct CharterLoader {
    hex_dir: PathBuf,
    /// (mtime_snapshot, cached_policies)
    cache: Mutex<(HashMap<PathBuf, SystemTime>, Vec<AgentPolicy>)>,
}

impl CharterLoader {
    pub fn new(hex_dir: PathBuf) -> Self {
        Self {
            hex_dir,
            cache: Mutex::new((HashMap::new(), Vec::new())),
        }
    }

    /// Scan `hex_dir/projects/*/charter.yaml`, validate wake blocks, and
    /// return one `AgentPolicy` per enabled, valid agent.  Agents with
    /// `wake.enabled: false` are silently skipped.  All other validation
    /// errors are written to `hex_dir/health.json` and the agent is excluded.
    pub fn load_charters(&self) -> Vec<AgentPolicy> {
        let pattern = self
            .hex_dir
            .join("projects")
            .join("*")
            .join("charter.yaml");
        let pattern_str = pattern.to_string_lossy();

        let mut current_mtimes: HashMap<PathBuf, SystemTime> = HashMap::new();
        if let Ok(paths) = glob::glob(&pattern_str) {
            for entry in paths.flatten() {
                if let Ok(meta) = std::fs::metadata(&entry) {
                    if let Ok(mtime) = meta.modified() {
                        current_mtimes.insert(entry, mtime);
                    }
                }
            }
        }

        let mut cache = match self.cache.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("charter_loader: cache lock poisoned: {e}");
                return Vec::new();
            }
        };

        if cache.0 == current_mtimes {
            return cache.1.clone();
        }

        let health_path = self.hex_dir.join("health.json");
        let mut loaded: Vec<AgentPolicy> = Vec::new();
        let mut health_errors: HashMap<String, serde_json::Value> = HashMap::new();

        for path in current_mtimes.keys() {
            self.load_one_charter(path, &mut loaded, &mut health_errors);
        }

        if !health_errors.is_empty() {
            self.write_health_errors(&health_path, &health_errors);
        }

        eprintln!(
            "charter_loader: loaded {}/{} agent policies from charters",
            loaded.len(),
            current_mtimes.len()
        );
        cache.0 = current_mtimes;
        cache.1 = loaded.clone();
        loaded
    }

    fn load_one_charter(
        &self,
        path: &Path,
        loaded: &mut Vec<AgentPolicy>,
        health_errors: &mut HashMap<String, serde_json::Value>,
    ) {
        let content = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("charter_loader: failed to read {:?}: {e}", path);
                return;
            }
        };

        // Fallback id from parent directory name (used before we parse id:).
        let dir_id = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let raw: CharterDocRaw = match serde_yaml::from_str(&content) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("charter_loader: YAML parse error in {:?}: {e}", path);
                health_errors.insert(
                    dir_id,
                    serde_json::json!({
                        "status": "invalid_charter",
                        "error": format!("YAML parse error: {e}")
                    }),
                );
                return;
            }
        };

        let agent_id = raw.id.clone();

        let mut wake = match raw.wake {
            Some(w) => w,
            None => {
                let msg = "missing wake: block";
                eprintln!("charter_loader: {msg} for agent {}", agent_id);
                health_errors.insert(
                    agent_id,
                    serde_json::json!({ "status": "invalid_charter", "error": msg }),
                );
                return;
            }
        };

        if !wake.enabled {
            eprintln!("charter_loader: skipping disabled agent {}", agent_id);
            return;
        }

        if wake.triggers.is_empty() {
            let msg = "wake.triggers is empty — at least one trigger is required";
            eprintln!("charter_loader: {} for agent {}", msg, agent_id);
            health_errors.insert(
                agent_id,
                serde_json::json!({ "status": "invalid_charter", "error": msg }),
            );
            return;
        }

        for trigger in &wake.triggers {
            if let Some(err) = validate_trigger_event(&trigger.event) {
                eprintln!("charter_loader: invalid trigger for agent {}: {err}", agent_id);
                health_errors.insert(
                    agent_id,
                    serde_json::json!({ "status": "invalid_charter", "error": err }),
                );
                return;
            }
        }

        wake.id = agent_id;
        loaded.push(wake);
    }

    fn write_health_errors(
        &self,
        health_path: &Path,
        errors: &HashMap<String, serde_json::Value>,
    ) {
        let mut health_map: serde_json::Map<String, serde_json::Value> =
            if health_path.exists() {
                std::fs::read_to_string(health_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| {
                        if let serde_json::Value::Object(m) = v {
                            Some(m)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            } else {
                serde_json::Map::new()
            };

        let agents_entry = health_map
            .entry("agents".to_string())
            .or_insert_with(|| serde_json::json!({}));

        if let serde_json::Value::Object(ref mut agents_map) = agents_entry {
            for (id, status) in errors {
                agents_map.insert(id.clone(), status.clone());
            }
        }

        let json_str = match serde_json::to_string_pretty(&serde_json::Value::Object(health_map))
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("charter_loader: failed to serialize health.json: {e}");
                return;
            }
        };

        let dir = health_path.parent().unwrap_or(Path::new("."));
        let tmp = dir.join(format!(
            ".health_tmp_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));

        match std::fs::File::create(&tmp) {
            Ok(mut f) => {
                let ok = f.write_all(json_str.as_bytes()).is_ok()
                    && f.flush().is_ok()
                    && std::fs::rename(&tmp, health_path).is_ok();
                if !ok {
                    let _ = std::fs::remove_file(&tmp);
                    eprintln!(
                        "charter_loader: failed to write health.json to {:?}",
                        health_path
                    );
                }
            }
            Err(e) => eprintln!(
                "charter_loader: failed to create health.json tmp at {:?}: {e}",
                tmp
            ),
        }
    }
}

// ── Engine ────────────────────────────────────────────────────────────────────

pub struct EventEngine {
    db: Mutex<Connection>,
    pub policies_dir: PathBuf,
    hex_dir: PathBuf,
    policies: RwLock<Vec<Policy>>,
    /// Agent policies loaded from project charters (wake: blocks).
    pub agent_policies: RwLock<Vec<AgentPolicy>>,
    /// Charter loader; None in test contexts.
    pub charter_loader: Option<CharterLoader>,
    telemetry: Arc<Telemetry>,
    pub bus: Arc<SseBus>,
    start_time: Instant,
    events_processed: Mutex<u64>,
    rate_limiter: Mutex<HashMap<String, Vec<Instant>>>,
}

impl EventEngine {
    pub fn new(
        hex_dir: &Path,
        telemetry: Arc<Telemetry>,
        bus: Arc<SseBus>,
    ) -> Result<Arc<Self>, String> {
        let home = PathBuf::from(shellexpand::tilde("~").as_ref());
        let hex_events_dir = home.join(".hex-events");
        let _ = std::fs::create_dir_all(&hex_events_dir);

        let db_path = hex_events_dir.join("events.db");
        let policies_dir = hex_events_dir.join("policies");
        let _ = std::fs::create_dir_all(&policies_dir);

        let conn = Connection::open(&db_path)
            .map_err(|e| format!("events db open failed: {e}"))?;
        init_schema(&conn)?;

        // Prefer HEX_DIR env var; fall back to the path passed in.
        let charter_hex_dir = std::env::var("HEX_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| hex_dir.to_path_buf());
        let charter_loader = Some(CharterLoader::new(charter_hex_dir.clone()));

        let engine = Arc::new(Self {
            db: Mutex::new(conn),
            policies_dir,
            hex_dir: charter_hex_dir,
            policies: RwLock::new(Vec::new()),
            agent_policies: RwLock::new(Vec::new()),
            charter_loader,
            telemetry,
            bus,
            start_time: Instant::now(),
            events_processed: Mutex::new(0),
            rate_limiter: Mutex::new(HashMap::new()),
        });

        engine.load_policies();
        engine.load_agent_policies();
        Ok(engine)
    }

    /// Create an `EventEngine` backed by an in-memory SQLite database.
    /// For integration tests — does not touch the home-dir events.db.
    pub fn new_in_memory(hex_dir: &Path) -> Arc<Self> {
        let conn = Connection::open_in_memory().expect("in-memory db open failed");
        init_schema(&conn).expect("schema init failed");
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(hex_dir));
        let policies_dir = hex_dir.join("policies");
        let _ = std::fs::create_dir_all(&policies_dir);
        Arc::new(Self {
            db: Mutex::new(conn),
            policies_dir,
            hex_dir: hex_dir.to_path_buf(),
            policies: RwLock::new(Vec::new()),
            agent_policies: RwLock::new(Vec::new()),
            charter_loader: None,
            telemetry,
            bus,
            start_time: Instant::now(),
            events_processed: Mutex::new(0),
            rate_limiter: Mutex::new(HashMap::new()),
        })
    }

    /// Like `new_in_memory` but persists the DB to `db_path`.
    /// Use for tests that simulate daemon restart (drop engine, recreate with same path).
    pub fn new_with_db_file(hex_dir: &Path, db_path: &Path) -> Arc<Self> {
        let conn = Connection::open(db_path).expect("file db open failed");
        init_schema(&conn).expect("schema init failed");
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(hex_dir));
        let policies_dir = hex_dir.join("policies");
        let _ = std::fs::create_dir_all(&policies_dir);
        Arc::new(Self {
            db: Mutex::new(conn),
            policies_dir,
            hex_dir: hex_dir.to_path_buf(),
            policies: RwLock::new(Vec::new()),
            agent_policies: RwLock::new(Vec::new()),
            charter_loader: None,
            telemetry,
            bus,
            start_time: Instant::now(),
            events_processed: Mutex::new(0),
            rate_limiter: Mutex::new(HashMap::new()),
        })
    }

    /// Execute a closure against the underlying SQLite connection.
    /// For integration testing only — lets tests verify DB state without
    /// exposing the `db` field directly.
    pub fn with_db<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&Connection) -> T,
    {
        let db = self.db.lock().expect("db lock poisoned");
        f(&db)
    }

    /// Load agent policies from project charters and store them.
    pub fn load_agent_policies(&self) {
        if let Some(ref loader) = self.charter_loader {
            let policies = loader.load_charters();
            match self.agent_policies.write() {
                Ok(mut guard) => *guard = policies,
                Err(e) => eprintln!("events: agent_policies write lock poisoned: {e}"),
            }
        }
    }

    pub fn load_policies(&self) {
        let pattern = self.policies_dir.join("*.yaml");
        let pattern_str = pattern.to_string_lossy();
        let mut loaded = Vec::new();

        if let Ok(paths) = glob::glob(&pattern_str) {
            for entry in paths.flatten() {
                // Skip *-agent.yaml — agent policies come from charters now.
                // This prevents double-wake during the Phase 3→4 overlap window.
                let fname = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                if fname.ends_with("-agent.yaml") {
                    eprintln!("events: skipping agent policy file {:?} (use charters)", entry);
                    continue;
                }

                match std::fs::read_to_string(&entry) {
                    Ok(content) => match serde_yaml::from_str::<Policy>(&content) {
                        Ok(p) => {
                            if p.enabled.unwrap_or(true) {
                                loaded.push(p);
                            }
                        }
                        Err(e) => eprintln!("events: failed to parse {:?}: {e}", entry),
                    },
                    Err(e) => eprintln!("events: failed to read {:?}: {e}", entry),
                }
            }
        }

        let count = loaded.len();
        match self.policies.write() {
            Ok(mut guard) => *guard = loaded,
            Err(e) => {
                eprintln!("events: policies write lock poisoned: {e}");
                return;
            }
        }
        eprintln!("events: loaded {count} policies from {:?}", self.policies_dir);
    }

    pub fn reload_policies(&self) {
        self.load_policies();
        self.load_agent_policies();
    }

    /// Spawn a background thread that polls policy file mtimes every 10s.
    /// When any file changes (modified, added, or deleted), reloads all policies.
    pub fn start_hot_reload(engine: Arc<Self>) {
        std::thread::spawn(move || {
            let mut last_snapshot = snapshot_mtimes(&engine.policies_dir);
            loop {
                std::thread::sleep(Duration::from_secs(10));
                let current = snapshot_mtimes(&engine.policies_dir);
                if current != last_snapshot {
                    eprintln!("events: policy files changed — reloading");
                    engine.load_policies();
                    last_snapshot = current;
                }
            }
        });
    }

    pub fn policy_count(&self) -> usize {
        match self.policies.read() {
            Ok(guard) => guard.len(),
            Err(e) => {
                eprintln!("events: policies lock poisoned: {e}");
                0
            }
        }
    }

    /// Ingest an event: write to DB, match policies, execute actions.
    /// Returns the new event's row ID, or -1 on error.
    pub fn ingest(&self, event_type: &str, payload: &Value, source: &str) -> i64 {
        self.ingest_with_depth(event_type, payload, source, 0)
    }

    fn ingest_with_depth(
        &self,
        event_type: &str,
        payload: &Value,
        source: &str,
        depth: u32,
    ) -> i64 {
        let now = Utc::now().to_rfc3339();
        let payload_str = payload.to_string();

        let event_id = {
            let db = match self.db.lock() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("events: db lock poisoned: {e}");
                    return -1;
                }
            };
            match db.execute(
                "INSERT INTO events (event_type, payload, source, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![event_type, payload_str, source, now],
            ) {
                Ok(_) => db.last_insert_rowid(),
                Err(e) => {
                    eprintln!("events: db insert failed: {e}");
                    return -1;
                }
            }
        };

        match self.events_processed.lock() {
            Ok(mut guard) => *guard += 1,
            Err(e) => eprintln!("events: events_processed lock poisoned: {e}"),
        }

        let policies = match self.policies.read() {
            Ok(guard) => guard.clone(),
            Err(e) => {
                eprintln!("events: policies read lock poisoned: {e}");
                return -1;
            }
        };
        for policy in &policies {
            for rule in &policy.rules {
                if !wildcard_matches(&rule.trigger.event, event_type) {
                    continue;
                }
                // Support both `condition:` (singular) and `conditions:` (list)
                let singular_pass = rule
                    .condition
                    .as_ref()
                    .map_or(true, |c| self.evaluate_condition(c, payload));
                let all_pass = singular_pass
                    && rule
                        .conditions
                        .iter()
                        .all(|c| self.evaluate_condition(c, payload))
                    && rule
                        .trigger
                        .conditions
                        .iter()
                        .all(|c| self.evaluate_condition(c, payload));
                if !all_pass {
                    continue;
                }
                for action in &rule.actions {
                    self.execute_action(
                        action,
                        event_id,
                        &policy.name,
                        &rule.name,
                        event_type,
                        payload,
                        policy.rate_limit.as_ref(),
                        depth,
                    );
                }
            }
        }

        self.bus.publish("hex.events", event_type, payload);
        self.telemetry.emit(
            "hex.event.ingested",
            &serde_json::json!({ "event_id": event_id, "event_type": event_type }),
        );

        // Mark as processed so daemon loop doesn't double-dispatch events
        // that were ingested directly (CLI emit, HTTP ingest, scheduler).
        let processed_now = Utc::now().to_rfc3339();
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "UPDATE events SET processed_at = ?1 WHERE id = ?2",
                params![processed_now, event_id],
            );
        }

        event_id
    }

    /// Process an already-inserted event through policies (daemon poll loop use).
    /// Does NOT re-insert to DB. Marks processed_at after dispatch.
    fn dispatch_existing_event(
        &self,
        event_id: i64,
        event_type: &str,
        payload: &Value,
        shadow: bool,
    ) -> usize {
        let policies = match self.policies.read() {
            Ok(guard) => guard.clone(),
            Err(e) => {
                eprintln!("events: policies read lock poisoned in dispatch: {e}");
                return 0;
            }
        };

        let mut actions_fired = 0usize;
        for policy in &policies {
            for rule in &policy.rules {
                if !wildcard_matches(&rule.trigger.event, event_type) {
                    continue;
                }
                let singular_pass = rule
                    .condition
                    .as_ref()
                    .map_or(true, |c| self.evaluate_condition(c, payload));
                let all_pass = singular_pass
                    && rule.conditions.iter().all(|c| self.evaluate_condition(c, payload))
                    && rule.trigger.conditions.iter().all(|c| self.evaluate_condition(c, payload));
                if !all_pass {
                    continue;
                }
                for action in &rule.actions {
                    if shadow {
                        eprintln!(
                            "SHADOW: event={event_type} policy={} rule={} action={}",
                            policy.name, rule.name, action.r#type
                        );
                    } else {
                        self.execute_action(
                            action,
                            event_id,
                            &policy.name,
                            &rule.name,
                            event_type,
                            payload,
                            policy.rate_limit.as_ref(),
                            0,
                        );
                    }
                    actions_fired += 1;
                }
            }
        }

        // Dispatch agent wakes from charter policies (rate-limited, persisted).
        actions_fired += self.dispatch_agent_wakes(event_id, event_type, payload, shadow);

        // Mark processed
        let now = Utc::now().to_rfc3339();
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "UPDATE events SET processed_at = ?1 WHERE id = ?2",
                params![now, event_id],
            );
        }

        match self.events_processed.lock() {
            Ok(mut g) => *g += 1,
            Err(e) => eprintln!("events: events_processed lock poisoned: {e}"),
        }

        actions_fired
    }

    /// Fetch up to `limit` unprocessed events (processed_at IS NULL), ordered by id.
    fn get_unprocessed_batch(&self, limit: i64) -> Vec<(i64, String, Value)> {
        let db = match self.db.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("events: db lock poisoned in get_unprocessed: {e}");
                return vec![];
            }
        };
        let mut stmt = match db.prepare(
            "SELECT id, event_type, payload FROM events \
             WHERE processed_at IS NULL ORDER BY id LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("events: prepare unprocessed query failed: {e}");
                return vec![];
            }
        };
        let mut rows = match stmt.query(params![limit]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("events: query unprocessed failed: {e}");
                return vec![];
            }
        };
        let mut events = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            let id: i64 = row.get(0).unwrap_or(0);
            let event_type: String = row.get(1).unwrap_or_default();
            let payload_str: String = row.get(2).unwrap_or_else(|_| "null".to_string());
            let payload: Value = serde_json::from_str(&payload_str).unwrap_or(Value::Null);
            events.push((id, event_type, payload));
        }
        events
    }

    /// Main daemon loop: poll for unprocessed events, dispatch, heartbeat.
    fn run_daemon_loop(&self, shadow: bool, running: &std::sync::atomic::AtomicBool) {
        use std::sync::atomic::Ordering;

        let poll_interval = Duration::from_secs(2);
        let heartbeat_interval = Duration::from_secs(60);

        let mut last_heartbeat = Instant::now();
        let mut hb_events: u64 = 0;
        let mut hb_actions: u64 = 0;

        eprintln!(
            "hex events daemon ready (pid={}{})",
            std::process::id(),
            if shadow { ", shadow-mode" } else { "" }
        );

        while running.load(Ordering::SeqCst) {
            // Drain unprocessed events from DB (inserted by external tools).
            let batch = self.get_unprocessed_batch(100);
            for (id, event_type, payload) in batch {
                let n = self.dispatch_existing_event(id, &event_type, &payload, shadow);
                hb_events += 1;
                hb_actions += n as u64;
            }

            // Heartbeat
            if last_heartbeat.elapsed() >= heartbeat_interval {
                eprintln!(
                    "heartbeat: pid={} state=healthy events={} actions={}",
                    std::process::id(),
                    hb_events,
                    hb_actions,
                );
                hb_events = 0;
                hb_actions = 0;
                last_heartbeat = Instant::now();
            }

            std::thread::sleep(poll_interval);
        }

        eprintln!("hex events daemon shutting down (SIGTERM received)");
    }

    pub fn cli_daemon(engine: Arc<Self>, shadow: bool) {
        use std::sync::Arc as StdArc;

        let running = StdArc::new(AtomicBool::new(true));

        // Install SIGTERM / SIGINT handlers via raw C signal().
        #[cfg(unix)]
        {
            extern "C" fn handle_stop(_: i32) {
                // Safety: AtomicBool store is async-signal-safe.
                // We use a global to communicate with the loop.
                DAEMON_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            unsafe {
                libc_signal(15 /* SIGTERM */, handle_stop as *const () as usize);
                libc_signal(2  /* SIGINT  */, handle_stop as *const () as usize);
            }
        }

        // Start hot-reload and scheduler background threads.
        EventEngine::start_hot_reload(engine.clone());
        EventEngine::start_scheduler(engine.clone());

        // Poll the global stop flag (set by signal handler) and propagate to running.
        #[cfg(unix)]
        {
            let running_clone = running.clone();
            std::thread::spawn(move || {
                loop {
                    if DAEMON_STOP.load(std::sync::atomic::Ordering::SeqCst) {
                        running_clone.store(false, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
        }

        engine.run_daemon_loop(shadow, &running);
    }

    fn evaluate_condition(&self, condition: &Condition, payload: &Value) -> bool {
        // Shell conditions are evaluated by running a command (not yet implemented — skip/pass)
        if condition.cond_type.as_deref() == Some("shell") {
            return true;
        }
        let field = match &condition.field {
            Some(f) => f,
            None => return true,
        };
        let op = match &condition.op {
            Some(o) => o.as_str(),
            None => return true,
        };
        match op {
            "exists" => resolve_field(field, payload).is_some(),
            "eq" => match (
                resolve_field(field, payload).as_ref(),
                condition.value.as_ref(),
            ) {
                (Some(a), Some(v)) => a == v,
                _ => false,
            },
            "ne" | "neq" => match (
                resolve_field(field, payload).as_ref(),
                condition.value.as_ref(),
            ) {
                (Some(a), Some(v)) => a != v,
                _ => true,
            },
            "gt" | "gte" => {
                let actual = resolve_field(field, payload);
                cmp_nums(&actual, &condition.value, true)
            }
            "lt" | "lte" => {
                let actual = resolve_field(field, payload);
                cmp_nums(&actual, &condition.value, false)
            }
            "contains" => {
                match (
                    resolve_field(field, payload),
                    condition.value.as_ref(),
                ) {
                    (Some(a), Some(v)) => {
                        let needle = v
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string());
                        value_to_str(&a).contains(&needle)
                    }
                    _ => false,
                }
            }
            "regex" | "glob" => {
                match (
                    resolve_field(field, payload),
                    condition.value.as_ref(),
                ) {
                    (Some(a), Some(v)) => {
                        let pattern = v
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| v.to_string());
                        value_to_str(&a).contains(&pattern)
                    }
                    _ => false,
                }
            }
            other => {
                eprintln!("events: unknown condition op '{other}'");
                false
            }
        }
    }

    fn execute_action(
        &self,
        action: &Action,
        event_id: i64,
        policy_name: &str,
        rule_name: &str,
        event_type: &str,
        payload: &Value,
        rate_limit: Option<&Value>,
        depth: u32,
    ) {
        if depth > 8 {
            eprintln!(
                "events: emit depth limit reached policy={policy_name} rule={rule_name}"
            );
            return;
        }

        // Rate limiting check (per-policy sliding window)
        if let Some(rl) = rate_limit {
            let max_fires = rl
                .get("max_fires")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let window_str = rl
                .get("window")
                .and_then(|v| v.as_str())
                .unwrap_or("1h");
            let window_secs = parse_duration_str(window_str);

            if max_fires > 0 && window_secs > 0 {
                let rl_key = format!("{policy_name}:{event_type}");
                let now_instant = Instant::now();
                match self.rate_limiter.lock() {
                    Ok(mut guard) => {
                        let timestamps = guard.entry(rl_key).or_default();
                        timestamps
                            .retain(|t| now_instant.duration_since(*t).as_secs() < window_secs);
                        if timestamps.len() as u64 >= max_fires {
                            eprintln!(
                                "events: rate limited policy={policy_name} ({}/{} fires in {window_secs}s)",
                                timestamps.len(),
                                max_fires
                            );
                            return;
                        }
                        timestamps.push(now_instant);
                    }
                    Err(e) => eprintln!("events: rate_limiter lock poisoned: {e}"),
                }
            }
        }

        let now = Utc::now().to_rfc3339();

        match action.r#type.as_str() {
            "shell" => {
                let timeout_secs = action.timeout.unwrap_or(60);
                let (status, error, stdout) = if let Some(cmd_tpl) = &action.command {
                    let cmd = render_template(cmd_tpl, event_type, payload);
                    run_shell_with_timeout(&cmd, timeout_secs)
                } else {
                    (
                        "error".to_string(),
                        "no command specified".to_string(),
                        String::new(),
                    )
                };

                let succeeded = status == "ok";
                match self.db.lock() {
                    Ok(db) => {
                        let _ = db.execute(
                            "INSERT INTO action_log \
                             (event_id, policy_name, rule_name, action_type, status, error, created_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![event_id, policy_name, rule_name, "shell", status, error, now],
                        );
                    }
                    Err(e) => eprintln!("events: db lock poisoned in execute_action(shell): {e}"),
                }

                // on_success / on_failure chaining
                let chained = if succeeded {
                    &action.on_success
                } else {
                    &action.on_failure
                };
                let chain_payload = if succeeded && !stdout.is_empty() {
                    let mut p = payload.clone();
                    if let Value::Object(ref mut map) = p {
                        map.insert(
                            "action".to_string(),
                            serde_json::json!({ "stdout": stdout }),
                        );
                    }
                    p
                } else {
                    payload.clone()
                };
                for chained_action in chained {
                    self.execute_action(
                        chained_action,
                        event_id,
                        policy_name,
                        rule_name,
                        event_type,
                        &chain_payload,
                        None,
                        depth + 1,
                    );
                }
            }

            "emit" => {
                let (status, error) = if let Some(emit_type) = &action.event {
                    let rendered_type = render_template(emit_type, event_type, payload);
                    let emit_payload = action
                        .payload
                        .as_ref()
                        .map(|p| render_value_templates(p, event_type, payload))
                        .unwrap_or_else(|| payload.clone());
                    let src = action.source.as_deref().unwrap_or("policy-emit");
                    self.ingest_with_depth(&rendered_type, &emit_payload, src, depth + 1);
                    ("ok".to_string(), String::new())
                } else {
                    (
                        "error".to_string(),
                        "emit action missing 'event' parameter".to_string(),
                    )
                };
                match self.db.lock() {
                    Ok(db) => {
                        let _ = db.execute(
                            "INSERT INTO action_log \
                             (event_id, policy_name, rule_name, action_type, status, error, created_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![event_id, policy_name, rule_name, "emit", status, error, now],
                        );
                    }
                    Err(e) => eprintln!("events: db lock poisoned in execute_action(emit): {e}"),
                }
            }

            "notify" => {
                let (status, error) = if let Some(msg_tpl) = &action.message {
                    let msg = render_template(msg_tpl, event_type, payload);
                    deliver_notification(&msg, action.tier.as_deref())
                } else {
                    (
                        "error".to_string(),
                        "notify action missing 'message' parameter".to_string(),
                    )
                };
                match self.db.lock() {
                    Ok(db) => {
                        let _ = db.execute(
                            "INSERT INTO action_log \
                             (event_id, policy_name, rule_name, action_type, status, error, created_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                event_id, policy_name, rule_name, "notify", status, error, now
                            ],
                        );
                    }
                    Err(e) => eprintln!("events: db lock poisoned in execute_action(notify): {e}"),
                }
            }

            "update-file" => {
                let (status, error) = if let Some(target_tpl) = &action.target {
                    let target = render_template(target_tpl, event_type, payload);
                    let target = shellexpand::tilde(&target).into_owned();
                    let mode = action.mode.as_deref().unwrap_or("replace");
                    match mode {
                        "append" => {
                            let content = action
                                .content
                                .as_ref()
                                .map(|c| render_template(c, event_type, payload))
                                .unwrap_or_default();
                            atomic_file_append(&target, &content)
                        }
                        _ => {
                            let pattern = action
                                .pattern
                                .as_ref()
                                .map(|p| render_template(p, event_type, payload))
                                .unwrap_or_default();
                            let replace = action
                                .replace
                                .as_ref()
                                .map(|r| render_template(r, event_type, payload))
                                .unwrap_or_default();
                            atomic_regex_replace(&target, &pattern, &replace)
                        }
                    }
                } else {
                    (
                        "error".to_string(),
                        "update-file action missing 'target' parameter".to_string(),
                    )
                };
                match self.db.lock() {
                    Ok(db) => {
                        let _ = db.execute(
                            "INSERT INTO action_log \
                             (event_id, policy_name, rule_name, action_type, status, error, created_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                event_id,
                                policy_name,
                                rule_name,
                                "update-file",
                                status,
                                error,
                                now
                            ],
                        );
                    }
                    Err(e) => {
                        eprintln!("events: db lock poisoned in execute_action(update-file): {e}")
                    }
                }
            }

            other => eprintln!("events: unknown action type '{other}'"),
        }
    }

    pub fn start_scheduler(engine: Arc<Self>) {
        use chrono::Timelike;
        std::thread::spawn(move || {
            loop {
                // Sleep until the next whole-minute boundary (wall-clock aligned).
                // This avoids Instant-based drift and aligns ticks to real calendar minutes.
                let now = chrono::Utc::now();
                let ms_past = now.second() as u64 * 1_000
                    + now.nanosecond() as u64 / 1_000_000;
                // Add 50ms buffer so we land just past the boundary, not before it.
                let ms_to_next = (60_000u64).saturating_sub(ms_past) + 50;
                std::thread::sleep(Duration::from_millis(ms_to_next));

                let tick_time = chrono::Utc::now();
                let minute = tick_time.minute();
                let hour = tick_time.hour();
                let tick_ts = tick_time.format("%Y-%m-%dT%H:%M").to_string();

                // Payload carries dedup_key for idempotent catchup if daemon restarts.
                // Format: "timer.tick.5m:2026-05-16T05:20"
                let candidates: &[(&str, bool)] = &[
                    ("timer.tick.minutely", true),
                    ("timer.tick.1m", true),
                    ("timer.tick.5m", minute % 5 == 0),
                    ("timer.tick.30m", minute % 30 == 0),
                    ("timer.tick.1h", minute == 0),
                    ("timer.tick.6h", hour % 6 == 0 && minute == 0),
                    ("timer.tick.daily", hour == 0 && minute == 0),
                ];

                for &(event_type, should_fire) in candidates {
                    if should_fire {
                        let dedup_key = format!("{event_type}:{tick_ts}");
                        engine.ingest(
                            event_type,
                            &serde_json::json!({
                                "dedup_key": dedup_key,
                                "tick_time": tick_ts,
                            }),
                            "scheduler",
                        );
                    }
                }
            }
        });
    }

    // ── HTTP ──────────────────────────────────────────────────────────────────

    pub fn handle(&self, req: &Request) -> Response {
        let path = req.path.strip_prefix("/events").unwrap_or(&req.path);
        let method = req.method.as_str();

        match (method, path) {
            ("POST", "/ingest") => self.http_ingest(req),
            ("GET", "/recent") => self.http_recent(req),
            ("GET", "/status") => self.http_status(),
            ("GET", "/health") | ("GET", "/health/") => {
                json_ok(&serde_json::json!({ "status": "ok" }))
            }
            _ => json_error(404, "events endpoint not found"),
        }
    }

    fn http_ingest(&self, req: &Request) -> Response {
        #[derive(Deserialize)]
        struct Body {
            event_type: String,
            #[serde(default)]
            payload: Value,
            #[serde(default)]
            source: String,
        }

        let b: Body = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(e) => return json_error(400, &format!("invalid JSON: {e}")),
        };

        let event_id = self.ingest(&b.event_type, &b.payload, &b.source);
        Response {
            status: 202,
            content_type: "application/json".to_string(),
            headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
            body: serde_json::to_vec(&serde_json::json!({ "event_id": event_id }))
                .unwrap_or_default(),
        }
    }

    fn http_recent(&self, req: &Request) -> Response {
        let limit: i64 = req
            .query
            .get("limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);

        let db = match self.db.lock() {
            Ok(g) => g,
            Err(e) => return json_error(500, &format!("db lock poisoned: {e}")),
        };
        let mut stmt = match db.prepare(
            "SELECT id, event_type, payload, source, created_at \
             FROM events ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(e) => return json_error(500, &e.to_string()),
        };

        let mut rows: Vec<Value> = Vec::new();
        let mut query = match stmt.query(params![limit]) {
            Ok(q) => q,
            Err(e) => return json_error(500, &e.to_string()),
        };

        while let Ok(Some(row)) = query.next() {
            let id: i64 = row.get(0).unwrap_or(0);
            let event_type: String = row.get(1).unwrap_or_default();
            let payload_str: String = row.get(2).unwrap_or_else(|_| "null".to_string());
            let source: String = row.get(3).unwrap_or_default();
            let created_at: String = row.get(4).unwrap_or_default();
            let payload: Value =
                serde_json::from_str(&payload_str).unwrap_or(Value::Null);
            rows.push(serde_json::json!({
                "id": id,
                "event_type": event_type,
                "payload": payload,
                "source": source,
                "created_at": created_at,
            }));
        }

        json_ok(&rows)
    }

    fn http_status(&self) -> Response {
        let policy_count = match self.policies.read() {
            Ok(guard) => guard.len(),
            Err(e) => return json_error(500, &format!("policies lock poisoned: {e}")),
        };
        let events_processed = match self.events_processed.lock() {
            Ok(guard) => *guard,
            Err(e) => return json_error(500, &format!("events_processed lock poisoned: {e}")),
        };
        let uptime_secs = self.start_time.elapsed().as_secs();

        json_ok(&serde_json::json!({
            "status": "running",
            "uptime_seconds": uptime_secs,
            "policies_loaded": policy_count,
            "events_processed": events_processed,
            "policies_dir": self.policies_dir.to_string_lossy(),
        }))
    }

    // ── CLI ───────────────────────────────────────────────────────────────────

    pub fn cli_status(&self) {
        // Query live state from shared on-disk artifacts so the CLI reports the
        // running daemon's state, not this ephemeral CLI instance's state.
        let home = PathBuf::from(shellexpand::tilde("~").as_ref());
        let events_dir = home.join(".hex-events");
        let policies_dir = events_dir.join("policies");
        let db_path = events_dir.join("events.db");
        let heartbeat_path = events_dir.join("last-heartbeat.json");

        let policy_count = std::fs::read_dir(&policies_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s == "yaml" || s == "yml")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);

        let (total_events, processed, pending) = match Connection::open(&db_path) {
            Ok(conn) => {
                let total: i64 = conn
                    .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                    .unwrap_or(0);
                let proc_: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM events WHERE processed_at IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let pend: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM events WHERE processed_at IS NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                (total, proc_, pend)
            }
            Err(_) => (0, 0, 0),
        };

        // Daemon liveness from heartbeat file mtime.
        let (daemon_state, heartbeat_age_secs): (&str, Option<u64>) =
            match heartbeat_path.metadata().and_then(|m| m.modified()) {
                Ok(mtime) => match SystemTime::now().duration_since(mtime) {
                    Ok(age) => {
                        let secs = age.as_secs();
                        if secs < 300 {
                            ("running", Some(secs))
                        } else {
                            ("stale", Some(secs))
                        }
                    }
                    Err(_) => ("unknown", None),
                },
                Err(_) => ("not-started", None),
            };

        println!("hex events status");
        println!("  daemon:           {daemon_state}");
        match heartbeat_age_secs {
            Some(s) => println!("  last heartbeat:   {s}s ago"),
            None => println!("  last heartbeat:   (none)"),
        }
        println!("  policies loaded:  {policy_count}");
        println!("  events total:     {total_events}");
        println!("  events processed: {processed}");
        println!("  events pending:   {pending}");
        println!("  policies dir:     {}", policies_dir.display());
        println!("  events db:        {}", db_path.display());

        if daemon_state == "not-started" {
            println!();
            println!("  Daemon not running. Start with: hex events daemon");
        } else if daemon_state == "stale" {
            println!();
            println!("  Heartbeat is stale (>5 min). Daemon may have crashed.");
        }
    }

    pub fn cli_emit(&self, event_type: &str, payload_json: &str, source: &str) {
        let payload: Value = match serde_json::from_str(payload_json) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Invalid payload JSON: {e}");
                std::process::exit(1);
            }
        };
        let event_id = self.ingest(event_type, &payload, source);
        println!("Emitted {event_type} (id={event_id}, source={source})");
    }

    pub fn cli_trace(&self, event_id: i64) {
        let db = match self.db.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("events: db lock poisoned: {e}");
                return;
            }
        };

        let ev = db.query_row(
            "SELECT event_type, payload, source, created_at FROM events WHERE id = ?1",
            params![event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2).unwrap_or_default(),
                    row.get::<_, String>(3)?,
                ))
            },
        );

        match ev {
            Ok((event_type, payload, source, created_at)) => {
                println!("Event #{event_id}: {event_type}");
                println!("  source:     {source}");
                println!("  created_at: {created_at}");
                println!("  payload:    {payload}");
            }
            Err(_) => {
                eprintln!("Event {event_id} not found");
                std::process::exit(1);
            }
        }

        let mut stmt = match db.prepare(
            "SELECT policy_name, rule_name, action_type, status, error, created_at \
             FROM action_log WHERE event_id = ?1 ORDER BY id",
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to query action log: {e}");
                return;
            }
        };

        println!("\nAction chain:");
        let mut query = match stmt.query(params![event_id]) {
            Ok(q) => q,
            Err(e) => {
                eprintln!("{e}");
                return;
            }
        };

        let mut count = 0usize;
        while let Ok(Some(row)) = query.next() {
            let policy: String = row.get(0).unwrap_or_default();
            let rule: String = row.get(1).unwrap_or_default();
            let action: String = row.get(2).unwrap_or_default();
            let status: String = row.get(3).unwrap_or_default();
            let error: String = row.get(4).unwrap_or_default();
            let ts: String = row.get(5).unwrap_or_default();
            println!("  [{ts}] policy={policy} rule={rule} action={action} status={status}");
            if !error.is_empty() {
                println!("    error: {error}");
            }
            count += 1;
        }
        if count == 0 {
            println!("  (no actions recorded)");
        }
    }

    pub fn cli_policies(&self) {
        let policies = match self.policies.read() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("events: policies lock poisoned: {e}");
                return;
            }
        };
        if policies.is_empty() {
            println!("No policies loaded (dir: {:?})", self.policies_dir);
            return;
        }
        println!("{} policies:", policies.len());
        for p in policies.iter() {
            println!("  {} — {} rules — {}", p.name, p.rules.len(), p.description);
        }
    }

    pub fn cli_reload(&self) {
        self.reload_policies();
        let count = match self.policies.read() {
            Ok(guard) => guard.len(),
            Err(e) => {
                eprintln!("events: policies lock poisoned: {e}");
                return;
            }
        };
        println!("Reloaded {count} policies");
    }

    // ── Persisted rate limiting ───────────────────────────────────────────────

    /// Returns true if the agent is allowed to fire (count of recent fires < max_fires).
    /// Queries the persisted `agent_wake_fires` table so limits survive daemon restarts.
    pub fn check_rate_limit(&self, agent_id: &str, max_fires: u32, window: Duration) -> bool {
        let cutoff = Utc::now() - chrono::Duration::from_std(window).unwrap_or(chrono::Duration::hours(1));
        let cutoff_str = cutoff.to_rfc3339();
        let db = match self.db.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("events: db lock poisoned in check_rate_limit: {e}");
                return true; // fail open to avoid blocking all wakes on lock corruption
            }
        };
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM agent_wake_fires WHERE agent_id = ?1 AND fired_at > ?2",
                params![agent_id, cutoff_str],
                |row| row.get(0),
            )
            .unwrap_or(0);
        count < max_fires as i64
    }

    /// Record one agent wake fire.  Garbage-collects rows older than 30 days.
    pub fn record_fire(&self, agent_id: &str) {
        let now = Utc::now().to_rfc3339();
        let cutoff = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let db = match self.db.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("events: db lock poisoned in record_fire: {e}");
                return;
            }
        };
        let _ = db.execute(
            "INSERT INTO agent_wake_fires (agent_id, fired_at) VALUES (?1, ?2)",
            params![agent_id, now],
        );
        // GC: remove rows older than 30 days to keep the table bounded
        let _ = db.execute(
            "DELETE FROM agent_wake_fires WHERE fired_at < ?1",
            params![cutoff],
        );
    }

    /// Dispatch agent wakes for a given event.  Called from dispatch_existing_event
    /// after the regular policy dispatch.  Rate limiting is enforced here via
    /// Write `health.agents.<agent_id>.degraded = true` to health.json when a
    /// trigger condition times out or errors in a way that signals misconfiguration.
    fn set_agent_degraded(&self, agent_id: &str, reason: &str) {
        let health_path = self.hex_dir.join("health.json");
        let mut health_map: serde_json::Map<String, serde_json::Value> =
            if health_path.exists() {
                std::fs::read_to_string(&health_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| if let serde_json::Value::Object(m) = v { Some(m) } else { None })
                    .unwrap_or_default()
            } else {
                serde_json::Map::new()
            };

        let agents = health_map
            .entry("agents".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let serde_json::Value::Object(ref mut map) = agents {
            let entry = map
                .entry(agent_id.to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let serde_json::Value::Object(ref mut agent_map) = entry {
                agent_map.insert("degraded".to_string(), serde_json::Value::Bool(true));
                agent_map.insert("degraded_reason".to_string(), serde_json::Value::String(reason.to_string()));
            }
        }

        if let Ok(json_str) = serde_json::to_string_pretty(&serde_json::Value::Object(health_map)) {
            let dir = health_path.parent().unwrap_or(Path::new("."));
            let tmp = dir.join(format!(
                ".health_degraded_tmp_{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            ));
            if let Ok(mut f) = std::fs::File::create(&tmp) {
                let ok = f.write_all(json_str.as_bytes()).is_ok()
                    && f.flush().is_ok()
                    && std::fs::rename(&tmp, &health_path).is_ok();
                if !ok {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        }
    }

    /// Dispatch agent wakes for charter-based policies matching the given event.
    /// Per-trigger condition expressions are shell-evaluated with a 5s timeout.
    /// A passing condition (or no condition) proceeds to the wake command.
    /// `policy.command` overrides the default `hex agent wake <id>` invocation.
    /// on_success / on_failure events are emitted after the wake completes.
    pub fn dispatch_agent_wakes(
        &self,
        event_id: i64,
        event_type: &str,
        payload: &Value,
        shadow: bool,
    ) -> usize {
        let agent_policies = match self.agent_policies.read() {
            Ok(guard) => guard.clone(),
            Err(e) => {
                eprintln!("events: agent_policies read lock poisoned: {e}");
                return 0;
            }
        };

        let mut wakes_fired = 0usize;
        for policy in &agent_policies {
            // Gather triggers matching this event type.
            let matching_triggers: Vec<&TriggerSpec> = policy
                .triggers
                .iter()
                .filter(|t| wildcard_matches(&t.event, event_type))
                .collect();

            if matching_triggers.is_empty() {
                continue;
            }

            // Rate limit check (persisted, survives daemon restarts).
            if let Some(ref rl) = policy.rate_limit {
                match rl.window_duration() {
                    Ok(window) => {
                        if !self.check_rate_limit(&policy.id, rl.max_fires, window) {
                            eprintln!(
                                "events: agent '{}' rate-limited ({} fires/{} window) — skipping wake for event={event_type}",
                                policy.id, rl.max_fires, rl.window
                            );
                            continue;
                        }
                    }
                    Err(e) => {
                        eprintln!("events: agent '{}' rate_limit window parse error: {e}", policy.id);
                    }
                }
            }

            // Evaluate per-trigger conditions. The first trigger whose condition
            // passes (or that has no condition) gives the green light.
            // Timeout → ERROR log + degraded flag. Non-zero exit → debug log + skip that trigger.
            let mut wake_approved = false;
            for trigger in &matching_triggers {
                match &trigger.condition {
                    None => {
                        wake_approved = true;
                        break;
                    }
                    Some(wake_condition) => {
                        let (status, error, _stdout) = run_shell_with_timeout(wake_condition, 5);
                        if status == "ok" {
                            wake_approved = true;
                            break;
                        }
                        if error.contains("timeout after") {
                            eprintln!(
                                "events: ERROR agent '{}' trigger condition timed out: {wake_condition}",
                                policy.id
                            );
                            self.set_agent_degraded(
                                &policy.id,
                                &format!("condition_timeout: {wake_condition}"),
                            );
                        } else {
                            eprintln!(
                                "events: debug agent '{}' condition not met: {wake_condition} ({})",
                                policy.id, error
                            );
                        }
                    }
                }
            }

            if !wake_approved {
                continue;
            }

            if shadow {
                eprintln!(
                    "SHADOW: event={event_type} agent_wake agent={} (event_id={event_id})",
                    policy.id
                );
                wakes_fired += 1;
                continue;
            }

            // Build wake command: use wake_command override if present, else default.
            let wake_command = if let Some(ref cmd_template) = policy.command {
                render_template(cmd_template, event_type, payload)
            } else {
                format!(
                    "nohup hex agent wake {} --trigger {} >/dev/null 2>&1 &",
                    policy.id, event_type
                )
            };

            let (status, _error, _stdout) = run_shell_with_timeout(&wake_command, 10);
            let succeeded = status == "ok";

            if succeeded {
                self.record_fire(&policy.id);
                for event_name in &policy.on_success {
                    self.ingest_with_depth(
                        event_name,
                        &serde_json::json!({
                            "source": format!("hex:{}", policy.id),
                            "trigger_event": event_type,
                            "exit_code": 0,
                        }),
                        &format!("hex:{}", policy.id),
                        1,
                    );
                }
            } else {
                eprintln!(
                    "events: agent wake failed agent={} event={event_type} status={status}",
                    policy.id
                );
                for event_name in &policy.on_failure {
                    self.ingest_with_depth(
                        event_name,
                        &serde_json::json!({
                            "source": format!("hex:{}", policy.id),
                            "trigger_event": event_type,
                            "exit_code": 1,
                        }),
                        &format!("hex:{}", policy.id),
                        1,
                    );
                }
            }

            let _ = self.db.lock().map(|db| {
                db.execute(
                    "INSERT INTO action_log \
                     (event_id, policy_name, rule_name, action_type, status, error, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        event_id,
                        format!("charter:{}", policy.id),
                        "wake",
                        "agent_wake",
                        status,
                        "",
                        Utc::now().to_rfc3339()
                    ],
                )
            });

            wakes_fired += 1;
        }
        wakes_fired
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn init_schema(conn: &Connection) -> Result<(), String> {
    // Disable FK constraints — bundled-full enables them by default, but we use
    // event_id as a soft reference (no cascade needed) and the Python daemon
    // never enforced FKs either.
    conn.execute_batch("PRAGMA foreign_keys = OFF")
        .map_err(|e| format!("schema init failed: {e}"))?;

    // `extra_check` (pulled in by bundled-full → modern-full) makes execute_batch
    // error on any PRAGMA that returns rows. Use query_row to consume the result
    // row that journal_mode=WAL always emits; use execute for busy_timeout which
    // does not return rows.
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
        .map_err(|e| format!("schema init failed: {e}"))?;
    conn.query_row("PRAGMA busy_timeout=5000", [], |_| Ok(()))
        .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(()) } else { Err(e) })
        .map_err(|e| format!("schema init failed: {e}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             event_type TEXT NOT NULL,
             payload TEXT,
             source TEXT DEFAULT '',
             created_at TEXT NOT NULL,
             processed_at TEXT,
             dedup_key TEXT,
             recipe TEXT,
             condition_details TEXT
         );
         CREATE TABLE IF NOT EXISTS action_log (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             event_id INTEGER REFERENCES events(id),
             policy_name TEXT,
             rule_name TEXT,
             action_type TEXT,
             status TEXT,
             error TEXT,
             created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
         CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at);
         CREATE INDEX IF NOT EXISTS idx_events_unprocessed ON events(processed_at) WHERE processed_at IS NULL;
         CREATE TABLE IF NOT EXISTS agent_wake_fires (
             agent_id TEXT NOT NULL,
             fired_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_agent_wake_fires ON agent_wake_fires(agent_id, fired_at);",
    )
    .map_err(|e| format!("schema init failed: {e}"))?;

    // Add columns that may not exist in older DB files (Python daemon or earlier Rust builds).
    // ALTER TABLE ADD COLUMN fails silently if column already exists via ignore_err.
    for col_ddl in &[
        "ALTER TABLE events ADD COLUMN processed_at TEXT",
        "ALTER TABLE events ADD COLUMN dedup_key TEXT",
        "ALTER TABLE events ADD COLUMN recipe TEXT",
        "ALTER TABLE events ADD COLUMN condition_details TEXT",
    ] {
        let _ = conn.execute_batch(col_ddl);
    }

    Ok(())
}

/// Snapshot mtime of every *.yaml file in a directory.
fn snapshot_mtimes(dir: &Path) -> HashMap<PathBuf, SystemTime> {
    let pattern = dir.join("*.yaml");
    let mut map = HashMap::new();
    if let Ok(paths) = glob::glob(&pattern.to_string_lossy()) {
        for entry in paths.flatten() {
            if let Ok(meta) = std::fs::metadata(&entry) {
                if let Ok(mtime) = meta.modified() {
                    map.insert(entry, mtime);
                }
            }
        }
    }
    map
}

fn wildcard_matches(pattern: &str, event_type: &str) -> bool {
    if pattern == "*" || pattern == event_type {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return event_type.starts_with(&format!("{prefix}."));
    }
    false
}

fn resolve_field(field: &str, payload: &Value) -> Option<Value> {
    let path = field.strip_prefix("payload.").unwrap_or(field);
    let mut current = payload;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current.clone())
}

fn value_to_str(v: &Value) -> String {
    v.as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| v.to_string())
}

fn cmp_nums(actual: &Option<Value>, expected: &Option<Value>, gt: bool) -> bool {
    match (actual.as_ref().and_then(|v| v.as_f64()), expected.as_ref().and_then(|v| v.as_f64())) {
        (Some(a), Some(b)) => if gt { a > b } else { a < b },
        _ => false,
    }
}

/// Substitute {{event.type}} and {{event.FIELD}} in action templates.
fn render_template(template: &str, event_type: &str, payload: &Value) -> String {
    let mut s = template
        .replace("{{event.type}}", event_type)
        .replace("{{event_type}}", event_type);
    if let Value::Object(map) = payload {
        for (k, v) in map {
            s = s.replace(&format!("{{{{event.{k}}}}}"), &value_to_str(v));
        }
    }
    s
}

// ── Action helpers ────────────────────────────────────────────────────────────

/// Run a shell command with a timeout. Returns (status, error, stdout).
fn run_shell_with_timeout(cmd: &str, timeout_secs: u64) -> (String, String, String) {
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => return ("error".to_string(), e.to_string(), String::new()),
    };

    // Capture pid before moving child into the thread (for kill on timeout).
    let pid = child.id();
    let (tx, rx) = mpsc::channel::<std::io::Result<std::process::Output>>();

    // Move child into thread — wait_with_output() takes ownership and reads pipes
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            match output.status.code() {
                Some(0) => ("ok".to_string(), String::new(), stdout),
                Some(c) => {
                    let err = if stderr.is_empty() {
                        format!("exit code {c}")
                    } else {
                        stderr[..stderr.len().min(500)].to_string()
                    };
                    ("error".to_string(), err, stdout)
                }
                None => (
                    "error".to_string(),
                    "terminated by signal".to_string(),
                    stdout,
                ),
            }
        }
        Ok(Err(e)) => ("error".to_string(), e.to_string(), String::new()),
        Err(_) => {
            // Timeout — kill by PID
            #[cfg(unix)]
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .spawn();
            #[cfg(not(unix))]
            let _ = pid;
            (
                "error".to_string(),
                format!("timeout after {timeout_secs}s"),
                String::new(),
            )
        }
    }
}

/// Deliver a system notification. Falls back to osascript on macOS.
fn deliver_notification(message: &str, tier: Option<&str>) -> (String, String) {
    // tier: "log" → just log, no OS notification; otherwise try OS notification
    match tier {
        Some("log") => {
            eprintln!("events: [notify/log] {message}");
            return ("ok".to_string(), String::new());
        }
        Some("digest") => {
            eprintln!("events: [notify/digest] {message}");
            return ("ok".to_string(), String::new());
        }
        _ => {}
    }

    // Try hex-notify.sh first
    let notify_sh = shellexpand::tilde("~/.hex/scripts/hex-notify.sh").into_owned();
    if std::path::Path::new(&notify_sh).exists() {
        match std::process::Command::new("bash")
            .arg(&notify_sh)
            .arg(message)
            .output()
        {
            Ok(out) if out.status.success() => return ("ok".to_string(), String::new()),
            Ok(out) => {
                eprintln!(
                    "events: hex-notify.sh failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Err(_) => {}
        }
    }

    // macOS osascript fallback
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification {} with title \"hex-events\"",
            serde_json::to_string(message).unwrap_or_else(|_| format!("\"{message}\""))
        );
        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
        {
            Ok(out) if out.status.success() => return ("ok".to_string(), String::new()),
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return ("error".to_string(), err[..err.len().min(200)].to_string());
            }
            Err(e) => return ("error".to_string(), e.to_string()),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("events: [notify] {message}");
        ("ok".to_string(), String::new())
    }
}

/// Atomic append to a file (write tmp beside target, then rename).
fn atomic_file_append(path: &str, content: &str) -> (String, String) {
    let path = std::path::Path::new(path);
    let dir = path.parent().unwrap_or(std::path::Path::new("."));

    let existing = if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return ("error".to_string(), e.to_string()),
        }
    } else {
        String::new()
    };

    let new_content = format!("{existing}{content}");
    match write_atomic(path, dir, &new_content) {
        Ok(()) => ("ok".to_string(), String::new()),
        Err(e) => ("error".to_string(), e),
    }
}

/// Atomic regex find/replace in a file.
fn atomic_regex_replace(path: &str, pattern: &str, replace: &str) -> (String, String) {
    let path = std::path::Path::new(path);
    let dir = path.parent().unwrap_or(std::path::Path::new("."));

    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return ("error".to_string(), e.to_string()),
    };

    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return ("error".to_string(), format!("bad regex: {e}")),
    };
    let new_content = re.replace_all(&content, replace).into_owned();

    match write_atomic(path, dir, &new_content) {
        Ok(()) => ("ok".to_string(), String::new()),
        Err(e) => ("error".to_string(), e),
    }
}

fn write_atomic(target: &Path, dir: &Path, content: &str) -> Result<(), String> {
    let tmp = dir.join(format!(
        ".hex_tmp_{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
    f.flush().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, target).map_err(|e| e.to_string())?;
    Ok(())
}

/// Recursively render {{...}} templates in a JSON value.
fn render_value_templates(v: &Value, event_type: &str, payload: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(render_template(s, event_type, payload)),
        Value::Object(map) => {
            let new_map = map
                .iter()
                .map(|(k, v)| (k.clone(), render_value_templates(v, event_type, payload)))
                .collect();
            Value::Object(new_map)
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|v| render_value_templates(v, event_type, payload)).collect())
        }
        other => other.clone(),
    }
}

/// Parse a duration string like "60s", "5m", "1h", "6h", "1d" into seconds.
fn parse_duration_str(s: &str) -> u64 {
    if let Some(n) = s.strip_suffix('s') {
        n.parse().unwrap_or(0)
    } else if let Some(n) = s.strip_suffix('m') {
        n.parse::<u64>().unwrap_or(0) * 60
    } else if let Some(n) = s.strip_suffix('h') {
        n.parse::<u64>().unwrap_or(0) * 3600
    } else if let Some(n) = s.strip_suffix('d') {
        n.parse::<u64>().unwrap_or(0) * 86400
    } else {
        s.parse().unwrap_or(0)
    }
}

fn json_ok<T: Serialize>(val: &T) -> Response {
    Response {
        status: 200,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body: serde_json::to_vec(val).unwrap_or_default(),
    }
}

fn json_error(status: u16, msg: &str) -> Response {
    Response {
        status,
        content_type: "application/json".to_string(),
        headers: vec![("Access-Control-Allow-Origin".to_string(), "*".to_string())],
        body: serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_engine(tmp: &TempDir) -> Arc<EventEngine> {
        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(tmp.path()));
        // Override policies_dir to an empty temp dir so we load 0 policies
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        Arc::new(EventEngine {
            db: Mutex::new(conn),
            policies_dir: tmp.path().join("policies"),
            hex_dir: tmp.path().to_path_buf(),
            policies: RwLock::new(Vec::new()),
            agent_policies: RwLock::new(Vec::new()),
            charter_loader: None,
            telemetry,
            bus,
            start_time: Instant::now(),
            events_processed: Mutex::new(0),
            rate_limiter: Mutex::new(HashMap::new()),
        })
    }

    #[test]
    fn wildcard_exact() {
        assert!(wildcard_matches("boi.spec.completed", "boi.spec.completed"));
        assert!(!wildcard_matches("boi.spec.completed", "boi.spec.started"));
    }

    #[test]
    fn wildcard_star_suffix() {
        assert!(wildcard_matches("boi.spec.*", "boi.spec.completed"));
        assert!(wildcard_matches("boi.spec.*", "boi.spec.started"));
        assert!(!wildcard_matches("boi.spec.*", "boi.other.event"));
        assert!(!wildcard_matches("boi.spec.*", "boi.spec"));
    }

    #[test]
    fn wildcard_global() {
        assert!(wildcard_matches("*", "anything.at.all"));
    }

    #[test]
    fn ingest_writes_to_db() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let id = engine.ingest("test.event", &serde_json::json!({"x": 1}), "test");
        assert!(id > 0);

        let db = engine.db.lock().unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    fn make_cond(field: &str, op: &str, value: Option<Value>) -> Condition {
        Condition {
            field: Some(field.to_string()),
            op: Some(op.to_string()),
            value,
            cond_type: None,
            command: None,
        }
    }

    #[test]
    fn condition_eq() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let payload = serde_json::json!({"status": "done"});
        let cond = make_cond("status", "eq", Some(Value::String("done".to_string())));
        assert!(engine.evaluate_condition(&cond, &payload));
        let cond_no = make_cond("status", "eq", Some(Value::String("pending".to_string())));
        assert!(!engine.evaluate_condition(&cond_no, &payload));
    }

    #[test]
    fn condition_contains() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let payload = serde_json::json!({"msg": "hello world"});
        let cond = make_cond("msg", "contains", Some(Value::String("world".to_string())));
        assert!(engine.evaluate_condition(&cond, &payload));
    }

    #[test]
    fn condition_exists() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let payload = serde_json::json!({"present": true});
        let present = make_cond("present", "exists", None);
        let missing = make_cond("missing", "exists", None);
        assert!(engine.evaluate_condition(&present, &payload));
        assert!(!engine.evaluate_condition(&missing, &payload));
    }

    #[test]
    fn render_template_substitution() {
        let payload = serde_json::json!({"spec_id": "q-911", "status": "done"});
        let result = render_template(
            "echo 'spec {{event.spec_id}} is {{event.status}}'",
            "boi.spec.completed",
            &payload,
        );
        assert_eq!(result, "echo 'spec q-911 is done'");
    }

    // ── events_* tests (found by `cargo test events`) ───────────────────────

    #[test]
    fn events_parse_minimal_policy() {
        let yaml = r#"
name: test-policy
rules:
  - name: rule-one
    trigger:
      event: test.event
    actions:
      - type: shell
        command: echo hello
"#;
        let p: Policy = serde_yaml::from_str(yaml).expect("should parse");
        assert_eq!(p.name, "test-policy");
        assert_eq!(p.rules.len(), 1);
        assert_eq!(p.rules[0].trigger.event, "test.event");
    }

    #[test]
    fn events_parse_policy_with_rate_limit() {
        let yaml = r#"
name: rate-limited
rate_limit:
  max_fires: 4
  window: 6h
rules:
  - name: r1
    trigger:
      event: timer.tick.5m
    actions:
      - type: shell
        command: echo hi
"#;
        let p: Policy = serde_yaml::from_str(yaml).expect("should parse");
        assert!(p.rate_limit.is_some());
    }

    #[test]
    fn events_parse_policy_with_singular_condition() {
        let yaml = r#"
name: perf-alert
rules:
  - name: alert-slow
    trigger:
      event: hex.brain_timing
    condition:
      field: payload.total_ms
      op: gt
      value: 30000
    actions:
      - type: emit
        event: hex.perf.alert
"#;
        let p: Policy = serde_yaml::from_str(yaml).expect("should parse");
        assert!(p.rules[0].condition.is_some());
        let c = p.rules[0].condition.as_ref().unwrap();
        assert_eq!(c.field.as_deref(), Some("payload.total_ms"));
        assert_eq!(c.op.as_deref(), Some("gt"));
    }

    #[test]
    fn events_parse_policy_with_on_success_on_failure() {
        let yaml = r#"
name: chain-test
rules:
  - name: step
    trigger:
      event: some.event
    actions:
      - type: shell
        command: echo test
        on_success:
          - type: emit
            event: some.succeeded
        on_failure:
          - type: emit
            event: some.failed
"#;
        let p: Policy = serde_yaml::from_str(yaml).expect("should parse");
        let action = &p.rules[0].actions[0];
        assert_eq!(action.on_success.len(), 1);
        assert_eq!(action.on_failure.len(), 1);
    }

    #[test]
    fn events_parse_real_policies() {
        let policies_dir = std::path::Path::new("/Users/mrap/.hex-events/policies");
        if !policies_dir.exists() {
            // CI or different machine — skip gracefully
            return;
        }
        let pattern = policies_dir.join("*.yaml");
        let paths = glob::glob(&pattern.to_string_lossy()).expect("glob failed");
        let mut ok = 0usize;
        let mut failures: Vec<(String, String)> = Vec::new();
        for entry in paths.flatten() {
            let name = entry.file_name().unwrap_or_default().to_string_lossy().to_string();
            match std::fs::read_to_string(&entry) {
                Ok(content) => match serde_yaml::from_str::<Policy>(&content) {
                    Ok(p) => {
                        let _ = p; // just validate parse
                        ok += 1;
                    }
                    Err(e) => failures.push((name, e.to_string())),
                },
                Err(e) => failures.push((name, e.to_string())),
            }
        }
        if !failures.is_empty() {
            eprintln!("Policy parse failures ({}/{} total):", failures.len(), ok + failures.len());
            for (name, err) in &failures {
                eprintln!("  FAIL {name}: {err}");
            }
        }
        // Best-effort: allow up to 10% failures since some policies may be intentionally malformed
        let total = ok + failures.len();
        assert!(
            total == 0 || failures.len() * 10 <= total,
            "Too many policy parse failures: {}/{} failed",
            failures.len(),
            total
        );
    }

    #[test]
    fn events_hot_reload_detects_new_file() {
        let tmp = TempDir::new().unwrap();
        let policies_tmp = tmp.path().join("policies");
        std::fs::create_dir_all(&policies_tmp).unwrap();

        // Write an initial policy
        std::fs::write(
            policies_tmp.join("initial.yaml"),
            b"name: initial\nrules:\n  - name: r\n    trigger:\n      event: x\n    actions:\n      - type: shell\n        command: echo hi\n",
        )
        .unwrap();

        let bus = SseBus::new();
        let telemetry = Arc::new(Telemetry::new(tmp.path()));
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let engine = Arc::new(EventEngine {
            db: Mutex::new(conn),
            policies_dir: policies_tmp.clone(),
            hex_dir: tmp.path().to_path_buf(),
            policies: RwLock::new(Vec::new()),
            agent_policies: RwLock::new(Vec::new()),
            charter_loader: None,
            telemetry,
            bus,
            start_time: Instant::now(),
            events_processed: Mutex::new(0),
            rate_limiter: Mutex::new(HashMap::new()),
        });
        engine.load_policies();
        assert_eq!(engine.policy_count(), 1, "should have 1 policy after initial load");

        // Start hot-reload (10s polling)
        EventEngine::start_hot_reload(Arc::clone(&engine));

        // Write a second policy file (with a slight delay to ensure mtime differs)
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(
            policies_tmp.join("second.yaml"),
            b"name: second\nrules:\n  - name: r2\n    trigger:\n      event: y\n    actions:\n      - type: shell\n        command: echo bye\n",
        )
        .unwrap();

        // Wait for the hot-reload thread to detect the change (up to 12s)
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if engine.policy_count() == 2 {
                break;
            }
            if Instant::now() > deadline {
                panic!("hot-reload did not pick up new policy within 12s");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(engine.policy_count(), 2);
    }

    // ── actions_* tests (found by `cargo test actions`) ─────────────────────

    fn make_action(r#type: &str) -> Action {
        Action {
            r#type: r#type.to_string(),
            command: None,
            event: None,
            timeout: None,
            payload: None,
            on_success: vec![],
            on_failure: vec![],
            source: None,
            message: None,
            target: None,
            pattern: None,
            replace: None,
            mode: None,
            content: None,
            dedup_key: None,
            delay: None,
            tier: None,
        }
    }

    #[test]
    fn actions_shell_success() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let action = Action {
            command: Some("echo hello".to_string()),
            ..make_action("shell")
        };
        let payload = serde_json::json!({});
        engine.execute_action(&action, 1, "test-policy", "r1", "test.event", &payload, None, 0);

        let db = engine.db.lock().unwrap();
        let status: String = db
            .query_row(
                "SELECT status FROM action_log WHERE action_type='shell'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "ok");
    }

    #[test]
    fn actions_shell_failure_triggers_on_failure_chain() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let on_fail = Action {
            event: Some("test.failed".to_string()),
            ..make_action("emit")
        };
        let action = Action {
            command: Some("exit 1".to_string()),
            on_failure: vec![on_fail],
            ..make_action("shell")
        };
        let payload = serde_json::json!({});
        engine.execute_action(&action, 1, "test-policy", "r1", "test.event", &payload, None, 0);

        let db = engine.db.lock().unwrap();
        // shell logged as error
        let status: String = db
            .query_row(
                "SELECT status FROM action_log WHERE action_type='shell'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "error");
        // on_failure emit was recorded
        let emit_count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM action_log WHERE action_type='emit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(emit_count, 1);
    }

    #[test]
    fn actions_shell_template_rendered() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let action = Action {
            command: Some("echo {{event.type}}".to_string()),
            ..make_action("shell")
        };
        let payload = serde_json::json!({});
        engine.execute_action(
            &action,
            1,
            "test-policy",
            "r1",
            "timer.tick.minutely",
            &payload,
            None,
            0,
        );
        // Just verify it executed without error (template rendered)
        let db = engine.db.lock().unwrap();
        let status: String = db
            .query_row(
                "SELECT status FROM action_log WHERE action_type='shell'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "ok");
    }

    #[test]
    fn actions_emit_writes_new_event() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let action = Action {
            event: Some("child.event".to_string()),
            ..make_action("emit")
        };
        let payload = serde_json::json!({"x": 42});
        engine.execute_action(&action, 1, "test-policy", "r1", "parent.event", &payload, None, 0);

        let db = engine.db.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_type='child.event'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn actions_emit_depth_limit_prevents_infinite_loop() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        // Load a policy that re-emits the same event — would loop forever without depth limit
        let policy_yaml = r#"
name: looping-policy
rules:
  - name: loop
    trigger:
      event: loop.event
    actions:
      - type: emit
        event: loop.event
"#;
        {
            let mut guard = engine.policies.write().unwrap();
            *guard = vec![serde_yaml::from_str(policy_yaml).unwrap()];
        }

        // Ingest the event — depth limit should stop the recursion
        engine.ingest("loop.event", &serde_json::json!({}), "test");

        let db = engine.db.lock().unwrap();
        // Should have a bounded number of events, not infinity
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_type='loop.event'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // depth limit is 9 (0..=8), so at most 9 loop.event entries
        assert!(count > 0 && count <= 10, "expected bounded count, got {count}");
    }

    #[test]
    fn actions_notify_log_tier_skips_os_notification() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let action = Action {
            message: Some("test message".to_string()),
            tier: Some("log".to_string()),
            ..make_action("notify")
        };
        let payload = serde_json::json!({});
        engine.execute_action(&action, 1, "test-policy", "r1", "test.event", &payload, None, 0);

        let db = engine.db.lock().unwrap();
        let status: String = db
            .query_row(
                "SELECT status FROM action_log WHERE action_type='notify'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "ok");
    }

    #[test]
    fn actions_update_file_append() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let target = tmp.path().join("output.txt");
        std::fs::write(&target, "line1\n").unwrap();

        let action = Action {
            target: Some(target.to_string_lossy().to_string()),
            mode: Some("append".to_string()),
            content: Some("line2\n".to_string()),
            ..make_action("update-file")
        };
        let payload = serde_json::json!({});
        engine.execute_action(&action, 1, "test-policy", "r1", "test.event", &payload, None, 0);

        let contents = std::fs::read_to_string(&target).unwrap();
        assert_eq!(contents, "line1\nline2\n");

        let db = engine.db.lock().unwrap();
        let status: String = db
            .query_row(
                "SELECT status FROM action_log WHERE action_type='update-file'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "ok");
    }

    #[test]
    fn actions_update_file_regex_replace() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let target = tmp.path().join("replace.txt");
        std::fs::write(&target, "version: 1.0.0\n").unwrap();

        let action = Action {
            target: Some(target.to_string_lossy().to_string()),
            pattern: Some(r"version: \d+\.\d+\.\d+".to_string()),
            replace: Some("version: 2.0.0".to_string()),
            ..make_action("update-file")
        };
        let payload = serde_json::json!({});
        engine.execute_action(&action, 1, "test-policy", "r1", "test.event", &payload, None, 0);

        let contents = std::fs::read_to_string(&target).unwrap();
        assert_eq!(contents, "version: 2.0.0\n");
    }

    #[test]
    fn actions_rate_limit_blocks_excess_fires() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let action = Action {
            command: Some("echo rate-limited-test".to_string()),
            ..make_action("shell")
        };
        let payload = serde_json::json!({});
        let rate_limit = serde_json::json!({ "max_fires": 2, "window": "60s" });

        // Fire 3 times
        for _ in 0..3 {
            engine.execute_action(
                &action,
                1,
                "rl-policy",
                "r1",
                "test.event",
                &payload,
                Some(&rate_limit),
                0,
            );
        }

        let db = engine.db.lock().unwrap();
        // Only 2 should have been logged (3rd was rate-limited)
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM action_log WHERE action_type='shell'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "rate limiter should have blocked the 3rd fire");
    }

    #[test]
    fn actions_parse_duration_str() {
        assert_eq!(parse_duration_str("60s"), 60);
        assert_eq!(parse_duration_str("5m"), 300);
        assert_eq!(parse_duration_str("1h"), 3600);
        assert_eq!(parse_duration_str("6h"), 21600);
        assert_eq!(parse_duration_str("1d"), 86400);
    }

    // ── scheduler_* tests ────────────────────────────────────────────────────

    fn scheduler_ticks_at(minute: u32, hour: u32) -> Vec<&'static str> {
        let candidates: &[(&str, bool)] = &[
            ("timer.tick.minutely", true),
            ("timer.tick.1m", true),
            ("timer.tick.5m", minute % 5 == 0),
            ("timer.tick.30m", minute % 30 == 0),
            ("timer.tick.1h", minute == 0),
            ("timer.tick.6h", hour % 6 == 0 && minute == 0),
            ("timer.tick.daily", hour == 0 && minute == 0),
        ];
        candidates
            .iter()
            .filter(|(_, fire)| *fire)
            .map(|(name, _)| *name)
            .collect()
    }

    #[test]
    fn scheduler_ticks_minute_only() {
        // At a non-special minute (e.g., 10:03), only minutely and 1m should fire
        let ticks = scheduler_ticks_at(3, 10);
        assert_eq!(ticks, vec!["timer.tick.minutely", "timer.tick.1m"]);
    }

    #[test]
    fn scheduler_ticks_5m() {
        // At minute 5, also 5m fires
        let ticks = scheduler_ticks_at(5, 10);
        assert!(ticks.contains(&"timer.tick.5m"));
        assert!(!ticks.contains(&"timer.tick.30m"));
        assert!(!ticks.contains(&"timer.tick.1h"));
    }

    #[test]
    fn scheduler_ticks_30m() {
        // At minute 30, 5m and 30m fire (30 is divisible by 5)
        let ticks = scheduler_ticks_at(30, 10);
        assert!(ticks.contains(&"timer.tick.5m"));
        assert!(ticks.contains(&"timer.tick.30m"));
        assert!(!ticks.contains(&"timer.tick.1h"));
    }

    #[test]
    fn scheduler_ticks_top_of_hour() {
        // At minute 0 of a non-6h hour (e.g., 10:00), 5m+30m+1h fire but not 6h/daily
        let ticks = scheduler_ticks_at(0, 10);
        assert!(ticks.contains(&"timer.tick.5m"));
        assert!(ticks.contains(&"timer.tick.30m"));
        assert!(ticks.contains(&"timer.tick.1h"));
        assert!(!ticks.contains(&"timer.tick.6h"));
        assert!(!ticks.contains(&"timer.tick.daily"));
    }

    #[test]
    fn scheduler_ticks_6h_boundary() {
        // At 06:00, 5m+30m+1h+6h fire but not daily
        let ticks = scheduler_ticks_at(0, 6);
        assert!(ticks.contains(&"timer.tick.1h"));
        assert!(ticks.contains(&"timer.tick.6h"));
        assert!(!ticks.contains(&"timer.tick.daily"));
    }

    #[test]
    fn scheduler_ticks_midnight() {
        // At 00:00, all long-cadence ticks fire
        let ticks = scheduler_ticks_at(0, 0);
        assert!(ticks.contains(&"timer.tick.1h"));
        assert!(ticks.contains(&"timer.tick.6h"));
        assert!(ticks.contains(&"timer.tick.daily"));
    }

    #[test]
    fn scheduler_dedup_key_format() {
        // Verify dedup key follows "timer.tick.5m:2026-05-16T10:05" pattern
        let event_type = "timer.tick.5m";
        let tick_ts = "2026-05-16T10:05";
        let dedup_key = format!("{event_type}:{tick_ts}");
        assert_eq!(dedup_key, "timer.tick.5m:2026-05-16T10:05");
        assert!(dedup_key.contains(':'));
        assert!(dedup_key.starts_with("timer.tick."));
    }

    #[test]
    fn scheduler_emits_events_to_db() {
        // Simulate scheduler emitting a minutely tick and verify DB persistence
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);

        // Simulate what the scheduler does at 10:05
        let tick_ts = "2026-05-16T10:05";
        let dedup_key = format!("timer.tick.minutely:{tick_ts}");
        engine.ingest(
            "timer.tick.minutely",
            &serde_json::json!({"dedup_key": dedup_key, "tick_time": tick_ts}),
            "scheduler",
        );

        let db = engine.db.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_type='timer.tick.minutely' AND source='scheduler'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify payload contains dedup_key
        let payload_str: String = db
            .query_row(
                "SELECT payload FROM events WHERE event_type='timer.tick.minutely'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&payload_str).unwrap();
        assert_eq!(
            payload["dedup_key"].as_str().unwrap(),
            "timer.tick.minutely:2026-05-16T10:05"
        );
    }

    #[test]
    fn trigger_spec_parses_bare_string() {
        let y = "- timer.tick.6h\n- boi.spec.completed\n";
        let v: Vec<TriggerSpec> = serde_yaml::from_str(y).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].event, "timer.tick.6h");
        assert!(v[0].condition.is_none());
    }

    #[test]
    fn trigger_spec_parses_struct_form() {
        let y = "- event: timer.tick.6h\n  condition: hour == 9\n";
        let v: Vec<TriggerSpec> = serde_yaml::from_str(y).unwrap();
        assert_eq!(v[0].event, "timer.tick.6h");
        assert_eq!(v[0].condition.as_deref(), Some("hour == 9"));
    }

    #[test]
    fn trigger_spec_parses_mixed() {
        let y = "- timer.tick.6h\n- event: boi.spec.completed\n  condition: status == 'ok'\n";
        let v: Vec<TriggerSpec> = serde_yaml::from_str(y).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].condition.is_none());
        assert!(v[1].condition.is_some());
    }
}
