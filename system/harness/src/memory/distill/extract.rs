use crate::memory::predicates;
use crate::memory::provider::{self, ProviderError};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct Candidate {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub importance: f32,
}

const MODEL: &str = "anthropic/claude-sonnet-4.5";
const MAX_TOK: u32 = 16384;

pub fn extract_from_span(text: &str) -> Result<Vec<Candidate>, ProviderError> {
    let template = std::fs::read_to_string(
        crate::memory::provider::hex_root().join(".hex/memory/prompts/extract.txt"),
    )
    .map_err(|e| ProviderError::Deferred(format!("extract.txt: {e}")))?;
    let prompt = template.replace("{{PREDICATES}}", &predicates::vocab_for_prompt())
        + "\n\n--- TEXT ---\n"
        + text;
    let raw = provider::generate(&prompt, MODEL, MAX_TOK)?;
    parse_response(&raw).map_err(|e| ProviderError::Upstream(format!("parse: {e}")))
}

pub fn parse_response(raw: &str) -> Result<Vec<Candidate>, String> {
    let body = strip_fence(raw);
    serde_json::from_str(body).map_err(|e| format!("json: {e} in {body}"))
}

fn strip_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")) {
        if let Some(body) = rest.strip_suffix("```") {
            return body.trim();
        }
        return rest.trim();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_response() {
        let raw = r#"[{"subject":"user","predicate":"prefers","object":"concrete framing","importance":0.8}]"#;
        let out = parse_response(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].subject, "user");
        assert_eq!(out[0].predicate, "prefers");
    }

    #[test]
    fn strips_markdown_fence_if_present() {
        let raw = "```json\n[{\"subject\":\"user\",\"predicate\":\"is\",\"object\":\"a dev\",\"importance\":0.5}]\n```";
        let out = parse_response(raw).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn empty_array_is_ok() {
        let out = parse_response("[]").unwrap();
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn invalid_json_returns_err() {
        assert!(parse_response("not json").is_err());
    }
}
