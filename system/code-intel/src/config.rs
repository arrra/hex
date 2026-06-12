//! `scipd` pool configuration from `<codeintel_home>/scipd.toml`, per
//! SPEC-A2 §4.
//!
//! Missing file → defaults. Malformed file → loud error — never
//! default-on-parse-failure (Standing Order S6). Defaults are the smoke
//! test #3 values from SPEC-A2 §4 (2026-06-11): pool_cap=2,
//! mem_limit_mb=3500, pool-wide alarm 7000, 180s post-spawn grace.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Pool policy knobs (SPEC-A2 §4). `vanish_reap` is always-on by design and
/// `max_warm_wait` does not exist by design (cq never blocks on warming),
/// so neither is configurable here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScipdConfig {
    /// LRU pool capacity; eviction on overflow (smoke #3, 2026-06-11).
    pub pool_cap: usize,
    /// Reaper kills instances idle past this TTL.
    pub idle_ttl_secs: u64,
    /// Watchdog kills instances whose physical footprint exceeds this
    /// (smoke #3: steady footprints ~1.4–2.0GB; limit 3500MB).
    pub mem_limit_mb: u64,
    /// Pool-wide footprint alarm threshold: log + status note only, no kill
    /// (SPEC-A2 §4).
    pub pool_alarm_mb: u64,
    /// No memory kill within this many seconds of spawn — priming spikes
    /// (SPEC-A2 §4).
    pub spawn_grace_secs: u64,
    /// Quiescent-signal fallback (SPEC-A2 §4 caveat): if no quiescent
    /// serverStatus after this long, probe with a cheap request — a
    /// successful response promotes the instance to Ready ("fallback-probe").
    /// Never Ready on time alone; never wait forever.
    pub warm_fallback_secs: u64,
}

impl Default for ScipdConfig {
    fn default() -> Self {
        ScipdConfig {
            pool_cap: 2,
            idle_ttl_secs: 1800,
            mem_limit_mb: 3500,
            pool_alarm_mb: 7000,
            spawn_grace_secs: 180,
            warm_fallback_secs: 240,
        }
    }
}

impl ScipdConfig {
    /// Load from `<home>/scipd.toml`. Missing file → defaults; unreadable or
    /// malformed file → loud error naming the path and the parse failure.
    pub fn load(home: &Path) -> Result<Self, String> {
        let path = home.join("scipd.toml");
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ScipdConfig::default());
            }
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        toml::from_str(&raw).map_err(|e| format!("malformed {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec_smoke3_values() {
        let c = ScipdConfig::default();
        assert_eq!(c.pool_cap, 2);
        assert_eq!(c.idle_ttl_secs, 1800);
        assert_eq!(c.mem_limit_mb, 3500);
        assert_eq!(c.pool_alarm_mb, 7000);
        assert_eq!(c.spawn_grace_secs, 180);
        assert_eq!(c.warm_fallback_secs, 240);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let home = tempfile::tempdir().unwrap();
        let c = ScipdConfig::load(home.path()).unwrap();
        assert_eq!(c, ScipdConfig::default());
    }

    #[test]
    fn full_file_overrides_all_knobs() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("scipd.toml"),
            "pool_cap = 4\nidle_ttl_secs = 60\nmem_limit_mb = 1024\npool_alarm_mb = 2048\nspawn_grace_secs = 30\nwarm_fallback_secs = 120\n",
        )
        .unwrap();
        let c = ScipdConfig::load(home.path()).unwrap();
        assert_eq!(
            c,
            ScipdConfig {
                pool_cap: 4,
                idle_ttl_secs: 60,
                mem_limit_mb: 1024,
                pool_alarm_mb: 2048,
                spawn_grace_secs: 30,
                warm_fallback_secs: 120,
            }
        );
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_knobs() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("scipd.toml"), "pool_cap = 3\n").unwrap();
        let c = ScipdConfig::load(home.path()).unwrap();
        assert_eq!(c.pool_cap, 3);
        assert_eq!(c.idle_ttl_secs, 1800);
        assert_eq!(c.mem_limit_mb, 3500);
        assert_eq!(c.pool_alarm_mb, 7000);
        assert_eq!(c.spawn_grace_secs, 180);
        assert_eq!(c.warm_fallback_secs, 240);
    }

    #[test]
    fn malformed_file_is_loud_error_not_defaults() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("scipd.toml"), "pool_cap = \"lots\"\n").unwrap();
        let err = ScipdConfig::load(home.path()).unwrap_err();
        assert!(err.contains("scipd.toml"), "error must name the file: {err}");
    }

    #[test]
    fn unknown_key_is_loud_error_catches_typos() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("scipd.toml"), "pool_capp = 4\n").unwrap();
        let err = ScipdConfig::load(home.path()).unwrap_err();
        assert!(err.contains("pool_capp"), "error must name the bad key: {err}");
    }

    #[test]
    fn invalid_toml_syntax_is_loud_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("scipd.toml"), "this is not toml ===").unwrap();
        assert!(ScipdConfig::load(home.path()).is_err());
    }
}
