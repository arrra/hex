//! Reply resolution: validate a structured Answer against its Prompt, derive
//! the assemble() query string, and render the pinned block. No prose parsing.

use crate::harness::{Answer, Prompt};

pub struct Resolved {
    /// Fed to `assemble(conn, &query, ...)` — derived from option descriptions
    /// + free_text. Never prose; always a deterministic concatenation.
    pub query: String,
    /// Rendered block prepended above assembled candidates before the worker
    /// runs. Names the question and the chosen options verbatim.
    pub pin: String,
}

/// Validate a structured Answer against its Prompt and produce the assemble
/// query + rendered pin. All failure paths are loud (S6).
pub fn resolve_answer(q: &Prompt, a: &Answer) -> Result<Resolved, String> {
    // cardinality (S6)
    if !q.multi && a.selected.len() > 1 {
        return Err(format!(
            "question {} is single-select but {} options were chosen",
            q.id,
            a.selected.len()
        ));
    }
    // membership (S6)
    let mut chosen = Vec::new();
    for sid in &a.selected {
        match q.options.iter().find(|o| &o.id == sid) {
            Some(o) => chosen.push(o.clone()),
            None => {
                return Err(format!(
                    "selected id {sid:?} is not an option of question {}",
                    q.id
                ))
            }
        }
    }
    // derive query: option descriptions + free_text
    let mut query = chosen
        .iter()
        .map(|o| o.description.clone())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(ft) = &a.free_text {
        if !query.is_empty() {
            query.push(' ');
        }
        query.push_str(ft);
    }

    // render pin
    let mut pin = format!("You asked: \"{}\"\n", q.text);
    for o in &chosen {
        pin.push_str(&format!("Mike chose: {} — {}\n", o.label, o.description));
    }
    if chosen.is_empty() {
        pin.push_str("Mike answered free-form.\n");
    }
    if let Some(ft) = &a.free_text {
        pin.push_str(&format!("Note: {ft}\n"));
    }

    Ok(Resolved {
        query: query.trim().to_string(),
        pin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Answer, Opt, Prompt};

    fn q() -> Prompt {
        Prompt {
            id: "Q".into(),
            text: "Rebuy?".into(),
            multi: false,
            options: vec![
                Opt {
                    id: "a".into(),
                    label: "pure".into(),
                    description: "keep mix".into(),
                },
                Opt {
                    id: "b".into(),
                    label: "tilt-BTC".into(),
                    description: "sell ETH on rebuy".into(),
                },
            ],
        }
    }

    #[test]
    fn happy_single_select_pins_choice() {
        let r = resolve_answer(
            &q(),
            &Answer {
                selected: vec!["b".into()],
                free_text: None,
            },
        )
        .unwrap();
        assert!(r.query.contains("sell ETH on rebuy"));
        assert!(r.pin.contains("tilt-BTC"));
    }

    #[test]
    fn unknown_id_fails_loud() {
        let e = resolve_answer(
            &q(),
            &Answer {
                selected: vec!["z".into()],
                free_text: None,
            },
        );
        assert!(e.is_err());
    }

    #[test]
    fn too_many_for_single_select_fails_loud() {
        let e = resolve_answer(
            &q(),
            &Answer {
                selected: vec!["a".into(), "b".into()],
                free_text: None,
            },
        );
        assert!(e.is_err());
    }

    #[test]
    fn free_text_rider_is_labeled() {
        let r = resolve_answer(
            &q(),
            &Answer {
                selected: vec!["b".into()],
                free_text: Some("only EU".into()),
            },
        )
        .unwrap();
        assert!(r.pin.contains("Note: only EU"));
        assert!(r.query.contains("only EU"));
    }

    #[test]
    fn free_text_only_is_carried() {
        let r = resolve_answer(
            &q(),
            &Answer {
                selected: vec![],
                free_text: Some("do something else".into()),
            },
        )
        .unwrap();
        assert!(r.query.contains("do something else"));
    }
}
