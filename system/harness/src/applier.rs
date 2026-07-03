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
use crate::rule_registry::{RuleEntry, RuleRegistry, RuleStatus};
use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

// ============================================================================
// Stage B — `hex apply run|revert|status`: the I/O shell around `classify`.
//
// Pure deterministic classification stays above this line, untouched. Below
// is every side effect: reading the verdict store + proposal files, writing
// the registry, appending ledger rows, writing escalation evidence packages,
// and firing alerts. No LLM calls anywhere in this path.
// ============================================================================

/// Dial gate constants for R1 (`modify-rule`) proposals — disclosed verbatim
/// in every R1 ledger payload (land or escalate) so the decision is
/// replayable from the ledger alone.
pub const DIAL_MIN_N: usize = 3;
pub const DIAL_LAND_THRESHOLD: f64 = 0.5;
pub const DIAL_AGENT: &str = "proposer";
pub const DIAL_ACTION_CLASS: &str = "proposal.land";

/// Watchdog thresholds (`hex apply watch`, deliverable 6) — disclosed in
/// every watchdog outcome-row payload so the decision is replayable from the
/// ledger alone, exactly like the [`DIAL_*`] constants above.
///
/// - `WATCH_REVERT_MIN_FIRES` / `WATCH_REVERT_MAX_PRECISION`: a landed rule
///   with at least this many raw fires AND a joined precision strictly below
///   this bar is auto-reverted — cheap, fast-acting harm containment.
/// - `WATCH_SUCCESS_MIN_JOINED` / `WATCH_SUCCESS_MIN_PRECISION`: a landed
///   rule with at least this many JOINED gates (a much higher bar than the
///   revert side — precision is only trustworthy with real volume) AND
///   precision at/above this floor earns a one-time `success=true` outcome
///   row, feeding the dial for future R1 `modify-rule` proposals.
pub const WATCH_REVERT_MIN_FIRES: usize = 5;
pub const WATCH_REVERT_MAX_PRECISION: f64 = 0.5;
pub const WATCH_SUCCESS_MIN_JOINED: usize = 30;
pub const WATCH_SUCCESS_MIN_PRECISION: f64 = 0.8;

/// Every path `hex apply` touches, resolved once per invocation. Every field
/// is independently overridable (CLI flags) so tests never read or write the
/// real workspace. Defaults mirror the hex-dir-relative convention already
/// established by `rule_registry::default_path` / `ledger::default_path`.
#[derive(Debug, Clone)]
pub struct ApplyPaths {
    pub hex_dir: PathBuf,
    pub store: PathBuf,
    pub registry: PathBuf,
    pub ledger: PathBuf,
    pub escalations: PathBuf,
    /// Proposal markdown directory. NOT part of the spec's literal CLI
    /// signature (`hex apply run [--store] [--registry] [--ledger]
    /// [--escalations]`) — added because the verdict store JSON has no
    /// `pattern` field (only the proposal file's `toml proposal` block
    /// does), so classification requires reading the proposal file too.
    /// Documented deviation; defaults resolve from hex_dir like every other
    /// path here.
    pub proposals: PathBuf,
}

impl ApplyPaths {
    pub fn defaults(hex_dir: &Path) -> Self {
        ApplyPaths {
            hex_dir: hex_dir.to_path_buf(),
            store: hex_dir.join("projects/agent-infra/gates/verdicts"),
            registry: crate::rule_registry::default_path(hex_dir),
            ledger: crate::ledger::default_path(hex_dir),
            escalations: hex_dir.join("projects/agent-infra/escalations"),
            proposals: hex_dir.join("projects/agent-infra/proposals"),
        }
    }
}

/// Errors surfaced by the apply shell. Every variant carries enough context
/// to log loudly (S6) — nothing here is ever silently swallowed.
#[derive(Debug)]
pub enum ApplyError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Registry(crate::rule_registry::RegistryError),
    Ledger(crate::ledger::LedgerError),
    Msg(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Io(e) => write!(f, "apply: io error: {e}"),
            ApplyError::Json(e) => write!(f, "apply: json error: {e}"),
            ApplyError::Registry(e) => write!(f, "apply: registry error: {e}"),
            ApplyError::Ledger(e) => write!(f, "apply: ledger error: {e}"),
            ApplyError::Msg(m) => write!(f, "apply: {m}"),
        }
    }
}
impl std::error::Error for ApplyError {}
impl From<std::io::Error> for ApplyError {
    fn from(e: std::io::Error) -> Self {
        ApplyError::Io(e)
    }
}
impl From<serde_json::Error> for ApplyError {
    fn from(e: serde_json::Error) -> Self {
        ApplyError::Json(e)
    }
}
impl From<crate::rule_registry::RegistryError> for ApplyError {
    fn from(e: crate::rule_registry::RegistryError) -> Self {
        ApplyError::Registry(e)
    }
}
impl From<crate::ledger::LedgerError> for ApplyError {
    fn from(e: crate::ledger::LedgerError) -> Self {
        ApplyError::Ledger(e)
    }
}

/// Result of one `hex apply run` invocation. Every field lists proposal ids.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RunReport {
    pub landed: Vec<String>,
    pub escalated: Vec<String>,
    pub skipped: Vec<String>,
}

impl RunReport {
    /// True when this run landed or escalated nothing — i.e. every survivor
    /// was already accounted for (registry / escalations / ledger). This is
    /// the idempotency contract: a second run against unchanged input must
    /// report `is_noop() == true` and must not have written anything new.
    pub fn is_noop(&self) -> bool {
        self.landed.is_empty() && self.escalated.is_empty()
    }
}

/// Result of one `hex apply watch` invocation — every field lists rule ids.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct WatchReport {
    /// Auto-reverted this run (>= WATCH_REVERT_MIN_FIRES fires, precision <
    /// WATCH_REVERT_MAX_PRECISION).
    pub reverted: Vec<String>,
    /// Scored `success=true` this run (>= WATCH_SUCCESS_MIN_JOINED joined,
    /// precision >= WATCH_SUCCESS_MIN_PRECISION, not previously scored).
    pub scored_success: Vec<String>,
    /// Neither threshold met — no outcome row written (insufficient
    /// evidence is not an outcome).
    pub insufficient_evidence: Vec<String>,
    /// Met the success bar but a `success=true` row already exists for this
    /// rule_id — refused to double-score.
    pub already_scored: Vec<String>,
}

/// Read-only summary for `hex apply status`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusReport {
    pub registry_entries: Vec<RuleEntry>,
    /// ACCEPT_FLAGGED proposal ids in the store not yet landed, escalated, or
    /// recorded in the ledger — i.e. what the next `hex apply run` would act on.
    pub pending: Vec<String>,
    /// Proposal ids with an escalation evidence file on disk.
    pub escalations: Vec<String>,
}

/// One ACCEPT_FLAGGED verdict pulled from the store, plus its raw JSON (kept
/// verbatim for `verdict_sha256` and for embedding in escalation evidence).
struct Survivor {
    proposal_id: String,
    rule_id: String,
    kind: String,
    raw_json: String,
}

/// Scan `store` for `*.verdict.json` files whose `verdict` field is
/// `ACCEPT_FLAGGED`. Missing store dir => empty (day one: nothing judged
/// yet). Sorted by filename for deterministic processing order.
fn scan_accept_flagged(store: &Path) -> Result<Vec<Survivor>, ApplyError> {
    let mut out = Vec::new();
    if !store.exists() {
        return Ok(out);
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(store)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".verdict.json"))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();

    for path in paths {
        let raw = std::fs::read_to_string(&path)?;
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            ApplyError::Msg(format!("verdict {}: malformed JSON: {e}", path.display()))
        })?;
        if v.get("verdict").and_then(|x| x.as_str()) != Some("ACCEPT_FLAGGED") {
            continue;
        }
        let proposal_id = v
            .get("proposal_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| {
                ApplyError::Msg(format!("verdict {}: missing proposal_id", path.display()))
            })?
            .to_string();
        let rule_id = v
            .get("rule_id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        let kind = v
            .get("kind")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        out.push(Survivor {
            proposal_id,
            rule_id,
            kind,
            raw_json: raw,
        });
    }
    Ok(out)
}

/// Proposal ids with an escalation evidence file already on disk. Missing
/// dir => empty set (not an error — nothing escalated yet).
fn escalated_ids(escalations_dir: &Path) -> Result<HashSet<String>, ApplyError> {
    let mut out = HashSet::new();
    if !escalations_dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(escalations_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.insert(stem.to_string());
            }
        }
    }
    Ok(out)
}

/// Proposal ids already recorded as `rule.land` / `rule.escalate` action rows
/// in the ledger. Reads the ledger directly with its own connection (mirrors
/// `main.rs`'s own outcome-row loader below — `applier.rs` lives in the LIB
/// crate and cannot call the BINARY crate's private `load_outcome_rows`).
fn ledger_recorded_proposal_ids(ledger_path: &Path) -> Result<HashSet<String>, ApplyError> {
    if !ledger_path.exists() {
        return Ok(HashSet::new());
    }
    let conn = rusqlite::Connection::open(ledger_path)
        .map_err(|e| ApplyError::Msg(format!("open ledger for scan: {e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT payload FROM ledger WHERE kind='action' AND action_class IN ('rule.land','rule.escalate')",
        )
        .map_err(|e| ApplyError::Msg(format!("prepare ledger scan: {e}")))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| ApplyError::Msg(format!("query ledger scan: {e}")))?;
    let mut out = HashSet::new();
    for row in rows {
        let payload = row.map_err(|e| ApplyError::Msg(format!("ledger row read: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| ApplyError::Msg(format!("ledger payload parse: {e}")))?;
        if let Some(id) = v.get("proposal_id").and_then(|x| x.as_str()) {
            out.insert(id.to_string());
        }
    }
    Ok(out)
}

/// Load every `outcome`-kind row from the ledger into [`crate::dial::OutcomeRow`]s
/// for the dial gate. Deliberately duplicates `main.rs`'s private
/// `load_outcome_rows` (SQL + parsing contract confirmed identical: `agent`/
/// `action_class` are ledger table columns, `success` is `payload["success"]`
/// as a JSON bool, default `false`) — the binary crate's helper is `fn`-private
/// and this module lives in the lib crate, so it cannot be called directly.
/// Errors loudly per S6 — no silent skip on a malformed row.
fn load_outcome_rows(ledger_path: &Path) -> Result<Vec<crate::dial::OutcomeRow>, ApplyError> {
    let conn = rusqlite::Connection::open(ledger_path)
        .map_err(|e| ApplyError::Msg(format!("open ledger for dial: {e}")))?;
    let mut stmt = conn
        .prepare("SELECT ts, agent, action_class, payload FROM ledger WHERE kind='outcome'")
        .map_err(|e| ApplyError::Msg(format!("prepare dial scan: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| ApplyError::Msg(format!("query dial scan: {e}")))?;
    let mut out = Vec::new();
    for row in rows {
        let (ts, agent, action_class, payload) =
            row.map_err(|e| ApplyError::Msg(format!("dial row read: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| ApplyError::Msg(format!("dial payload parse (ts={ts}): {e}")))?;
        let success = v.get("success").and_then(|x| x.as_bool()).unwrap_or(false);
        out.push(crate::dial::OutcomeRow {
            agent,
            action_class,
            success,
            ts,
        });
    }
    Ok(out)
}

/// True if the ledger already carries a `(proposer, proposal.land)` OUTCOME
/// row with `success=true` for `rule_id` — the watchdog's double-score
/// refusal check. Scans every outcome row (cheap: outcome volume is bounded
/// by rule count, not command volume) rather than trusting an in-memory
/// per-run set, so double-scoring is refused even across separate `hex apply
/// watch` process invocations (the actual repeated-cron scenario).
fn watch_success_already_recorded(ledger_path: &Path, rule_id: &str) -> Result<bool, ApplyError> {
    if !ledger_path.exists() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open(ledger_path)
        .map_err(|e| ApplyError::Msg(format!("open ledger for watch scan: {e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT payload FROM ledger \
             WHERE kind='outcome' AND agent=?1 AND action_class=?2",
        )
        .map_err(|e| ApplyError::Msg(format!("prepare watch scan: {e}")))?;
    let rows = stmt
        .query_map([DIAL_AGENT, DIAL_ACTION_CLASS], |r| r.get::<_, String>(0))
        .map_err(|e| ApplyError::Msg(format!("query watch scan: {e}")))?;
    for row in rows {
        let payload = row.map_err(|e| ApplyError::Msg(format!("watch scan row read: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| ApplyError::Msg(format!("watch scan payload parse: {e}")))?;
        let matches_rule = v.get("rule_id").and_then(|x| x.as_str()) == Some(rule_id);
        let is_success = v.get("success").and_then(|x| x.as_bool()).unwrap_or(false);
        if matches_rule && is_success {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Constants disclosed in every land/escalate ledger payload — makes the
/// decision replayable from the ledger alone, without cross-referencing
/// source at the time of read.
fn constants_json() -> serde_json::Value {
    serde_json::json!({
        "constitution_class": CONSTITUTION_CLASS,
        "dial_min_n": DIAL_MIN_N,
        "dial_land_threshold": DIAL_LAND_THRESHOLD,
        "dial_agent": DIAL_AGENT,
        "dial_action_class": DIAL_ACTION_CLASS,
    })
}

/// Scan the verdict store for new ACCEPT_FLAGGED survivors and land (R0) or
/// escalate (R1 below dial threshold, R2, constitution-class refusal,
/// missing/unparseable proposal file) each one. Idempotent: a proposal id
/// already landed (registry), already escalated (escalations dir), or
/// already recorded in the ledger is skipped and counted in
/// `RunReport::skipped` — no new I/O happens for it, so a second run against
/// unchanged input is a true no-op (`RunReport::is_noop()`).
pub fn run(paths: &ApplyPaths) -> Result<RunReport, ApplyError> {
    let mut report = RunReport::default();

    let mut registry = crate::rule_registry::load(&paths.registry)?;
    let escalated_already = escalated_ids(&paths.escalations)?;
    let ledger_ids = ledger_recorded_proposal_ids(&paths.ledger)?;
    let survivors = scan_accept_flagged(&paths.store)?;

    for s in survivors {
        let already = registry
            .entries
            .iter()
            .any(|e| e.proposal_id == s.proposal_id)
            || escalated_already.contains(&s.proposal_id)
            || ledger_ids.contains(&s.proposal_id);
        if already {
            report.skipped.push(s.proposal_id.clone());
            continue;
        }

        // Apply-time defense in depth: refuse CONSTITUTION_CLASS rule_ids
        // regardless of verdict or classification — checked here, before we
        // even open the proposal file, and again at the actual land site in
        // `land()` below (belt-and-suspenders against a future classify()
        // regression).
        if CONSTITUTION_CLASS.iter().any(|c| s.rule_id.trim() == *c) {
            let reasons = vec![format!(
                "apply-time defense-in-depth: rule_id '{}' is constitution-class ({:?}) — refused regardless of verdict",
                s.rule_id, CONSTITUTION_CLASS
            )];
            escalate(paths, &s, None, "R2(constitution-class)", reasons)?;
            report.escalated.push(s.proposal_id.clone());
            continue;
        }

        let proposal_path = paths.proposals.join(format!("{}.md", s.proposal_id));
        let proposal_md = match std::fs::read_to_string(&proposal_path) {
            Ok(md) => md,
            Err(e) => {
                let reasons = vec![format!(
                    "proposal file {} missing/unreadable: {e} — escalate for manual review",
                    proposal_path.display()
                )];
                escalate(paths, &s, None, "R2(missing-proposal-file)", reasons)?;
                report.escalated.push(s.proposal_id.clone());
                continue;
            }
        };
        let parsed = match crate::gatekeeper::parse_proposal(&proposal_md) {
            Ok(p) => p,
            Err(e) => {
                let reasons = vec![format!(
                    "proposal file {} failed to parse: {e} — escalate for manual review",
                    proposal_path.display()
                )];
                escalate(
                    paths,
                    &s,
                    Some(proposal_md.clone()),
                    "R2(unparseable-proposal)",
                    reasons,
                )?;
                report.escalated.push(s.proposal_id.clone());
                continue;
            }
        };

        let for_classify = ProposalForClassify {
            kind: parsed.proposal.kind.clone(),
            rule_id: parsed.proposal.rule_id.clone(),
            pattern: parsed.proposal.pattern.clone(),
        };
        let class = classify(&for_classify, &registry);

        match class.risk {
            RiskClass::R0 => {
                land(
                    paths,
                    &mut registry,
                    &s,
                    &for_classify,
                    &class.reasons,
                    "R0",
                )?;
                report.landed.push(s.proposal_id.clone());
            }
            RiskClass::R1 => {
                let dial_rows = load_outcome_rows(&paths.ledger)?;
                let dial = crate::dial::compute(
                    &dial_rows,
                    DIAL_AGENT,
                    DIAL_ACTION_CLASS,
                    DIAL_MIN_N,
                    false,
                );
                let mut reasons = class.reasons.clone();
                let should_land = match dial {
                    crate::dial::DialOutcome::Score(sc) if sc >= DIAL_LAND_THRESHOLD => {
                        reasons.push(format!(
                            "dial({DIAL_AGENT},{DIAL_ACTION_CLASS}) = Score({sc:.4}) >= {DIAL_LAND_THRESHOLD} — land as R0 (dial-lifted)"
                        ));
                        true
                    }
                    crate::dial::DialOutcome::Score(sc) => {
                        reasons.push(format!(
                            "dial({DIAL_AGENT},{DIAL_ACTION_CLASS}) = Score({sc:.4}) < {DIAL_LAND_THRESHOLD} — escalate"
                        ));
                        false
                    }
                    crate::dial::DialOutcome::Insufficient { n, min_n } => {
                        reasons.push(format!(
                            "dial({DIAL_AGENT},{DIAL_ACTION_CLASS}) = INSUFFICIENT (n={n} < min_n={min_n}) — escalate"
                        ));
                        false
                    }
                    crate::dial::DialOutcome::Ask => {
                        reasons.push(format!(
                            "dial({DIAL_AGENT},{DIAL_ACTION_CLASS}) = ASK (irreversible) — escalate"
                        ));
                        false
                    }
                };
                if should_land {
                    land(
                        paths,
                        &mut registry,
                        &s,
                        &for_classify,
                        &reasons,
                        "R1(dial-lifted)",
                    )?;
                    report.landed.push(s.proposal_id.clone());
                } else {
                    escalate(paths, &s, Some(proposal_md.clone()), "R1", reasons)?;
                    report.escalated.push(s.proposal_id.clone());
                }
            }
            RiskClass::R2 => {
                escalate(
                    paths,
                    &s,
                    Some(proposal_md.clone()),
                    "R2",
                    class.reasons.clone(),
                )?;
                report.escalated.push(s.proposal_id.clone());
            }
        }
    }

    Ok(report)
}

/// Land one survivor: write the registry entry, then the `rule.land` ledger
/// action row. `modify-rule` (R1) supersedes the existing active entry for
/// the same `rule_id` (registry `revert()` + append new) rather than
/// appending a second active row — the one-active-per-`rule_id` invariant
/// must hold, since `lint_gates::analyze_command_with` compiles every ACTIVE
/// entry. This internal supersede is NOT a user revert (`hex apply revert`):
/// it writes no `proposal.land` outcome row, only the `rule.land` action row.
fn land(
    paths: &ApplyPaths,
    registry: &mut RuleRegistry,
    s: &Survivor,
    for_classify: &ProposalForClassify,
    reasons: &[String],
    risk_label: &str,
) -> Result<(), ApplyError> {
    // Belt-and-suspenders: this should be unreachable (classify() already
    // refuses constitution-class rule_ids, and `run()` checks again before
    // calling `land`), but the actual write path re-checks once more so a
    // future regression upstream can never land one.
    if CONSTITUTION_CLASS
        .iter()
        .any(|c| for_classify.rule_id.trim() == *c)
    {
        return Err(ApplyError::Msg(format!(
            "refusing to land constitution-class rule_id '{}' — unreachable via classify(), refused again at the write path",
            for_classify.rule_id
        )));
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let verdict_sha256 = crate::gatekeeper::sha256_hex(&s.raw_json);

    if registry.has_active_rule_id(&for_classify.rule_id) {
        registry.revert(
            &for_classify.rule_id,
            &format!(
                "superseded by {} (applier auto-land, modify-rule)",
                s.proposal_id
            ),
            &now,
        )?;
    }

    registry.append(RuleEntry {
        rule_id: for_classify.rule_id.clone(),
        pattern: for_classify.pattern.clone(),
        proposal_id: s.proposal_id.clone(),
        verdict_sha256: verdict_sha256.clone(),
        landed_ts: now.clone(),
        status: RuleStatus::Active,
        reverted_ts: None,
        revert_reason: None,
    });
    crate::rule_registry::save(&paths.registry, registry)?;

    let registry_bytes = std::fs::read(&paths.registry)?;
    let registry_sha256 = crate::gatekeeper::sha256_hex(&String::from_utf8_lossy(&registry_bytes));

    let ledger = crate::ledger::Ledger::open(&paths.ledger)?;
    let payload = serde_json::json!({
        "proposal_id": s.proposal_id,
        "rule_id": for_classify.rule_id,
        "kind": for_classify.kind,
        "risk": risk_label,
        "reasons": reasons,
        "constants": constants_json(),
        "verdict_sha256": verdict_sha256,
        "registry_sha256": registry_sha256,
    });
    ledger.append("applier", "rule.land", "action", &payload)?;

    Ok(())
}

/// Write an evidence-package escalation for one survivor: markdown to
/// `escalations/<proposal-id>.md` (proposal body when available, the raw
/// verdict JSON, risk reasons, suggested next action), a `rule.escalate`
/// ledger action row, and an alert.
fn escalate(
    paths: &ApplyPaths,
    s: &Survivor,
    proposal_md: Option<String>,
    risk_label: &str,
    reasons: Vec<String>,
) -> Result<(), ApplyError> {
    std::fs::create_dir_all(&paths.escalations)?;
    let evidence_path = paths.escalations.join(format!("{}.md", s.proposal_id));

    let body = proposal_md.unwrap_or_else(|| {
        "_(proposal markdown file was missing or unreadable at escalation time — see reasons below)_"
            .to_string()
    });
    let reasons_md = reasons
        .iter()
        .map(|r| format!("- {r}"))
        .collect::<Vec<_>>()
        .join("\n");
    let suggested = suggested_next_action(&s.kind, risk_label);

    let doc = format!(
        "# Escalation: {id}\n\n\
- **rule_id:** `{rule_id}`\n\
- **kind:** `{kind}`\n\
- **risk:** {risk_label}\n\n\
## Risk classification reasons\n\n{reasons_md}\n\n\
## Proposal body\n\n{body}\n\n\
## Verdict (store copy)\n\n```json\n{verdict_json}\n```\n\n\
## Suggested next action\n\n{suggested}\n",
        id = s.proposal_id,
        rule_id = s.rule_id,
        kind = s.kind,
        risk_label = risk_label,
        reasons_md = reasons_md,
        body = body,
        verdict_json = s.raw_json,
        suggested = suggested,
    );
    std::fs::write(&evidence_path, doc)?;

    let ledger = crate::ledger::Ledger::open(&paths.ledger)?;
    let payload = serde_json::json!({
        "proposal_id": s.proposal_id,
        "rule_id": s.rule_id,
        "kind": s.kind,
        "risk": risk_label,
        "reasons": reasons,
        "constants": constants_json(),
        "escalation_path": evidence_path.to_string_lossy(),
    });
    ledger.append("applier", "rule.escalate", "action", &payload)?;

    crate::alert::notify_at(
        &paths.hex_dir,
        &format!("apply-escalate-{}", s.proposal_id),
        "hex apply: proposal escalated",
        &format!(
            "{} (rule_id={}) risk={} — see {}",
            s.proposal_id,
            s.rule_id,
            risk_label,
            evidence_path.display()
        ),
    );

    Ok(())
}

fn suggested_next_action(kind: &str, risk_label: &str) -> &'static str {
    if kind == "kill-rule" {
        "kill-rule always requires a human decision — review the proposal and either apply the \
kill manually or reject the proposal upstream. The applier never auto-executes kill-rule."
    } else if risk_label == "R1" {
        "Dial-gated modify-rule below the land threshold — either wait for more successful \
`proposal.land` outcomes to accumulate (raises the dial score), or have a human review and land \
it manually."
    } else if risk_label.starts_with("R2(constitution-class)") {
        "This rule_id is constitution-class — it must never be auto-landed regardless of verdict. \
Manual review required; if the change is legitimate it must go through the charter/constitution \
amendment path, not the applier."
    } else if risk_label.contains("missing-proposal-file")
        || risk_label.contains("unparseable-proposal")
    {
        "The proposal markdown file could not be read/parsed — investigate why it's missing or \
malformed before deciding whether to land or reject."
    } else {
        "Review the proposal and verdict manually. If it should land, apply it by hand (e.g. edit \
the rule registry directly) and record the decision; if not, reject the proposal upstream."
    }
}

/// Flip a landed rule's registry status to `reverted` (entry preserved,
/// never deleted) and append exactly one `proposal.land` outcome row with
/// `success=false` recording the manual revert. Data-only: does not touch
/// the shadow linter directly — the next `lint-gates` invocation reloads the
/// registry and picks up the status flip.
pub fn revert(paths: &ApplyPaths, rule_id: &str, why: &str) -> Result<(), ApplyError> {
    let mut registry = crate::rule_registry::load(&paths.registry)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let proposal_id = registry
        .entries
        .iter()
        .rev()
        .find(|e| e.rule_id == rule_id && e.status == RuleStatus::Active)
        .map(|e| e.proposal_id.clone());

    registry.revert(rule_id, why, &now)?;
    crate::rule_registry::save(&paths.registry, &registry)?;

    let ledger = crate::ledger::Ledger::open(&paths.ledger)?;
    let payload = serde_json::json!({
        "success": false,
        "rule_id": rule_id,
        "proposal_id": proposal_id,
        "manual_revert": true,
        "why": why,
        "reverted_ts": now,
    });
    ledger.append("proposer", "proposal.land", "outcome", &payload)?;

    crate::alert::notify_at(
        &paths.hex_dir,
        &format!("apply-revert-{rule_id}"),
        "hex apply: rule reverted",
        &format!("rule_id '{rule_id}' manually reverted: {why}"),
    );

    Ok(())
}

/// The outcome watchdog (`hex apply watch`, deliverable 6): for each ACTIVE
/// landed rule, compute wild stats via [`crate::wild::rule_wild_stats`] (the
/// SAME join `hex ledger wild` uses — extracted/shared, not duplicated) and:
///
/// - `fires >= WATCH_REVERT_MIN_FIRES && precision < WATCH_REVERT_MAX_PRECISION`
///   => AUTO-REVERT: registry status flip + `success=false` outcome row +
///   loud alert.
/// - `joined >= WATCH_SUCCESS_MIN_JOINED && precision >= WATCH_SUCCESS_MIN_PRECISION`
///   => `success=true` outcome row, EXACTLY ONCE per rule_id (checked against
///   the ledger, not just this run's in-memory state, so repeated `hex apply
///   watch` invocations — the real cron scenario — never double-score).
/// - Otherwise: no row. Insufficient evidence is not an outcome.
///
/// Both outcome-row kinds go to `(DIAL_AGENT, DIAL_ACTION_CLASS)` =
/// `("proposer", "proposal.land")` — the exact pair the existing gatekeeper
/// dial glue (`main.rs`'s `load_outcome_rows` + `hex::dial::compute`)
/// already consumes, so R1 `modify-rule` dial-gating benefits from watchdog
/// evidence with ZERO gatekeeper changes.
pub fn watch(paths: &ApplyPaths) -> Result<WatchReport, ApplyError> {
    let mut report = WatchReport::default();
    let mut registry = crate::rule_registry::load(&paths.registry)?;

    // Snapshot BEFORE mutating — reverting rule N must not skip/reprocess
    // rule N+1 in the same pass, and a rule reverted this run is still
    // scored/reported exactly once (as reverted, not re-evaluated).
    let active_rule_ids: Vec<String> = registry
        .active_entries()
        .map(|e| e.rule_id.clone())
        .collect();

    for rule_id in active_rule_ids {
        let stats =
            crate::wild::rule_wild_stats(&paths.ledger, &rule_id).map_err(ApplyError::Msg)?;

        let revert_hit = stats.fires >= WATCH_REVERT_MIN_FIRES
            && stats
                .precision
                .map(|p| p < WATCH_REVERT_MAX_PRECISION)
                .unwrap_or(false);
        let success_hit = stats.joined >= WATCH_SUCCESS_MIN_JOINED
            && stats
                .precision
                .map(|p| p >= WATCH_SUCCESS_MIN_PRECISION)
                .unwrap_or(false);

        if revert_hit {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let reason = format!(
                "auto-revert: fires={} joined={} precision={:.4} < {} (tp={}, fp={})",
                stats.fires,
                stats.joined,
                stats.precision.unwrap_or(0.0),
                WATCH_REVERT_MAX_PRECISION,
                stats.tp,
                stats.fp,
            );
            registry.revert(&rule_id, &reason, &now)?;
            crate::rule_registry::save(&paths.registry, &registry)?;

            let ledger = crate::ledger::Ledger::open(&paths.ledger)?;
            let payload = serde_json::json!({
                "success": false,
                "rule_id": rule_id,
                "auto_revert": true,
                "why": reason,
                "fires": stats.fires,
                "joined": stats.joined,
                "tp": stats.tp,
                "fp": stats.fp,
                "precision": stats.precision,
                "threshold_min_fires": WATCH_REVERT_MIN_FIRES,
                "threshold_max_precision": WATCH_REVERT_MAX_PRECISION,
                "ts": now,
            });
            ledger.append("proposer", "proposal.land", "outcome", &payload)?;

            crate::alert::notify_at(
                &paths.hex_dir,
                &format!("apply-watch-revert-{rule_id}"),
                "hex apply watch: rule auto-reverted",
                &format!("rule_id '{rule_id}' auto-reverted: {reason}"),
            );

            report.reverted.push(rule_id);
            continue;
        }

        if success_hit {
            if watch_success_already_recorded(&paths.ledger, &rule_id)? {
                report.already_scored.push(rule_id);
                continue;
            }
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let ledger = crate::ledger::Ledger::open(&paths.ledger)?;
            let payload = serde_json::json!({
                "success": true,
                "rule_id": rule_id,
                "watchdog_scored": true,
                "fires": stats.fires,
                "joined": stats.joined,
                "tp": stats.tp,
                "fp": stats.fp,
                "precision": stats.precision,
                "threshold_min_joined": WATCH_SUCCESS_MIN_JOINED,
                "threshold_min_precision": WATCH_SUCCESS_MIN_PRECISION,
                "ts": now,
            });
            ledger.append("proposer", "proposal.land", "outcome", &payload)?;

            report.scored_success.push(rule_id);
            continue;
        }

        report.insufficient_evidence.push(rule_id);
    }

    Ok(report)
}

/// Read-only summary: registry entries + pending ACCEPT_FLAGGED verdicts
/// (what the next `run` would act on) + escalations on disk. Never mutates
/// anything; callers should treat this as always-exit-0.
pub fn status(paths: &ApplyPaths) -> Result<StatusReport, ApplyError> {
    let registry = crate::rule_registry::load(&paths.registry)?;
    let escalated_already = escalated_ids(&paths.escalations)?;
    let ledger_ids = ledger_recorded_proposal_ids(&paths.ledger)?;
    let survivors = scan_accept_flagged(&paths.store)?;

    let mut pending = Vec::new();
    for s in &survivors {
        let already = registry
            .entries
            .iter()
            .any(|e| e.proposal_id == s.proposal_id)
            || escalated_already.contains(&s.proposal_id)
            || ledger_ids.contains(&s.proposal_id);
        if !already {
            pending.push(s.proposal_id.clone());
        }
    }

    let mut escalations: Vec<String> = escalated_already.into_iter().collect();
    escalations.sort();

    Ok(StatusReport {
        registry_entries: registry.entries,
        pending,
        escalations,
    })
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

    // =======================================================================
    // Stage B — `hex apply run|revert|status` (I/O, ledger trail, escalations)
    // =======================================================================

    use tempfile::TempDir;

    /// Keeps the tempdir alive for the fixture's lifetime (paths point inside it).
    struct Fixture {
        _dir: TempDir,
        paths: ApplyPaths,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("tempdir");
        let paths = ApplyPaths::defaults(dir.path());
        Fixture { _dir: dir, paths }
    }

    fn write_verdict(
        paths: &ApplyPaths,
        proposal_id: &str,
        rule_id: &str,
        kind: &str,
        verdict: &str,
    ) {
        std::fs::create_dir_all(&paths.store).unwrap();
        let json = serde_json::json!({
            "verdict": verdict,
            "proposal_id": proposal_id,
            "rule_id": rule_id,
            "kind": kind,
            "reasons": ["synthetic test fixture"],
            "precision": 0.9,
            "tp": 9,
            "fp": 1,
            "fn_": 0,
            "floor": 0.5,
            "dial": "UNAVAILABLE",
            "now": "2026-06-12T00:00:00Z",
            "meta": {},
        });
        std::fs::write(
            paths.store.join(format!("{proposal_id}.verdict.json")),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();
    }

    /// Mirrors the fenced-block format `gatekeeper::parse_proposal` expects
    /// (same shape as gatekeeper.rs's own `proposal_md` test fixture).
    fn write_proposal_md(paths: &ApplyPaths, id: &str, kind: &str, rule_id: &str, pattern: &str) {
        std::fs::create_dir_all(&paths.proposals).unwrap();
        let md = format!(
            "# proposal\n\n```toml proposal\nid = \"{id}\"\nagent = \"proposer\"\ncreated = \"2026-06-10T00:00:00Z\"\ntype = \"{kind}\"\nrule_id = \"{rule_id}\"\npattern = {pattern:?}\nrationale = \"synthetic test fixture\"\n```\n\n```toml selftest\nfire = []\nclean = []\n```\n"
        );
        std::fs::write(paths.proposals.join(format!("{id}.md")), md).unwrap();
    }

    /// Seed `successes` consecutive `(proposer, proposal.land)` outcome rows
    /// with success=true. Per dial.rs (EARN_ALPHA=0.20): 3 wins -> 0.488
    /// (< 0.5 threshold), 4 wins -> 0.5904 (>= 0.5 threshold).
    fn seed_dial_outcomes(paths: &ApplyPaths, successes: usize) {
        let ledger = crate::ledger::Ledger::open(&paths.ledger).unwrap();
        for _ in 0..successes {
            ledger
                .append(
                    DIAL_AGENT,
                    DIAL_ACTION_CLASS,
                    "outcome",
                    &serde_json::json!({"success": true}),
                )
                .unwrap();
        }
    }

    fn count_ledger_rows(paths: &ApplyPaths) -> i64 {
        let conn = rusqlite::Connection::open(&paths.ledger).unwrap();
        conn.query_row("SELECT COUNT(*) FROM ledger", [], |r| r.get(0))
            .unwrap()
    }

    // -- run: R0 lands -----------------------------------------------------

    #[test]
    fn applier_run_lands_r0_add_rule() {
        let f = fixture();
        write_verdict(
            &f.paths,
            "p-r0",
            "new-footgun-r0",
            "add-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(&f.paths, "p-r0", "add-rule", "new-footgun-r0", "foo.*bar");

        let report = run(&f.paths).expect("run");
        assert_eq!(report.landed, vec!["p-r0".to_string()]);
        assert!(report.escalated.is_empty());
        assert!(report.skipped.is_empty());

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        assert_eq!(reg.entries.len(), 1);
        assert_eq!(reg.entries[0].rule_id, "new-footgun-r0");
        assert_eq!(reg.entries[0].proposal_id, "p-r0");
        assert_eq!(reg.entries[0].status, RuleStatus::Active);

        assert_eq!(count_ledger_rows(&f.paths), 1);
    }

    // -- run: idempotency ----------------------------------------------------

    #[test]
    fn applier_run_idempotent_second_run_is_noop() {
        let f = fixture();
        write_verdict(
            &f.paths,
            "p-idem",
            "idem-footgun",
            "add-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(
            &f.paths,
            "p-idem",
            "add-rule",
            "idem-footgun",
            "idempotent.*pattern",
        );

        let first = run(&f.paths).expect("first run");
        assert_eq!(first.landed, vec!["p-idem".to_string()]);

        let registry_before = std::fs::read_to_string(&f.paths.registry).unwrap();
        let ledger_rows_before = count_ledger_rows(&f.paths);

        let second = run(&f.paths).expect("second run");
        assert!(second.is_noop(), "second run must be a no-op: {second:?}");
        assert_eq!(second.skipped, vec!["p-idem".to_string()]);

        let registry_after = std::fs::read_to_string(&f.paths.registry).unwrap();
        let ledger_rows_after = count_ledger_rows(&f.paths);
        assert_eq!(
            registry_before, registry_after,
            "registry must be byte-identical after a no-op rerun"
        );
        assert_eq!(
            ledger_rows_before, ledger_rows_after,
            "no new ledger rows may be written on a no-op rerun"
        );
    }

    // -- run: R1 dial gating, both sides of 0.5 ------------------------------

    #[test]
    fn applier_run_r1_lands_when_dial_score_at_or_above_threshold() {
        let f = fixture();
        let mut reg = RuleRegistry::default();
        reg.append(landed("modify-me", RuleStatus::Active));
        crate::rule_registry::save(&f.paths.registry, &reg).unwrap();

        seed_dial_outcomes(&f.paths, 4); // score 0.5904 >= 0.5

        write_verdict(
            &f.paths,
            "p-r1-land",
            "modify-me",
            "modify-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(
            &f.paths,
            "p-r1-land",
            "modify-rule",
            "modify-me",
            "new.*pattern",
        );

        let report = run(&f.paths).expect("run");
        assert_eq!(report.landed, vec!["p-r1-land".to_string()]);
        assert!(report.escalated.is_empty());

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        assert_eq!(
            reg.entries.len(),
            2,
            "old entry superseded, new one appended"
        );
        assert_eq!(
            reg.active_entries().count(),
            1,
            "exactly one active entry per rule_id"
        );
        let active = reg.active_entries().next().unwrap();
        assert_eq!(active.proposal_id, "p-r1-land");
        assert_eq!(active.pattern, "new.*pattern");
        assert_eq!(reg.entries[0].status, RuleStatus::Reverted);
    }

    #[test]
    fn applier_run_r1_escalates_when_dial_score_below_threshold() {
        let f = fixture();
        let mut reg = RuleRegistry::default();
        reg.append(landed("modify-me-2", RuleStatus::Active));
        crate::rule_registry::save(&f.paths.registry, &reg).unwrap();

        seed_dial_outcomes(&f.paths, 3); // score 0.488 < 0.5

        write_verdict(
            &f.paths,
            "p-r1-esc",
            "modify-me-2",
            "modify-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(
            &f.paths,
            "p-r1-esc",
            "modify-rule",
            "modify-me-2",
            "new.*pattern",
        );

        let report = run(&f.paths).expect("run");
        assert_eq!(report.escalated, vec!["p-r1-esc".to_string()]);
        assert!(report.landed.is_empty());

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        assert_eq!(
            reg.entries.len(),
            1,
            "nothing landed — original entry untouched"
        );
        assert_eq!(reg.entries[0].status, RuleStatus::Active);

        let evidence = std::fs::read_to_string(f.paths.escalations.join("p-r1-esc.md")).unwrap();
        assert!(
            evidence.contains("dial"),
            "evidence should disclose the dial reasoning: {evidence}"
        );
    }

    // -- run: R2 + constitution-class refusal --------------------------------

    #[test]
    fn applier_run_r2_kill_rule_escalates_with_evidence() {
        let f = fixture();
        write_verdict(
            &f.paths,
            "p-kill",
            "footgun-y",
            "kill-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(&f.paths, "p-kill", "kill-rule", "footgun-y", "");

        let report = run(&f.paths).expect("run");
        assert_eq!(report.escalated, vec!["p-kill".to_string()]);

        let evidence = std::fs::read_to_string(f.paths.escalations.join("p-kill.md")).unwrap();
        assert!(evidence.contains("kill-rule"));
        assert!(evidence.contains("human decision"));

        assert_eq!(count_ledger_rows(&f.paths), 1);
    }

    #[test]
    fn applier_run_refuses_constitution_class_regardless_of_verdict() {
        let f = fixture();
        // add-rule would otherwise classify R0 — constitution-class refusal
        // must override that at apply time, regardless of verdict/classify.
        write_verdict(
            &f.paths,
            "p-const",
            "kill-gates",
            "add-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(
            &f.paths,
            "p-const",
            "add-rule",
            "kill-gates",
            "valid.*regex",
        );

        let report = run(&f.paths).expect("run");
        assert_eq!(report.escalated, vec!["p-const".to_string()]);
        assert!(report.landed.is_empty());

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        assert!(
            reg.entries.is_empty(),
            "constitution-class rule_id must never land"
        );

        let evidence = std::fs::read_to_string(f.paths.escalations.join("p-const.md")).unwrap();
        assert!(evidence.contains("constitution-class"));
    }

    #[test]
    fn applier_run_escalates_loudly_on_missing_proposal_file() {
        let f = fixture();
        write_verdict(
            &f.paths,
            "p-nofile",
            "footgun-z",
            "add-rule",
            "ACCEPT_FLAGGED",
        );
        // deliberately do NOT write a matching proposal .md file

        let report = run(&f.paths).expect("run");
        assert_eq!(report.escalated, vec!["p-nofile".to_string()]);

        let evidence = std::fs::read_to_string(f.paths.escalations.join("p-nofile.md")).unwrap();
        assert!(
            evidence.contains("missing/unreadable"),
            "must surface the missing-file reason loudly: {evidence}"
        );
    }

    // -- revert ---------------------------------------------------------------

    #[test]
    fn applier_revert_flips_status_and_appends_one_outcome_row() {
        let f = fixture();
        let mut reg = RuleRegistry::default();
        let mut entry = landed("revert-me", RuleStatus::Active);
        entry.proposal_id = "p-revert-src".to_string();
        reg.append(entry);
        crate::rule_registry::save(&f.paths.registry, &reg).unwrap();

        revert(&f.paths, "revert-me", "manual revert: caused wild fires").expect("revert");

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        assert_eq!(reg.entries.len(), 1, "entry preserved, never deleted");
        assert_eq!(reg.entries[0].status, RuleStatus::Reverted);
        assert_eq!(
            reg.entries[0].revert_reason.as_deref(),
            Some("manual revert: caused wild fires")
        );

        let conn = rusqlite::Connection::open(&f.paths.ledger).unwrap();
        let mut stmt = conn
            .prepare("SELECT agent, action_class, kind, payload FROM ledger")
            .unwrap();
        let rows: Vec<(String, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1, "exactly one ledger row from a revert");
        let (agent, action_class, kind, payload) = &rows[0];
        assert_eq!(agent, "proposer");
        assert_eq!(action_class, "proposal.land");
        assert_eq!(kind, "outcome");
        let v: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(v["success"], serde_json::json!(false));
        assert_eq!(v["proposal_id"], serde_json::json!("p-revert-src"));
    }

    #[test]
    fn applier_revert_errors_loudly_when_no_active_entry() {
        let f = fixture();
        let err = revert(&f.paths, "nonexistent-rule", "why").unwrap_err();
        assert!(
            format!("{err}").contains("no active entry"),
            "revert of a non-landed rule_id must error loudly: {err}"
        );
    }

    // -- status -----------------------------------------------------------------

    #[test]
    fn applier_status_reports_registry_pending_and_escalations_read_only() {
        let f = fixture();
        write_verdict(
            &f.paths,
            "p-status-land",
            "status-footgun",
            "add-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(
            &f.paths,
            "p-status-land",
            "add-rule",
            "status-footgun",
            "abc.*",
        );
        write_verdict(
            &f.paths,
            "p-status-esc",
            "status-footgun-2",
            "kill-rule",
            "ACCEPT_FLAGGED",
        );
        write_proposal_md(
            &f.paths,
            "p-status-esc",
            "kill-rule",
            "status-footgun-2",
            "",
        );

        let first = run(&f.paths).expect("run");
        assert_eq!(first.landed, vec!["p-status-land".to_string()]);
        assert_eq!(first.escalated, vec!["p-status-esc".to_string()]);

        // A third survivor that `run` has never seen — this is what `status`
        // must report as pending.
        write_verdict(
            &f.paths,
            "p-status-pending",
            "status-footgun-3",
            "add-rule",
            "ACCEPT_FLAGGED",
        );

        let registry_bytes_before = std::fs::read(&f.paths.registry).unwrap();
        let st = status(&f.paths).expect("status");
        let registry_bytes_after = std::fs::read(&f.paths.registry).unwrap();
        assert_eq!(
            registry_bytes_before, registry_bytes_after,
            "status() must never mutate the registry"
        );

        assert_eq!(st.registry_entries.len(), 1);
        assert_eq!(st.registry_entries[0].proposal_id, "p-status-land");
        assert_eq!(st.pending, vec!["p-status-pending".to_string()]);
        assert_eq!(st.escalations, vec!["p-status-esc".to_string()]);
    }

    // =======================================================================
    // `hex apply watch` — the outcome watchdog (deliverable 6)
    // =======================================================================

    /// Land an ACTIVE rule directly into the registry (bypassing a full
    /// `run()` — watch() only cares that the rule is active, not how it got
    /// there).
    fn seed_active_rule(paths: &ApplyPaths, rule_id: &str) {
        let mut reg = crate::rule_registry::load(&paths.registry).unwrap();
        reg.append(RuleEntry {
            rule_id: rule_id.to_string(),
            pattern: "x.*y".to_string(),
            proposal_id: format!("p-{rule_id}"),
            verdict_sha256: "a".repeat(64),
            landed_ts: "2026-06-12T00:00:00Z".to_string(),
            status: RuleStatus::Active,
            reverted_ts: None,
            revert_reason: None,
        });
        crate::rule_registry::save(&paths.registry, &reg).unwrap();
    }

    /// Seed `joined` gates for `rule_id`: each gate gets a `lint-gates`
    /// intent (`predicted="fail"`, `rules_fired=[rule_id]`) AND a matching
    /// `reconciler` outcome — the first `tp` gates fail in the wild
    /// (linter correct), the remaining `fp` succeed (linter false-positive).
    /// `tp + fp` must equal `joined`.
    fn seed_wild_evidence(paths: &ApplyPaths, rule_id: &str, joined: usize, tp: usize, fp: usize) {
        assert_eq!(
            tp + fp,
            joined,
            "seed_wild_evidence: tp + fp must equal joined"
        );
        let ledger = crate::ledger::Ledger::open(&paths.ledger).unwrap();
        for i in 0..joined {
            let gate_hash = format!("g-{rule_id}-{i}");
            let outcome_success = i >= tp; // first `tp` gates fail in the wild (tp), rest succeed (fp)
            ledger
                .append(
                    "lint-gates",
                    "verify-gate",
                    "intent",
                    &serde_json::json!({
                        "gate_hash": gate_hash, "predicted": "fail", "rules_fired": [rule_id],
                        "shadow": true, "command": format!("cmd-{gate_hash}"),
                    }),
                )
                .unwrap();
            ledger
                .append(
                    "reconciler",
                    "verify-gate",
                    "outcome",
                    &serde_json::json!({
                        "gate_hash": gate_hash, "command": format!("cmd-{gate_hash}"),
                        "success": outcome_success,
                        "final_exit_code": if outcome_success {0} else {1},
                        "spec_id": "S00000000", "task_id": "T0", "attempts": 1,
                        "first_started_at": "2026-06-10T01:00:00+00:00",
                    }),
                )
                .unwrap();
        }
    }

    /// Seed extra `lint-gates` intents for `rule_id` with NO matching
    /// reconciler outcome — raises `fires` without raising `joined`.
    fn seed_wild_fires_only(paths: &ApplyPaths, rule_id: &str, count: usize) {
        let ledger = crate::ledger::Ledger::open(&paths.ledger).unwrap();
        for i in 0..count {
            let gate_hash = format!("g-{rule_id}-unjoined-{i}");
            ledger
                .append(
                    "lint-gates",
                    "verify-gate",
                    "intent",
                    &serde_json::json!({
                        "gate_hash": gate_hash, "predicted": "fail", "rules_fired": [rule_id],
                        "shadow": true, "command": format!("cmd-{gate_hash}"),
                    }),
                )
                .unwrap();
        }
    }

    fn count_outcome_rows_for_rule(paths: &ApplyPaths, rule_id: &str) -> usize {
        let conn = rusqlite::Connection::open(&paths.ledger).unwrap();
        let mut stmt = conn
            .prepare("SELECT payload FROM ledger WHERE kind='outcome'")
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.iter()
            .filter(|payload| {
                let v: serde_json::Value = serde_json::from_str(payload).unwrap();
                v.get("rule_id").and_then(|x| x.as_str()) == Some(rule_id)
            })
            .count()
    }

    // -- auto-revert: fires >= 5 AND precision < 0.5 -------------------------

    #[test]
    fn applier_watch_auto_reverts_high_fires_low_precision() {
        let f = fixture();
        seed_active_rule(&f.paths, "watch-bad");
        // 5 joined gates, tp=1 fp=4 => precision 0.2 < 0.5, fires=5 >= 5.
        seed_wild_evidence(&f.paths, "watch-bad", 5, 1, 4);

        let report = watch(&f.paths).expect("watch");
        assert_eq!(report.reverted, vec!["watch-bad".to_string()]);
        assert!(report.scored_success.is_empty());
        assert!(report.insufficient_evidence.is_empty());

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        let entry = reg
            .entries
            .iter()
            .find(|e| e.rule_id == "watch-bad")
            .unwrap();
        assert_eq!(
            entry.status,
            RuleStatus::Reverted,
            "entry preserved, status flipped"
        );

        // Note: seed_wild_evidence itself writes `reconciler`/outcome rows
        // (the wild-join evidence) — filter to the applier's own dial-facing
        // outcome rows (agent='proposer') to isolate watch()'s output.
        let conn = rusqlite::Connection::open(&f.paths.ledger).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT agent, action_class, kind, payload FROM ledger \
                 WHERE kind='outcome' AND agent='proposer'",
            )
            .unwrap();
        let rows: Vec<(String, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "exactly one dial-facing outcome row from the auto-revert"
        );
        let (agent, action_class, kind, payload) = &rows[0];
        assert_eq!(agent, "proposer");
        assert_eq!(action_class, "proposal.land");
        assert_eq!(kind, "outcome");
        let v: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(v["success"], serde_json::json!(false));
        assert_eq!(v["rule_id"], serde_json::json!("watch-bad"));
    }

    #[test]
    fn applier_watch_below_min_fires_never_reverts_even_at_zero_precision() {
        let f = fixture();
        seed_active_rule(&f.paths, "watch-too-new");
        // 4 joined gates, all fp (precision 0.0) — but fires=4 < WATCH_REVERT_MIN_FIRES(5).
        seed_wild_evidence(&f.paths, "watch-too-new", 4, 0, 4);

        let report = watch(&f.paths).expect("watch");
        assert!(report.reverted.is_empty());
        assert_eq!(
            report.insufficient_evidence,
            vec!["watch-too-new".to_string()]
        );

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        let entry = reg
            .entries
            .iter()
            .find(|e| e.rule_id == "watch-too-new")
            .unwrap();
        assert_eq!(
            entry.status,
            RuleStatus::Active,
            "must not revert below the fires floor"
        );
    }

    // -- one-time success scoring: joined >= 30 AND precision >= 0.8 ---------

    #[test]
    fn applier_watch_scores_success_once_and_refuses_to_double_score() {
        let f = fixture();
        seed_active_rule(&f.paths, "watch-good");
        // 30 joined gates, tp=27 fp=3 => precision 0.9 >= 0.8, joined=30 >= 30.
        seed_wild_evidence(&f.paths, "watch-good", 30, 27, 3);

        let first = watch(&f.paths).expect("first watch");
        assert_eq!(first.scored_success, vec!["watch-good".to_string()]);
        assert!(first.already_scored.is_empty());
        assert_eq!(count_outcome_rows_for_rule(&f.paths, "watch-good"), 1);

        let reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        assert_eq!(
            reg.entries
                .iter()
                .find(|e| e.rule_id == "watch-good")
                .unwrap()
                .status,
            RuleStatus::Active,
            "scoring success never reverts"
        );

        // Repeated watch run against UNCHANGED wild evidence must not
        // double-score — this is the real cron scenario (05:10 UTC daily).
        let second = watch(&f.paths).expect("second watch");
        assert!(
            second.scored_success.is_empty(),
            "must not score a second time"
        );
        assert_eq!(second.already_scored, vec!["watch-good".to_string()]);
        assert_eq!(
            count_outcome_rows_for_rule(&f.paths, "watch-good"),
            1,
            "no new outcome row on the second run"
        );
    }

    #[test]
    fn applier_watch_below_min_joined_never_scores_even_at_high_precision() {
        let f = fixture();
        seed_active_rule(&f.paths, "watch-promising");
        // 10 joined gates, all tp (precision 1.0) — but joined=10 < WATCH_SUCCESS_MIN_JOINED(30).
        seed_wild_evidence(&f.paths, "watch-promising", 10, 10, 0);

        let report = watch(&f.paths).expect("watch");
        assert!(report.scored_success.is_empty());
        assert_eq!(
            report.insufficient_evidence,
            vec!["watch-promising".to_string()]
        );
        assert_eq!(count_outcome_rows_for_rule(&f.paths, "watch-promising"), 0);
    }

    // -- insufficient evidence is not an outcome (no row at all) -------------

    #[test]
    fn applier_watch_no_row_when_precision_is_none() {
        let f = fixture();
        seed_active_rule(&f.paths, "watch-unjoined");
        // Plenty of raw fires, but NONE joined to a reconciler outcome —
        // precision is None, not comparable to either threshold.
        seed_wild_fires_only(&f.paths, "watch-unjoined", 6);

        let report = watch(&f.paths).expect("watch");
        assert!(report.reverted.is_empty());
        assert!(report.scored_success.is_empty());
        assert_eq!(
            report.insufficient_evidence,
            vec!["watch-unjoined".to_string()]
        );
        assert_eq!(count_outcome_rows_for_rule(&f.paths, "watch-unjoined"), 0);
    }

    #[test]
    fn applier_watch_skips_already_reverted_rules() {
        let f = fixture();
        let mut reg = crate::rule_registry::load(&f.paths.registry).unwrap();
        reg.append(landed("watch-dead", RuleStatus::Reverted));
        crate::rule_registry::save(&f.paths.registry, &reg).unwrap();
        // Evidence that WOULD trigger auto-revert if the rule were active.
        seed_wild_evidence(&f.paths, "watch-dead", 5, 0, 5);

        let report = watch(&f.paths).expect("watch");
        assert!(report.reverted.is_empty());
        assert!(report.insufficient_evidence.is_empty());
        assert!(report.scored_success.is_empty());
        assert_eq!(count_outcome_rows_for_rule(&f.paths, "watch-dead"), 0);
    }

    // -- round-trip through the existing dial loader -------------------------

    #[test]
    fn applier_watch_outcome_rows_round_trip_through_dial_loader() {
        let f = fixture();
        seed_active_rule(&f.paths, "watch-dial");
        seed_wild_evidence(&f.paths, "watch-dial", 30, 27, 3);

        let report = watch(&f.paths).expect("watch");
        assert_eq!(report.scored_success, vec!["watch-dial".to_string()]);

        let rows = load_outcome_rows(&f.paths.ledger).expect("load_outcome_rows");
        let matched: Vec<_> = rows
            .iter()
            .filter(|r| r.agent == DIAL_AGENT && r.action_class == DIAL_ACTION_CLASS)
            .collect();
        assert_eq!(
            matched.len(),
            1,
            "the watchdog row must round-trip through load_outcome_rows"
        );
        assert!(matched[0].success);

        // hex::dial::compute must actually consume it (not reject/ignore it).
        match crate::dial::compute(&rows, DIAL_AGENT, DIAL_ACTION_CLASS, 1, false) {
            crate::dial::DialOutcome::Score(s) => {
                assert!(s > 0.0, "success row must raise the score")
            }
            other => panic!("expected Score, got {other:?}"),
        }
    }
}
