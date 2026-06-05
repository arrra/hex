//! `Ctx` — the handle passed to a worker handler. Provides emit/state/run.
//!
//! `emit` delegates to `crate::ops::emit` in normal operation. The runtime
//! wraps this with diversion-to-outbox during the shutdown drain window.

use anyhow::{anyhow, Error};
use serde_json::Value;

/// Handler-facing context. Created by the runtime per invocation.
pub struct Ctx;

impl Ctx {
    /// Construct a plain Ctx (the runtime will replace this with a richer
    /// constructor once the lifecycle wires the outbox in).
    pub fn new() -> Self {
        Ctx
    }

    /// Emit an event. In normal operation this delegates to `ops::emit`,
    /// which writes the `{event,producer,ts,data}` envelope to iii state.
    pub fn emit(&self, event: &str, data: Value) -> Result<(), Error> {
        crate::ops::emit(event, data, None).map_err(|e| anyhow!(e))
    }

    /// Handle for direct state access (placeholder; expanded by runtime task).
    pub fn state(&self) -> StateHandle {
        StateHandle
    }

    /// Run a shell command from within a handler.
    pub fn run(&self, argv: &[String]) -> Result<std::process::Output, Error> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| anyhow!("Ctx::run called with empty argv"))?;
        std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| anyhow!("Ctx::run spawn failed: {e}"))
    }
}

impl Default for Ctx {
    fn default() -> Self {
        Self::new()
    }
}

pub struct StateHandle;
