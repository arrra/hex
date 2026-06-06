//! Worker invocation: run the LLM worker so it returns EITHER a text answer OR a
//! structured question (Prompt), discriminated by a `kind` tag enforced via
//! `claude -p --json-schema`. See spikes/2026-06-05-json-schema-spike.md.
use crate::harness::Prompt;
use std::process::Command;

pub enum WorkerOutput {
    Answer(String),
    Question(Prompt),
}

/// The output schema handed to `claude --json-schema`. FLAT object (no top-level
/// oneOf — the API rejects it; see spike). `kind` discriminates; the parser
/// enforces per-kind required fields.
pub const OUTPUT_SCHEMA: &str = r#"{"type":"object","required":["kind"],"properties":{"kind":{"type":"string","enum":["answer","prompt"]},"text":{"type":"string"},"multi":{"type":"boolean"},"options":{"type":"array","items":{"type":"object","required":["id","label","description"],"properties":{"id":{"type":"string"},"label":{"type":"string"},"description":{"type":"string"}}}}}}"#;

pub fn parse_worker_json(s: &str) -> Result<WorkerOutput, String> {
    let v: serde_json::Value = serde_json::from_str(s.trim())
        .map_err(|e| format!("worker output not JSON: {e}"))?;
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("answer") => {
            let t = v
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("answer missing text")?;
            Ok(WorkerOutput::Answer(t.to_string()))
        }
        Some("prompt") => {
            let text = v
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("prompt missing text")?
                .to_string();
            let multi = v
                .get("multi")
                .and_then(|m| m.as_bool())
                .ok_or("prompt missing multi")?;
            let opts_v = v
                .get("options")
                .and_then(|o| o.as_array())
                .ok_or("prompt missing options")?;
            let mut options = Vec::new();
            for o in opts_v {
                options.push(crate::harness::Opt {
                    id: o
                        .get("id")
                        .and_then(|x| x.as_str())
                        .ok_or("option missing id")?
                        .to_string(),
                    label: o
                        .get("label")
                        .and_then(|x| x.as_str())
                        .ok_or("option missing label")?
                        .to_string(),
                    description: o
                        .get("description")
                        .and_then(|x| x.as_str())
                        .ok_or("option missing description")?
                        .to_string(),
                });
            }
            Ok(WorkerOutput::Question(Prompt {
                id: crate::harness::id::mint(),
                text,
                multi,
                options,
            }))
        }
        _ => Err(format!("worker output missing/!kind: {s}")),
    }
}

/// Run the worker over an assembled prompt string. Returns the discriminated output.
///
/// TEST SEAM (`HEX_QUESTION_WORKER` env var) — makes the e2e suite deterministic
/// and CI-safe without a live LLM:
///   - unset            → live `claude --json-schema` (production)
///   - `echo`           → returns Answer(input) verbatim, so an e2e can assert the
///                        pinned option *description* actually reached the worker
///   - `<path-to-json>` → returns parse_worker_json(file contents); point it at a
///                        prompt fixture to deterministically make hex "ask".
pub fn run_worker(input: &str) -> Result<WorkerOutput, String> {
    match std::env::var("HEX_QUESTION_WORKER").ok().as_deref() {
        Some("echo") => return Ok(WorkerOutput::Answer(input.to_string())),
        Some(path) if !path.is_empty() => {
            let s = std::fs::read_to_string(path)
                .map_err(|e| format!("read HEX_QUESTION_WORKER fixture {path}: {e}"))?;
            return parse_worker_json(&s);
        }
        _ => {}
    }
    let out = Command::new("claude")
        .args([
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            OUTPUT_SCHEMA,
            input,
        ])
        .output()
        .map_err(|e| format!("spawn claude failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "claude exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    // The schema'd object is the envelope's `.structured_output` (NOT raw stdout).
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("claude envelope not JSON: {e}"))?;
    let so = envelope
        .get("structured_output")
        .ok_or("claude envelope missing structured_output")?;
    parse_worker_json(&so.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_answer() {
        let o = parse_worker_json(r#"{"kind":"answer","text":"hello"}"#).unwrap();
        match o {
            WorkerOutput::Answer(t) => assert_eq!(t, "hello"),
            _ => panic!("want answer"),
        }
    }
    #[test]
    fn parses_prompt() {
        let j = r#"{"kind":"prompt","text":"pick","multi":false,"options":[{"id":"a","label":"A","description":"da"}]}"#;
        let o = parse_worker_json(j).unwrap();
        match o {
            WorkerOutput::Question(p) => assert_eq!(p.options[0].id, "a"),
            _ => panic!("want prompt"),
        }
    }
    #[test]
    fn malformed_fails_loud() {
        assert!(parse_worker_json(r#"{"kind":"prompt"}"#).is_err()); // missing options
        assert!(parse_worker_json(r#"not json"#).is_err());
    }
}
