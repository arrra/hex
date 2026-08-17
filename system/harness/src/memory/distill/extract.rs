use crate::memory::predicates;
use crate::memory::provider::{self, ProviderError};
use serde::Deserialize;
use std::path::Path;

/// Embedded default extraction prompt, checked into the repo at
/// `system/memory/prompts/extract.txt` and compiled in via `include_str!`.
///
/// Tradeoff (deliberate): `memory/` is NOT registered in `upgrade.rs`
/// SourceDirs — its apply_sync would clobber a user-edited instance prompt — and
/// we do not depend on any prompt file being present at runtime. `install.sh`'s
/// bulk `cp -r system/ .hex/` still lands editable copies on fresh installs, but
/// with this embedded fallback an already-deployed box needs no file at all. The
/// missing-file `Deferred` that used to silently discard transcript slices is
/// gone: a missing or empty instance prompt is the normal case, not an error.
const EXTRACT_PROMPT: &str = include_str!("../../../../memory/prompts/extract.txt");

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct Candidate {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub importance: f32,
}

/// Resolve the effective prompt template. A non-empty instance file at
/// `instance_path` wins (user override, read fresh per call so edits take effect
/// live); a missing or blank instance file falls back to the embedded default,
/// silently. "Blank" means whitespace-only — a file that trims to nothing counts
/// as empty on purpose, so a stray newline never shadows the default.
pub(crate) fn resolve_prompt(instance_path: &Path, embedded: &'static str) -> String {
    match std::fs::read_to_string(instance_path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => embedded.to_string(),
    }
}

pub fn extract_from_span(text: &str) -> Result<Vec<Candidate>, ProviderError> {
    let instance = provider::hex_root().join(".hex/memory/prompts/extract.txt");
    let template = resolve_prompt(&instance, EXTRACT_PROMPT);
    // Square brackets are deliberate: the old curly-brace placeholder form
    // collided with the repo's own agent-orchestration recipe templating
    // (BOI/goose), which made these prompt templates un-editable by automated
    // workers. `[[NAME]]` is safe.
    let prompt = template.replace("[[PREDICATES]]", &predicates::vocab_for_prompt())
        + "\n\n--- TEXT ---\n"
        + text;
    let raw = provider::generate_for("memory_extract", &prompt)?;
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

    #[test]
    fn embedded_prompt_uses_bracket_placeholder() {
        assert!(
            EXTRACT_PROMPT.contains("[[PREDICATES]]"),
            "embedded extract prompt must carry the [[PREDICATES]] placeholder"
        );
    }

    #[test]
    fn substitution_fills_predicate_vocabulary() {
        let filled = EXTRACT_PROMPT.replace("[[PREDICATES]]", &predicates::vocab_for_prompt());
        assert!(
            !filled.contains("[[PREDICATES]]"),
            "placeholder must be fully substituted"
        );
        assert!(
            filled.contains("prefers"),
            "predicate vocabulary must be injected in place of the placeholder"
        );
    }

    #[test]
    fn resolve_prompt_falls_back_to_embedded_when_instance_missing() {
        let td = tempfile::tempdir().unwrap();
        let missing = td.path().join("does-not-exist.txt");
        assert_eq!(resolve_prompt(&missing, EXTRACT_PROMPT), EXTRACT_PROMPT);
    }

    #[test]
    fn resolve_prompt_falls_back_to_embedded_when_instance_blank() {
        let td = tempfile::tempdir().unwrap();
        let blank = td.path().join("blank.txt");
        std::fs::write(&blank, "   \n\t\n").unwrap();
        assert_eq!(resolve_prompt(&blank, EXTRACT_PROMPT), EXTRACT_PROMPT);
    }

    #[test]
    fn resolve_prompt_prefers_nonempty_instance_override() {
        let td = tempfile::tempdir().unwrap();
        let f = td.path().join("extract.txt");
        std::fs::write(&f, "custom [[PREDICATES]] template").unwrap();
        let out = resolve_prompt(&f, EXTRACT_PROMPT);
        assert_eq!(out, "custom [[PREDICATES]] template");
        assert_ne!(out, EXTRACT_PROMPT);
    }
}
