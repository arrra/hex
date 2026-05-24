use crate::memory::provider::{self, ProviderError};
use serde::Deserialize;

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

const MODEL: &str = "anthropic/claude-sonnet-4.5";
const MAX_TOK: u32 = 256;

pub fn judge(
    cand_subject: &str,
    cand_predicate: &str,
    cand_object: &str,
    context: &str,
    existing: &[(String, String, String, String)], // (id, subject, predicate, object)
) -> Result<Decision, ProviderError> {
    let template = std::fs::read_to_string(
        crate::memory::provider::hex_root().join(".hex/memory/prompts/judge.txt"),
    )
    .map_err(|e| ProviderError::Deferred(format!("judge.txt: {e}")))?;
    let existing_md = existing
        .iter()
        .map(|(id, s, p, o)| format!("- id={id}: ({s}, {p}, {o})"))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = template
        .replace("{{SUBJ}}", cand_subject)
        .replace("{{PRED}}", cand_predicate)
        .replace("{{OBJ}}", cand_object)
        .replace("{{CTX}}", context)
        .replace("{{EXISTING}}", &existing_md);
    let raw = provider::generate(&prompt, MODEL, MAX_TOK)?;
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
}
