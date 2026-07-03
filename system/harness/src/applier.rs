//! Risk classifier for gatekeeper `ACCEPT_FLAGGED` proposals — P2 applier
//! deliverable 3 (pure, deterministic, exhaustively unit-tested; see
//! `projects/agent-infra/decisions/p2-apply-mechanism-deterministic-applier-2026-06-12.md`).
//!
//! `classify` is the ONLY thing in this file for Stage A of the build — the
//! CLI (`hex apply run|watch|revert|status`) lands in a later stage. No I/O,
//! no ledger, no LLM: a pure function over a proposal + the current
//! registry, returning a risk class and machine-readable reasons.
//!
//! Risk classes (constants disclosed in every ledger payload downstream):
//! - **R0 — auto-land**: `add-rule`, pattern compiles as regex, `rule_id`
//!   not constitution-class, and `rule_id` collides with neither a builtin
//!   lint rule id nor an active registry entry.
//! - **R1 — dial-gated**: `modify-rule` of an existing (active) landed rule.
//! - **R2 — always escalate**: `kill-rule`, unknown proposal types, and
//!   every failure mode above (empty/invalid pattern, constitution-class
//!   refusal, collision refusal, modify-rule of a rule that isn't landed).

use crate::gatekeeper::CONSTITUTION_CLASS;
use crate::lint_gates::footgun_rules;
use crate::rule_registry::RuleRegistry;
use regex::Regex;

/// Risk class assigned by [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum RiskClass {
    R0,
    R1,
    R2,
}

/// Minimal proposal shape the classifier needs — deliberately decoupled from
/// `gatekeeper::ProposalBlock` (which also carries `id`/`agent`/`created`/
/// `rationale`, irrelevant here) so this module stays a pure function over
/// exactly the fields the risk rules consult.
#[derive(Debug, Clone)]
pub struct ProposalForClassify {
    /// `"add-rule"` | `"modify-rule"` | `"kill-rule"` (or anything else,
    /// which always classifies R2).
    pub kind: String,
    pub rule_id: String,
    /// Regex pattern; empty/irrelevant for `kill-rule`.
    pub pattern: String,
}

/// The classifier's verdict: risk class + an ordered list of
/// machine-readable reasons (the last reason is always the deciding one).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Classification {
    pub risk: RiskClass,
    pub reasons: Vec<String>,
}

/// Classify one proposal against the CURRENT registry state.
///
/// Apply-time defense in depth: constitution-class refusal is checked here
/// too, independent of whatever the gatekeeper already enforced at judge
/// time — a proposal that somehow reaches the applier with a
/// constitution-class `rule_id` is refused (R2), never landed.
pub fn classify(p: &ProposalForClassify, registry: &RuleRegistry) -> Classification {
    let mut reasons = Vec::new();

    if CONSTITUTION_CLASS.iter().any(|c| p.rule_id.trim() == *c) {
        reasons.push(format!(
            "rule_id '{}' is constitution-class ({:?}) — refused regardless of verdict",
            p.rule_id, CONSTITUTION_CLASS
        ));
        return Classification {
            risk: RiskClass::R2,
            reasons,
        };
    }

    match p.kind.as_str() {
        "add-rule" => {
            if p.pattern.trim().is_empty() {
                reasons.push("add-rule: empty pattern — escalate".into());
                return Classification {
                    risk: RiskClass::R2,
                    reasons,
                };
            }
            if let Err(e) = Regex::new(&p.pattern) {
                reasons.push(format!(
                    "add-rule: pattern does not compile as regex: {e} — escalate"
                ));
                return Classification {
                    risk: RiskClass::R2,
                    reasons,
                };
            }
            if let Some(id) = builtin_rule_id_collision(&p.rule_id) {
                reasons.push(format!(
                    "add-rule: rule_id '{id}' collides with a builtin lint rule — escalate"
                ));
                return Classification {
                    risk: RiskClass::R2,
                    reasons,
                };
            }
            if registry.has_active_rule_id(&p.rule_id) {
                reasons.push(format!(
                    "add-rule: rule_id '{}' collides with an active registry entry — escalate",
                    p.rule_id
                ));
                return Classification {
                    risk: RiskClass::R2,
                    reasons,
                };
            }
            reasons.push(
                "add-rule: valid regex, not constitution-class, no builtin/registry collision — auto-land (R0)"
                    .into(),
            );
            Classification {
                risk: RiskClass::R0,
                reasons,
            }
        }
        "modify-rule" => {
            if registry.has_active_rule_id(&p.rule_id) {
                reasons.push(format!(
                    "modify-rule of existing landed rule '{}' — dial-gated (R1)",
                    p.rule_id
                ));
                Classification {
                    risk: RiskClass::R1,
                    reasons,
                }
            } else {
                reasons.push(format!(
                    "modify-rule targets '{}' which is not an existing active landed rule — escalate",
                    p.rule_id
                ));
                Classification {
                    risk: RiskClass::R2,
                    reasons,
                }
            }
        }
        "kill-rule" => {
            reasons.push("kill-rule always escalates — always a human decision".into());
            Classification {
                risk: RiskClass::R2,
                reasons,
            }
        }
        other => {
            reasons.push(format!("unknown proposal type '{other}' — escalate"));
            Classification {
                risk: RiskClass::R2,
                reasons,
            }
        }
    }
}

/// The builtin lint rule id `rule_id` collides with, if any.
fn builtin_rule_id_collision(rule_id: &str) -> Option<&'static str> {
    footgun_rules()
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| *id == rule_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule_registry::{RuleEntry, RuleStatus};

    fn proposal(kind: &str, rule_id: &str, pattern: &str) -> ProposalForClassify {
        ProposalForClassify {
            kind: kind.to_string(),
            rule_id: rule_id.to_string(),
            pattern: pattern.to_string(),
        }
    }

    fn landed(rule_id: &str, status: RuleStatus) -> RuleEntry {
        RuleEntry {
            rule_id: rule_id.to_string(),
            pattern: "x".to_string(),
            proposal_id: "p-existing".to_string(),
            verdict_sha256: "b".repeat(64),
            landed_ts: "2026-06-11T00:00:00Z".to_string(),
            status,
            reverted_ts: None,
            revert_reason: None,
        }
    }

    // -- R0: auto-land --------------------------------------------------------

    #[test]
    fn applier_r0_add_rule_valid_regex_no_collision_auto_lands() {
        let reg = RuleRegistry::default();
        let p = proposal("add-rule", "new-footgun", "foo.*bar");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R0);
        assert!(c.reasons.last().unwrap().contains("auto-land"));
    }

    #[test]
    fn applier_r0_add_rule_ignores_reverted_registry_entries() {
        // A reverted entry frees up its rule_id for re-landing.
        let mut reg = RuleRegistry::default();
        reg.append(landed("footgun-x", RuleStatus::Reverted));
        let p = proposal("add-rule", "footgun-x", "abc");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R0);
    }

    // -- R2: constitution-class refusal (checked before anything else) ------

    #[test]
    fn applier_r2_constitution_class_refused_for_add_rule() {
        let reg = RuleRegistry::default();
        let p = proposal("add-rule", "kill-gates", "abc");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c.reasons.last().unwrap().contains("constitution-class"));
    }

    #[test]
    fn applier_r2_constitution_class_refused_regardless_of_kind() {
        let reg = RuleRegistry::default();
        for cc in crate::gatekeeper::CONSTITUTION_CLASS {
            for kind in ["add-rule", "modify-rule", "kill-rule"] {
                let p = proposal(kind, cc, "abc");
                let c = classify(&p, &reg);
                assert_eq!(c.risk, RiskClass::R2, "kind={kind} rule_id={cc}");
                assert!(c.reasons.last().unwrap().contains("constitution-class"));
            }
        }
    }

    // -- R2: invalid / empty pattern ------------------------------------------

    #[test]
    fn applier_r2_add_rule_invalid_regex_refused() {
        let reg = RuleRegistry::default();
        let p = proposal("add-rule", "new-footgun", "(unclosed[");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c
            .reasons
            .last()
            .unwrap()
            .contains("does not compile as regex"));
    }

    #[test]
    fn applier_r2_add_rule_empty_pattern_refused() {
        let reg = RuleRegistry::default();
        let p = proposal("add-rule", "new-footgun", "");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c.reasons.last().unwrap().contains("empty pattern"));
    }

    // -- R2: collision refusal (builtin AND active registry) -----------------

    #[test]
    fn applier_r2_add_rule_builtin_collision_refused() {
        let reg = RuleRegistry::default();
        let p = proposal("add-rule", "stderr-swallow", "abc");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c.reasons.last().unwrap().contains("builtin lint rule"));
    }

    #[test]
    fn applier_r2_add_rule_active_registry_collision_refused() {
        let mut reg = RuleRegistry::default();
        reg.append(landed("already-landed", RuleStatus::Active));
        let p = proposal("add-rule", "already-landed", "abc");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c.reasons.last().unwrap().contains("active registry entry"));
    }

    // -- R1: modify-rule of an existing landed rule ---------------------------

    #[test]
    fn applier_r1_modify_rule_of_existing_active_rule() {
        let mut reg = RuleRegistry::default();
        reg.append(landed("footgun-x", RuleStatus::Active));
        let p = proposal("modify-rule", "footgun-x", "new-pattern");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R1);
        assert!(c.reasons.last().unwrap().contains("dial-gated"));
    }

    #[test]
    fn applier_r2_modify_rule_of_nonexistent_rule_escalates() {
        let reg = RuleRegistry::default();
        let p = proposal("modify-rule", "never-landed", "new-pattern");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c
            .reasons
            .last()
            .unwrap()
            .contains("not an existing active landed rule"));
    }

    #[test]
    fn applier_r2_modify_rule_of_reverted_rule_escalates() {
        let mut reg = RuleRegistry::default();
        reg.append(landed("footgun-x", RuleStatus::Reverted));
        let p = proposal("modify-rule", "footgun-x", "new-pattern");
        let c = classify(&p, &reg);
        assert_eq!(
            c.risk,
            RiskClass::R2,
            "reverted rule is not 'existing landed' for modify"
        );
    }

    // -- R2: kill-rule and unknown types always escalate ----------------------

    #[test]
    fn applier_r2_kill_rule_always_escalates() {
        let reg = RuleRegistry::default();
        let p = proposal("kill-rule", "footgun-x", "");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c.reasons.last().unwrap().contains("kill-rule"));
    }

    #[test]
    fn applier_r2_unknown_proposal_type_escalates() {
        let reg = RuleRegistry::default();
        let p = proposal("nuke-everything", "footgun-x", "abc");
        let c = classify(&p, &reg);
        assert_eq!(c.risk, RiskClass::R2);
        assert!(c.reasons.last().unwrap().contains("unknown proposal type"));
    }

    // -- determinism -----------------------------------------------------------

    #[test]
    fn applier_classify_is_deterministic() {
        let reg = RuleRegistry::default();
        let p = proposal("add-rule", "new-footgun", "foo.*bar");
        let c1 = classify(&p, &reg);
        let c2 = classify(&p, &reg);
        assert_eq!(c1.risk, c2.risk);
        assert_eq!(c1.reasons, c2.reasons);
    }
}
