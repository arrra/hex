//! On-disk store for the HITL queue: item TOML files, the append-only
//! `log.jsonl`, and `config.toml`.
//!
//! Every function takes `hex_dir: &Path` explicitly (same idiom as
//! `alert::notify_at`) so tests drive a tempdir and never touch the
//! process-global `$HEX_DIR`.
//!
//! Failure stance (S6 — no quiet failures): a MISSING `config.toml` means
//! "all defaults" and is fine; a MALFORMED one is a loud `Err`, never a
//! silent fallback to defaults. Same for item files — an unparseable item is
//! an error, not a skipped row.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Urgency class. Drives the ping policy (P1 re-pings, P2 deadline
/// escalation, P3 digest-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    P1,
    P2,
    P3,
}

impl Priority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Priority::P1 => "P1",
            Priority::P2 => "P2",
            Priority::P3 => "P3",
        }
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Priority {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "P1" => Ok(Priority::P1),
            "P2" => Ok(Priority::P2),
            "P3" => Ok(Priority::P3),
            other => Err(format!("invalid priority {other:?} (want P1|P2|P3)")),
        }
    }
}

/// Item lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Snoozed,
    Done,
    Skipped,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Snoozed => "snoozed",
            Status::Done => "done",
            Status::Skipped => "skipped",
        }
    }
    /// Open + snoozed are "live"; done + skipped are closed.
    pub fn is_closed(&self) -> bool {
        matches!(self, Status::Done | Status::Skipped)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ping mode: `immediate` pings on file; `batched` suppresses P2/P3 pings and
/// leans on the daily digest (P1 always pings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Immediate,
    Batched,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Immediate => "immediate",
            Mode::Batched => "batched",
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "immediate" => Ok(Mode::Immediate),
            "batched" => Ok(Mode::Batched),
            other => Err(format!("invalid mode {other:?} (want immediate|batched)")),
        }
    }
}

/// One queued human action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub title: String,
    pub project: String,
    /// Markdown: the exact steps + links the human needs, nothing implied.
    #[serde(default)]
    pub body: String,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est_minutes: Option<u32>,
    /// Ids this item is blocked by. Blocked items are never pinged
    /// individually — the digest flags them instead.
    #[serde(default)]
    pub depends_on: Vec<u64>,
    pub status: Status,
    pub created: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze_until: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pinged: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The caller-supplied half of a new item; `create` fills in id/status/created.
#[derive(Debug, Clone, Default)]
pub struct NewItem {
    pub title: String,
    pub project: String,
    pub body: String,
    pub priority: Option<Priority>,
    pub deadline: Option<NaiveDate>,
    pub est_minutes: Option<u32>,
    pub depends_on: Vec<u64>,
}

fn default_mode() -> Mode {
    Mode::Immediate
}
fn default_digest_hour() -> u32 {
    9
}
fn default_quiet_start() -> u32 {
    22
}
fn default_quiet_end() -> u32 {
    8
}
fn default_max_pings_per_day() -> u32 {
    3
}

/// `config.toml`. Every field has a spec-mandated default; a missing file means
/// all defaults, a malformed file is a loud error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_mode")]
    pub mode: Mode,
    /// iMessage destination (phone/email). Unset ⇒ transport degrades to
    /// `alert::notify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imessage_handle: Option<String>,
    #[serde(default = "default_digest_hour")]
    pub digest_hour: u32,
    #[serde(default = "default_quiet_start")]
    pub quiet_start: u32,
    #[serde(default = "default_quiet_end")]
    pub quiet_end: u32,
    /// Individual pings per calendar day; the digest is excluded from the cap.
    #[serde(default = "default_max_pings_per_day")]
    pub max_pings_per_day: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: default_mode(),
            imessage_handle: None,
            digest_hour: default_digest_hour(),
            quiet_start: default_quiet_start(),
            quiet_end: default_quiet_end(),
            max_pings_per_day: default_max_pings_per_day(),
        }
    }
}

/// One `log.jsonl` row: every state transition and every ping actually sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<u64>,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub fn hitl_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex").join("hitl")
}
pub fn items_dir(hex_dir: &Path) -> PathBuf {
    hitl_dir(hex_dir).join("items")
}
pub fn item_path(hex_dir: &Path, id: u64) -> PathBuf {
    items_dir(hex_dir).join(format!("{id}.toml"))
}
pub fn log_path(hex_dir: &Path) -> PathBuf {
    hitl_dir(hex_dir).join("log.jsonl")
}
pub fn config_path(hex_dir: &Path) -> PathBuf {
    hitl_dir(hex_dir).join("config.toml")
}
pub fn state_dir(hex_dir: &Path) -> PathBuf {
    hitl_dir(hex_dir).join("state")
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("hitl: cannot create {}: {e}", dir.display()))
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Missing file ⇒ defaults. Malformed file ⇒ loud `Err` (never silent defaults).
pub fn load_config(hex_dir: &Path) -> Result<Config, String> {
    let p = config_path(hex_dir);
    let raw = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(format!("hitl: cannot read {}: {e}", p.display())),
    };
    toml::from_str(&raw).map_err(|e| format!("hitl: malformed config {}: {e}", p.display()))
}

pub fn save_config(hex_dir: &Path, cfg: &Config) -> Result<(), String> {
    ensure_dir(&hitl_dir(hex_dir))?;
    let p = config_path(hex_dir);
    let body = toml::to_string_pretty(cfg).map_err(|e| format!("hitl: encode config: {e}"))?;
    std::fs::write(&p, body).map_err(|e| format!("hitl: cannot write {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// Every item on disk, ordered by id. Missing dir ⇒ empty queue.
pub fn load_items(hex_dir: &Path) -> Result<Vec<Item>, String> {
    let dir = items_dir(hex_dir);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("hitl: cannot read {}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| format!("hitl: cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        out.push(read_item_file(&path)?);
    }
    out.sort_by_key(|i| i.id);
    Ok(out)
}

/// One item by id. Absent ⇒ loud `Err` (the caller quoted an id that isn't there).
pub fn load_item(hex_dir: &Path, id: u64) -> Result<Item, String> {
    let p = item_path(hex_dir, id);
    if !p.exists() {
        return Err(format!("hitl: no item {id} (looked at {})", p.display()));
    }
    read_item_file(&p)
}

fn read_item_file(path: &Path) -> Result<Item, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("hitl: cannot read {}: {e}", path.display()))?;
    toml::from_str(&raw).map_err(|e| format!("hitl: malformed item {}: {e}", path.display()))
}

pub fn save_item(hex_dir: &Path, item: &Item) -> Result<(), String> {
    ensure_dir(&items_dir(hex_dir))?;
    let p = item_path(hex_dir, item.id);
    let body =
        toml::to_string_pretty(item).map_err(|e| format!("hitl: encode item {}: {e}", item.id))?;
    std::fs::write(&p, body).map_err(|e| format!("hitl: cannot write {}: {e}", p.display()))
}

/// Next sequential id: max existing + 1, or 1 for an empty queue.
pub fn next_id(hex_dir: &Path) -> Result<u64, String> {
    let dir = items_dir(hex_dir);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(1),
        Err(e) => return Err(format!("hitl: cannot read {}: {e}", dir.display())),
    };
    let mut max = 0u64;
    for entry in rd {
        let entry = entry.map_err(|e| format!("hitl: cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        if let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
        {
            max = max.max(id);
        }
    }
    Ok(max + 1)
}

/// Allocate an id, persist the item, log `created`.
pub fn create(hex_dir: &Path, new: NewItem, now: DateTime<Utc>) -> Result<Item, String> {
    if new.title.trim().is_empty() {
        return Err("hitl: --title is required and cannot be empty".to_string());
    }
    if new.project.trim().is_empty() {
        return Err("hitl: --project is required and cannot be empty".to_string());
    }
    let item = Item {
        id: next_id(hex_dir)?,
        title: new.title,
        project: new.project,
        body: new.body,
        priority: new.priority.unwrap_or(Priority::P2),
        deadline: new.deadline,
        est_minutes: new.est_minutes,
        depends_on: new.depends_on,
        status: Status::Open,
        created: now,
        snooze_until: None,
        last_pinged: None,
        closed_at: None,
        note: None,
    };
    save_item(hex_dir, &item)?;
    append_log(
        hex_dir,
        now,
        Some(item.id),
        "created",
        Some(format!(
            "[{}] {} ({})",
            item.priority, item.title, item.project
        )),
    )?;
    Ok(item)
}

/// Close an item as `done` or `skipped`. Idempotent-ish: re-closing an already
/// closed item is a loud error rather than a silent no-op.
pub fn close(
    hex_dir: &Path,
    id: u64,
    status: Status,
    note: Option<String>,
    now: DateTime<Utc>,
) -> Result<Item, String> {
    if !status.is_closed() {
        return Err(format!("hitl: {status} is not a closing status"));
    }
    let mut item = load_item(hex_dir, id)?;
    if item.status.is_closed() {
        return Err(format!(
            "hitl: item {id} is already {} (closed {})",
            item.status,
            item.closed_at
                .map(|t| t.to_rfc3339_opts(SecondsFormat::Secs, true))
                .unwrap_or_else(|| "?".into())
        ));
    }
    item.status = status;
    item.closed_at = Some(now);
    if note.is_some() {
        item.note = note;
    }
    save_item(hex_dir, &item)?;
    append_log(hex_dir, now, Some(id), status.as_str(), item.note.clone())?;
    Ok(item)
}

/// Snooze until `until` (exclusive of pings/digest until that date passes).
pub fn snooze(
    hex_dir: &Path,
    id: u64,
    until: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Item, String> {
    let mut item = load_item(hex_dir, id)?;
    if item.status.is_closed() {
        return Err(format!(
            "hitl: item {id} is {} — cannot snooze",
            item.status
        ));
    }
    item.status = Status::Snoozed;
    item.snooze_until = Some(until);
    save_item(hex_dir, &item)?;
    append_log(hex_dir, now, Some(id), "snoozed", Some(until.to_string()))?;
    Ok(item)
}

/// Wake a snoozed item back to open (policy treats a lapsed snooze as open;
/// this makes that durable once a nudge run observes it).
pub fn reopen(hex_dir: &Path, id: u64, now: DateTime<Utc>) -> Result<Item, String> {
    let mut item = load_item(hex_dir, id)?;
    item.status = Status::Open;
    item.snooze_until = None;
    item.closed_at = None;
    save_item(hex_dir, &item)?;
    append_log(hex_dir, now, Some(id), "reopened", None)?;
    Ok(item)
}

/// Stamp `last_pinged` after a ping is actually sent.
pub fn mark_pinged(hex_dir: &Path, id: u64, now: DateTime<Utc>) -> Result<Item, String> {
    let mut item = load_item(hex_dir, id)?;
    item.last_pinged = Some(now);
    save_item(hex_dir, &item)?;
    Ok(item)
}

// ---------------------------------------------------------------------------
// Log
// ---------------------------------------------------------------------------

/// Append one row to `log.jsonl` (created if absent).
pub fn append_log(
    hex_dir: &Path,
    now: DateTime<Utc>,
    item_id: Option<u64>,
    event: &str,
    detail: Option<String>,
) -> Result<(), String> {
    use std::io::Write;
    ensure_dir(&hitl_dir(hex_dir))?;
    let entry = LogEntry {
        ts: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        item_id,
        event: event.to_string(),
        detail,
    };
    let line = serde_json::to_string(&entry).map_err(|e| format!("hitl: encode log entry: {e}"))?;
    let p = log_path(hex_dir);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .map_err(|e| format!("hitl: cannot open {}: {e}", p.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("hitl: cannot append {}: {e}", p.display()))
}

/// Read the log back (used by tests and `hex hitl` diagnostics).
pub fn read_log(hex_dir: &Path) -> Result<Vec<LogEntry>, String> {
    let p = log_path(hex_dir);
    let raw = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("hitl: cannot read {}: {e}", p.display())),
    };
    let mut out = Vec::new();
    for (n, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .map_err(|e| format!("hitl: malformed {}:{}: {e}", p.display(), n + 1))?,
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn new_item(title: &str) -> NewItem {
        NewItem {
            title: title.to_string(),
            project: "studio".to_string(),
            body: "1. do the thing\n2. https://example.com".to_string(),
            priority: Some(Priority::P1),
            ..Default::default()
        }
    }

    #[test]
    fn next_id_starts_at_one_then_increments() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        assert_eq!(next_id(hex).unwrap(), 1);

        let a = create(hex, new_item("first"), ts("2026-07-22T10:00:00Z")).unwrap();
        assert_eq!(a.id, 1);
        assert_eq!(next_id(hex).unwrap(), 2);

        let b = create(hex, new_item("second"), ts("2026-07-22T10:05:00Z")).unwrap();
        assert_eq!(b.id, 2);
        assert_eq!(next_id(hex).unwrap(), 3);

        // Ids are max+1, not count+1 — a hole must not hand out a used id.
        std::fs::remove_file(item_path(hex, 1)).unwrap();
        assert_eq!(next_id(hex).unwrap(), 3);
    }

    #[test]
    fn create_persists_and_logs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        let item = create(
            hex,
            new_item("sign the LLC docs"),
            ts("2026-07-22T10:00:00Z"),
        )
        .unwrap();

        assert!(item_path(hex, 1).exists());
        let loaded = load_item(hex, 1).unwrap();
        assert_eq!(loaded, item);
        assert_eq!(loaded.status, Status::Open);
        assert_eq!(loaded.priority, Priority::P1);
        assert!(loaded.last_pinged.is_none());

        let log = read_log(hex).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].event, "created");
        assert_eq!(log[0].item_id, Some(1));
        assert_eq!(log[0].ts, "2026-07-22T10:00:00Z");
    }

    #[test]
    fn roundtrip_all_optionals_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        let item = Item {
            id: 7,
            title: "wire the retainer".into(),
            project: "studio".into(),
            body: "* open bank\n* send $5k".into(),
            priority: Priority::P2,
            deadline: Some(day(2026, 8, 1)),
            est_minutes: Some(15),
            depends_on: vec![1, 2],
            status: Status::Snoozed,
            created: Utc.with_ymd_and_hms(2026, 7, 22, 9, 0, 0).unwrap(),
            snooze_until: Some(day(2026, 7, 30)),
            last_pinged: Some(ts("2026-07-22T11:00:00Z")),
            closed_at: Some(ts("2026-07-23T11:00:00Z")),
            note: Some("waiting on counsel".into()),
        };
        save_item(hex, &item).unwrap();
        assert_eq!(load_item(hex, 7).unwrap(), item);

        // The on-disk shape is the documented schema, not serde internals.
        let raw = std::fs::read_to_string(item_path(hex, 7)).unwrap();
        assert!(raw.contains("priority = \"P2\""), "{raw}");
        assert!(raw.contains("status = \"snoozed\""), "{raw}");
        assert!(raw.contains("deadline = \"2026-08-01\""), "{raw}");
        assert!(raw.contains("depends_on = ["), "{raw}");
        assert!(raw.contains("snooze_until = \"2026-07-30\""), "{raw}");
        assert!(raw.contains("created = \"2026-07-22T09:00:00Z\""), "{raw}");
    }

    #[test]
    fn roundtrip_all_optionals_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        let item = Item {
            id: 3,
            title: "renew domain".into(),
            project: "ops".into(),
            body: String::new(),
            priority: Priority::P3,
            deadline: None,
            est_minutes: None,
            depends_on: vec![],
            status: Status::Open,
            created: ts("2026-07-22T10:00:00Z"),
            snooze_until: None,
            last_pinged: None,
            closed_at: None,
            note: None,
        };
        save_item(hex, &item).unwrap();
        let raw = std::fs::read_to_string(item_path(hex, 3)).unwrap();
        assert!(!raw.contains("deadline"), "{raw}");
        assert_eq!(load_item(hex, 3).unwrap(), item);
    }

    #[test]
    fn load_items_sorted_and_empty_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        assert!(load_items(hex).unwrap().is_empty());

        for n in ["a", "b", "c"] {
            create(hex, new_item(n), ts("2026-07-22T10:00:00Z")).unwrap();
        }
        let ids: Vec<u64> = load_items(hex).unwrap().iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn malformed_item_is_loud() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        ensure_dir(&items_dir(hex)).unwrap();
        std::fs::write(item_path(hex, 1), "this is not toml {{{").unwrap();
        let err = load_items(hex).unwrap_err();
        assert!(err.contains("malformed item"), "{err}");
        assert!(load_item(hex, 1).is_err());
    }

    #[test]
    fn missing_item_is_loud() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = load_item(tmp.path(), 42).unwrap_err();
        assert!(err.contains("no item 42"), "{err}");
    }

    #[test]
    fn config_missing_yields_spec_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = load_config(tmp.path()).unwrap();
        assert_eq!(cfg.mode, Mode::Immediate);
        assert_eq!(cfg.imessage_handle, None);
        assert_eq!(cfg.digest_hour, 9);
        assert_eq!(cfg.quiet_start, 22);
        assert_eq!(cfg.quiet_end, 8);
        assert_eq!(cfg.max_pings_per_day, 3);
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn config_partial_fills_gaps_with_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        ensure_dir(&hitl_dir(hex)).unwrap();
        std::fs::write(
            config_path(hex),
            "mode = \"batched\"\nimessage_handle = \"+15551234567\"\n",
        )
        .unwrap();
        let cfg = load_config(hex).unwrap();
        assert_eq!(cfg.mode, Mode::Batched);
        assert_eq!(cfg.imessage_handle.as_deref(), Some("+15551234567"));
        // Gaps fill with the spec defaults, NOT serde's zero values.
        assert_eq!(cfg.digest_hour, 9);
        assert_eq!(cfg.quiet_start, 22);
        assert_eq!(cfg.quiet_end, 8);
        assert_eq!(cfg.max_pings_per_day, 3);
    }

    #[test]
    fn config_malformed_is_loud_not_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        ensure_dir(&hitl_dir(hex)).unwrap();
        std::fs::write(config_path(hex), "mode = \"sometimes\"\n").unwrap();
        let err = load_config(hex).unwrap_err();
        assert!(err.contains("malformed config"), "{err}");
    }

    #[test]
    fn config_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        let cfg = Config {
            mode: Mode::Batched,
            imessage_handle: Some("mike@example.com".into()),
            digest_hour: 7,
            quiet_start: 21,
            quiet_end: 6,
            max_pings_per_day: 5,
        };
        save_config(hex, &cfg).unwrap();
        assert_eq!(load_config(hex).unwrap(), cfg);
    }

    #[test]
    fn close_done_sets_fields_and_logs_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        create(hex, new_item("sign"), ts("2026-07-22T10:00:00Z")).unwrap();

        let closed = close(
            hex,
            1,
            Status::Done,
            Some("signed via DocuSign".into()),
            ts("2026-07-22T12:00:00Z"),
        )
        .unwrap();
        assert_eq!(closed.status, Status::Done);
        assert_eq!(closed.closed_at, Some(ts("2026-07-22T12:00:00Z")));
        assert_eq!(closed.note.as_deref(), Some("signed via DocuSign"));
        assert_eq!(load_item(hex, 1).unwrap(), closed);

        let log = read_log(hex).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[1].event, "done");
        assert_eq!(log[1].detail.as_deref(), Some("signed via DocuSign"));

        // Double-close is loud, and appends no second row.
        assert!(close(hex, 1, Status::Done, None, ts("2026-07-22T13:00:00Z")).is_err());
        assert_eq!(read_log(hex).unwrap().len(), 2);
    }

    #[test]
    fn close_skip_sets_skipped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        create(hex, new_item("kyc"), ts("2026-07-22T10:00:00Z")).unwrap();
        let it = close(hex, 1, Status::Skipped, None, ts("2026-07-22T12:00:00Z")).unwrap();
        assert_eq!(it.status, Status::Skipped);
        assert!(it.status.is_closed());
        assert_eq!(read_log(hex).unwrap()[1].event, "skipped");
    }

    #[test]
    fn close_rejects_non_closing_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        create(hex, new_item("x"), ts("2026-07-22T10:00:00Z")).unwrap();
        assert!(close(hex, 1, Status::Open, None, ts("2026-07-22T12:00:00Z")).is_err());
    }

    #[test]
    fn snooze_sets_until_and_logs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        create(hex, new_item("pay invoice"), ts("2026-07-22T10:00:00Z")).unwrap();

        let it = snooze(hex, 1, day(2026, 7, 30), ts("2026-07-22T12:00:00Z")).unwrap();
        assert_eq!(it.status, Status::Snoozed);
        assert_eq!(it.snooze_until, Some(day(2026, 7, 30)));
        let log = read_log(hex).unwrap();
        assert_eq!(log[1].event, "snoozed");
        assert_eq!(log[1].detail.as_deref(), Some("2026-07-30"));

        // Reopen clears the snooze.
        let it = reopen(hex, 1, ts("2026-07-31T09:00:00Z")).unwrap();
        assert_eq!(it.status, Status::Open);
        assert_eq!(it.snooze_until, None);
        assert_eq!(read_log(hex).unwrap()[2].event, "reopened");

        // Closed items cannot be snoozed.
        close(hex, 1, Status::Done, None, ts("2026-07-31T10:00:00Z")).unwrap();
        assert!(snooze(hex, 1, day(2026, 8, 5), ts("2026-07-31T11:00:00Z")).is_err());
    }

    #[test]
    fn mark_pinged_stamps_last_pinged_without_log_row() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        create(hex, new_item("grant access"), ts("2026-07-22T10:00:00Z")).unwrap();
        let it = mark_pinged(hex, 1, ts("2026-07-22T10:01:00Z")).unwrap();
        assert_eq!(it.last_pinged, Some(ts("2026-07-22T10:01:00Z")));
        assert_eq!(load_item(hex, 1).unwrap().last_pinged, it.last_pinged);
        // The ping itself is logged by transport (one row per send attempt),
        // so the stamp alone must not double-log.
        assert_eq!(read_log(hex).unwrap().len(), 1);
    }

    #[test]
    fn append_log_is_append_only_jsonl() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        append_log(
            hex,
            ts("2026-07-22T10:00:00Z"),
            Some(1),
            "ping",
            Some("imessage".into()),
        )
        .unwrap();
        append_log(hex, ts("2026-07-22T11:00:00Z"), None, "digest", None).unwrap();
        let raw = std::fs::read_to_string(log_path(hex)).unwrap();
        assert_eq!(raw.lines().count(), 2);
        let log = read_log(hex).unwrap();
        assert_eq!(log[0].event, "ping");
        assert_eq!(log[0].detail.as_deref(), Some("imessage"));
        assert_eq!(log[1].item_id, None);
        assert_eq!(log[1].event, "digest");
    }

    #[test]
    fn create_rejects_empty_title_or_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        let mut bad = new_item("  ");
        assert!(create(hex, bad.clone(), ts("2026-07-22T10:00:00Z")).is_err());
        bad.title = "ok".into();
        bad.project = String::new();
        assert!(create(hex, bad, ts("2026-07-22T10:00:00Z")).is_err());
        assert!(load_items(hex).unwrap().is_empty());
    }

    #[test]
    fn priority_and_mode_parse_from_str() {
        use std::str::FromStr;
        assert_eq!(Priority::from_str("p1").unwrap(), Priority::P1);
        assert_eq!(Priority::from_str("P3").unwrap(), Priority::P3);
        assert!(Priority::from_str("P9").is_err());
        assert_eq!(Mode::from_str("Batched").unwrap(), Mode::Batched);
        assert!(Mode::from_str("hourly").is_err());
        // P1 sorts before P2 before P3 — the digest and cap ordering depend on it.
        assert!(Priority::P1 < Priority::P2 && Priority::P2 < Priority::P3);
    }
}
