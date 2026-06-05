//! Minimal harness contract: the first vertical slice of submit(Event) -> Result.
//! These types are intentionally thin; the full submit() loop will EXTEND them.
//!
//! NAMING (review 2026-06-05): a DIFFERENT `Event` already exists at
//! `crate::worker::event::Event` (the iii-worker JSON envelope). This `harness::Event`
//! is unrelated. They do not collide because they live in separate modules — BUT
//! never `use crate::worker::event::Event` in `harness/` or `messages/`, and always
//! refer to this one as `crate::harness::Event`.

pub mod id;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Opt {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prompt {
    pub id: String,
    pub text: String,
    pub multi: bool,
    pub options: Vec<Opt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Answer {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub source: String,
    pub kind: String, // v1 only "request"; present for forward-compat routing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultOut {
    pub event_id: String,
    pub status: String, // "done" | "failed"
    pub output: String, // the worker's text answer (empty when output is a prompt)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Prompt>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_and_prompt_roundtrip_json() {
        let p = Prompt {
            id: "Q1".into(),
            text: "pick".into(),
            multi: false,
            options: vec![Opt {
                id: "a".into(),
                label: "A".into(),
                description: "the a".into(),
            }],
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: Prompt = serde_json::from_str(&j).unwrap();
        assert_eq!(back.options[0].label, "A");

        let e = Event {
            id: "E1".into(),
            source: "mike-cli".into(),
            kind: "request".into(),
            body: None,
            reply_to: Some("Q1".into()),
            answer: Some(Answer {
                selected: vec!["a".into()],
                free_text: None,
            }),
            refs: None,
            scope: None,
            ts: "2026-06-05T00:00:00Z".into(),
        };
        let j = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&j).unwrap();
        assert_eq!(back.answer.unwrap().selected, vec!["a".to_string()]);
    }
}
