//! Recall ranking parameters, lifted from hardcoded constants into a config
//! struct loaded from `$HEX_DIR/.hex/config/recall.toml` (spec Tx4px1hxf).
//!
//! The compiled defaults are EXACTLY the constants that were previously
//! hardcoded in the recall/assemble path:
//!   * the RRF fusion constant (`rrf::RRF_K = 60.0`),
//!   * the two dual-weighted bm25 arm weightings in `recall::facts_recall`
//!     (`"1.0, 0.25, 2.0"` content, `"2.0, 1.0, 0.25"` entity), and
//!   * the M5 relevance-move demotion (`move_fired_relevance`: 1.0 fired /
//!     0.3 not-fired).
//!
//! An ABSENT config file yields those defaults, so behavior is byte-for-byte
//! unchanged for every instance that has not opted in. A file that EXISTS but
//! fails to parse is LOUD on stderr (SO S6) and falls back to defaults — the
//! hot recall path must never die on a bad tuning file, and a silently-ignored
//! bad config is exactly the quiet failure S6 forbids.
//!
//! The eval/sweep path threads an explicit `&RecallConfig` (`recall_with_config`,
//! `eval::run`'s override) so a variant can be scored WITHOUT touching the live
//! config file — the seam the weekly auto-tuner (spec Tzxmamhr8) builds on.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The full set of tunable recall ranking parameters.
///
/// `Serialize` is the seam the weekly auto-tuner (spec Tzxmamhr8) uses to write
/// a landed variant back to `recall.toml` and to stash it as `params_json`; it
/// does not touch the ranking algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecallConfig {
    /// RRF fusion constant. Default 60.0 (was `rrf::RRF_K`).
    pub rrf_k: f64,
    /// Dual-weighted bm25 arm weights over `facts_fts` (was the two hardcoded
    /// weight triples in `recall::facts_recall`).
    pub arm_weights: ArmWeights,
    /// M5 relevance-move confidence multipliers (was `move_fired_relevance`).
    pub move_relevance: MoveRelevance,
    /// Semantic KNN (vector) arm over `facts_vec`. DEFAULT OFF — an absent
    /// config or an absent `[vector]` section leaves it disabled, so recall is
    /// byte-for-byte identical to the BM25-only behavior (spec Sdnap37he
    /// task Ttrmaca6q).
    pub vector: VectorArm,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60.0,
            arm_weights: ArmWeights::default(),
            move_relevance: MoveRelevance::default(),
            vector: VectorArm::default(),
        }
    }
}

/// bm25 `(subject, predicate, object)` column weights, one triple per FTS arm.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct ArmWeights {
    /// Content-question weighting (object-heavy).
    pub content: [f64; 3],
    /// Entity/attribute weighting (subject+predicate-heavy).
    pub entity: [f64; 3],
}

impl Default for ArmWeights {
    fn default() -> Self {
        Self {
            content: [1.0, 0.25, 2.0],
            entity: [2.0, 1.0, 0.25],
        }
    }
}

impl ArmWeights {
    /// The content arm's `bm25()` weight-list SQL fragment.
    pub fn content_sql(&self) -> String {
        bm25_weights(&self.content)
    }
    /// The entity arm's `bm25()` weight-list SQL fragment.
    pub fn entity_sql(&self) -> String {
        bm25_weights(&self.entity)
    }
}

/// Render a weight triple as the `bm25()` weight-list SQL fragment. SQLite
/// consumes the numeric VALUE, so `1.0` → `"1"` ranks identically to the old
/// literal `"1.0, 0.25, 2.0"` — the fusion is unchanged at the defaults.
fn bm25_weights(w: &[f64; 3]) -> String {
    format!("{}, {}, {}", w[0], w[1], w[2])
}

/// Confidence multiplier applied to a move's candidates by whether the move
/// fired (was `move_fired_relevance`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct MoveRelevance {
    /// Multiplier when the move fired. Default 1.0.
    pub fired: f32,
    /// Multiplier when the move did NOT fire (the demotion). Default 0.3.
    pub unfired: f32,
}

impl Default for MoveRelevance {
    fn default() -> Self {
        Self {
            fired: 1.0,
            unfired: 0.3,
        }
    }
}

impl MoveRelevance {
    /// The multiplier for a move given its fired state.
    pub fn factor(&self, fired: bool) -> f32 {
        if fired {
            self.fired
        } else {
            self.unfired
        }
    }
}

/// Config for the semantic KNN (vector) arm over `facts_vec` — the query-side
/// embedding path chosen in `docs/research/2026-08-19-recall-vector-arm.md`
/// (option (b): a resident hex-engine embed endpoint served over a local unix
/// socket). The recall hot path is a fresh OS process per message, so it must
/// NOT cold-load the ~522 MB nomic model itself (measured 13–15 s under load);
/// instead it asks the resident endpoint for the query vector.
///
/// DEFAULT OFF. With `enabled = false` (or an absent config / absent `[vector]`
/// section) the recall path produces no query vector and the facts KNN arm is
/// never fused — recall is byte-for-byte identical to the BM25-only behavior.
///
/// When enabled, any embed failure (missing socket, dead/slow endpoint,
/// malformed reply, timeout) degrades LOUDLY to BM25-only (stderr WARN, SO S6)
/// within `timeout_ms`. It never errors recall and never adds unbounded
/// latency. No network calls — the endpoint is a local unix socket only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VectorArm {
    /// Master switch for the facts KNN arm. Default `false` (byte-identical
    /// BM25-only recall). The compiled default ships OFF until the task-3 A/B
    /// adoption gates pass (spec Sdnap37he, SO 11).
    pub enabled: bool,
    /// Unix-socket path of the resident embed endpoint (`hex memory
    /// embed-serve`). A relative path resolves under `$HEX_DIR`; an absolute
    /// path is used verbatim. Default: `.hex/run/embed.sock`.
    pub socket_path: String,
    /// Hard upper bound (ms) on the WHOLE embed step — connect + write + read.
    /// The recall caller waits at most this long before falling back to
    /// BM25-only. Default 150 ms: inside the p95 ≤ 500 ms adoption gate with
    /// headroom over the memo's tens-of-ms expected per-query cost.
    pub timeout_ms: u64,
}

impl Default for VectorArm {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: ".hex/run/embed.sock".to_string(),
            timeout_ms: 150,
        }
    }
}

/// Path to the instance recall-tuning config: `$HEX_DIR/.hex/config/recall.toml`.
pub fn config_path(hex_root: &Path) -> PathBuf {
    hex_root.join(".hex/config/recall.toml")
}

impl RecallConfig {
    /// Load the recall config for `hex_root`.
    ///
    /// * absent file → compiled defaults (identical to the pre-config behavior);
    /// * present but unparseable → LOUD on stderr (SO S6), then defaults, so the
    ///   hot recall path never dies on a bad tuning file.
    ///
    /// Fields absent from a partial file keep their compiled defaults.
    pub fn load(hex_root: &Path) -> Self {
        let path = config_path(hex_root);
        match std::fs::read_to_string(&path) {
            Ok(body) => match toml::from_str::<RecallConfig>(&body) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!(
                        "[recall config] {} exists but failed to parse ({e}); \
                         using compiled defaults",
                        path.display()
                    );
                    RecallConfig::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RecallConfig::default(),
            Err(e) => {
                eprintln!(
                    "[recall config] cannot read {} ({e}); using compiled defaults",
                    path.display()
                );
                RecallConfig::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_defaults_equal_prior_constants() {
        let d = RecallConfig::default();
        assert_eq!(d.rrf_k, 60.0, "RRF_K default drifted");
        assert_eq!(d.arm_weights.content, [1.0, 0.25, 2.0]);
        assert_eq!(d.arm_weights.entity, [2.0, 1.0, 0.25]);
        assert_eq!(d.move_relevance.factor(true), 1.0);
        assert_eq!(d.move_relevance.factor(false), 0.3);
        // SQL fragments are numerically identical to the old string literals.
        assert_eq!(d.arm_weights.content_sql(), "1, 0.25, 2");
        assert_eq!(d.arm_weights.entity_sql(), "2, 1, 0.25");
        // The semantic KNN arm ships DEFAULT OFF (spec Sdnap37he / SO 11) so
        // the compiled default is byte-identical BM25-only recall.
        assert!(!d.vector.enabled, "vector arm must default OFF");
    }

    #[test]
    fn absent_vector_section_stays_disabled() {
        // A config file that tunes other params but omits `[vector]` must leave
        // the arm OFF (byte-identical recall for every already-tuned instance).
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".hex/config")).unwrap();
        std::fs::write(config_path(tmp.path()), "rrf_k = 45.0\n").unwrap();
        let cfg = RecallConfig::load(tmp.path());
        assert_eq!(cfg.rrf_k, 45.0);
        assert!(!cfg.vector.enabled, "absent [vector] section must stay OFF");
        assert_eq!(cfg.vector.socket_path, ".hex/run/embed.sock");
        assert_eq!(cfg.vector.timeout_ms, 150);
    }

    #[test]
    fn absent_file_yields_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = RecallConfig::load(tmp.path());
        let def = RecallConfig::default();
        assert_eq!(cfg.rrf_k, def.rrf_k);
        assert_eq!(cfg.arm_weights.content, def.arm_weights.content);
        assert_eq!(cfg.arm_weights.entity, def.arm_weights.entity);
        assert_eq!(cfg.move_relevance.fired, def.move_relevance.fired);
        assert_eq!(cfg.move_relevance.unfired, def.move_relevance.unfired);
    }

    #[test]
    fn config_file_overrides_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".hex/config")).unwrap();
        std::fs::write(
            config_path(tmp.path()),
            r#"
rrf_k = 30.0

[arm_weights]
content = [3.0, 0.5, 4.0]
entity = [4.0, 2.0, 0.5]

[move_relevance]
fired = 0.9
unfired = 0.1
"#,
        )
        .unwrap();
        let cfg = RecallConfig::load(tmp.path());
        assert_eq!(cfg.rrf_k, 30.0);
        assert_eq!(cfg.arm_weights.content, [3.0, 0.5, 4.0]);
        assert_eq!(cfg.arm_weights.entity, [4.0, 2.0, 0.5]);
        assert_eq!(cfg.move_relevance.fired, 0.9);
        assert_eq!(cfg.move_relevance.unfired, 0.1);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".hex/config")).unwrap();
        std::fs::write(config_path(tmp.path()), "rrf_k = 12.0\n").unwrap();
        let cfg = RecallConfig::load(tmp.path());
        assert_eq!(cfg.rrf_k, 12.0);
        // Untouched fields keep compiled defaults.
        assert_eq!(cfg.arm_weights.content, [1.0, 0.25, 2.0]);
        assert_eq!(cfg.arm_weights.entity, [2.0, 1.0, 0.25]);
        assert_eq!(cfg.move_relevance.unfired, 0.3);
    }

    #[test]
    fn malformed_file_is_loud_and_defaults() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".hex/config")).unwrap();
        // rrf_k expects a float; a string is a parse error.
        std::fs::write(config_path(tmp.path()), "rrf_k = \"not a number\"\n").unwrap();
        let cfg = RecallConfig::load(tmp.path());
        // Falls back to defaults rather than dying on the hot path.
        assert_eq!(cfg.rrf_k, 60.0);
    }
}
