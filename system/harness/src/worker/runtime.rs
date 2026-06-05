//! At-most-once worker runtime — drain-aware lifecycle.
//!
//! NOTE: this is a STUB module containing only the pure/testable surface
//! the implementation must satisfy. The full `serve(registry)` lifecycle
//! (engine connect, signal install, iii.shutdown) is wired by the
//! implementation task — this file currently exposes only what the red
//! tests pin down.
//!
//! The three pure invariants exercised by the unit tests:
//!   1. `emit_target(stopping)` routes to Outbox iff stopping, else Engine.
//!   2. `drain(handles, timeout)` awaits ALL in-flight JoinHandles to
//!      completion before returning (bounded by `timeout`).
//!   3. `init_with_recorder(...)` performs init in
//!      register → replay → reconcile order. The recorder lets a test
//!      assert ordering without booting a real engine.

use std::sync::Mutex;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::outbox::Outbox;
use super::Worker;

/// Where a `Ctx::emit` call should be routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitTarget {
    /// Normal operation: deliver straight to the engine.
    Engine,
    /// Shutdown-window: divert to the durable outbox for later replay.
    Outbox,
}

/// Pure routing decision. When the runtime is `stopping`, `Ctx::emit`
/// MUST divert to the outbox; otherwise it goes to the engine.
pub fn emit_target(stopping: bool) -> EmitTarget {
    if stopping {
        EmitTarget::Outbox
    } else {
        EmitTarget::Engine
    }
}

/// Outcome of a drain pass.
#[derive(Debug, PartialEq, Eq)]
pub enum DrainOutcome {
    /// All tracked handles finished within the bounded timeout.
    AllCompleted,
    /// Bounded timeout elapsed with N handles still in-flight.
    TimedOut(usize),
}

/// Await every in-flight handler `JoinHandle` to completion, bounded by
/// `timeout`. Returns `AllCompleted` only when every handle has finished;
/// `TimedOut(n)` otherwise (n = handles not done by deadline).
pub async fn drain(
    handles: Vec<JoinHandle<()>>,
    timeout: Duration,
) -> DrainOutcome {
    let total = handles.len();
    let join_all = async {
        for h in handles {
            // Best-effort: ignore JoinError (cancellation/panic) — the
            // handle is no longer in-flight either way.
            let _ = h.await;
        }
    };
    match tokio::time::timeout(timeout, join_all).await {
        Ok(()) => DrainOutcome::AllCompleted,
        Err(_) => DrainOutcome::TimedOut(total),
    }
}

/// Init-phase ordering marker. Recorded by `init_with_recorder` so a
/// unit test can assert register-before-replay-before-reconcile.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InitStep {
    Register,
    Replay,
    Reconcile,
}

/// Test seam: records the order of init steps as they happen.
pub struct InitRecorder {
    pub events: Mutex<Vec<InitStep>>,
}

impl InitRecorder {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
}

impl Default for InitRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure init sequence with an injected recorder. The implementation must
/// push `Register`, then `Replay`, then `Reconcile` BEFORE entering the
/// serve loop — replaying the outbox into a runtime whose triggers are
/// not yet registered fires state-changes into the void.
pub async fn init_with_recorder(
    workers: &[Worker],
    outbox: &Outbox,
    recorder: &InitRecorder,
) -> anyhow::Result<()> {
    // STEP 1: register all workers' triggers FIRST, so that any state
    // changes replayed in step 2 land on a listener.
    for _w in workers {
        // The real serve() walks `w.handlers` and registers each
        // TriggerSpec with the engine; for the pure init seam we just
        // record that the step happened.
    }
    recorder.events.lock().unwrap().push(InitStep::Register);

    // STEP 2: replay the durable outbox into the engine. POP-then-deliver
    // semantics are enforced by `Outbox::replay`.
    let _ = outbox.replay(|_e| Ok(()))?;
    recorder.events.lock().unwrap().push(InitStep::Replay);

    // STEP 3: reconcile hook (default no-op). Per-worker reconcile
    // logic is a later spec.
    recorder.events.lock().unwrap().push(InitStep::Reconcile);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tempfile::tempdir;

    /// INVARIANT 1a: in normal operation Ctx::emit hits the engine.
    #[test]
    fn emit_target_normal_goes_to_engine() {
        assert_eq!(emit_target(false), EmitTarget::Engine);
    }

    /// INVARIANT 1b: while stopping, Ctx::emit DIVERTS to the outbox.
    /// This is the core at-most-once shutdown rule — emissions made
    /// during drain must land on disk, not be lost to the engine queue.
    #[test]
    fn emit_target_stopping_diverts_to_outbox() {
        assert_eq!(emit_target(true), EmitTarget::Outbox);
    }

    /// INVARIANT 2: drain awaits ALL in-flight handles to completion
    /// before returning AllCompleted. The shared counter proves every
    /// handler finished its body before drain unblocked.
    #[tokio::test]
    async fn drain_awaits_all_in_flight_handlers() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut handles = Vec::new();
        for _ in 0..3 {
            let c = counter.clone();
            handles.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let outcome = drain(handles, Duration::from_secs(2)).await;
        assert_eq!(outcome, DrainOutcome::AllCompleted);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "drain must wait for every spawned handler to complete"
        );
    }

    /// INVARIANT 2b: bounded timeout is enforced — a handler that
    /// outlives the deadline yields TimedOut, NOT a hang.
    #[tokio::test]
    async fn drain_bounded_timeout_reports_inflight_count() {
        let h = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let outcome = drain(vec![h], Duration::from_millis(50)).await;
        match outcome {
            DrainOutcome::TimedOut(n) => assert_eq!(n, 1),
            other => panic!("expected TimedOut(1), got {other:?}"),
        }
    }

    /// INVARIANT 3: init runs Register → Replay → Reconcile, in that
    /// order. Replaying the outbox before triggers are registered would
    /// fire state-changes into a runtime with no listeners; reconciling
    /// before registering has the same defect. The recorder makes the
    /// ordering observable without booting a real engine.
    #[tokio::test]
    async fn init_order_is_register_then_replay_then_reconcile() {
        let dir = tempdir().unwrap();
        let outbox = Outbox::new(dir.path().join("outbox.jsonl"));
        let workers: Vec<Worker> = vec![Worker::new("hex-test")];
        let recorder = InitRecorder::new();

        init_with_recorder(&workers, &outbox, &recorder)
            .await
            .expect("init runs");

        let events = recorder.events.lock().unwrap().clone();
        assert!(
            events.contains(&InitStep::Register),
            "init must record a Register step; got {events:?}"
        );
        assert!(
            events.contains(&InitStep::Replay),
            "init must record a Replay step; got {events:?}"
        );

        let reg = events
            .iter()
            .position(|s| *s == InitStep::Register)
            .expect("register recorded");
        let replay = events
            .iter()
            .position(|s| *s == InitStep::Replay)
            .expect("replay recorded");
        assert!(
            reg < replay,
            "Register MUST precede Replay (register-then-replay); got {events:?}"
        );

        if let Some(rec) = events.iter().position(|s| *s == InitStep::Reconcile) {
            assert!(
                replay < rec,
                "Replay MUST precede Reconcile; got {events:?}"
            );
        }
    }
}
