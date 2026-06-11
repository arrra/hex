# `hex failures` — Unexpected-Failure Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A daily-scheduled detector + on-demand `hex failures` CLI that surfaces unexpected failures from the telemetry store: runs that failed, runs that silently didn't happen, modules on disk that never landed in the binary, and harness downtime — detection only, never remediation.

**Architecture:** A new `src/failures.rs` library module (pure evaluation logic, unit-testable with injected `now`) + a new `Failures` CLI subcommand in `main.rs` + a thin `failures.worker.rs` daily cron stub + an out-of-process probe (`hex failures probe`) on its own launchd schedule. Expectations come from the worker registry (`workers::registry()`); actuals come from `$HEX_DIR/.hex/telemetry/events.db`. A prerequisite refactor adds *named triggers* so history keys on stable ids instead of positional indexes.

**Tech Stack:** Rust (harness crate at `system/harness/`), rusqlite, chrono, `cron = "=0.15.0"` (new direct dep — must match the engine fork's version), clap. Tests follow the crate's existing pattern: `#[cfg(test)]` in-module, `telemetry::test_support::isolate()` for anything touching `HEX_DIR`.

**Context paths (read before starting):**
- Proposal: `/Users/mrap/hex/projects/hex-ops/proposals/telemetry-consumption-layer-2026-06-11.md` (v2, review-hardened)
- Trigger/fid mechanics: `system/harness/src/worker/mod.rs`, `system/harness/src/worker/runtime.rs:118-213`
- Telemetry store API: `system/harness/src/telemetry/mod.rs` (events table: ts, source, event, status, duration_ms, exit_code, detail; WAL)
- Disabled modules: `system/harness/src/module_state.rs` (`disabled_set(hex_dir)`)
- Alert pathway: `system/harness/src/alert.rs` (`notify_at(hex_dir, key, title, msg)`, 6h dedupe per key)
- Pattern for cron-stub workers: `system/harness/src/modules/freshness.worker.rs`, `backup.worker.rs`

**Verification baseline:** before Task 1, run `cd system/harness && cargo test` — must be green. If not, STOP and report.

**STOP conditions:** code ≠ this plan's description of it (re-verify against live code, report the drift); a step's verification fails twice; a fix wants a file outside `system/harness/` + `docs/`.

---

### Task 1: Named triggers — stable fids

Positional fids (`{worker}::{idx}`) mis-attribute history when handlers are reordered (it already happened: commit `09f6fb8b`). Add an optional name carried with each handler; fid = `{worker}::{name}` when named, `{worker}::{idx}` fallback (backward compatible — instance overlay modules keep working unchanged).

**Files:**
- Modify: `system/harness/src/worker/mod.rs`
- Modify: `system/harness/src/worker/runtime.rs:121-124`
- Test: in-module `#[cfg(test)]` in `worker/mod.rs`

- [ ] **Step 1: Write the failing tests** (append to `worker/mod.rs` tests module)

```rust
/// Named cron triggers carry their name; fid derivation uses it.
#[test]
fn on_cron_named_carries_name() {
    let w = Worker::new("hex-test").on_cron_named("nightly", "0 0 3 * * * *", noop);
    let (name, spec, _h) = w.handlers.into_iter().next().expect("one handler");
    assert_eq!(name.as_deref(), Some("nightly"));
    assert_eq!(spec, TriggerSpec::Cron { expression: "0 0 3 * * * *".to_string() });
}

/// fid_for: named → worker::name; unnamed → worker::idx (legacy fallback).
#[test]
fn fid_for_named_and_positional() {
    assert_eq!(fid_for("hex-x", 0, Some("nightly")), "hex-x::nightly");
    assert_eq!(fid_for("hex-x", 2, None), "hex-x::2");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd system/harness && cargo test worker::tests 2>&1 | tail -20`
Expected: FAIL — `on_cron_named` not found / handlers tuple arity mismatch.

- [ ] **Step 3: Implement**

In `worker/mod.rs`: change `handlers` to carry the optional name and add named builders + the shared fid helper:

```rust
pub struct Worker {
    pub name: String,
    pub handlers: Vec<(Option<String>, TriggerSpec, Handler)>,
}

/// THE single fid derivation — used by the runtime at registration AND by
/// failures.rs when computing expectations, so they cannot drift.
pub fn fid_for(worker: &str, idx: usize, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{worker}::{n}"),
        None => format!("{worker}::{idx}"),
    }
}
```

Update each existing builder (`on_event`, `on_state`, `on_queue`, `on_cron`) to push `(None, spec, Box::new(f))`, and add named variants — same body, `Some(name.to_string())` first element:

```rust
pub fn on_cron_named<F>(mut self, name: &str, expr: &str, f: F) -> Self
where
    F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
{
    self.handlers.push((
        Some(name.to_string()),
        TriggerSpec::Cron { expression: expr.to_string() },
        Box::new(f),
    ));
    self
}

pub fn on_event_named<F>(mut self, name: &str, event: &str, f: F) -> Self
where
    F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
{
    self.handlers.push((
        Some(name.to_string()),
        TriggerSpec::State { scope: "events".to_string(), key: event.to_string() },
        Box::new(f),
    ));
    self
}
```

In `runtime.rs:123-124` update the destructure + fid:

```rust
for (idx, (tname, spec, handler)) in worker.handlers.into_iter().enumerate() {
    let fid = crate::worker::fid_for(&wname, idx, tname.as_deref());
```

Fix the two existing tests in `worker/mod.rs` that destructure 2-tuples (`on_event_maps_to_state_events_scope`, `on_cron_maps_to_cron_trigger`) to destructure `(_name, spec, _h)`. Fix any other compile errors the change surfaces (`grep -rn "\.handlers" system/harness/src/`).

- [ ] **Step 4: Run the full suite**

Run: `cd system/harness && cargo test 2>&1 | tail -5`
Expected: PASS (all existing tests + 2 new).

- [ ] **Step 5: Name the core module triggers** (stable ids for multi-trigger workers; single-trigger modules too, for uniformity)

In `src/modules/`:
- `memory_maintenance.worker.rs`: the 5 `.on_cron(...)` calls become `.on_cron_named("index", …)`, `.on_cron_named("quick", …)`, `.on_cron_named("parse-transcripts", …)`, `.on_cron_named("consolidate-full", …)`, `.on_cron_named("maintain-weekly", …)` — match names to each handler's doc comment.
- `backup.worker.rs` → `.on_cron_named("daily", …)`
- `freshness.worker.rs` → `.on_cron_named("daily", …)`
- `reconciler.worker.rs` → `.on_cron_named("hourly", …)`
- `code_intel.worker.rs` → `.on_cron_named("nightly", …)`
- `oss_releaser.worker.rs` → `.on_event_named("release-requested", …, …)` keeping its event constant as the subscription key.

Do NOT touch instance overlay modules (`$HEX_DIR/.hex/modules/` is another repo; positional fallback keeps them working).

- [ ] **Step 6: Run suite + grep for stale fid references in THIS repo**

Run: `cd system/harness && cargo test 2>&1 | tail -5` → PASS.
Run: `grep -rn "::0\b\|::1\b" system/harness/src/ docs/ --include="*.rs" --include="*.md" | grep -v target | head -20` — update any foundation doc/code that hardcodes old positional fids for the renamed core workers (note them in the commit message; events.db history keeps old fids — the detector's NEVER-RAN section will show the renamed fids as new, which is expected; Task 6's digest header says so).

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(worker): named triggers + shared fid_for — stable telemetry identity"
```

---

### Task 2: `failures.rs` — expectations model + cron math

**Files:**
- Create: `system/harness/src/failures.rs`
- Modify: the crate's module-declaration file (find it: `grep -rn "pub mod alert" system/harness/src/` — add `pub mod failures;` beside it)
- Modify: `system/harness/Cargo.toml` (add `cron = "=0.15.0"  # MUST match engine fork's version — parity test below`)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// prev_fire: most recent expected fire at-or-before `now`, UTC.
    /// CONTRACT TEST for the cron crate's reverse iteration — if next_back()
    /// semantics differ (strictly-before vs at-or-before), adjust prev_fire's
    /// implementation, NOT this expected value.
    #[test]
    fn prev_fire_daily_cron() {
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 10, 0, 0).unwrap();
        let prev = prev_fire("0 0 4 * * * *", now).unwrap();
        assert_eq!(prev, Utc.with_ymd_and_hms(2026, 6, 11, 4, 0, 0).unwrap());
    }

    #[test]
    fn prev_fire_weekly_cron() {
        // 2026-06-11 is a Thursday; previous SUN 04:30 is 2026-06-07.
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 10, 0, 0).unwrap();
        let prev = prev_fire("0 30 4 * * SUN *", now).unwrap();
        assert_eq!(prev, Utc.with_ymd_and_hms(2026, 6, 7, 4, 30, 0).unwrap());
    }

    /// cadence = gap between the two most recent expected fires.
    #[test]
    fn cadence_of_15min_cron_is_900s() {
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 10, 7, 0).unwrap();
        assert_eq!(cadence_secs("0 */15 * * * * *", now).unwrap(), 900);
    }

    /// Expectations: cron fids only, disabled excluded.
    #[test]
    fn expectations_skip_event_triggers_and_disabled() {
        let regs = vec![
            RegisteredTrigger { worker: "a".into(), fid: "a::daily".into(),
                cron: Some("0 0 4 * * * *".into()) },
            RegisteredTrigger { worker: "b".into(), fid: "b::0".into(), cron: None },
            RegisteredTrigger { worker: "c".into(), fid: "c::daily".into(),
                cron: Some("0 0 5 * * * *".into()) },
        ];
        let disabled: std::collections::BTreeSet<String> = ["c".to_string()].into();
        let exp = cron_expectations(&regs, &disabled);
        assert_eq!(exp.len(), 1);
        assert_eq!(exp[0].fid, "a::daily");
    }

    /// Parity: every cron expression in the live registry must parse with OUR
    /// cron crate (the engine fires them with the same crate version — a parse
    /// divergence would silently exempt a module from detection).
    #[test]
    fn registry_cron_expressions_all_parse() {
        for t in registered_triggers() {
            if let Some(expr) = &t.cron {
                expr.parse::<cron::Schedule>()
                    .unwrap_or_else(|e| panic!("{}: `{expr}` does not parse: {e}", t.fid));
            }
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd system/harness && cargo test failures:: 2>&1 | tail -10`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement the model**

```rust
//! `hex failures` — unexpected-failure detection over the telemetry store.
//! Detection only: this module NEVER remediates (proposal: telemetry-consumption-layer v2).

use chrono::{DateTime, Utc};
use std::collections::BTreeSet;
use std::str::FromStr;

/// One registered trigger, flattened from workers::registry().
#[derive(Debug, Clone)]
pub struct RegisteredTrigger {
    pub worker: String,
    pub fid: String,
    pub cron: Option<String>, // None for state/queue triggers
}

/// Flatten the live registry. Handlers are not constructible in tests —
/// tests build RegisteredTrigger vectors by hand instead.
pub fn registered_triggers() -> Vec<RegisteredTrigger> {
    crate::workers::registry()
        .into_iter()
        .flat_map(|w| {
            let wname = w.name.clone();
            w.handlers
                .into_iter()
                .enumerate()
                .map(move |(idx, (name, spec, _h))| RegisteredTrigger {
                    worker: wname.clone(),
                    fid: crate::worker::fid_for(&wname, idx, name.as_deref()),
                    cron: match spec {
                        crate::worker::TriggerSpec::Cron { expression } => Some(expression),
                        _ => None,
                    },
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Most recent expected fire at-or-before `now` (UTC — engine ground truth).
pub fn prev_fire(expr: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let schedule = cron::Schedule::from_str(expr).ok()?;
    schedule.after(&now).next_back()
}

/// Seconds between the two most recent expected fires.
pub fn cadence_secs(expr: &str, now: DateTime<Utc>) -> Option<i64> {
    let schedule = cron::Schedule::from_str(expr).ok()?;
    let mut back = schedule.after(&now);
    let t1 = back.next_back()?;
    let t2 = back.next_back()?;
    Some((t1 - t2).num_seconds())
}

/// A cron expectation the detector evaluates.
#[derive(Debug, Clone)]
pub struct CronExpectation {
    pub worker: String,
    pub fid: String,
    pub expr: String,
}

pub fn cron_expectations(
    regs: &[RegisteredTrigger],
    disabled: &BTreeSet<String>,
) -> Vec<CronExpectation> {
    regs.iter()
        .filter(|t| !disabled.contains(&t.worker))
        .filter_map(|t| {
            t.cron.as_ref().map(|expr| CronExpectation {
                worker: t.worker.clone(),
                fid: t.fid.clone(),
                expr: expr.clone(),
            })
        })
        .collect()
}
```

If `prev_fire`'s `next_back()` semantics differ from the contract test (at-or-before vs strictly-before), fix the implementation (e.g. `schedule.after(&(now + chrono::Duration::seconds(1))).next_back()`), never the test.

- [ ] **Step 4: Run tests**

Run: `cd system/harness && cargo test failures:: 2>&1 | tail -10` → PASS (all 5).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(failures): expectations model + cron prev-fire math (cron =0.15.0 pinned to engine)"
```

---

### Task 3: MISSED evaluation with duration-aware slack + downtime subtraction

**Files:**
- Modify: `system/harness/src/failures.rs`
- Modify: `system/harness/src/telemetry/mod.rs` (add `open_ro()`)

The rules (from the adversarial review of the proposal):
- A fid is MISSED ⇔ zero rows since `last_expected_fire`, evaluated only when `now > last_expected + slack`, where `slack = max(cadence/4, recent MAX(duration_ms)/1000 + 60)` — fires serialize behind slow handlers (observed: 182-min gaps on a healthy 15-min cron).
- DOWNTIME: any gap between consecutive rows (across ALL sources) `> 2 × shortest_cadence` is a downtime interval; expected fires inside downtime are excused from MISSED and reported once, collectively.

- [ ] **Step 1: Add the shared test helpers** (`failures.rs`)

```rust
#[cfg(test)]
pub(crate) mod testutil {
    use chrono::{DateTime, Utc};
    pub fn seed_schema() {
        crate::telemetry::record(&crate::telemetry::TelemetryEvent {
            source: "seed".into(), event: "seed".into(), status: "ok".into(),
            duration_ms: None, exit_code: None, detail: None,
        }).unwrap();
    }
    fn conn() -> rusqlite::Connection {
        rusqlite::Connection::open(
            std::path::PathBuf::from(std::env::var("HEX_DIR").unwrap())
                .join(".hex/telemetry/events.db"),
        ).unwrap()
    }
    pub fn row(fid: &str, ts: DateTime<Utc>, status: &str, duration_ms: i64) {
        conn().execute(
            "INSERT INTO events (ts, source, event, status, duration_ms) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![ts.to_rfc3339(), "w", fid, status, duration_ms],
        ).unwrap();
    }
    pub fn row_d(fid: &str, ts: DateTime<Utc>, status: &str, detail: &str) {
        conn().execute(
            "INSERT INTO events (ts, source, event, status, detail) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![ts.to_rfc3339(), "w", fid, status, detail],
        ).unwrap();
    }
}
```

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod missed_tests {
    use super::testutil::*;
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn missed_fires_alert_when_expected_fire_has_no_row() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        seed_schema();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        // daily 04:00 cron, last row 2 days ago → MISSED
        row("a::daily", now - Duration::days(2), "ok", 1000);
        let exp = vec![CronExpectation { worker: "a".into(), fid: "a::daily".into(),
            expr: "0 0 4 * * * *".into() }];
        let report = evaluate(&exp, now, &[]).unwrap();
        assert_eq!(report.missed.len(), 1, "{:?}", report.missed);
        assert_eq!(report.missed[0].fid, "a::daily");
    }

    #[test]
    fn not_missed_within_duration_slack() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        seed_schema();
        // 15-min cron whose recent runs take 30 min: at 12:07 the 12:00 fire is
        // still legitimately in-flight → NOT missed.
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 7, 0).unwrap();
        row("a::quarter", now - Duration::minutes(40), "ok", 1_800_000);
        let exp = vec![CronExpectation { worker: "a".into(), fid: "a::quarter".into(),
            expr: "0 */15 * * * * *".into() }];
        let report = evaluate(&exp, now, &[]).unwrap();
        assert!(report.missed.is_empty(), "{:?}", report.missed);
    }

    #[test]
    fn never_ran_listed_not_missed() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        seed_schema();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        let exp = vec![CronExpectation { worker: "a".into(), fid: "a::daily".into(),
            expr: "0 0 4 * * * *".into() }];
        let report = evaluate(&exp, now, &[]).unwrap();
        assert!(report.missed.is_empty());
        assert_eq!(report.never_ran.len(), 1);
    }

    #[test]
    fn downtime_excuses_missed_and_reports_once() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        seed_schema();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        // Heartbeat stream rows every 15 min EXCEPT a 5h hole covering 04:00.
        let mut t = now - Duration::hours(10);
        while t < now {
            let in_hole = t > now - Duration::hours(9) && t < now - Duration::hours(4);
            if !in_hole {
                row("hb::quarter", t, "ok", 10);
            }
            t = t + Duration::minutes(15);
        }
        // Daily 04:00 fid (04:00 = now-8h, inside the hole), no row today.
        row("a::daily", now - Duration::days(1) - Duration::hours(8), "ok", 1000);
        let exp = vec![
            CronExpectation { worker: "hb".into(), fid: "hb::quarter".into(),
                expr: "0 */15 * * * * *".into() },
            CronExpectation { worker: "a".into(), fid: "a::daily".into(),
                expr: "0 0 4 * * * *".into() },
        ];
        let report = evaluate(&exp, now, &[]).unwrap();
        assert!(report.missed.iter().all(|m| m.fid != "a::daily"),
            "downtime must excuse a::daily: {:?}", report.missed);
        assert_eq!(report.downtime.len(), 1);
        assert!(report.downtime[0].excused_fids.contains(&"a::daily".to_string()));
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test missed_tests 2>&1 | tail -10` → FAIL (`evaluate` undefined).

- [ ] **Step 4: Implement `evaluate` + `telemetry::open_ro`**

In `telemetry/mod.rs` (NEVER `immutable=1` — WAL readers with immutable silently miss un-checkpointed rows, the freshest data):

```rust
/// Read-only connection for consumers (failures detector, probe). Plain
/// read-only on a WAL db reads checkpointed + WAL frames correctly;
/// `immutable=1` would silently miss the WAL — never use it here.
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

In `failures.rs`:

```rust
#[derive(Debug, Clone)]
pub struct Missed {
    pub fid: String,
    pub expr: String,
    pub expected_at: DateTime<Utc>,
    pub last_seen: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Downtime {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub excused_fids: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub missed: Vec<Missed>,
    pub never_ran: Vec<CronExpectation>,
    pub downtime: Vec<Downtime>,
}

/// Evaluate expectations against events.db at `now`. `extra_excused` lets
/// callers exempt fids (used by the probe self-check in main.rs).
pub fn evaluate(
    exp: &[CronExpectation],
    now: DateTime<Utc>,
    extra_excused: &[String],
) -> rusqlite::Result<Report> {
    let conn = crate::telemetry::open_ro()?;
    let mut report = Report::default();

    // 1. Downtime intervals: gaps > 2× shortest cadence across ALL rows.
    let shortest = exp
        .iter()
        .filter_map(|e| cadence_secs(&e.expr, now))
        .min()
        .unwrap_or(900);
    let lookback = (now - chrono::Duration::hours(36)).to_rfc3339();
    let mut stmt = conn.prepare("SELECT ts FROM events WHERE ts >= ?1 ORDER BY ts")?;
    let times: Vec<DateTime<Utc>> = stmt
        .query_map([&lookback], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter_map(|s| DateTime::parse_from_rfc3339(&s).ok().map(|d| d.with_timezone(&Utc)))
        .collect();
    let mut downtimes: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    for pair in times.windows(2) {
        if (pair[1] - pair[0]).num_seconds() > 2 * shortest {
            downtimes.push((pair[0], pair[1]));
        }
    }

    // 2. Per-fid evaluation.
    for e in exp {
        let (last_ts, max_dur): (Option<String>, Option<i64>) = conn.query_row(
            "SELECT MAX(ts), MAX(duration_ms) FROM events WHERE event = ?1",
            [&e.fid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if last_ts.is_none() {
            report.never_ran.push(e.clone());
            continue;
        }
        let Some(expected) = prev_fire(&e.expr, now) else { continue };
        let cadence = cadence_secs(&e.expr, now).unwrap_or(86_400);
        let slack = std::cmp::max(cadence / 4, max_dur.unwrap_or(0) / 1000 + 60);
        if now < expected + chrono::Duration::seconds(slack) {
            continue; // fire may legitimately still be in flight
        }
        let row_since: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE event = ?1 AND ts >= ?2",
            rusqlite::params![&e.fid, expected.to_rfc3339()],
            |r| r.get(0),
        )?;
        if row_since > 0 || extra_excused.contains(&e.fid) {
            continue;
        }
        // Excused by downtime?
        if let Some((from, to)) = downtimes
            .iter()
            .find(|(from, to)| expected >= *from && expected <= *to)
        {
            match report.downtime.iter_mut().find(|d| d.from == *from) {
                Some(d) => d.excused_fids.push(e.fid.clone()),
                None => report.downtime.push(Downtime {
                    from: *from,
                    to: *to,
                    excused_fids: vec![e.fid.clone()],
                }),
            }
            continue;
        }
        report.missed.push(Missed {
            fid: e.fid.clone(),
            expr: e.expr.clone(),
            expected_at: expected,
            last_seen: last_ts,
        });
    }
    Ok(report)
}
```

- [ ] **Step 5: Run** — `cargo test missed_tests 2>&1 | tail -10` → PASS (4).

- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat(failures): MISSED with duration-aware slack, NEVER-RAN, downtime subtraction"`

---

### Task 4: FAILURES signature grouping + double-fire anomaly

**Files:**
- Modify: `system/harness/src/failures.rs`

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod signature_tests {
    use super::testutil::*;
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn signatures_group_and_flag_new() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        seed_schema();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        // chronic: 5 days of the same error; new: one today
        for d in 1..=5 {
            row_d("old::daily", now - Duration::days(d), "error",
                "`hex` exited 2: error: unrecognized subcommand backup");
        }
        row_d("new::daily", now - Duration::hours(2), "error",
            "`hex` exited 1: gate battery BLOCKED");
        let sigs = failure_signatures(now, 24).unwrap();
        let newsig = sigs.iter().find(|s| s.fid == "new::daily").unwrap();
        assert!(newsig.is_new);
        let oldsig = sigs.iter().find(|s| s.fid == "old::daily").unwrap();
        assert!(!oldsig.is_new);
        assert_eq!(oldsig.count, 5);
    }

    #[test]
    fn double_fire_detected_per_expected_window() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        seed_schema();
        let now = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();
        // two rows 150ms apart around the 04:00 fire (live phenomenon: engine
        // double-fires — 4 of hex-backup's 6 nights)
        let fire = Utc.with_ymd_and_hms(2026, 6, 11, 3, 59, 59).unwrap();
        row("a::daily", fire, "error", 100);
        row("a::daily", fire + Duration::milliseconds(150), "error", 100);
        let exp = vec![CronExpectation { worker: "a".into(), fid: "a::daily".into(),
            expr: "0 0 4 * * * *".into() }];
        let dups = duplicate_fires(&exp, now).unwrap();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].fid, "a::daily");
        assert_eq!(dups[0].rows_in_window, 2);
    }

    #[test]
    fn signature_head_normalizes_digits() {
        assert_eq!(
            signature_head("`hex` exited 2: slice 12345 failed\nsecond line"),
            "`hex` exited #: slice # failed"
        );
    }
}
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone)]
pub struct FailureSignature {
    pub fid: String,
    pub head: String, // normalized detail head
    pub status: String,
    pub count: i64,
    pub first_seen: String,
    pub last_seen: String,
    pub is_new: bool, // first_seen inside the digest window
}

/// Normalize a detail string into a stable signature head: first line,
/// digit runs collapsed to '#', truncated to 80 chars.
pub fn signature_head(detail: &str) -> String {
    let first = detail.lines().next().unwrap_or("");
    let mut out = String::with_capacity(80);
    let mut in_digits = false;
    for c in first.chars().take(160) {
        if c.is_ascii_digit() {
            if !in_digits { out.push('#'); in_digits = true; }
        } else {
            in_digits = false;
            out.push(c);
        }
        if out.len() >= 80 { break; }
    }
    out
}

/// Failures grouped by (fid, signature head), with is_new flagged when
/// first_seen falls inside the last `window_hours`. Only signatures ACTIVE in
/// the window are returned. status semantics: error/panic/failed = failures;
/// skipped/warn are excluded here (CLI lists their counts separately).
pub fn failure_signatures(
    now: DateTime<Utc>,
    window_hours: i64,
) -> rusqlite::Result<Vec<FailureSignature>> {
    let conn = crate::telemetry::open_ro()?;
    let mut stmt = conn.prepare(
        "SELECT event, status, COALESCE(detail,''), ts FROM events
         WHERE status IN ('error','panic','failed') ORDER BY ts",
    )?;
    let mut map: std::collections::BTreeMap<(String, String), FailureSignature> =
        Default::default();
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?,
            r.get::<_, String>(2)?, r.get::<_, String>(3)?))
    })?;
    for r in rows {
        let (fid, status, detail, ts) = r?;
        let head = signature_head(&detail);
        let e = map.entry((fid.clone(), head.clone())).or_insert(FailureSignature {
            fid, head, status, count: 0, first_seen: ts.clone(),
            last_seen: ts.clone(), is_new: false,
        });
        e.count += 1;
        e.last_seen = ts;
    }
    let window_start = (now - chrono::Duration::hours(window_hours)).to_rfc3339();
    let mut out: Vec<_> = map
        .into_values()
        .filter(|s| s.last_seen >= window_start)
        .map(|mut s| { s.is_new = s.first_seen >= window_start; s })
        .collect();
    out.sort_by(|a, b| (b.is_new, b.count).cmp(&(a.is_new, a.count)));
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct DuplicateFire {
    pub fid: String,
    pub window_start: DateTime<Utc>,
    pub rows_in_window: i64,
}

/// >1 row per expected-fire window = engine anomaly (observed: double-fires
/// ~150ms apart). Checks the most recent expected fire per cron fid.
pub fn duplicate_fires(
    exp: &[CronExpectation],
    now: DateTime<Utc>,
) -> rusqlite::Result<Vec<DuplicateFire>> {
    let conn = crate::telemetry::open_ro()?;
    let mut out = Vec::new();
    for e in exp {
        let Some(expected) = prev_fire(&e.expr, now) else { continue };
        let cadence = cadence_secs(&e.expr, now).unwrap_or(86_400);
        let lo = (expected - chrono::Duration::seconds(60)).to_rfc3339();
        let hi = (expected + chrono::Duration::seconds(cadence / 2)).to_rfc3339();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE event = ?1 AND ts >= ?2 AND ts < ?3",
            rusqlite::params![&e.fid, lo, hi],
            |r| r.get(0),
        )?;
        if n > 1 {
            out.push(DuplicateFire { fid: e.fid.clone(), window_start: expected,
                rows_in_window: n });
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run** — `cargo test failures 2>&1 | tail -5` → PASS.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(failures): failure signatures with new-flag + duplicate-fire anomaly"`

---

### Task 5: MODULE NOT LANDED — disk-vs-binary diff

The real orbstack-prune incident: a `.worker.rs` on disk for weeks, never compiled into the running binary. Registry enumeration is structurally blind to it.

**Files:**
- Modify: `system/harness/src/failures.rs`

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod not_landed_tests {
    use super::*;

    #[test]
    fn detects_disk_module_missing_from_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let modules = tmp.path().join(".hex/modules");
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::write(modules.join("orbstack_prune.worker.rs"), "// w").unwrap();
        std::fs::write(modules.join("known.worker.rs"), "// w").unwrap();
        let compiled = vec!["known.worker.rs".to_string()];
        let missing = modules_not_landed(tmp.path(), &compiled);
        assert_eq!(missing, vec!["orbstack_prune.worker.rs".to_string()]);
    }
}
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

```rust
/// Compare *.worker.rs files on disk under $HEX_DIR/.hex/modules/ against the
/// basenames compiled into this binary. A file on disk absent from the binary
/// = written-but-never-deployed (the actual orbstack-prune failure mode).
/// Recursive to mirror build.rs's glob.
pub fn modules_not_landed(hex_dir: &std::path::Path, compiled_basenames: &[String]) -> Vec<String> {
    let root = hex_dir.join(".hex").join("modules");
    let mut found = Vec::new();
    collect_worker_files(&root, &mut found);
    let compiled: std::collections::BTreeSet<&str> =
        compiled_basenames.iter().map(|s| s.as_str()).collect();
    let mut out: Vec<String> = found
        .into_iter()
        .filter_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
        .filter(|base| !compiled.contains(base.as_str()))
        .collect();
    out.sort();
    out
}

fn collect_worker_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_worker_files(&p, out);
        } else if p.file_name().map_or(false, |f| f.to_string_lossy().ends_with(".worker.rs")) {
            out.push(p);
        }
    }
}

/// Compiled basenames from the build-generated module_paths().
/// VERIFY the generated signature first (it is build.rs-emitted):
/// `grep -rn "module_paths" system/harness/src/workers/ system/harness/build.rs`
/// — adapt if the shape differs; the modules_not_landed test stays as-is
/// since it takes the list as input.
pub fn compiled_module_basenames() -> Vec<String> {
    crate::workers::module_paths()
        .into_iter()
        .filter_map(|(_name, path)| {
            std::path::Path::new(path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .collect()
}
```

- [ ] **Step 4: Run** — PASS. **Step 5: Commit** — `git commit -am "feat(failures): module-not-landed disk-vs-binary diff"`

---

### Task 6: `hex failures` CLI + alerts + probe

**Files:**
- Modify: `system/harness/src/main.rs` (new `Failures` variant in `enum Commands` at main.rs:39-207, handler functions, dispatch arm — mirror how `Commands::Backup` dispatches)
- Modify: `system/harness/src/failures.rs` (alert key sanitization)
- Modify: `system/harness/src/doctor/checks/telemetry_health.rs` (remediation string)

- [ ] **Step 1: Failing test for alert-key sanitization** (in failures.rs)

```rust
#[test]
fn alert_keys_are_path_safe() {
    assert_eq!(alert_key("missed", "hex-backup::daily"), "failures-missed-hex-backup-daily");
    assert_eq!(alert_key("missed", "a::b/c"), "failures-missed-a-b-c");
}
```

- [ ] **Step 2: Implement**

```rust
/// Per-condition alert keys, sanitized to [A-Za-z0-9._-] — alert::notify
/// interpolates the key into a stamp-file path (alert.rs:57) and dedupes 6h
/// per key, so keys must be path-safe and per-condition (a shared key would
/// suppress a different worker's distinct MISS).
pub fn alert_key(kind: &str, ident: &str) -> String {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' })
        .collect::<String>()
        .split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    format!("failures-{kind}-{safe}")
}
```

Run the test → PASS.

- [ ] **Step 3: CLI wiring in main.rs**

Add to `enum Commands`:

```rust
/// Unexpected-failure digest: MISSED runs, NEVER-RAN, modules not landed,
/// failure signatures, downtime. Detection only — never remediates.
#[command(display_order = 6)]
Failures {
    #[command(subcommand)]
    command: Option<FailuresCommands>,
    /// Digest window in hours for new-signature flagging
    #[arg(long, default_value_t = 24)]
    window: i64,
    /// Emit alerts (used by the cron worker; plain runs just print)
    #[arg(long)]
    alert: bool,
},
```

```rust
#[derive(Subcommand)]
enum FailuresCommands {
    /// Out-of-process liveness probe: events.db staleness + harness launchd
    /// state. Run from its OWN launchd job, never from inside the harness.
    Probe,
}
```

Handler functions (place near `run_freshness`; note crate paths — main.rs may use `hex::` or `crate::` depending on bin/lib layout; mirror how existing handlers call `hex::ledger`/`crate::telemetry` and adjust):

```rust
fn run_failures(window: i64, alert: bool) -> i32 {
    let now = chrono::Utc::now();
    let hex_dir = std::path::PathBuf::from(
        std::env::var("HEX_DIR").unwrap_or_else(|_| ".".into()),
    );
    let regs = hex::failures::registered_triggers();
    let disabled = hex::module_state::disabled_set(&hex_dir).unwrap_or_else(|e| {
        eprintln!("failures: disabled-set unreadable ({e}) — evaluating ALL modules");
        Default::default()
    });
    let exp = hex::failures::cron_expectations(&regs, &disabled);
    let report = match hex::failures::evaluate(&exp, now, &[]) {
        Ok(r) => r,
        Err(e) => { eprintln!("failures: events.db read failed: {e}"); return 2; }
    };
    let sigs = hex::failures::failure_signatures(now, window).unwrap_or_default();
    let dups = hex::failures::duplicate_fires(&exp, now).unwrap_or_default();
    let compiled = hex::failures::compiled_module_basenames();
    let not_landed = hex::failures::modules_not_landed(&hex_dir, &compiled);

    let mut bad = false;
    println!("== hex failures (window {window}h, {} cron fids, {} disabled) ==",
        exp.len(), disabled.len());
    if !report.missed.is_empty() {
        bad = true;
        println!("\nMISSED ({}):", report.missed.len());
        for m in &report.missed {
            println!("  {}  expected {}  last-seen {}", m.fid,
                m.expected_at.to_rfc3339(), m.last_seen.as_deref().unwrap_or("never"));
            if alert {
                hex::alert::notify(&hex::failures::alert_key("missed", &m.fid),
                    "hex worker missed its scheduled run",
                    &format!("{} expected at {}", m.fid, m.expected_at.to_rfc3339()));
            }
        }
    }
    if !not_landed.is_empty() {
        bad = true;
        println!("\nMODULE NOT LANDED — on disk, not in this binary ({}):", not_landed.len());
        for f in &not_landed {
            println!("  {f}  (rebuild + redeploy the harness to land it)");
            if alert {
                hex::alert::notify(&hex::failures::alert_key("notlanded", f),
                    "hex module on disk but not in the running binary", f);
            }
        }
    }
    if !report.never_ran.is_empty() {
        bad = true; // visible during grace by design (proposal: defaults chosen)
        println!("\nNEVER-RAN cron fids ({}) — loud until first fire (note: core fids were renamed by the named-trigger change; old history lives under positional fids):",
            report.never_ran.len());
        for e in &report.never_ran {
            println!("  {}  cron({})", e.fid, e.expr);
        }
    }
    for d in &report.downtime {
        bad = true;
        let msg = format!("no telemetry {} → {} — harness down, box asleep, or restarted; excused: {}",
            d.from.to_rfc3339(), d.to.to_rfc3339(), d.excused_fids.join(", "));
        println!("\nDOWNTIME: {msg}");
        if alert {
            hex::alert::notify(&hex::failures::alert_key("downtime",
                &d.from.timestamp().to_string()), "telemetry gap", &msg);
        }
    }
    if !sigs.is_empty() {
        println!("\nFAILURE SIGNATURES (active in window; NEW first):");
        for s in &sigs {
            if s.is_new { bad = true; }
            println!("  [{}] {:>4}x  {}  {}  first {}  last {}",
                if s.is_new { "NEW" } else { "old" }, s.count, s.fid, s.head,
                s.first_seen, s.last_seen);
        }
    }
    if !dups.is_empty() {
        println!("\nDUPLICATE FIRES (engine anomaly — >1 row per expected window):");
        for d in &dups {
            println!("  {}  {} rows at {}", d.fid, d.rows_in_window,
                d.window_start.to_rfc3339());
        }
    }
    let event_fids: Vec<_> = regs.iter().filter(|t| t.cron.is_none()).collect();
    if !event_fids.is_empty() {
        println!("\nEVENT SUBSCRIBERS (informational — no cadence, no MISSED semantics):");
        for t in &event_fids {
            println!("  {}", t.fid);
        }
    }
    if bad { 1 } else { println!("\nall clear"); 0 }
}
```

Probe handler (out-of-process — alerts via osascript DIRECTLY, never via `alert::notify`, since events.db/the harness may be the broken thing):

```rust
fn run_failures_probe() -> i32 {
    // events.db freshness: the 15-min maintenance stream means a healthy
    // harness writes at least one row per ~20 min.
    let stale_after_secs: i64 = 45 * 60;
    let fresh = hex::telemetry::recent(1)
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|r| chrono::DateTime::parse_from_rfc3339(&r.ts).ok())
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds());
    let launchd = std::process::Command::new("launchctl")
        .args(["list", "com.hex.harness"])
        .output();
    let harness_listed = launchd.map(|o| o.status.success()).unwrap_or(false);
    let mut problems = Vec::new();
    match fresh {
        Some(age) if age > stale_after_secs =>
            problems.push(format!("events.db stale: last row {age}s ago")),
        None => problems.push("events.db unreadable or empty".to_string()),
        _ => {}
    }
    if !harness_listed {
        problems.push("com.hex.harness not loaded in launchd".to_string());
    }
    if problems.is_empty() {
        println!("probe ok");
        return 0;
    }
    let msg = problems.join("; ");
    eprintln!("PROBE ALERT: {msg}");
    let script = format!(
        "display notification \"{}\" with title \"hex harness liveness probe\"",
        msg.replace('"', "'")
    );
    let _ = std::process::Command::new("osascript").arg("-e").arg(&script).status();
    1
}
```

- [ ] **Step 4: Update the doctor remediation string** — in `telemetry_health.rs`, change the remediation text to: ``Run `hex failures` (digest) or `hex telemetry failures` (raw rows) to inspect``.

- [ ] **Step 5: Build + run by hand (read-only against live data)**

Run: `cd system/harness && cargo build 2>&1 | tail -3` → compiles.
Run: `HEX_DIR=/Users/mrap/hex ./target/debug/hex failures | head -50` → digest prints; eyeball MISSED/NEVER-RAN against `HEX_DIR=/Users/mrap/hex /Users/mrap/hex/.hex/bin/hex module list` reality. Record the output in the task report. (If `CARGO_TARGET_DIR` is set in your environment, the binary is at `$CARGO_TARGET_DIR/debug/hex` instead.)

- [ ] **Step 6: Run full suite** — `cargo test 2>&1 | tail -5` → PASS.
- [ ] **Step 7: Commit** — `git commit -am "feat(failures): hex failures CLI digest + alerts + out-of-process probe"`

---

### Task 7: Cron worker + launchd probe plist + freshness tz fix

**Files:**
- Create: `system/harness/src/modules/failures.worker.rs`
- Modify: `system/harness/src/modules/freshness.worker.rs` (one-line cron fix)
- Create: launchd plist — check where plist templates live first (`find . -name "*.plist" -not -path "*/target/*"`); if a template dir exists, put it there; otherwise create `system/templates/launchd/com.hex.failures-probe.plist` and say so in the report.

- [ ] **Step 1: Worker stub**

```rust
//! `hex-failures` — daily unexpected-failure digest over the telemetry store.
//! Detection only; alerts via hex::alert::notify (deduped per condition key).
//! Runs INSIDE the harness — its own absence is covered by the out-of-process
//! probe (`hex failures probe`, launchd: com.hex.failures-probe).
//!
//! `hex failures` exits 1 when anything is bad and ctx.run treats non-zero as
//! Err — so a bad digest records status=error for this fire with the digest
//! tail in detail. Correct and intentional: the digest IS the failure surface.

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// 13:30 UTC daily ≈ 06:30 PT — digest lands at the start of Mike's day.
pub const CRON_DAILY_1330_UTC: &str = "0 30 13 * * * *";

fn run_failures(_e: Event, ctx: Ctx) -> Result<()> {
    ctx.run(&["hex".to_string(), "failures".to_string(), "--alert".to_string()])
        .map(|_| ())
}

pub fn worker() -> Worker {
    Worker::new("hex-failures").on_cron_named("daily", CRON_DAILY_1330_UTC, run_failures)
}
```

(Check how other module stubs import the crate — `freshness.worker.rs` uses `use hex::worker::...`; mirror exactly. Verify `ctx.run`'s argv type by reading `ctx.rs` — adjust `&[...]` vs `&Vec<String>` to match.)

- [ ] **Step 2: Freshness tz fix** — in `freshness.worker.rs`: cron value `"0 0 9 * * * *"` → `"0 0 16 * * * *"`, doc comment → `09:00 PT (16:00 UTC) — engine crons evaluate UTC; see telemetry-consumption-layer proposal`.

- [ ] **Step 3: Probe plist**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.hex.failures-probe</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/mrap/hex/.hex/bin/hex</string>
    <string>failures</string>
    <string>probe</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict><key>HEX_DIR</key><string>/Users/mrap/hex</string></dict>
  <key>StartInterval</key><integer>1800</integer>
  <key>StandardErrorPath</key><string>/Users/mrap/hex/.hex/logs/failures-probe.log</string>
</dict>
</plist>
```

Do NOT load the plist (deploy-time op, instance-side). Note in the report: install = copy to `~/Library/LaunchAgents/` + `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.hex.failures-probe.plist`. If the repo's sanitize gate (`hex sanitize`) flags the absolute `/Users/mrap` paths, follow whatever convention existing templates use for instance paths (placeholder + install-time substitution) and document it.

- [ ] **Step 4: Suite + commit**

Run: `cargo test 2>&1 | tail -5` → PASS. If any test asserts the registry handler count (grep `registered.*handler` in tests), update it for the new module.

```bash
git add -A && git commit -m "feat(failures): daily digest worker + out-of-process probe plist; fix freshness cron to 09:00 PT"
```

---

### Task 8: Docs + final gates

**Files:**
- Modify: `docs/hex-ops.md` (add a Failures row near the Telemetry row; document the WAL/immutable footgun where events.db is documented)
- Modify: `docs/workflows.md` "Health check" section (add `hex failures`)

- [ ] **Step 1: Doc edits** — one row/line each, follow surrounding format exactly.

- [ ] **Step 2: Full suite + release build**

Run: `cd system/harness && cargo test 2>&1 | tail -3` → PASS.
Run: `cargo build --release 2>&1 | tail -3` → compiles.

- [ ] **Step 3: Exit-gate smoke against live data (read-only)**

Run: `HEX_DIR=/Users/mrap/hex ./target/release/hex failures; echo "exit=$?"`
Expected: digest prints; NEVER-RAN lists the renamed core fids (expected post-rename); MODULE NOT LANDED section empty.
Run: `HEX_DIR=/Users/mrap/hex ./target/release/hex failures probe; echo "exit=$?"` → `probe ok`, exit 0 (harness is up).

- [ ] **Step 4: Commit** — `git commit -am "docs: hex failures surfaces + WAL footgun"`

- [ ] **Step 5: Report** — branch name, test counts, live-digest output sample, every deviation from this plan (documented deviations are judged on merit; undocumented ones are review failures).
