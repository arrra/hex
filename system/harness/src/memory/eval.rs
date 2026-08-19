//! `hex memory eval` — recall golden-set eval + regression gate.
//!
//! Runs a checked-in set of (query, expected-content) cases through the real
//! `recall::recall` path against the instance's live memory DB and scores
//! whether the expected fact reaches the injected context. The gate is
//! no-regression-from-baseline: a case that passed in the committed baseline
//! and fails now exits non-zero, loudly. Absolute score floors are deliberate
//! non-goals — the suite grows from real misses, so the honest gate is
//! "nothing that worked stopped working" (industry pattern; see
//! projects/hex-ops/research 2026-08-18 recall investigation).
//!
//! Cases live at `$HEX_DIR/.hex/eval/recall-cases.toml` (instance data — real
//! queries contain personal context and are NOT shipped with foundation).
//! Baseline: `$HEX_DIR/.hex/eval/recall-baseline.json`, updated only via
//! `--update-baseline` (review the diff like a snapshot test).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::recall_config::RecallConfig;

#[derive(Debug, Deserialize)]
pub struct CaseFile {
    #[serde(default)]
    pub cases: Vec<EvalCase>,
}

#[derive(Debug, Deserialize)]
pub struct EvalCase {
    pub id: String,
    pub query: String,
    /// Substring that must appear in an injected fact line (case-insensitive).
    pub expect: String,
    /// Optional additional substring the SAME fact line must contain (use for
    /// subject identity so a coincidental text match can't pass the case).
    #[serde(default)]
    pub expect_also: Option<String>,
    /// Free-form slice tag for reporting (e.g. "fact-relevance", "entity",
    /// "temporal", "control"). Regressions localize per-slice.
    #[serde(default)]
    pub slice: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
pub struct CaseResult {
    /// Expected content reached the injected `### Facts` section.
    pub facts: bool,
    /// Expected content appears anywhere in the injected context (facts or
    /// chunk snippets). Secondary, non-gating.
    pub anywhere: bool,
}

pub fn cases_path(hex_root: &Path) -> PathBuf {
    hex_root.join(".hex/eval/recall-cases.toml")
}

pub fn baseline_path(hex_root: &Path) -> PathBuf {
    hex_root.join(".hex/eval/recall-baseline.json")
}

/// Score one recall context block against a case. Fact lines are rendered as
/// `- **subject** predicate object`; a facts hit requires `expect` (and
/// `expect_also` when set) on a single fact line.
fn score(context: &str, case: &EvalCase) -> CaseResult {
    let lc = context.to_lowercase();
    let expect = case.expect.to_lowercase();
    let also = case.expect_also.as_ref().map(|s| s.to_lowercase());

    let facts_section = lc
        .split("### facts")
        .nth(1)
        .map(|rest| rest.split("### ").next().unwrap_or(rest))
        .unwrap_or("");
    let facts = facts_section
        .lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .any(|l| l.contains(&expect) && also.as_ref().is_none_or(|a| l.contains(a)));

    let anywhere = lc.contains(&expect) && also.as_ref().is_none_or(|a| lc.contains(a));
    CaseResult { facts, anywhere }
}

pub struct EvalReport {
    pub results: BTreeMap<String, CaseResult>,
    pub regressions: Vec<String>,
    pub new_passes: Vec<String>,
}

/// Compare current results to a baseline. Regression = baseline facts-hit now
/// missing. New passes are informational (candidates for --update-baseline).
pub fn compare(
    results: &BTreeMap<String, CaseResult>,
    baseline: &BTreeMap<String, CaseResult>,
) -> (Vec<String>, Vec<String>) {
    let mut regressions = Vec::new();
    let mut new_passes = Vec::new();
    for (id, r) in results {
        match baseline.get(id) {
            Some(b) => {
                if b.facts && !r.facts {
                    regressions.push(id.clone());
                } else if !b.facts && r.facts {
                    new_passes.push(id.clone());
                }
            }
            None => {
                if r.facts {
                    new_passes.push(id.clone());
                }
            }
        }
    }
    (regressions, new_passes)
}

/// Machine-readable summary of one eval run, for trend recording. Every count
/// is exactly what the JSON output already reports — this struct just hands
/// them back in-process instead of over stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalSummary {
    pub cases_total: usize,
    pub facts_hits: usize,
    pub anywhere_hits: usize,
    /// Number of baseline facts-hits that now miss (0 when no baseline).
    pub regressions: usize,
    pub baseline_present: bool,
}

/// Outcome of a headless eval run. Separates the benign "instance shipped no
/// cases file" skip from a genuine error so the caller can SKIP loudly vs FAIL
/// loudly (SO S6) without string-matching.
#[derive(Debug)]
pub enum EvalRunError {
    /// Cases file does not exist — the instance has not opted into the eval.
    CasesAbsent(PathBuf),
    /// Any other failure (unreadable/parse-broken cases, empty suite,
    /// unreadable baseline). Always loud.
    Other(String),
}

impl std::fmt::Display for EvalRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalRunError::CasesAbsent(p) => write!(f, "cases file absent at {}", p.display()),
            EvalRunError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for EvalRunError {}

/// Internal API: run the recall eval and return machine-readable counts WITHOUT
/// printing, gating, or touching the baseline file. Reuses the exact `score`
/// and `compare` path the CLI `run` uses, so trend numbers match the gate. Used
/// by the `hex-eval-trend` cron worker.
pub fn summarize(
    hex_root: &Path,
    cases_override: Option<&Path>,
) -> std::result::Result<EvalSummary, EvalRunError> {
    let cpath = cases_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cases_path(hex_root));
    let raw = match std::fs::read_to_string(&cpath) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(EvalRunError::CasesAbsent(cpath));
        }
        Err(e) => {
            return Err(EvalRunError::Other(format!(
                "cannot read cases file {}: {e}",
                cpath.display()
            )));
        }
    };
    let file: CaseFile = toml::from_str(&raw)
        .map_err(|e| EvalRunError::Other(format!("cases file parse error: {e}")))?;
    if file.cases.is_empty() {
        return Err(EvalRunError::Other(format!(
            "no cases in {}",
            cpath.display()
        )));
    }

    let mut results: BTreeMap<String, CaseResult> = BTreeMap::new();
    for case in &file.cases {
        let outcome = super::recall::recall(hex_root, &case.query, false);
        results.insert(case.id.clone(), score(&outcome.context, case));
    }
    let cases_total = file.cases.len();
    let facts_hits = results.values().filter(|r| r.facts).count();
    let anywhere_hits = results.values().filter(|r| r.anywhere).count();

    // A baseline that EXISTS but fails to parse is a hard error (S6) — a corrupt
    // baseline silently reporting zero regressions is exactly the quiet failure
    // the trend must never record.
    let bpath = baseline_path(hex_root);
    let baseline: Option<BTreeMap<String, CaseResult>> = match std::fs::read_to_string(&bpath) {
        Ok(s) => Some(serde_json::from_str(&s).map_err(|e| {
            EvalRunError::Other(format!("baseline {} is unreadable: {e}", bpath.display()))
        })?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(EvalRunError::Other(format!(
                "cannot read baseline {}: {e}",
                bpath.display()
            )));
        }
    };
    let regressions = match &baseline {
        Some(b) => compare(&results, b).0.len(),
        None => 0,
    };

    Ok(EvalSummary {
        cases_total,
        facts_hits,
        anywhere_hits,
        regressions,
        baseline_present: baseline.is_some(),
    })
}

/// Run the eval. Returns the process exit code (0 = pass, 1 = regression or
/// setup error). Loud on every failure path per SO S6.
///
/// `cfg_override` lets a sweep score a parameter variant WITHOUT touching the
/// live `recall.toml` (spec Tx4px1hxf): `Some(cfg)` scores every case through
/// that config; `None` runs the live recall path (which loads the instance
/// config, or compiled defaults when absent).
pub fn run(
    hex_root: &Path,
    cases_override: Option<&Path>,
    update_baseline: bool,
    json_out: bool,
    cfg_override: Option<&RecallConfig>,
) -> i32 {
    let cpath = cases_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cases_path(hex_root));
    let raw = match std::fs::read_to_string(&cpath) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[memory eval] cannot read cases file {}: {e}",
                cpath.display()
            );
            return 1;
        }
    };
    let file: CaseFile = match toml::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[memory eval] cases file parse error: {e}");
            return 1;
        }
    };
    if file.cases.is_empty() {
        eprintln!("[memory eval] no cases in {}", cpath.display());
        return 1;
    }

    let mut results: BTreeMap<String, CaseResult> = BTreeMap::new();
    let mut slice_tally: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for case in &file.cases {
        let outcome = match cfg_override {
            Some(cfg) => super::recall::recall_with_config(hex_root, &case.query, false, cfg),
            None => super::recall::recall(hex_root, &case.query, false),
        };
        let r = score(&outcome.context, case);
        let slice = case.slice.clone().unwrap_or_else(|| "unsliced".into());
        let t = slice_tally.entry(slice).or_insert((0, 0));
        t.1 += 1;
        if r.facts {
            t.0 += 1;
        }
        if !json_out {
            println!(
                "{}: facts={} anywhere={}",
                case.id,
                if r.facts { "HIT" } else { "miss" },
                if r.anywhere { "HIT" } else { "miss" }
            );
        }
        results.insert(case.id.clone(), r);
    }

    let n = file.cases.len();
    let facts_hits = results.values().filter(|r| r.facts).count();
    let any_hits = results.values().filter(|r| r.anywhere).count();

    // Missing baseline = benign first run. A baseline that EXISTS but fails
    // to parse must be loud and fatal — a corrupt file silently disabling
    // the regression gate is exactly the quiet failure S6 forbids.
    let bpath = baseline_path(hex_root);
    let baseline: Option<BTreeMap<String, CaseResult>> = match std::fs::read_to_string(&bpath) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(b) => Some(b),
            Err(e) if update_baseline => {
                eprintln!(
                    "[memory eval] baseline {} was unreadable ({e}) — replacing it",
                    bpath.display()
                );
                None
            }
            Err(e) => {
                eprintln!(
                    "[memory eval] baseline {} exists but is unreadable ({e}) — \
                         regression gate cannot run; restore it or re-run --update-baseline",
                    bpath.display()
                );
                return 1;
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!(
                "[memory eval] cannot read baseline {}: {e}",
                bpath.display()
            );
            return 1;
        }
    };

    let (regressions, new_passes) = match &baseline {
        Some(b) => compare(&results, b),
        None => (Vec::new(), Vec::new()),
    };

    if json_out {
        println!(
            "{}",
            serde_json::json!({
                "cases": n,
                "facts_hits": facts_hits,
                "anywhere_hits": any_hits,
                "per_case": results,
                "per_slice": slice_tally.iter().map(|(s,(h,t))| (s.clone(), serde_json::json!({"hits": h, "total": t}))).collect::<BTreeMap<_,_>>(),
                "regressions": regressions,
                "new_passes": new_passes,
                "baseline_present": baseline.is_some(),
            })
        );
    } else {
        println!("\nSUMMARY: facts {facts_hits}/{n}, anywhere {any_hits}/{n}");
        for (slice, (h, t)) in &slice_tally {
            println!("  slice {slice}: {h}/{t}");
        }
        if !new_passes.is_empty() {
            println!("new passes vs baseline: {}", new_passes.join(", "));
        }
    }

    if update_baseline {
        if let Some(parent) = bpath.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Atomic replace: an interrupted in-place write would leave a
        // truncated baseline that disables the gate on every later run.
        let tmp = bpath.with_extension("json.tmp");
        let write = std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&results).unwrap_or_default(),
        )
        .and_then(|()| std::fs::rename(&tmp, &bpath));
        match write {
            Ok(()) => println!("baseline updated: {}", bpath.display()),
            Err(e) => {
                eprintln!("[memory eval] baseline write failed: {e}");
                return 1;
            }
        }
        return 0;
    }

    if baseline.is_none() {
        eprintln!(
            "[memory eval] no baseline at {} — run with --update-baseline to set one",
            bpath.display()
        );
        return 0;
    }
    if !regressions.is_empty() {
        eprintln!(
            "[memory eval] REGRESSION: {} case(s) that passed in baseline now fail: {}",
            regressions.len(),
            regressions.join(", ")
        );
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, expect: &str, also: Option<&str>) -> EvalCase {
        EvalCase {
            id: id.into(),
            query: "q".into(),
            expect: expect.into(),
            expect_also: also.map(String::from),
            slice: None,
        }
    }

    #[test]
    fn score_requires_expect_on_a_fact_line() {
        let ctx = "## Relevant workspace memory\n\n### Facts\n\n- **Mike** knows Justin Frankel, who referred him\n\n### Chunks\n\n#### a.md — h\nlawyer stuff\n";
        let r = score(ctx, &case("c", "justin frankel", None));
        assert!(r.facts && r.anywhere);
        // Present only in chunks → anywhere but not facts.
        let r2 = score(ctx, &case("c", "lawyer stuff", None));
        assert!(!r2.facts && r2.anywhere);
        let r3 = score(ctx, &case("c", "absent", None));
        assert!(!r3.facts && !r3.anywhere);
    }

    #[test]
    fn score_expect_also_binds_to_same_fact_line() {
        let ctx = "### Facts\n\n- **Mike** works-on hex\n- **Whit** works-on garden\n";
        assert!(score(ctx, &case("c", "hex", Some("mike"))).facts);
        // Both substrings exist in the section but never on one line.
        assert!(!score(ctx, &case("c", "garden", Some("mike"))).facts);
    }

    #[test]
    fn compare_flags_regressions_and_new_passes() {
        let mut base = BTreeMap::new();
        base.insert(
            "a".to_string(),
            CaseResult {
                facts: true,
                anywhere: true,
            },
        );
        base.insert(
            "b".to_string(),
            CaseResult {
                facts: false,
                anywhere: false,
            },
        );
        let mut now = BTreeMap::new();
        now.insert(
            "a".to_string(),
            CaseResult {
                facts: false,
                anywhere: true,
            },
        );
        now.insert(
            "b".to_string(),
            CaseResult {
                facts: true,
                anywhere: true,
            },
        );
        now.insert(
            "c".to_string(),
            CaseResult {
                facts: true,
                anywhere: true,
            },
        );
        let (reg, newp) = compare(&now, &base);
        assert_eq!(reg, vec!["a".to_string()]);
        assert_eq!(newp, vec!["b".to_string(), "c".to_string()]);
    }
}
