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
    // No telemetry store yet → no du history → floor rule only. (open_ro on a
    // missing file is an open error, not empty history — never create the db
    // from a read-only consumer.)
    if !crate::telemetry::db_exists() {
        return Ok(out);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// df -k output fixture (macOS shape) → free/used GB.
    #[test]
    fn parses_df_output() {
        let fixture = "Filesystem   1024-blocks       Used  Available Capacity iused ifree %iused  Mounted on\n/dev/disk3s1s1  1942700360  248000000 1536000000    14%  500000 4294467295    0%   /\n";
        let d = parse_df(fixture).unwrap();
        assert_eq!(d.free_gb, 1464); // 1536000000 KiB / 1048576 ≈ 1464.8 → trunc(1464)
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
