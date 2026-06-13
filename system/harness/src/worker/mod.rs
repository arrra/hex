//! `hex::worker` — the typed Rust worker authoring API.
//!
//! Workers are values built with the `Worker` builder: each `.on_*` call
//! pushes a `(TriggerSpec, Handler)` pair. The runtime walks `handlers`,
//! registers the triggers with iii, and dispatches incoming events to the
//! matching closure.

pub mod ctx;
pub mod event;
pub mod outbox;
pub mod run;
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

impl TriggerSpec {
    /// Map this spec to an iii `RegisterTriggerInput` bound to `function_id`.
    /// The single place the typed worker triggers cross into `iii_sdk` — same
    /// builtin_triggers surface the legacy YAML host used in `iii_worker.rs`.
    pub fn to_register_input(
        &self,
        function_id: &str,
    ) -> iii_sdk::protocol::RegisterTriggerInput {
        use iii_sdk::builtin_triggers::{
            CronTriggerConfig, IIITrigger, QueueTriggerConfig, StateTriggerConfig,
        };
        let trigger = match self {
            TriggerSpec::Cron { expression } => {
                IIITrigger::Cron(CronTriggerConfig::new(expression.clone()))
            }
            TriggerSpec::State { scope, key } => IIITrigger::State(
                StateTriggerConfig::new().scope(scope.clone()).key(key.clone()),
            ),
            TriggerSpec::Queue { queue } => {
                IIITrigger::Queue(QueueTriggerConfig::new(queue.clone()))
            }
        };
        trigger.for_function(function_id.to_string())
    }
}

pub struct Worker {
    pub name: String,
    /// `(name, spec, handler)` — `name` is the optional stable trigger name.
    /// Named triggers get fid `{worker}::{name}`; unnamed fall back to the
    /// legacy positional `{worker}::{idx}` (instance overlay modules keep
    /// working unchanged).
    pub handlers: Vec<(Option<String>, TriggerSpec, Handler)>,
}

/// THE single fid derivation — used by the runtime at registration AND by
/// failures.rs when computing expectations, so they cannot drift.
pub fn fid_for(worker: &str, idx: usize, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{worker}::{n}"),
        None => format!("{worker}::{idx}"),
    }
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
            None,
            TriggerSpec::State {
                scope: "events".to_string(),
                key: event.to_string(),
            },
            Box::new(f),
        ));
        self
    }

    /// Named variant of `on_event` — fid becomes `{worker}::{name}`.
    pub fn on_event_named<F>(mut self, name: &str, event: &str, f: F) -> Self
    where
        F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.push((
            Some(name.to_string()),
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
            None,
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
            None,
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
            None,
            TriggerSpec::Cron {
                expression: expr.to_string(),
            },
            Box::new(f),
        ));
        self
    }

    /// Named variant of `on_cron` — fid becomes `{worker}::{name}`.
    pub fn on_cron_named<F>(mut self, name: &str, expr: &str, f: F) -> Self
    where
        F: Fn(event::Event, ctx::Ctx) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.push((
            Some(name.to_string()),
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
        let (_name, spec, _h) = w.handlers.into_iter().next().expect("one handler");
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
        let (_name, spec, _h) = w.handlers.into_iter().next().expect("one handler");
        assert_eq!(
            spec,
            TriggerSpec::Cron {
                expression: "0 0 3 * * * *".to_string(),
            }
        );
    }

    /// Named cron triggers carry their name; fid derivation uses it.
    #[test]
    fn on_cron_named_carries_name() {
        let w = Worker::new("hex-test").on_cron_named("nightly", "0 0 3 * * * *", noop);
        let (name, spec, _h) = w.handlers.into_iter().next().expect("one handler");
        assert_eq!(name.as_deref(), Some("nightly"));
        assert_eq!(spec, TriggerSpec::Cron { expression: "0 0 3 * * * *".to_string() });
    }

    /// fid_for: named → worker::name; unnamed → worker::idx (legacy fallback).
    #[test]
    fn fid_for_named_and_positional() {
        assert_eq!(fid_for("hex-x", 0, Some("nightly")), "hex-x::nightly");
        assert_eq!(fid_for("hex-x", 2, None), "hex-x::2");
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
