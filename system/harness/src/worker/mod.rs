//! `hex::worker` — the typed Rust worker authoring API.
//!
//! Workers are values built with the `Worker` builder: each `.on_*` call
//! pushes a `(TriggerSpec, Handler)` pair. The runtime walks `handlers`,
//! registers the triggers with iii, and dispatches incoming events to the
//! matching closure.

pub mod ctx;
pub mod event;
pub mod outbox;
pub mod runtime;

pub type Result<T> = std::result::Result<T, anyhow::Error>;

pub type Handler =
    Box<dyn Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static>;

/// A trigger specification stored alongside a handler.
///
/// Maps 1:1 to `iii_sdk::builtin_triggers::IIITrigger::{Cron,State,Queue}`
/// when the runtime registers it with the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerSpec {
    Cron { expression: String },
    State { scope: String, key: String },
    Queue { queue: String },
}

pub struct Worker {
    pub name: String,
    pub handlers: Vec<(TriggerSpec, Handler)>,
}

impl Worker {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            handlers: Vec::new(),
        }
    }

    /// React to an emitted event. Maps to a state trigger
    /// `scope="events", key=<event>` — same convention as `ops::emit_target`.
    pub fn on_event<F>(mut self, event: &str, f: F) -> Self
    where
        F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.push((
            TriggerSpec::State {
                scope: "events".to_string(),
                key: event.to_string(),
            },
            Box::new(f),
        ));
        self
    }

    pub fn on_state<F>(mut self, scope: &str, key: &str, f: F) -> Self
    where
        F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.push((
            TriggerSpec::State {
                scope: scope.to_string(),
                key: key.to_string(),
            },
            Box::new(f),
        ));
        self
    }

    pub fn on_queue<F>(mut self, queue: &str, f: F) -> Self
    where
        F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.push((
            TriggerSpec::Queue {
                queue: queue.to_string(),
            },
            Box::new(f),
        ));
        self
    }

    pub fn on_cron<F>(mut self, expr: &str, f: F) -> Self
    where
        F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.push((
            TriggerSpec::Cron {
                expression: expr.to_string(),
            },
            Box::new(f),
        ));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn noop(_e: event::Event, _c: ctx::Ctx) -> Result<()> {
        Ok(())
    }

    /// Builder collects (TriggerSpec, Handler) pairs in registration order.
    #[test]
    fn worker_builder_collects_handlers() {
        let w = Worker::new("hex-test")
            .on_event("boi.spec.complete", noop)
            .on_cron("0 0 3 * * * *", noop);
        assert_eq!(w.name, "hex-test");
        assert_eq!(w.handlers.len(), 2);
    }

    /// `.on_event(name)` must map to a State trigger
    /// `scope="events", key=<name>` — same convention used by `ops::emit_target`.
    #[test]
    fn on_event_maps_to_state_events_scope() {
        let w = Worker::new("hex-test").on_event("boi.spec.complete", noop);
        let (spec, _h) = w.handlers.into_iter().next().expect("one handler");
        assert_eq!(
            spec,
            TriggerSpec::State {
                scope: "events".to_string(),
                key: "boi.spec.complete".to_string(),
            }
        );
    }

    /// `.on_cron(expr)` must map to a Cron trigger with the exact expression.
    #[test]
    fn on_cron_maps_to_cron_trigger() {
        let w = Worker::new("hex-test").on_cron("0 0 3 * * * *", noop);
        let (spec, _h) = w.handlers.into_iter().next().expect("one handler");
        assert_eq!(
            spec,
            TriggerSpec::Cron {
                expression: "0 0 3 * * * *".to_string(),
            }
        );
    }

    /// Event::from_envelope parses the {event,producer,ts,data} envelope and
    /// `data().str(k)` returns the typed string.
    #[test]
    fn event_envelope_accessors() {
        let envelope = json!({
            "event": "boi.spec.complete",
            "producer": "boi",
            "ts": "2026-06-04T22:53:00Z",
            "data": { "spec_id": "S123" }
        });
        let e = event::Event::from_envelope(envelope);
        assert_eq!(e.event(), "boi.spec.complete");
        assert_eq!(e.producer(), "boi");
        assert_eq!(e.ts(), "2026-06-04T22:53:00Z");
        assert_eq!(e.data().str("spec_id").unwrap(), "S123");
    }
}
