# `hex-resources` — Resource Sampling (Tiers 0+1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An hourly sampler that records disk state (df + watched-directory sizes) as durable telemetry rows, evaluates deterministic floor/trend rules over that history, and on breach alerts loudly + emits a level-triggered `resource.pressure` event. No subscribers in v1 (tier-2 routing is staged until the first real pressure event); no remediation ever.

**Architecture:** New `src/resources.rs` library module (df/du sampling, JSON sample rows into events.db, rule evaluation over history) + `hex resources sample|status` CLI + thin hourly `resources.worker.rs` cron stub. Pressure events go out via the existing `ops::emit` path (`state::set scope=events`), re-emitted on every evaluation while the rule remains in breach (level-triggered — at-most-once delivery means a single edge emit can vanish); `hex::alert::notify`'s 6h dedup caps human-facing noise.

**Tech Stack:** Rust (harness crate at `system/harness/`), rusqlite, chrono, serde_json, clap. Tests: `#[cfg(test)]` in-module; `telemetry::test_support::isolate()` for anything touching `HEX_DIR`.

**Context paths (read before starting):**
- Proposal: `$HEX_DIR/projects/hex-ops/proposals/telemetry-consumption-layer-2026-06-11.md` (v2 — C2 section)
- Telemetry store: `system/harness/src/telemetry/mod.rs` (`record_loud`, `TelemetryEvent`, events table)
- Emit path: `system/harness/src/ops.rs` (~line 33: `emit_target` — scope="events", key=event, envelope `{event,producer,ts,data}`) and `main.rs` `TriggersCommands::Emit` (~line 642) for the CLI form
- Alert pathway: `system/harness/src/alert.rs` (`notify`, 6h dedupe per key)
- Worker stub pattern: `system/harness/src/modules/freshness.worker.rs`
- Measured costs (from research, 2026-06-11): `df -k /` ≈ 4ms; `du -sk` over the 15-dir watch-list ≈ 13–14s; full `$HOME` top-level pass ≈ 72s. `du` under-reports docker logical sizes ~300x — docker probing is on-pressure only and gated on OrbStack actually running.

**NOTE on trigger naming:** a sibling plan (`2026-06-11-hex-failures-detection.md`) adds `.on_cron_named` to the Worker builder. This plan's worker uses plain `.on_cron` so the two branches stay independent — the merge commit may switch it to `.on_cron_named("hourly", …)`; note that in your report.

**Verification baseline:** before Task 1, `cd system/harness && cargo test` must be green. STOP conditions: code ≠ plan's description (report drift); a verification fails twice; a fix wants files outside `system/harness/` + `docs/`.

---

### Task 1: `resources.rs` — watch-list, df, du sampling

**Files:**
- Create: `system/harness/src/resources.rs`
- Modify: the crate's module-declaration file (`grep -rn "pub mod alert" system/harness/src/` — add `pub mod resources;` beside it)

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// df -k output fixture (macOS shape) → free/used GB.
    #[test]
    fn parses_df_output() {
        let fixture = "Filesystem   1024-blocks       Used  Available Capacity iused ifree %iused  Mounted on\n/dev/disk3s1s1  1942700360  248000000 1536000000    14%  500000 4294467295    0%   /\n";
        let d = parse_df(fixture).unwrap();
        assert_eq!(d.free_gb, 1465); // 1536000000 KiB / 1048576 ≈ 1464.8 → trunc
        assert_eq!(d.used_gb, 236);
    }

    /// du over a tempdir returns a size; missing dirs are skipped (None), not errors.
    #[test]
    fn du_sizes_tolerate_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("f"), vec![0u8; 1024 * 100]).unwrap();
        let sizes = du_sizes(&[
            tmp.path().to_string_lossy().into_owned(),
            "/nonexistent/definitely/missing".to_string(),
        ]);
        assert!(sizes.contains_key(tmp.path().to_string_lossy().as_ref()));
        assert!(!sizes.contains_key("/nonexistent/definitely/missing"));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cd system/harness && cargo test resources:: 2>&1 | tail -10` → FAIL (module missing).

- [ ] **Step 3: Implement**

```rust
//! `hex-resources` — disk/resource sampling (tier 0) + deterministic pressure
//! rules (tier 1). Detection + emission only: NEVER cleans anything up
//! (proposal: telemetry-consumption-layer v2, C2).

use std::collections::BTreeMap;

/// Watched directories — hardcoded const, seeded from the 2026-06-11 cruft
/// survey offenders. Promote to a config file only after edits prove churn
/// (review: a config file means format+parser+validation for a ~quarterly
/// list). `~` is expanded against $HOME at runtime.
pub const WATCH_LIST: &[&str] = &[
    "~/github.com/mrap/boi/target",
    "~/github.com/mrap/hex-foundation/target",
    "~/hex/target",
    "~/hex/.hex/harness/target",
    "~/hex/raw",
    "~/.boi/v2",
    "~/worktrees",
    "~/Library/pnpm",
    "~/.npm",
    "~/.iii/cache",
    "~/.claude",
];

/// Floor: alert + pressure when root free space drops below this.
pub const FLOOR_FREE_GB: i64 = 150;
/// Trend: alert + pressure when a watched dir grows more than this across
/// the trend window.
pub const TREND_GROWTH_GB: i64 = 20;
pub const TREND_WINDOW_HOURS: i64 = 72;
/// Re-run the (13s) du pass when this much free space vanished since the
/// last du sample, else every DU_INTERVAL_HOURS.
pub const DU_DELTA_GB: i64 = 30;
pub const DU_INTERVAL_HOURS: i64 = 6;

pub fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => p.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DfSample {
    pub free_gb: i64,
    pub used_gb: i64,
}

/// Parse `df -k /` output (KiB blocks → GB, truncating).
pub fn parse_df(out: &str) -> Option<DfSample> {
    let line = out.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let used_kb: i64 = cols.get(2)?.parse().ok()?;
    let free_kb: i64 = cols.get(3)?.parse().ok()?;
    Some(DfSample { free_gb: free_kb / 1_048_576, used_gb: used_kb / 1_048_576 })
}

pub fn sample_df() -> Option<DfSample> {
    let out = std::process::Command::new("df").args(["-k", "/"]).output().ok()?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// `du -sk` per watched dir (expanded), GB truncating. Missing/unreadable
/// dirs are skipped silently — a watched dir that was cleaned up is normal.
pub fn du_sizes(dirs: &[String]) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for d in dirs {
        if !std::path::Path::new(d).exists() {
            continue;
        }
        let Ok(o) = std::process::Command::new("du").args(["-sk", d]).output() else {
            continue;
        };
        let s = String::from_utf8_lossy(&o.stdout);
        if let Some(kb) = s.split_whitespace().next().and_then(|v| v.parse::<i64>().ok()) {
            out.insert(d.clone(), kb / 1_048_576);
        }
    }
    out
}

/// Persist samples as telemetry rows (durable trend history).
/// df row every call: event=sample::df, detail {"free_gb":N,"used_gb":N}.
/// du row when taken:  event=sample::du, detail {"<dir>":gb,...}.
pub fn record_df(d: &DfSample) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "hex-resources".into(),
        event: "sample::df".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(
            serde_json::json!({ "free_gb": d.free_gb, "used_gb": d.used_gb }).to_string(),
        ),
    });
}

pub fn record_du(sizes: &BTreeMap<String, i64>) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "hex-resources".into(),
        event: "sample::du".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: serde_json::to_string(sizes).ok(),
    });
}
```

- [ ] **Step 4: Run** — `cargo test resources:: 2>&1 | tail -5` → PASS. (If the df fixture math is off-by-one from your implementation's truncation, recompute the fixture's expected values from the implementation's exact arithmetic — integer division semantics — and fix the TEST fixture numbers, since they encode arithmetic, not behavior.)
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(resources): watch-list + df/du sampling into telemetry rows"`

---

### Task 2: Rule evaluation over history

**Files:**
- Modify: `system/harness/src/resources.rs`
- Modify (if not already present from the sibling plan): `system/harness/src/telemetry/mod.rs` — add `open_ro()`:

```rust
/// Read-only connection for consumers. Plain read-only on a WAL db reads
/// checkpointed + WAL frames; `immutable=1` would silently miss the WAL —
/// never use it here.
pub fn open_ro() -> rusqlite::Result<Connection> {
    let path = db_path().map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("telemetry: cannot resolve db path: {e}"),
        )))
    })?;
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
}
```

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod rule_tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    fn seed_row(event: &str, ts: chrono::DateTime<Utc>, detail: &str) {
        // schema first
        crate::telemetry::record(&crate::telemetry::TelemetryEvent {
            source: "seed".into(), event: "seed".into(), status: "ok".into(),
            duration_ms: None, exit_code: None, detail: None }).unwrap();
        let conn = rusqlite::Connection::open(
            std::path::PathBuf::from(std::env::var("HEX_DIR").unwrap())
                .join(".hex/telemetry/events.db")).unwrap();
        conn.execute(
            "INSERT INTO events (ts, source, event, status, detail) VALUES (?1,'hex-resources',?2,'ok',?3)",
            rusqlite::params![ts.to_rfc3339(), event, detail]).unwrap();
    }

    #[test]
    fn floor_breach_detected() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        let breaches = evaluate_rules(
            &DfSample { free_gb: 100, used_gb: 900 }, now).unwrap();
        assert!(breaches.iter().any(|b| matches!(b, Breach::Floor { free_gb: 100 })));
    }

    #[test]
    fn trend_breach_from_history() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        seed_row("sample::du", now - Duration::hours(70), r#"{"/x/target":5}"#);
        seed_row("sample::du", now - Duration::hours(1), r#"{"/x/target":40}"#);
        let breaches = evaluate_rules(
            &DfSample { free_gb: 999, used_gb: 1 }, now).unwrap();
        match breaches.iter().find(|b| matches!(b, Breach::Trend { .. })) {
            Some(Breach::Trend { dir, growth_gb, .. }) => {
                assert_eq!(dir, "/x/target");
                assert_eq!(*growth_gb, 35);
            }
            _ => panic!("expected trend breach: {breaches:?}"),
        }
    }

    #[test]
    fn no_breach_when_healthy() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        seed_row("sample::du", now - Duration::hours(70), r#"{"/x/target":5}"#);
        seed_row("sample::du", now - Duration::hours(1), r#"{"/x/target":6}"#);
        let breaches = evaluate_rules(
            &DfSample { free_gb: 999, used_gb: 1 }, now).unwrap();
        assert!(breaches.is_empty(), "{breaches:?}");
    }
}
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Breach {
    Floor { free_gb: i64 },
    Trend { dir: String, growth_gb: i64, window_hours: i64 },
}

/// Deterministic tier-1 rules over the current df sample + du history rows.
/// LEVEL-TRIGGERED by design: callers re-evaluate every sample tick and
/// re-emit while in breach (at-most-once event delivery means a single edge
/// emit can vanish; alert::notify's 6h dedupe caps human-facing noise).
pub fn evaluate_rules(
    df: &DfSample,
    now: chrono::DateTime<chrono::Utc>,
) -> rusqlite::Result<Vec<Breach>> {
    let mut out = Vec::new();
    if df.free_gb < FLOOR_FREE_GB {
        out.push(Breach::Floor { free_gb: df.free_gb });
    }
    // Trend: compare oldest du sample inside the window to the newest.
    let conn = crate::telemetry::open_ro()?;
    let since = (now - chrono::Duration::hours(TREND_WINDOW_HOURS)).to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT detail FROM events
         WHERE source='hex-resources' AND event='sample::du' AND ts >= ?1 AND detail IS NOT NULL
         ORDER BY ts",
    )?;
    let details: Vec<String> = stmt
        .query_map([&since], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    if details.len() >= 2 {
        let parse = |s: &str| -> BTreeMap<String, i64> {
            serde_json::from_str(s).unwrap_or_default()
        };
        let oldest = parse(&details[0]);
        let newest = parse(details.last().unwrap());
        for (dir, new_gb) in &newest {
            if let Some(old_gb) = oldest.get(dir) {
                let growth = new_gb - old_gb;
                if growth > TREND_GROWTH_GB {
                    out.push(Breach::Trend {
                        dir: dir.clone(),
                        growth_gb: growth,
                        window_hours: TREND_WINDOW_HOURS,
                    });
                }
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run** — PASS. **Step 5: Commit** — `git commit -am "feat(resources): floor + trend rule evaluation over durable history"`

---

### Task 3: `hex resources` CLI — sample, breach actions, on-pressure attribution

**Files:**
- Modify: `system/harness/src/main.rs`
- Modify: `system/harness/src/resources.rs`

- [ ] **Step 1: Implement the orchestration fn in resources.rs** (no new unit test — composition of tested parts + shell-outs; the worker smoke in Task 4 covers it)

```rust
/// One sampler tick. Policy:
/// - df every tick (4ms).
/// - du when none in the last DU_INTERVAL_HOURS OR free fell ≥ DU_DELTA_GB
///   since the last du tick (attribution data for the trend rule).
/// - On breach: alert (deduped) + emit resource.pressure (LEVEL-triggered:
///   re-emitted every tick while in breach) + on-pressure-only discovery
///   pass and docker probe (gated on OrbStack actually running — du
///   under-reports docker ~300x and `docker system df` can wake the VM).
pub fn sample_tick(now: chrono::DateTime<chrono::Utc>) -> Result<Vec<Breach>, String> {
    let df = sample_df().ok_or("df sample failed")?;
    record_df(&df);

    let conn = crate::telemetry::open_ro().map_err(|e| e.to_string())?;
    let last_du: Option<(String, String)> = conn
        .query_row(
            "SELECT ts, COALESCE(detail,'') FROM events
             WHERE source='hex-resources' AND event='sample::du'
             ORDER BY ts DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let last_df_free: Option<i64> = last_du.as_ref().and_then(|(ts, _)| {
        conn.query_row(
            "SELECT detail FROM events
             WHERE source='hex-resources' AND event='sample::df' AND ts <= ?1
             ORDER BY ts DESC LIMIT 1",
            [ts],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok())
        .and_then(|v| v["free_gb"].as_i64())
    });
    let du_due = match &last_du {
        None => true,
        Some((ts, _)) => chrono::DateTime::parse_from_rfc3339(ts)
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_hours() >= DU_INTERVAL_HOURS)
            .unwrap_or(true)
            || last_df_free.map_or(false, |prev| prev - df.free_gb >= DU_DELTA_GB),
    };
    if du_due {
        let dirs: Vec<String> = WATCH_LIST.iter().map(|d| expand_home(d)).collect();
        record_du(&du_sizes(&dirs));
    }

    let breaches = evaluate_rules(&df, now).map_err(|e| e.to_string())?;
    for b in &breaches {
        let (key_ident, msg, data) = match b {
            Breach::Floor { free_gb } => (
                "floor".to_string(),
                format!("root free space {free_gb}G < {FLOOR_FREE_GB}G floor"),
                serde_json::json!({ "category": "floor", "free_gb": free_gb }),
            ),
            Breach::Trend { dir, growth_gb, window_hours } => (
                format!("trend-{dir}"),
                format!("{dir} grew {growth_gb}G in {window_hours}h"),
                serde_json::json!({ "category": "trend", "path": dir,
                    "growth_gb": growth_gb, "window_hours": window_hours }),
            ),
        };
        crate::alert::notify(
            &crate::failures::alert_key("resource", &key_ident),
            "resource pressure",
            &msg,
        );
        // Level-triggered emission. VERIFY ops::emit's exact signature first
        // (src/ops.rs ~:33 and the TriggersCommands::Emit call site in
        // main.rs ~:642) and adapt this call — if it is not directly callable
        // from here, shell out instead:
        //   hex triggers emit resource.pressure --producer hex-resources --data '<json>'
        if let Err(e) = crate::ops::emit("resource.pressure", "hex-resources", data.clone()) {
            eprintln!("resources: pressure emit failed (engine down?): {e}");
        }
    }
    if !breaches.is_empty() {
        // On-pressure attribution: discovery pass over $HOME top-level (find
        // NEW offenders — survey lesson: they shift) + docker logical sizes,
        // only if OrbStack is already running (never wake the VM).
        if let Ok(home) = std::env::var("HOME") {
            let tops: Vec<String> = std::fs::read_dir(&home)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.path().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            record_du(&du_sizes(&tops));
        }
        let orb_running = std::process::Command::new("pgrep")
            .args(["-x", "OrbStack"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if orb_running {
            if let Ok(o) = std::process::Command::new("docker").args(["system", "df"]).output() {
                crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                    source: "hex-resources".into(),
                    event: "sample::docker".into(),
                    status: "ok".into(),
                    duration_ms: None,
                    exit_code: None,
                    detail: Some(String::from_utf8_lossy(&o.stdout).lines()
                        .collect::<Vec<_>>().join(" | ")),
                });
            }
        }
    }
    Ok(breaches)
}
```

If the sibling plan's `failures::alert_key` isn't on this branch, inline the same sanitizer as a private fn `alert_key(kind, ident)` here with a comment to dedupe at merge.

- [ ] **Step 2: CLI wiring in main.rs**

```rust
/// Resource sampling (tier 0) + pressure rules (tier 1). Detection only.
#[command(display_order = 6)]
Resources {
    #[command(subcommand)]
    command: ResourcesCommands,
},
```

```rust
#[derive(Subcommand)]
enum ResourcesCommands {
    /// One sampler tick: df (+du when due), evaluate rules, alert+emit on breach.
    Sample,
    /// Print the latest df/du samples and any current breaches.
    Status,
}
```

Handlers: `Sample` → `hex::resources::sample_tick(chrono::Utc::now())`, print breaches one per line, exit 1 if any (loud), 0 if clean, 2 on Err. `Status` → query the newest `sample::df` and `sample::du` rows via `telemetry::open_ro()`, pretty-print detail JSON, then run `evaluate_rules` on the latest df values and print breaches.

- [ ] **Step 3: Build + live smoke (read-only emit-free check)**

Run: `cd system/harness && cargo build 2>&1 | tail -3` → compiles.
Run: `HEX_DIR=$HEX_DIR ./target/debug/hex resources sample; echo "exit=$?"` (binary under `$CARGO_TARGET_DIR/debug/` if set)
Expected: exit 0, no breaches on the freshly-cleaned disk; TWO new rows in events.db (`sample::df` + first-ever `sample::du`). Verify: `HEX_DIR=$HEX_DIR ./target/debug/hex resources status`.
NOTE: this writes 2 telemetry rows + may take ~14s (first du pass) — acceptable; say so in the report.

- [ ] **Step 4: Full suite** — `cargo test 2>&1 | tail -5` → PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(resources): hex resources sample|status — tiered sampling, level-triggered pressure"`

---

### Task 4: Hourly worker stub

**Files:**
- Create: `system/harness/src/modules/resources.worker.rs`

- [ ] **Step 1: Implement** (mirror freshness.worker.rs imports exactly)

```rust
//! `hex-resources` — hourly disk/resource sampler (tier 0+1).
//! Detection + emission only; never cleans anything. Subscribers to
//! resource.pressure are staged until the first real pressure event
//! (proposal: telemetry-consumption-layer v2, C2 tier-2).

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Hourly, on the half-hour offset to avoid colliding with the
/// hex-reconciler's on-the-hour cron.
pub const CRON_HOURLY: &str = "0 30 * * * * *";

fn run_sample(_e: Event, ctx: Ctx) -> Result<()> {
    ctx.run(&["hex".to_string(), "resources".to_string(), "sample".to_string()])
        .map(|_| ())
}

pub fn worker() -> Worker {
    Worker::new("hex-resources").on_cron(CRON_HOURLY, run_sample)
}
```

NOTE: worker name `hex-resources` matches the telemetry `source` used by the CLI rows — auto-trace rows for the worker fire land as `hex-resources::0` (event) which does not collide with `sample::df`/`sample::du` (different event names, same source). Say this in a comment.

- [ ] **Step 2: Suite** — `cargo test 2>&1 | tail -5` → PASS (fix any registry-count assertions).
- [ ] **Step 3: Commit** — `git commit -am "feat(resources): hourly sampler worker"`

---

### Task 5: Docs + final gates

- [ ] **Step 1:** `docs/hex-ops.md`: add a Resources row near the Telemetry row (one line, match surrounding format).
- [ ] **Step 2:** Full suite + release build: `cargo test 2>&1 | tail -3` → PASS; `cargo build --release 2>&1 | tail -3` → compiles.
- [ ] **Step 3: Synthetic breach exit gate** (temp HEX_DIR so live telemetry stays clean):

```bash
export TEST_HEX=$(mktemp -d)
HEX_DIR=$TEST_HEX ./target/release/hex resources sample
# seed a fake 3-day-old du row showing /tmp tiny, then a fresh huge one:
sqlite3 $TEST_HEX/.hex/telemetry/events.db \
  "INSERT INTO events (ts,source,event,status,detail) VALUES
   (datetime('now','-70 hours'),'hex-resources','sample::du','ok','{\"/tmp/growzone\":1}'),
   (datetime('now','-1 hour'),'hex-resources','sample::du','ok','{\"/tmp/growzone\":99}');"
HEX_DIR=$TEST_HEX ./target/release/hex resources sample; echo "exit=$?"
```

Expected: trend breach printed for `/tmp/growzone` (+98G), exit 1, alert line on stderr; emit may fail loudly if the engine rejects (fine — the emit failure must be LOUD, not silent). Clean up: `rm -rf $TEST_HEX`.

- [ ] **Step 4: Commit + report** — `git commit -am "docs: hex resources row"`; report branch, test counts, smoke outputs, deviations.
