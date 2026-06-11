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
/// (Generated signature verified in build.rs: `hex_modules::module_paths()
/// -> Vec<(String, &'static str)>` — name, absolute source path.)
pub fn compiled_module_basenames() -> Vec<String> {
    crate::workers::hex_modules::module_paths()
        .into_iter()
        .filter_map(|(_name, path)| {
            std::path::Path::new(path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .collect()
}

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

    #[test]
    fn alert_keys_are_path_safe() {
        assert_eq!(alert_key("missed", "hex-backup::daily"), "failures-missed-hex-backup-daily");
        assert_eq!(alert_key("missed", "a::b/c"), "failures-missed-a-b-c");
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
