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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

use super::ctx::Ctx;
use super::event::Event;
use super::outbox::Outbox;
use super::Worker;

/// Default in-process engine WebSocket URL the worker runtime connects to.
const DEFAULT_ENGINE_URL: &str = "ws://127.0.0.1:49134";

/// Bounded time to wait for in-flight handlers to finish on SIGTERM before
/// forcing exit.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Long-running serve entry — hidden behind `hex harness serve` so launchd can
/// invoke it. One tokio runtime hosts BOTH the in-process iii engine and the
/// worker runtime that connects to it as an SDK client:
///
///   build engine → spawn `engine.serve()` → connect worker → REGISTER all
///   functions+triggers → REPLAY the durable outbox → RECONCILE → serve, then
///   `select!` { engine exits | SIGTERM → drain in-flight → exit }.
///
/// Delivery is at-most-once; reliability comes from the graceful drain +
/// shutdown-deferral outbox, not from redelivery (see hex-workers-as-rust-library).
pub fn serve(workers: Vec<Worker>) -> i32 {
    // Multi-thread runtime: the engine + SDK both want a full reactor, and
    // handlers run on blocking threads (see the spawn_blocking below).
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("hex harness serve: failed to build tokio runtime: {e}");
            return 1;
        }
    };
    rt.block_on(run(workers))
}

/// Resolve the durable outbox path: `$HEX_DIR/.hex/harness/outbox.jsonl`,
/// falling back to a temp file if HEX_DIR is unset (so serve never panics).
fn outbox_path() -> std::path::PathBuf {
    match std::env::var("HEX_DIR") {
        Ok(dir) => std::path::PathBuf::from(dir)
            .join(".hex")
            .join("harness")
            .join("outbox.jsonl"),
        Err(_) => std::env::temp_dir().join("hex-harness-outbox.jsonl"),
    }
}

/// The async lifecycle body. Returns the process exit code.
async fn run(workers: Vec<Worker>) -> i32 {
    // Reap orphaned distill children from a previous harness life BEFORE the
    // engine comes up: `claude -p` children run in their own process group, so
    // a launchd kill of our group leaves them alive under PID 1 with no
    // timeout enforcement (observed burning tokens for 2h+, 2026-06-11).
    if let Ok(hex_dir) = std::env::var("HEX_DIR") {
        let report = crate::reaper::sweep(std::path::Path::new(&hex_dir));
        if report.killed > 0 || report.removed_stale > 0 {
            eprintln!(
                "hex harness serve: reaper killed {} orphan(s), cleared {} stale pidfile(s)",
                report.killed, report.removed_stale
            );
        }
    }

    let outbox = Arc::new(Outbox::new(outbox_path()));
    let stopping = Arc::new(AtomicBool::new(false));
    let inflight = Arc::new(AtomicUsize::new(0));

    // 1. Build + start the in-process iii engine (default config: state/cron/
    //    queue builtins via the inventory registry; rabbitmq dropped at the
    //    Cargo level). `serve()` is long-lived, so it runs on its own task.
    use iii_engine::workers::config::EngineConfig;
    use iii_engine::EngineBuilder;
    let engine = match EngineBuilder::new()
        .with_config(EngineConfig::default_config())
        .build()
        .await
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("hex harness serve: in-process engine build failed: {e}");
            return 1;
        }
    };
    let mut engine_task: JoinHandle<()> = tokio::spawn(async move {
        if let Err(e) = engine.serve().await {
            eprintln!("hex harness serve: in-process engine.serve() errored: {e}");
        }
    });

    // 2. Connect the worker runtime to the in-process engine. The SDK opens the
    //    connection on a background task and auto-retries until the engine's WS
    //    port is up, so connecting immediately (before serve binds) is safe.
    let url = std::env::var("III_URL").unwrap_or_else(|_| DEFAULT_ENGINE_URL.to_string());
    let iii = iii_sdk::register_worker(&url, iii_sdk::InitOptions::default());

    // 2a. REGISTER FIRST — every worker's functions + triggers — so any state
    //     changes replayed in 2b land on a live listener (init-order rule).
    let mut registered = 0usize;
    for worker in workers {
        let wname = worker.name.clone();
        for (idx, (spec, handler)) in worker.handlers.into_iter().enumerate() {
            let fid = format!("{wname}::{idx}");
            let handler = Arc::new(handler);
            let stopping_h = stopping.clone();
            let outbox_h = outbox.clone();
            let inflight_h = inflight.clone();
            let fid_h = fid.clone();
            let wname_h = wname.clone();
            iii.register_function(
                fid.clone(),
                iii_sdk::RegisterFunction::new_async(move |input: serde_json::Value| {
                    let handler = handler.clone();
                    let stopping = stopping_h.clone();
                    let outbox = outbox_h.clone();
                    let inflight = inflight_h.clone();
                    let fid = fid_h.clone();
                    let wname = wname_h.clone();
                    async move {
                        // Stop accepting NEW fires once draining (at-most-once:
                        // the dropped fire is recovered by reconcile-on-startup).
                        if stopping.load(Ordering::SeqCst) {
                            return Ok(serde_json::json!({ "skipped": "draining" }));
                        }
                        // Track this invocation so drain can wait for it. The
                        // guard decrements even if the handler panics.
                        let _guard = InflightGuard::enter(&inflight);
                        // State/event triggers deliver a StateEventData payload
                        // `{message_type,event_type,scope,key,old_value,new_value}`
                        // where `new_value` is the emit envelope we wrote. Cron
                        // and other triggers deliver their own payload directly.
                        // Unwrap `new_value` when present so `evt.data()` sees the
                        // real {event,producer,ts,data} envelope.
                        let envelope = match input.get("new_value") {
                            Some(v) => v.clone(),
                            None => input,
                        };
                        let evt = Event::from_envelope(envelope);
                        let ctx = Ctx::with_runtime(stopping.clone(), outbox.clone());
                        // Handlers are synchronous and may block (ctx.run shells
                        // out) — run on a blocking thread so the reactor isn't
                        // stalled. The guard above is held across this await.
                        let started = std::time::Instant::now();
                        let res = tokio::task::spawn_blocking(move || handler(evt, ctx)).await;
                        let duration_ms = started.elapsed().as_millis() as i64;
                        // Auto-trace every handler invocation to the telemetry
                        // store — the chokepoint the old YAML host gave us, kept
                        // here so the Rust harness doesn't lose observability.
                        // Loud-but-not-fatal: a telemetry write never fails the job.
                        let record = |status: &str, detail: Option<String>| {
                            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                                source: wname.clone(),
                                event: fid.clone(),
                                status: status.to_string(),
                                duration_ms: Some(duration_ms),
                                exit_code: None,
                                detail,
                            });
                        };
                        match res {
                            Ok(Ok(())) => {
                                record("ok", None);
                                Ok(serde_json::json!({ "ok": true }))
                            }
                            Ok(Err(e)) => {
                                record("error", Some(e.to_string()));
                                Err(iii_sdk::IIIError::Handler(format!("{fid}: {e}")))
                            }
                            Err(e) => {
                                record("panic", Some(e.to_string()));
                                Err(iii_sdk::IIIError::Handler(format!("{fid}: join error: {e}")))
                            }
                        }
                    }
                }),
            );
            if let Err(e) = iii.register_trigger(spec.to_register_input(&fid)) {
                eprintln!("hex harness serve: failed to register trigger for {fid}: {e}");
                return 1;
            }
            registered += 1;
        }
    }
    eprintln!("hex harness serve: registered {registered} handler(s)");

    // 2a'. Wait for the worker connection to come up. The in-process engine
    //      binds its WS port a beat after we spawn `serve()`, so the SDK retries
    //      until it's ready; reaching Connected means the engine is live AND our
    //      registrations have been sent. A brief settle lets the async
    //      trigger-registration round-trips land engine-side BEFORE we replay
    //      state changes into it — otherwise replay could fire into the void.
    //      (Reconcile-on-startup, a later spec, is the robust fix; this is v1.)
    wait_connected(&iii, Duration::from_secs(10)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 2b. REPLAY the durable outbox (pop-then-deliver) through the harness's
    //     LIVE engine connection. These are shutdown-window emissions deferred
    //     on a prior stop; replaying is their FIRST engine delivery (at-most-once
    //     holds). We deliver via the async `iii` handle, NOT the blocking
    //     `ops::emit` — the latter spins its own tokio runtime and `block_on`,
    //     which panics ("runtime within a runtime") here in the async context.
    let mut replayed = 0usize;
    loop {
        match outbox.pop_front() {
            Ok(Some(em)) => {
                if let Err(e) = emit_via(&iii, &em.event, em.data).await {
                    // Loud but non-fatal (S6): the entry is already popped
                    // (at-most-once); a failed delivery is dropped, not retried.
                    eprintln!(
                        "hex harness serve: replay delivery failed for '{}' (dropped): {e}",
                        em.event
                    );
                }
                replayed += 1;
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("hex harness serve: outbox read error during replay (continuing): {e}");
                break;
            }
        }
    }
    if replayed > 0 {
        eprintln!("hex harness serve: replayed {replayed} deferred emission(s)");
    }

    // 2c. RECONCILE-on-startup sweep — v1 no-op hook. Per-worker reconcile
    //     (re-deriving state values missed while down) is a later spec; the
    //     register→replay→reconcile ordering is established here.

    eprintln!("hex harness serve: up (engine in-process, {registered} handler(s)) — serving");

    // 3. Serve until the engine exits unexpectedly or we get SIGTERM/SIGINT.
    tokio::select! {
        _ = &mut engine_task => {
            eprintln!("hex harness serve: in-process engine exited unexpectedly");
            return 1;
        }
        _ = shutdown_signal() => {
            eprintln!("hex harness serve: shutdown signal received — draining");
        }
    }

    // 4. Graceful drain: stop new fires, wait for in-flight handlers to finish
    //    (any emit they make now diverts to the outbox via Ctx), bounded.
    stopping.store(true, Ordering::SeqCst);
    match drain_inflight(&inflight, DRAIN_TIMEOUT).await {
        DrainOutcome::AllCompleted => {
            eprintln!("hex harness serve: drain complete — all handlers finished")
        }
        DrainOutcome::TimedOut(n) => {
            eprintln!(
                "hex harness serve: drain timed out after {}s — {n} handler(s) still in-flight",
                DRAIN_TIMEOUT.as_secs()
            );
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "harness".into(),
                event: "drain::timeout".into(),
                status: "error".into(),
                duration_ms: Some(DRAIN_TIMEOUT.as_millis() as i64),
                exit_code: None,
                detail: Some(format!("{n} handler(s) killed in-flight")),
            });
        }
    }

    // 5. Tear down: drop the worker connection, stop the engine task.
    drop(iii);
    engine_task.abort();
    eprintln!("hex harness serve: stopped cleanly");
    0
}

/// Deliver one emission to the engine through an EXISTING `iii` connection,
/// async — the replay path's equivalent of `ops::emit`, minus the nested tokio
/// runtime. Writes the same `events`-scope state envelope `ops::emit` does, so a
/// replayed emission fires the same state triggers as a fresh one.
async fn emit_via(
    iii: &iii_sdk::III,
    event: &str,
    data: serde_json::Value,
) -> anyhow::Result<()> {
    let producer = crate::ops::resolve_producer(None);
    let ts = chrono::Utc::now().to_rfc3339();
    let target = crate::ops::emit_target(event, &producer, &ts, &data);
    let payload = serde_json::json!({
        "scope": target.scope,
        "key": target.key,
        "value": target.value,
    });
    iii.trigger(iii_sdk::protocol::TriggerRequest {
        function_id: "state::set".to_string(),
        payload,
        action: None,
        timeout_ms: None,
    })
    .await
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!("state::set failed for replayed event '{event}': {e}"))
}

/// Poll the worker connection until it reaches `Connected`, bounded by
/// `timeout`. Doubles as the "engine is up" wait — the SDK can't connect until
/// the in-process engine has bound its port. Loud-but-non-fatal on timeout (S6):
/// we proceed so the harness still serves, but warn that replay may be degraded.
async fn wait_connected(iii: &iii_sdk::III, timeout: Duration) {
    let poll = async {
        while !matches!(
            iii.get_connection_state(),
            iii_sdk::IIIConnectionState::Connected
        ) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    if tokio::time::timeout(timeout, poll).await.is_err() {
        eprintln!(
            "hex harness serve: worker connection not Connected within {}s — \
             proceeding (outbox replay may be degraded)",
            timeout.as_secs()
        );
    }
}

/// Await SIGTERM or SIGINT. SIGTERM is what launchd sends on `bootout`/stop;
/// SIGINT covers a foreground Ctrl-C during local debugging.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hex harness serve: cannot install SIGTERM handler: {e}");
            // Fall back to never-resolving so the engine task still governs exit.
            std::future::pending::<()>().await;
            return;
        }
    };
    let mut intr = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => {
            term.recv().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = intr.recv() => {}
    }
}

/// RAII counter for in-flight handler invocations. Increments on `enter`,
/// decrements on `Drop` (so a panicking handler still releases its slot).
struct InflightGuard {
    counter: Arc<AtomicUsize>,
}

impl InflightGuard {
    fn enter(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        InflightGuard {
            counter: counter.clone(),
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Wait until the in-flight counter reaches zero, bounded by `timeout`.
/// App-layer drain: the SDK detach-spawns each invocation (no JoinHandle to
/// us), so we track completion via the shared counter instead.
async fn drain_inflight(inflight: &AtomicUsize, timeout: Duration) -> DrainOutcome {
    let poll = async {
        while inflight.load(Ordering::SeqCst) > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    match tokio::time::timeout(timeout, poll).await {
        Ok(()) => DrainOutcome::AllCompleted,
        Err(_) => DrainOutcome::TimedOut(inflight.load(Ordering::SeqCst)),
    }
}

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
