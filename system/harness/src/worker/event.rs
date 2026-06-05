//! Typed event delivered to a worker handler.

use anyhow::{anyhow, Error};
use serde_json::Value;

/// A typed event envelope: `{event, producer, ts, data}`.
pub struct Event {
    envelope: Value,
}

/// Typed accessor over the `data` object inside an event envelope.
pub struct Data<'a> {
    v: &'a Value,
}

impl Event {
    /// Wrap a raw JSON envelope. The envelope is expected to have keys
    /// `event`, `producer`, `ts`, and `data` (as written by `ops::emit`).
    pub fn from_envelope(v: Value) -> Self {
        Self { envelope: v }
    }

    pub fn event(&self) -> &str {
        self.envelope
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    pub fn producer(&self) -> &str {
        self.envelope
            .get("producer")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    pub fn ts(&self) -> &str {
        self.envelope
            .get("ts")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    pub fn data(&self) -> Data<'_> {
        let v = self.envelope.get("data").unwrap_or(&Value::Null);
        Data { v }
    }

    /// Access the raw envelope value.
    pub fn envelope(&self) -> &Value {
        &self.envelope
    }
}

impl<'a> Data<'a> {
    /// Read a string-valued key from the data object.
    pub fn str(&self, k: &str) -> Result<&'a str, Error> {
        self.v
            .get(k)
            .ok_or_else(|| anyhow!("missing key {k:?} in event data"))?
            .as_str()
            .ok_or_else(|| anyhow!("key {k:?} in event data is not a string"))
    }

    /// Raw access to the underlying JSON value.
    pub fn raw(&self) -> &'a Value {
        self.v
    }
}
