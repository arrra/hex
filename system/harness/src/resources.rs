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

/// Per-condition alert keys, sanitized to [A-Za-z0-9._-] — alert::notify
/// interpolates the key into a stamp-file path (alert.rs) and dedupes 6h per
/// key, so keys must be path-safe and per-condition.
///
/// MERGE NOTE: identical to `failures::alert_key` on the sibling branch
/// (feature/hex-failures, plan 2026-06-11-hex-failures-detection.md). That
/// module is not on this branch — dedupe to `crate::failures::alert_key` at
/// merge and delete this private copy.
fn alert_key(kind: &str, ident: &str) -> String {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' })
        .collect::<String>()
        .split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    format!("failures-{kind}-{safe}")
}

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
        crate::alert::notify(&alert_key("resource", &key_ident), "resource pressure", &msg);
        // Level-triggered emission. ops::emit signature on this branch is
        // emit(event, data, producer: Option<&str>) — adapted from the plan's
        // sketch per its VERIFY note.
        if let Err(e) = crate::ops::emit("resource.pressure", data.clone(), Some("hex-resources")) {
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
