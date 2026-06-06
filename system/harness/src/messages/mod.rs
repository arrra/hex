//! Authoritative messages store (memory.db).
//!
//! Persisting a message is a real commit — callers MUST treat an Err as fatal
//! (S6, no silent drop). v1 ships the schema + insert/lookup; resolution and
//! the CLI driver land in later tasks.

use crate::harness::{id, Answer, Event, Prompt};
use rusqlite::{params, Connection};

pub mod resolve;

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: String,
    pub source: String,
    pub kind: String,
    pub body: Option<String>,
    pub reply_to: Option<String>,
    pub answer: Option<Answer>,
    pub prompt: Option<Prompt>,
    pub ts: String,
}

impl StoredMessage {
    pub fn question(id: String, source: String, prompt: Prompt, ts: String) -> Self {
        Self {
            id,
            source,
            kind: "request".into(),
            body: None,
            reply_to: None,
            answer: None,
            prompt: Some(prompt),
            ts,
        }
    }
}

pub fn insert(conn: &Connection, m: &StoredMessage) -> rusqlite::Result<()> {
    let answer_json = m
        .answer
        .as_ref()
        .map(|a| serde_json::to_string(a).expect("serialize answer"));
    let prompt_json = m
        .prompt
        .as_ref()
        .map(|p| serde_json::to_string(p).expect("serialize prompt"));
    conn.execute(
        "INSERT INTO messages (id,source,kind,body,reply_to,answer_json,prompt_json,resolved,ts)
         VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8)",
        params![
            m.id,
            m.source,
            m.kind,
            m.body,
            m.reply_to,
            answer_json,
            prompt_json,
            m.ts
        ],
    )?;
    Ok(())
}

pub fn lookup(conn: &Connection, id: &str) -> rusqlite::Result<Option<StoredMessage>> {
    conn.query_row(
        "SELECT id,source,kind,body,reply_to,answer_json,prompt_json,ts FROM messages WHERE id=?1",
        params![id],
        |r| {
            let answer_json: Option<String> = r.get(5)?;
            let prompt_json: Option<String> = r.get(6)?;
            Ok(StoredMessage {
                id: r.get(0)?,
                source: r.get(1)?,
                kind: r.get(2)?,
                body: r.get(3)?,
                reply_to: r.get(4)?,
                answer: answer_json.map(|j| serde_json::from_str(&j).expect("deserialize answer")),
                prompt: prompt_json.map(|j| serde_json::from_str(&j).expect("deserialize prompt")),
                ts: r.get(7)?,
            })
        },
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

// ── CLI driver helpers (build the Event the `hex messages` subcommand submits) ──

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Build a normal submit Event from CLI text.
pub fn build_submit_event(body: &str) -> Event {
    Event {
        id: id::mint(),
        source: "mike-cli".into(),
        kind: "request".into(),
        body: Some(body.to_string()),
        reply_to: None,
        answer: None,
        refs: None,
        scope: None,
        ts: now_rfc3339(),
    }
}

/// Build a reply Event from CLI args: a target question id + selected option ids
/// and/or free-form text.
pub fn build_reply_event(reply_to: &str, selected: &[&str], free_text: Option<String>) -> Event {
    Event {
        id: id::mint(),
        source: "mike-cli".into(),
        kind: "request".into(),
        body: None,
        reply_to: Some(reply_to.to_string()),
        answer: Some(Answer {
            selected: selected.iter().map(|s| s.to_string()).collect(),
            free_text,
        }),
        refs: None,
        scope: None,
        ts: now_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn cli_submit_then_reply_roundtrip() {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        crate::memory::schema::apply_messages_schema(&c).unwrap();
        use crate::harness::{Opt, Prompt};
        insert(
            &c,
            &StoredMessage::question(
                "Q".into(),
                "hex".into(),
                Prompt {
                    id: "Q".into(),
                    text: "pick".into(),
                    multi: false,
                    options: vec![Opt {
                        id: "b".into(),
                        label: "tilt".into(),
                        description: "sell ETH".into(),
                    }],
                },
                "t".into(),
            ),
        )
        .unwrap();
        let e = build_reply_event("Q", &["b"], None);
        assert_eq!(e.reply_to.as_deref(), Some("Q"));
        assert_eq!(e.answer.as_ref().unwrap().selected, vec!["b".to_string()]);
        // and a plain submit builds a body event
        let s = build_submit_event("hello");
        assert_eq!(s.body.as_deref(), Some("hello"));
        assert!(s.reply_to.is_none());
    }

    #[test]
    fn schema_creates_messages_table() {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_messages_schema(&c).unwrap();
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='messages'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn insert_then_lookup_question_and_plain() {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_messages_schema(&c).unwrap();
        use crate::harness::{Opt, Prompt};
        let q = StoredMessage::question(
            "Qx".into(),
            "hex".into(),
            Prompt {
                id: "Qx".into(),
                text: "pick".into(),
                multi: false,
                options: vec![Opt {
                    id: "a".into(),
                    label: "A".into(),
                    description: "da".into(),
                }],
            },
            "ts".into(),
        );
        insert(&c, &q).unwrap();
        let got = lookup(&c, "Qx").unwrap().expect("found");
        assert!(got.prompt.is_some());
        assert_eq!(got.prompt.unwrap().options[0].id, "a");
        assert!(lookup(&c, "nope").unwrap().is_none());
    }
}
