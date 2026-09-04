use crate::memory::provider::{self, ProviderError};
use serde::Deserialize;
use std::path::Path;

/// Embedded default judge prompt, checked into the repo at
/// `system/harness/src/memory/distill/prompts/judge.txt` — inside the harness
/// source tree so the path survives every deploy layout (see extract.rs).
///
/// Tradeoff (deliberate): `memory/` is NOT registered in `upgrade.rs`
/// SourceDirs — its apply_sync would clobber a user-edited instance prompt — and
/// we do not depend on any prompt file being present at runtime. `install.sh`'s
/// bulk `cp -r system/ .hex/` still lands editable copies on fresh installs, but
/// with this embedded fallback an already-deployed box needs no file at all. The
/// missing-file `Deferred` that used to silently discard transcript slices is
/// gone: a missing or empty instance prompt is the normal case, not an error.
const JUDGE_PROMPT: &str = include_str!("prompts/judge.txt");

/// Resolve the effective prompt template. A non-empty instance file wins (user
/// override, read fresh per call); a missing or whitespace-only instance file
/// falls back to the embedded default, silently. See `extract.rs::resolve_prompt`
/// for the same contract.
pub(crate) fn resolve_prompt(instance_path: &Path, embedded: &'static str) -> String {
    match std::fs::read_to_string(instance_path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => embedded.to_string(),
    }
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "UPPERCASE")]
pub enum Action {
    Add,
    Update,
    Noop,
    Flag,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Decision {
    pub action: Action,
    pub target_id: Option<String>,
    pub reason: String,
}

pub fn judge(
    cand_subject: &str,
    cand_predicate: &str,
    cand_object: &str,
    context: &str,
    existing: &[(String, String, String, String)], // (id, subject, predicate, object)
) -> Result<Decision, ProviderError> {
    let instance = provider::hex_root().join(".hex/memory/prompts/judge.txt");
    let template = resolve_prompt(&instance, JUDGE_PROMPT);
    let existing_md = existing
        .iter()
        .map(|(id, s, p, o)| format!("- id={id}: ({s}, {p}, {o})"))
        .collect::<Vec<_>>()
        .join("\n");
    // Square brackets are deliberate (see extract.rs): the old curly-brace
    // placeholder form collided with the repo's own agent-orchestration recipe
    // templating (BOI/goose), which made these prompt templates un-editable by
    // automated workers. `[[NAME]]` is safe.
    let prompt = template
        .replace("[[SUBJ]]", cand_subject)
        .replace("[[PRED]]", cand_predicate)
        .replace("[[OBJ]]", cand_object)
        .replace("[[CTX]]", context)
        .replace("[[EXISTING]]", &existing_md);
    let raw = provider::generate_for("memory_judge", &prompt)?;
    parse_decision(&raw).map_err(|e| ProviderError::Upstream(format!("judge parse: {e}")))
}

pub fn parse_decision(raw: &str) -> Result<Decision, String> {
    let body = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(body).map_err(|e| format!("json: {e} in {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_decision() {
        let d = parse_decision(r#"{"action":"ADD","target_id":null,"reason":"new"}"#).unwrap();
        assert!(matches!(d.action, Action::Add));
    }

    #[test]
    fn parses_flag_decision_with_target() {
        let d =
            parse_decision(r#"{"action":"FLAG","target_id":"f1","reason":"contradicts"}"#).unwrap();
        assert!(matches!(d.action, Action::Flag));
        assert_eq!(d.target_id.as_deref(), Some("f1"));
    }

    #[test]
    fn embedded_prompt_contains_all_bracket_placeholders() {
        for p in ["[[SUBJ]]", "[[PRED]]", "[[OBJ]]", "[[CTX]]", "[[EXISTING]]"] {
            assert!(
                JUDGE_PROMPT.contains(p),
                "embedded judge prompt must carry the {p} placeholder"
            );
        }
    }

    #[test]
    fn substitution_fills_all_placeholders() {
        let filled = JUDGE_PROMPT
            .replace("[[SUBJ]]", "alice")
            .replace("[[PRED]]", "prefers")
            .replace("[[OBJ]]", "tabs")
            .replace("[[CTX]]", "")
            .replace("[[EXISTING]]", "- id=f1: (alice, prefers, spaces)");
        for p in ["[[SUBJ]]", "[[PRED]]", "[[OBJ]]", "[[CTX]]", "[[EXISTING]]"] {
            assert!(!filled.contains(p), "placeholder {p} must be substituted");
        }
        assert!(filled.contains("alice") && filled.contains("id=f1"));
    }

    #[test]
    fn resolve_prompt_prefers_nonempty_instance_override() {
        let td = tempfile::tempdir().unwrap();
        let f = td.path().join("judge.txt");
        std::fs::write(&f, "custom judge [[SUBJ]]").unwrap();
        assert_eq!(resolve_prompt(&f, JUDGE_PROMPT), "custom judge [[SUBJ]]");
    }

    #[test]
    fn resolve_prompt_falls_back_when_missing_or_blank() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_prompt(&td.path().join("missing.txt"), JUDGE_PROMPT),
            JUDGE_PROMPT
        );
        let blank = td.path().join("blank.txt");
        std::fs::write(&blank, "").unwrap();
        assert_eq!(resolve_prompt(&blank, JUDGE_PROMPT), JUDGE_PROMPT);
    }
}
