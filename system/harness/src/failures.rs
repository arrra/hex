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
