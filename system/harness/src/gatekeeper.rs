//! `hex gatekeeper` — the deterministic judge of the agent-infra improvement
//! plane (P1a). Ports the bakeoff5 harness.py patterns to Rust per the
//! all-Rust order (2026-06-10): kill gates (~1144) → embedded self-test
//! (clearability ~1588, negative ceiling ~203) → grounded eval on the frozen
//! held-out corpus (~954) → verdict. Replay-deterministic (~1657): same
//! proposal + same corpus ⇒ byte-identical verdict JSON (BTreeMap keys, no
//! wall-clock — an optional `--now` string is recorded verbatim when given).
//!
//! NOT an agent: no LLM, no autonomy. In P1, ACCEPT always lands as
//! ACCEPT_FLAGGED — a human (Mike) merges; the dial is consulted and
//! RECORDED but never upgrades a verdict.
//!
//! Proposal file contract (the personal-instance conventions doc mirrors this):
//! a markdown file containing two fenced TOML blocks:
//!
//! ```toml proposal
//! id = "p-<slug>"
//! agent = "proposer"
//! created = "<iso8601>"
//! type = "add-rule" | "modify-rule" | "kill-rule"
//! rule_id = "<linter rule id>"
//! pattern = "<regex>"            # empty/omitted for kill-rule
//! rationale = "<one paragraph>"
//! ```
//! ```toml selftest
//! fire  = ["<command the rule MUST flag>", ...]   # ≥1
//! clean = ["<command the rule MUST NOT flag>", ...] # ≥1
//! ```

use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Honest wild-precision floor: the lower CI bound from
/// `projects/agent-infra/baseline-honest.md`. A rule whose held-out precision
/// falls below this can never ACCEPT. CLI-overridable via `--floor`.
pub const DEFAULT_PRECISION_FLOOR: f64 = 0.007;

/// Rule ids / artifacts no proposal may target — constitution-class
/// (kill gates + ledger schema are Mike-only; design.md anti-scope).
pub const CONSTITUTION_CLASS: &[&str] = &["kill-gates", "ledger-schema", "charters"];

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProposalBlock {
    pub id: String,
    pub agent: String,
    #[allow(dead_code)]
    pub created: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub rule_id: String,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub rationale: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SelftestBlock {
    #[serde(default)]
    pub fire: Vec<String>,
    #[serde(default)]
    pub clean: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub proposal: ProposalBlock,
    pub selftest: SelftestBlock,
}

/// One labeled corpus record (bakeoff5 corpus schema; extra fields ignored).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CorpusRecord {
    pub command: String,
    /// 1 = the gate failed in the wild (the thing rules predict).
    pub label: i64,
}

/// The frozen corpus file: `{train: [...], held: [...], provenance: ...}`.
/// The gatekeeper judges on `held` ONLY (template-disjoint split).
#[derive(Debug, serde::Deserialize)]
pub struct CorpusFile {
    #[serde(default)]
    #[allow(dead_code)]
    pub train: Vec<CorpusRecord>,
    #[serde(default)]
    pub held: Vec<CorpusRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum VerdictKind {
    /// Survived every gate. P1: still only ever a FLAG for Mike.
    #[serde(rename = "ACCEPT_FLAGGED")]
    AcceptFlagged,
    #[serde(rename = "REJECT")]
    Reject,
    /// Gates passed but the held-out evidence is too thin to score
    /// (zero fires on held). Never lands; never counts as a rejection.
    #[serde(rename = "INSUFFICIENT_DATA")]
    InsufficientData,
}

/// The full, deterministic judgment. Serialized with sorted keys via
/// BTreeMap so two runs are byte-identical.
#[derive(Debug, serde::Serialize)]
pub struct Verdict {
    pub verdict: VerdictKind,
    pub proposal_id: String,
    pub rule_id: String,
    pub kind: String,
    pub reasons: Vec<String>,
    /// Held-out metrics; absent (all None) for kill-rule proposals.
    pub precision: Option<f64>,
    pub tp: Option<i64>,
    pub fp: Option<i64>,
    pub fn_: Option<i64>,
    pub floor: f64,
    /// `hex dial proposer proposal.land` at judge time — recorded, NEVER
    /// upgrades the verdict in P1. "UNAVAILABLE" when the ledger is absent.
    pub dial: String,
    /// The `--now` arg verbatim, or None — never a clock read.
    pub now: Option<String>,
    /// Extra deterministic context, sorted.
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug)]
pub enum GateError {
    /// Parse/IO problems — the proposal cannot even be judged.
    Malformed(String),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::Malformed(m) => write!(f, "gatekeeper: {m}"),
        }
    }
}
impl std::error::Error for GateError {}

/// Extract ALL named fenced TOML blocks (```toml <name> … ```) from markdown,
/// in document order. Auditor verdicts are append-only and may repeat; the
/// proposal/selftest blocks use only the first match.
fn fenced_toml_blocks(md: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut buf = String::new();
    for line in md.lines() {
        let trimmed = line.trim_end();
        if !in_block {
            let fence = trimmed.trim_start();
            if let Some(rest) = fence.strip_prefix("```") {
                let rest = rest.trim();
                // accept "toml proposal" / "toml  proposal"
                let mut parts = rest.split_whitespace();
                if parts.next() == Some("toml") && parts.next() == Some(name) {
                    in_block = true;
                    buf.clear();
                }
            }
        } else if trimmed.trim_start().starts_with("```") {
            in_block = false;
            out.push(buf.clone());
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    out
}

/// First matching fenced TOML block, if any.
fn fenced_toml_block(md: &str, name: &str) -> Option<String> {
    fenced_toml_blocks(md, name).into_iter().next()
}

/// Parse a proposal markdown file into its two blocks.
pub fn parse_proposal(md: &str) -> Result<Proposal, GateError> {
    let pb = fenced_toml_block(md, "proposal")
        .ok_or_else(|| GateError::Malformed("missing ```toml proposal``` block".into()))?;
    let sb = fenced_toml_block(md, "selftest")
        .ok_or_else(|| GateError::Malformed("missing ```toml selftest``` block".into()))?;
    let proposal: ProposalBlock =
        toml::from_str(&pb).map_err(|e| GateError::Malformed(format!("proposal block: {e}")))?;
    let selftest: SelftestBlock =
        toml::from_str(&sb).map_err(|e| GateError::Malformed(format!("selftest block: {e}")))?;
    Ok(Proposal { proposal, selftest })
}

// ---------------------------------------------------------------------------
// Canary registry + auditor verdicts (P1b pipeline enforcement)
// ---------------------------------------------------------------------------

/// One entry in `gates/canaries.json`. Keyed by sha256(proposal id) so the
/// registry leaks nothing about which visible file is a canary; matching is
/// done here, never by filename.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CanaryEntry {
    pub id_hash: String,
    pub expected_verdict: String,
    pub sabotage_class: String,
    #[allow(dead_code)]
    pub seeded: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct CanaryRegistry {
    #[serde(default)]
    pub canaries: Vec<CanaryEntry>,
}

/// Load + validate the canary registry. A malformed registry is a HARD error
/// (S6): the file is the gatekeeper's source of truth — a silently-skipped
/// entry would let a canary through unwatched.
pub fn load_canaries(src: &str) -> Result<CanaryRegistry, GateError> {
    let reg: CanaryRegistry = serde_json::from_str(src)
        .map_err(|e| GateError::Malformed(format!("canary registry parse: {e}")))?;
    for (i, c) in reg.canaries.iter().enumerate() {
        if c.id_hash.len() != 64
            || !c
                .id_hash
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        {
            return Err(GateError::Malformed(format!(
                "canary[{i}]: id_hash must be 64 lowercase hex chars (got {:?})",
                c.id_hash
            )));
        }
        match c.expected_verdict.as_str() {
            "REJECT" | "defer" => {}
            other => {
                return Err(GateError::Malformed(format!(
                    "canary[{i}]: expected_verdict must be REJECT|defer (got {other:?})"
                )))
            }
        }
        if c.sabotage_class.trim().is_empty() || c.seeded.trim().is_empty() {
            return Err(GateError::Malformed(format!(
                "canary[{i}]: empty sabotage_class/seeded"
            )));
        }
    }
    Ok(reg)
}

/// sha256 of a proposal id (UTF-8, no trailing newline) — the registry key.
pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// One appended `toml auditor_verdict` block. Lenient on extra fields;
/// required fields defaulting to "" get voided by the identity checks below
/// rather than silently passing.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AuditorVerdictBlock {
    #[serde(default)]
    pub auditor: String,
    #[serde(default)]
    pub spec_id: String,
    #[serde(default)]
    pub verdict: String,
}

/// Parse every auditor_verdict block. A block that fails TOML parse is
/// returned as a voided placeholder (S6: recorded loudly, never dropped).
pub fn parse_auditor_verdicts(md: &str) -> Vec<(AuditorVerdictBlock, Option<String>)> {
    fenced_toml_blocks(md, "auditor_verdict")
        .into_iter()
        .map(|b| match toml::from_str::<AuditorVerdictBlock>(&b) {
            Ok(av) => (av, None),
            Err(e) => (
                AuditorVerdictBlock {
                    auditor: String::new(),
                    spec_id: String::new(),
                    verdict: String::new(),
                },
                Some(format!("unparseable auditor_verdict block: {e}")),
            ),
        })
        .collect()
}

/// Identity checks on one auditor verdict. String claims are not trusted:
/// `boi_specs` (when provided) is the set of real spec ids from
/// `~/.boi/v2/boi.db` — a verdict whose spec_id isn't a real run is voided,
/// as is author-as-auditor.
pub fn void_reason(
    av: &AuditorVerdictBlock,
    proposal_agent: &str,
    boi_specs: Option<&std::collections::BTreeSet<String>>,
) -> Option<String> {
    if av.auditor.trim().is_empty() {
        return Some("empty auditor identity".into());
    }
    if av.auditor.trim() == proposal_agent.trim() {
        return Some(format!(
            "auditor '{}' equals the proposal's author agent — author/auditor separation violated",
            av.auditor
        ));
    }
    if let Some(specs) = boi_specs {
        if av.spec_id.trim().is_empty() {
            return Some("empty spec_id — audit run unverifiable".into());
        }
        if !specs.contains(av.spec_id.trim()) {
            return Some(format!(
                "spec_id '{}' not found in boi.db — audit run unverifiable",
                av.spec_id
            ));
        }
    }
    None
}

/// Pipeline context applied AFTER the core judgment: identity voiding is
/// recorded in reasons; a registered canary is never eligible for accept;
/// an auditor ACCEPT on a canary voids that auditor's approvals and returns
/// alert payloads for the ledger (loud, F4 mechanic).
pub struct PipelineContext {
    pub canary: Option<CanaryEntry>,
    /// (block, pre-existing void reason from parse/identity checks)
    pub auditor_verdicts: Vec<(AuditorVerdictBlock, Option<String>)>,
}

pub fn apply_pipeline_context(v: &mut Verdict, ctx: &PipelineContext) -> Vec<serde_json::Value> {
    let mut alerts = Vec::new();
    for (av, voided) in &ctx.auditor_verdicts {
        if let Some(why) = voided {
            v.reasons.push(format!(
                "auditor verdict ('{}', spec '{}', verdict '{}') VOIDED: {why}",
                av.auditor, av.spec_id, av.verdict
            ));
        }
    }
    v.meta.insert(
        "auditor_verdicts".into(),
        ctx.auditor_verdicts.len().to_string(),
    );
    v.meta.insert(
        "auditor_verdicts_voided".into(),
        ctx.auditor_verdicts
            .iter()
            .filter(|(_, voided)| voided.is_some())
            .count()
            .to_string(),
    );
    v.meta
        .insert("canary".into(), ctx.canary.is_some().to_string());

    if let Some(c) = &ctx.canary {
        if v.verdict != VerdictKind::Reject {
            v.reasons
                .push("canary: registered canary — never eligible for accept".into());
            v.verdict = VerdictKind::Reject;
        }
        for (av, _) in &ctx.auditor_verdicts {
            if av.verdict.trim().eq_ignore_ascii_case("accept") {
                // Voided or not: an accept on a canary alerts. Defense in depth.
                v.reasons.push(format!(
                    "canary: auditor '{}' APPROVED a registered canary (sabotage_class '{}') — approvals voided",
                    av.auditor, c.sabotage_class
                ));
                alerts.push(serde_json::json!({
                    "proposal_id": v.proposal_id,
                    "auditor": av.auditor,
                    "spec_id": av.spec_id,
                    "sabotage_class": c.sabotage_class,
                }));
            } else if !av.verdict.trim().eq_ignore_ascii_case(&c.expected_verdict)
                && !av.verdict.trim().is_empty()
            {
                v.reasons.push(format!(
                    "canary: auditor '{}' emitted '{}', expected '{}' — canary not cleared",
                    av.auditor, av.verdict, c.expected_verdict
                ));
            }
        }
    }
    alerts
}

/// The append-only contract: a proposal already carrying a gatekeeper verdict
/// is never re-judged — a revision is a NEW file with a NEW id.
pub fn has_gatekeeper_verdict(md: &str) -> bool {
    md.contains("```json gatekeeper-verdict")
}

/// Kill gates — first, absolute, every trip is a REJECT reason. Ported from
/// the bakeoff5 V1-wedge stance: outward/forbidden actions never pass, no
/// matter what the evidence says.
pub fn kill_gate_reasons(p: &Proposal) -> Vec<String> {
    let mut r = Vec::new();
    match p.proposal.kind.as_str() {
        "add-rule" | "modify-rule" | "kill-rule" => {}
        other => r.push(format!("kill-gate: unknown proposal type '{other}'")),
    }
    if p.proposal.id.trim().is_empty() {
        r.push("kill-gate: empty proposal id".into());
    }
    if p.proposal.agent.trim().is_empty() {
        r.push("kill-gate: empty author agent".into());
    }
    if CONSTITUTION_CLASS
        .iter()
        .any(|c| p.proposal.rule_id.trim() == *c)
    {
        r.push(format!(
            "kill-gate: rule_id '{}' is constitution-class (Mike-only)",
            p.proposal.rule_id
        ));
    }
    if p.selftest.fire.is_empty() {
        r.push("kill-gate: selftest has no fire cases (≥1 required)".into());
    }
    if p.selftest.clean.is_empty() {
        r.push("kill-gate: selftest has no clean cases (≥1 required — negative ceiling)".into());
    }
    let needs_pattern = matches!(p.proposal.kind.as_str(), "add-rule" | "modify-rule");
    if needs_pattern {
        if p.proposal.pattern.trim().is_empty() {
            r.push("kill-gate: add/modify proposal with empty pattern".into());
        } else if Regex::new(&p.proposal.pattern).is_err() {
            r.push(format!(
                "kill-gate: pattern is not a valid regex: {}",
                p.proposal.pattern
            ));
        }
    }
    r
}

/// Embedded self-test (clearability + negative form): the pattern must fire
/// on every `fire` case and stay silent on every `clean` case. kill-rule
/// proposals have no pattern; their self-test is interpreted against the
/// CURRENT rule they want killed and is checked by the auditor — here we
/// only require the cases to exist (kill gates above).
pub fn selftest_reasons(p: &Proposal) -> Vec<String> {
    let mut r = Vec::new();
    if !matches!(p.proposal.kind.as_str(), "add-rule" | "modify-rule") {
        return r;
    }
    let re = match Regex::new(&p.proposal.pattern) {
        Ok(re) => re,
        Err(_) => return r, // already a kill-gate reason
    };
    for (i, c) in p.selftest.fire.iter().enumerate() {
        if !re.is_match(c) {
            r.push(format!("selftest: fire[{i}] not matched by pattern"));
        }
    }
    for (i, c) in p.selftest.clean.iter().enumerate() {
        if re.is_match(c) {
            r.push(format!("selftest: clean[{i}] wrongly matched by pattern"));
        }
    }
    r
}

/// Grounded eval on the frozen held-out corpus (bakeoff5 ~954): precision
/// over `label == 1` fires. Returns None for kill-rule (nothing to run).
pub fn grounded_eval(p: &Proposal, held: &[CorpusRecord]) -> Option<(f64, i64, i64, i64)> {
    if !matches!(p.proposal.kind.as_str(), "add-rule" | "modify-rule") {
        return None;
    }
    let re = Regex::new(&p.proposal.pattern).ok()?;
    let (mut tp, mut fp, mut fn_) = (0i64, 0i64, 0i64);
    for rec in held {
        let fires = re.is_match(&rec.command);
        match (fires, rec.label == 1) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, true) => fn_ += 1,
            (false, false) => {}
        }
    }
    let precision = tp as f64 / std::cmp::max(1, tp + fp) as f64;
    Some((precision, tp, fp, fn_))
}

/// The full deterministic judgment. `dial` is recorded verbatim; it never
/// changes the verdict (P1: day-one everything flags to Mike anyway).
pub fn judge(
    p: &Proposal,
    held: &[CorpusRecord],
    floor: f64,
    dial: String,
    now: Option<String>,
) -> Verdict {
    let mut reasons = kill_gate_reasons(p);
    let killed = !reasons.is_empty();
    if !killed {
        reasons.extend(selftest_reasons(p));
    }
    let selftest_failed = !killed && !reasons.is_empty();

    let metrics = if killed { None } else { grounded_eval(p, held) };

    let verdict = if killed || selftest_failed {
        VerdictKind::Reject
    } else {
        match (p.proposal.kind.as_str(), metrics) {
            // kill-rule: structurally sound; always a human decision.
            ("kill-rule", _) => VerdictKind::AcceptFlagged,
            (_, Some((prec, tp, _, _))) => {
                if tp == 0 {
                    reasons.push(
                        "grounded: zero true-positive fires on held-out — unscoreable".into(),
                    );
                    VerdictKind::InsufficientData
                } else if prec < floor {
                    reasons.push(format!(
                        "grounded: held-out precision {prec:.4} below floor {floor} — can never ACCEPT"
                    ));
                    VerdictKind::Reject
                } else {
                    VerdictKind::AcceptFlagged
                }
            }
            (_, None) => {
                reasons.push("grounded: no evaluable pattern".into());
                VerdictKind::InsufficientData
            }
        }
    };

    let (precision, tp, fp, fn_) = match metrics {
        Some((p_, t, f, n)) => (Some(p_), Some(t), Some(f), Some(n)),
        None => (None, None, None, None),
    };

    let mut meta = BTreeMap::new();
    meta.insert("held_records".into(), held.len().to_string());
    meta.insert("engine".into(), "hex-gatekeeper-v1".into());

    Verdict {
        verdict,
        proposal_id: p.proposal.id.clone(),
        rule_id: p.proposal.rule_id.clone(),
        kind: p.proposal.kind.clone(),
        reasons,
        precision,
        tp,
        fp,
        fn_,
        floor,
        dial,
        now,
        meta,
    }
}

/// Byte-stable serialization: serde_json on a struct whose only map is a
/// BTreeMap; field order is declaration order — identical across runs.
pub fn verdict_json(v: &Verdict) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

// ---------------------------------------------------------------------------
// CLI runners — main.rs declares the clap variants and delegates here.
// ---------------------------------------------------------------------------

/// `hex gatekeeper judge <proposal.md> --corpus <json> [--floor f] [--out p]
/// [--now iso] [--store dir] [--canaries reg.json] [--boi-db boi.db]` —
/// returns the process exit code.
/// Exit 0 on ANY verdict (REJECT is a successful judgment); exit 2 on
/// unreadable/malformed inputs (the proposal cannot be judged at all),
/// including a proposal that already carries a gatekeeper verdict
/// (append-only: re-judgement requires a new file with a new id).
#[allow(clippy::too_many_arguments)]
pub fn cli_judge(
    proposal_path: &std::path::Path,
    corpus_path: &std::path::Path,
    floor: f64,
    out: Option<&std::path::Path>,
    now: Option<String>,
    store: Option<&std::path::Path>,
    canaries: Option<&std::path::Path>,
    boi_db: Option<&std::path::Path>,
    hex_dir: &std::path::Path,
    dial: String,
) -> i32 {
    let md = match std::fs::read_to_string(proposal_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "hex gatekeeper judge: cannot read {}: {e}",
                proposal_path.display()
            );
            return 2;
        }
    };
    if has_gatekeeper_verdict(&md) {
        eprintln!(
            "hex gatekeeper judge: {} already carries a gatekeeper verdict — append-only; a revision is a NEW file with a NEW id",
            proposal_path.display()
        );
        return 2;
    }
    let prop = match parse_proposal(&md) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hex gatekeeper judge: {e}");
            return 2;
        }
    };
    let corpus_src = match std::fs::read_to_string(corpus_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "hex gatekeeper judge: cannot read corpus {}: {e}",
                corpus_path.display()
            );
            return 2;
        }
    };
    let corpus: CorpusFile = match serde_json::from_str(&corpus_src) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex gatekeeper judge: corpus parse: {e}");
            return 2;
        }
    };

    // Canary registry: explicit input, malformed = hard stop (S6 — a skipped
    // entry would let a canary through unwatched).
    let canary_match: Option<CanaryEntry> = match canaries {
        None => None,
        Some(reg_path) => {
            let src = match std::fs::read_to_string(reg_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "hex gatekeeper judge: cannot read canary registry {}: {e}",
                        reg_path.display()
                    );
                    return 2;
                }
            };
            let reg = match load_canaries(&src) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("hex gatekeeper judge: {e}");
                    return 2;
                }
            };
            let want = sha256_hex(&prop.proposal.id);
            reg.canaries.into_iter().find(|c| c.id_hash == want)
        }
    };

    // boi.db identity ground truth (read-only). Explicit input: unreadable
    // db with the flag given = hard stop, not a silent skip.
    let boi_specs: Option<std::collections::BTreeSet<String>> = match boi_db {
        None => None,
        Some(db) => {
            let conn = match rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "hex gatekeeper judge: cannot open boi db {} read-only: {e}",
                        db.display()
                    );
                    return 2;
                }
            };
            let ids: Result<std::collections::BTreeSet<String>, _> = (|| {
                let mut stmt = conn.prepare("SELECT spec_id FROM specs")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect()
            })();
            match ids {
                Ok(set) => Some(set),
                Err(e) => {
                    eprintln!("hex gatekeeper judge: boi db query failed: {e}");
                    return 2;
                }
            }
        }
    };

    let auditor_verdicts: Vec<(AuditorVerdictBlock, Option<String>)> = parse_auditor_verdicts(&md)
        .into_iter()
        .map(|(av, parse_void)| {
            let why =
                parse_void.or_else(|| void_reason(&av, &prop.proposal.agent, boi_specs.as_ref()));
            (av, why)
        })
        .collect();

    // Dial consult is computed by the main.rs glue (it owns the ledger->
    // OutcomeRow loader) and passed in — recorded, never upgrades (P1).
    let mut v = judge(&prop, &corpus.held, floor, dial, now);
    let ctx = PipelineContext {
        canary: canary_match,
        auditor_verdicts,
    };
    let alerts = apply_pipeline_context(&mut v, &ctx);
    let json = verdict_json(&v);

    if let Some(out_path) = out {
        if let Err(e) = std::fs::write(out_path, &json) {
            eprintln!(
                "hex gatekeeper judge: write {} failed: {e}",
                out_path.display()
            );
            return 1;
        }
    }

    // Append-only verdict block on the proposal file itself.
    let block = format!(
        "\n## gatekeeper verdict ({})\n\n```json gatekeeper-verdict\n{}\n```\n",
        match v.verdict {
            VerdictKind::AcceptFlagged => "ACCEPT_FLAGGED",
            VerdictKind::Reject => "REJECT",
            VerdictKind::InsufficientData => "INSUFFICIENT_DATA",
        },
        json
    );
    if let Err(e) = append_to_file(proposal_path, &block) {
        eprintln!("hex gatekeeper judge: verdict append failed: {e}");
        return 1;
    }

    // Verdict store copy via the containment sequence (chmod-up/write/chmod-down).
    if let Some(store_dir) = store {
        if let Err(e) = store_write(store_dir, &format!("{}.verdict.json", v.proposal_id), &json) {
            eprintln!("hex gatekeeper judge: store write failed: {e}");
            return 1;
        }
    }

    // One outcome ledger row (plus one ALERT row per canary approval),
    // in-process. Loud-but-nonfatal on failure.
    let ledger_path = hex::ledger::default_path(hex_dir);
    match hex::ledger::Ledger::open(&ledger_path) {
        Ok(l) => {
            let payload: serde_json::Value =
                serde_json::from_str(&json).unwrap_or(serde_json::json!({ "raw": json }));
            if let Err(e) = l.append("gatekeeper", "proposal.judge", "outcome", &payload) {
                eprintln!("hex gatekeeper judge: ledger append failed: {e}");
            }
            for alert in &alerts {
                eprintln!("hex gatekeeper judge: CANARY APPROVED — {alert} (approvals voided)");
                if let Err(e) = l.append("gatekeeper", "canary.approved", "alert", alert) {
                    eprintln!("hex gatekeeper judge: canary alert append failed: {e}");
                }
            }
        }
        Err(e) => eprintln!("hex gatekeeper judge: ledger open failed: {e} — verdict NOT recorded"),
    }

    println!("{json}");
    0
}

/// `hex gatekeeper probe --store <dir>` — bakeoff5 containment (~590-647):
/// the store is 0555; a candidate-context subprocess must FAIL to write into
/// it. Blocked probe → evidence + exit 0. A successful unauthorized write →
/// ledger alert + exit 1, loudly.
pub fn cli_probe(store_dir: &std::path::Path, hex_dir: &std::path::Path) -> i32 {
    if let Err(e) = std::fs::create_dir_all(store_dir) {
        eprintln!(
            "hex gatekeeper probe: create {} failed: {e}",
            store_dir.display()
        );
        return 2;
    }
    if let Err(e) = set_mode(store_dir, 0o555) {
        eprintln!("hex gatekeeper probe: chmod 0555 failed: {e}");
        return 2;
    }
    let probe_target = store_dir.join("probe-write-test");
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("echo breach > {}", probe_target.display()))
        .status();
    let wrote = probe_target.exists();
    if wrote {
        let _ = std::fs::remove_file(&probe_target);
        let payload = serde_json::json!({
            "store": store_dir.display().to_string(),
            "breach": true,
            "subprocess_status": format!("{status:?}"),
        });
        if let Ok(l) = hex::ledger::Ledger::open(hex::ledger::default_path(hex_dir)) {
            let _ = l.append("gatekeeper", "containment.probe", "alert", &payload);
        }
        eprintln!(
            "hex gatekeeper probe: BREACH — candidate subprocess wrote into {} (mode 0555 did not hold)",
            store_dir.display()
        );
        return 1;
    }
    println!(
        "hex gatekeeper probe: contained — write into {} blocked (mode 0555)",
        store_dir.display()
    );
    0
}

fn append_to_file(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    f.write_all(text.as_bytes())
}

fn set_mode(p: &std::path::Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(p)?.permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(p, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (p, mode);
    }
    Ok(())
}

/// chmod-up / write / chmod-down — the ONLY sanctioned write path into the
/// verdict store. The store is append-only history: an existing verdict file
/// is never overwritten with DIFFERENT content (split-brain protection for
/// re-judges of unmerged worktree copies); byte-identical rewrites are
/// idempotent no-ops (verdicts are deterministic, so a true replay matches).
fn store_write(store_dir: &std::path::Path, name: &str, content: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir).ok();
    let target = store_dir.join(name);
    if let Ok(existing) = std::fs::read_to_string(&target) {
        if existing == content {
            return Ok(()); // idempotent replay
        }
        return Err(std::io::Error::other(format!(
            "verdict store already holds {} with DIFFERENT content — refusing to overwrite (append-only store; a re-judge needs a new proposal id)",
            target.display()
        )));
    }
    set_mode(store_dir, 0o755)?;
    let res = std::fs::write(&target, content);
    let down = set_mode(store_dir, 0o555);
    res.and(down)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal_md(kind: &str, pattern: &str, fire: &[&str], clean: &[&str]) -> String {
        let fire_t = fire
            .iter()
            .map(|c| format!("{c:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let clean_t = clean
            .iter()
            .map(|c| format!("{c:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "# proposal\n\n```toml proposal\nid = \"p-test\"\nagent = \"proposer\"\ncreated = \"2026-06-10T00:00:00Z\"\ntype = \"{kind}\"\nrule_id = \"stderr-swallow-2\"\npattern = {pattern:?}\nrationale = \"r\"\n```\n\n```toml selftest\nfire = [{fire_t}]\nclean = [{clean_t}]\n```\n"
        )
    }

    fn held() -> Vec<CorpusRecord> {
        vec![
            CorpusRecord {
                command: "cargo test 2>/dev/null".into(),
                label: 1,
            },
            CorpusRecord {
                command: "cargo test 2>/dev/null && echo ok".into(),
                label: 1,
            },
            CorpusRecord {
                command: "test -f Cargo.toml".into(),
                label: 0,
            },
            CorpusRecord {
                command: "grep -q foo bar.txt".into(),
                label: 0,
            },
        ]
    }

    #[test]
    fn gk_parses_wellformed_proposal() {
        let md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["x"]);
        let p = parse_proposal(&md).unwrap();
        assert_eq!(p.proposal.id, "p-test");
        assert_eq!(p.selftest.fire.len(), 1);
    }

    #[test]
    fn gk_kill_gate_unknown_type_rejects() {
        let md = proposal_md("nuke-everything", "x", &["x"], &["y"]);
        let p = parse_proposal(&md).unwrap();
        let v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(v.verdict, VerdictKind::Reject);
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("unknown proposal type")));
    }

    #[test]
    fn gk_kill_gate_constitution_class_rejects() {
        let mut md = proposal_md("modify-rule", "x", &["x"], &["y"]);
        md = md.replace("stderr-swallow-2", "ledger-schema");
        let p = parse_proposal(&md).unwrap();
        let v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(v.verdict, VerdictKind::Reject);
        assert!(v.reasons.iter().any(|r| r.contains("constitution-class")));
    }

    #[test]
    fn gk_selftest_failure_never_accepts() {
        // Pattern misses its own fire case.
        let md = proposal_md("add-rule", "2>/dev/null", &["echo clean"], &["ok"]);
        let p = parse_proposal(&md).unwrap();
        let v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(v.verdict, VerdictKind::Reject);
        assert!(v.reasons.iter().any(|r| r.contains("selftest")));
    }

    #[test]
    fn gk_below_floor_never_accepts() {
        // A perfect-selftest rule with held-out precision 1.0 still rejects
        // when the floor is set above it — the floor rule is absolute.
        let md = proposal_md(
            "add-rule",
            "2>/dev/null",
            &["x 2>/dev/null"],
            &["plain command"],
        );
        let p = parse_proposal(&md).unwrap();
        let v = judge(&p, &held(), 1.01, "UNAVAILABLE".into(), None);
        assert_eq!(v.verdict, VerdictKind::Reject);
        assert!(v.reasons.iter().any(|r| r.contains("below floor")));
    }

    #[test]
    fn gk_zero_tp_is_insufficient_data() {
        let md = proposal_md(
            "add-rule",
            "zzz-never-matches",
            &["zzz-never-matches"],
            &["ok"],
        );
        let p = parse_proposal(&md).unwrap();
        let v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(v.verdict, VerdictKind::InsufficientData);
    }

    #[test]
    fn gk_good_rule_accept_flagged_never_plain_accept() {
        let md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        let p = parse_proposal(&md).unwrap();
        let v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(v.verdict, VerdictKind::AcceptFlagged);
        assert_eq!(v.precision, Some(1.0));
        assert_eq!(v.tp, Some(2));
    }

    #[test]
    fn gk_kill_rule_flags_for_human() {
        let md = proposal_md(
            "kill-rule",
            "",
            &["the rule misfires here"],
            &["and not here"],
        );
        let p = parse_proposal(&md).unwrap();
        let v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(v.verdict, VerdictKind::AcceptFlagged);
        assert!(v.precision.is_none(), "kill-rule has no pattern metrics");
    }

    #[test]
    fn gk_determinism_byte_identical() {
        let md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        let p = parse_proposal(&md).unwrap();
        let v1 = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        let v2 = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(verdict_json(&v1), verdict_json(&v2));
    }

    #[test]
    fn gk_dial_recorded_never_upgrades() {
        // A killed proposal stays REJECT no matter what the dial says.
        let md = proposal_md("nuke-everything", "x", &["x"], &["y"]);
        let p = parse_proposal(&md).unwrap();
        let v = judge(&p, &held(), DEFAULT_PRECISION_FLOOR, "0.99".into(), None);
        assert_eq!(v.verdict, VerdictKind::Reject);
        assert_eq!(v.dial, "0.99");
    }

    // --- P1b pipeline enforcement -----------------------------------------

    fn auditor_block(auditor: &str, spec_id: &str, verdict: &str) -> String {
        format!(
            "\n```toml auditor_verdict\nauditor = \"{auditor}\"\nspec_id = \"{spec_id}\"\naudited = \"2026-06-10T04:20:00Z\"\nverdict = \"{verdict}\"\nreasoning = \"because\"\n```\n"
        )
    }

    #[test]
    fn gk_sha256_known_vector() {
        assert_eq!(
            sha256_hex("p-test"),
            "3b6fb8f2904728bb7cf558a705c0ab774b6401ba0d98b47542e1b99997de4597"
        );
    }

    #[test]
    fn gk_parses_all_auditor_verdict_blocks() {
        let mut md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        md.push_str(&auditor_block("auditor", "S11111111", "reject"));
        md.push_str(&auditor_block("auditor-b", "S22222222", "accept"));
        let avs = parse_auditor_verdicts(&md);
        assert_eq!(avs.len(), 2);
        assert_eq!(avs[0].0.verdict, "reject");
        assert_eq!(avs[1].0.auditor, "auditor-b");
        assert!(avs.iter().all(|(_, voided)| voided.is_none()));
    }

    #[test]
    fn gk_unparseable_auditor_block_is_voided_not_dropped() {
        let mut md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        md.push_str("\n```toml auditor_verdict\nthis is not toml ===\n```\n");
        let avs = parse_auditor_verdicts(&md);
        assert_eq!(avs.len(), 1);
        assert!(avs[0].1.as_deref().unwrap().contains("unparseable"));
    }

    #[test]
    fn gk_void_author_as_auditor() {
        let av = AuditorVerdictBlock {
            auditor: "proposer".into(),
            spec_id: "S11111111".into(),
            verdict: "accept".into(),
        };
        let why = void_reason(&av, "proposer", None).unwrap();
        assert!(why.contains("author/auditor separation"));
    }

    #[test]
    fn gk_void_unknown_spec_id_against_boi_truth() {
        let mut specs = std::collections::BTreeSet::new();
        specs.insert("S11111111".to_string());
        let good = AuditorVerdictBlock {
            auditor: "auditor".into(),
            spec_id: "S11111111".into(),
            verdict: "reject".into(),
        };
        let fake = AuditorVerdictBlock {
            auditor: "auditor".into(),
            spec_id: "Sfakefake".into(),
            verdict: "accept".into(),
        };
        assert!(void_reason(&good, "proposer", Some(&specs)).is_none());
        assert!(void_reason(&fake, "proposer", Some(&specs))
            .unwrap()
            .contains("not found in boi.db"));
    }

    fn canary_for(id: &str) -> CanaryEntry {
        CanaryEntry {
            id_hash: sha256_hex(id),
            expected_verdict: "REJECT".into(),
            sabotage_class: "precision-fake".into(),
            seeded: "2026-06-10T03:30:00Z".into(),
            notes: "n".into(),
        }
    }

    #[test]
    fn gk_canary_never_eligible_for_accept() {
        // A canary crafted well enough to pass every gate still REJECTs.
        let md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        let p = parse_proposal(&md).unwrap();
        let mut v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        assert_eq!(
            v.verdict,
            VerdictKind::AcceptFlagged,
            "precondition: passes gates"
        );
        let ctx = PipelineContext {
            canary: Some(canary_for("p-test")),
            auditor_verdicts: vec![],
        };
        let alerts = apply_pipeline_context(&mut v, &ctx);
        assert_eq!(v.verdict, VerdictKind::Reject);
        assert!(alerts.is_empty(), "no auditor approval, no alert");
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("never eligible for accept")));
    }

    #[test]
    fn gk_canary_approval_voids_and_alerts() {
        let md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        let p = parse_proposal(&md).unwrap();
        let mut v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        let ctx = PipelineContext {
            canary: Some(canary_for("p-test")),
            auditor_verdicts: vec![(
                AuditorVerdictBlock {
                    auditor: "auditor".into(),
                    spec_id: "S11111111".into(),
                    verdict: "accept".into(),
                },
                None,
            )],
        };
        let alerts = apply_pipeline_context(&mut v, &ctx);
        assert_eq!(v.verdict, VerdictKind::Reject);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["auditor"], "auditor");
        assert!(v
            .reasons
            .iter()
            .any(|r| r.contains("APPROVED a registered canary")));
    }

    #[test]
    fn gk_canary_cleared_by_expected_verdict_stays_quiet() {
        let md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        let p = parse_proposal(&md).unwrap();
        let mut v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        let ctx = PipelineContext {
            canary: Some(canary_for("p-test")),
            auditor_verdicts: vec![(
                AuditorVerdictBlock {
                    auditor: "auditor".into(),
                    spec_id: "S11111111".into(),
                    verdict: "reject".into(),
                },
                None,
            )],
        };
        let alerts = apply_pipeline_context(&mut v, &ctx);
        assert!(alerts.is_empty());
        assert!(!v.reasons.iter().any(|r| r.contains("not cleared")));
    }

    #[test]
    fn gk_organic_proposal_untouched_by_pipeline_context() {
        let md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        let p = parse_proposal(&md).unwrap();
        let mut v = judge(
            &p,
            &held(),
            DEFAULT_PRECISION_FLOOR,
            "UNAVAILABLE".into(),
            None,
        );
        let ctx = PipelineContext {
            canary: None,
            auditor_verdicts: vec![],
        };
        let alerts = apply_pipeline_context(&mut v, &ctx);
        assert_eq!(v.verdict, VerdictKind::AcceptFlagged);
        assert!(alerts.is_empty());
        assert_eq!(v.meta.get("canary").map(String::as_str), Some("false"));
    }

    #[test]
    fn gk_already_judged_detected() {
        let mut md = proposal_md("add-rule", "2>/dev/null", &["x 2>/dev/null"], &["plain"]);
        assert!(!has_gatekeeper_verdict(&md));
        md.push_str("\n## gatekeeper verdict (REJECT)\n\n```json gatekeeper-verdict\n{}\n```\n");
        assert!(has_gatekeeper_verdict(&md));
    }

    #[test]
    fn gk_store_write_append_only_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join("verdicts");
        store_write(&store, "p-x.verdict.json", "{\"a\":1}").unwrap();
        // Identical replay: idempotent no-op.
        store_write(&store, "p-x.verdict.json", "{\"a\":1}").unwrap();
        // Divergent overwrite: refused loudly.
        let err = store_write(&store, "p-x.verdict.json", "{\"a\":2}").unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
        // Original content intact; store back at 0555.
        let kept = std::fs::read_to_string(store.join("p-x.verdict.json")).unwrap();
        assert_eq!(kept, "{\"a\":1}");
        // Restore writability so tempdir cleanup succeeds.
        set_mode(&store, 0o755).unwrap();
    }

    #[test]
    fn gk_registry_validation_is_strict() {
        // Valid empty registry.
        assert!(load_canaries(r#"{"canaries": []}"#).is_ok());
        // Bad hash length.
        let bad_hash = r#"{"canaries": [{"id_hash": "abc", "expected_verdict": "REJECT", "sabotage_class": "x", "seeded": "t", "notes": ""}]}"#;
        assert!(load_canaries(bad_hash).is_err());
        // Bad expected verdict (auditor vocabulary is reject/defer; accept is never expected).
        let bad_verdict = format!(
            r#"{{"canaries": [{{"id_hash": "{}", "expected_verdict": "accept", "sabotage_class": "x", "seeded": "t", "notes": ""}}]}}"#,
            sha256_hex("p-x")
        );
        assert!(load_canaries(&bad_verdict).is_err());
        // Well-formed entry.
        let ok = format!(
            r#"{{"canaries": [{{"id_hash": "{}", "expected_verdict": "REJECT", "sabotage_class": "selftest-leaks", "seeded": "2026-06-10T03:30:00Z", "notes": "n"}}]}}"#,
            sha256_hex("p-x")
        );
        assert_eq!(load_canaries(&ok).unwrap().canaries.len(), 1);
    }
}
