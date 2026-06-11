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
