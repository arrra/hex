//! Capped LRU pool of live rust-analyzer instances (SPEC-A2 §2, §4).
//!
//! One instance == one canonical worktree root. Policy (all knobs from
//! [`ScipdConfig`], SPEC-A2 §4):
//!
//! - **Cap + LRU evict:** spawning past `pool_cap` shuts down the
//!   least-recently-used instance first.
//! - **Idle-TTL reap:** `sweep()` kills instances idle past `idle_ttl_secs`.
//! - **Vanish reap (always on):** an instance whose worktree path no longer
//!   exists is killed on the next sweep.
//! - **Memory watchdog:** an instance whose *physical footprint*
//!   ([`LiveBackend::footprint_mb`], NOT `ps` RSS — SPEC-A2 §4 "Memory
//!   metric") exceeds `mem_limit_mb` is killed — but never within
//!   `spawn_grace_secs` of spawn (priming spikes), and only the WORST
//!   offender per sweep. The kill leaves a red note in `status` until the
//!   next successful spawn.
//! - **Pool-wide alarm:** total footprint ≥ `pool_alarm_mb` logs an alarm
//!   and surfaces a status note — no kill.
//!
//! Every transition (spawn / evict / reap / kill) goes through one
//! [`log_transition`] sink: stderr plus an in-memory ring surfaced via
//! [`PoolStatus::notes`] (SPEC-A2 A2-S9 — no silent transitions).
//!
//! The pool is synchronous and `Mutex`-protected; `sweep()` is called by a
//! daemon-owned reaper/watchdog thread (Task 5). Time is an injected `now`
//! closure so policy tests run on a fake clock — no sleeps. Lock order is
//! pool map → entry; never call back into the pool while holding an entry
//! lock.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::config::ScipdConfig;
use crate::live::{InstanceState, LiveBackend};
use crate::proto;

/// Transition-log ring capacity — enough recent history for `status`
/// without unbounded growth.
const RING_CAP: usize = 64;

type SpawnFn<B> = Box<dyn Fn(&Path) -> Result<B> + Send + Sync>;
type NowFn = Box<dyn Fn() -> Instant + Send + Sync>;

/// A live instance plus the pool-side bookkeeping the policy runs on.
/// `last_used`/`spawned_at` come from the pool's injected clock (NOT the
/// backend's wall clock) so the reaper and the LRU agree on one timeline.
struct Entry<B> {
    backend: Arc<Mutex<B>>,
    spawned_at: Instant,
    last_used: Instant,
}

struct Inner<B> {
    instances: HashMap<PathBuf, Entry<B>>,
    /// Recent transitions, oldest first — surfaced via `status`.
    ring: VecDeque<String>,
    /// Memory-watchdog kill notes, retained until the next successful spawn.
    red_notes: Vec<String>,
    /// Pool-wide alarm note while the alarm condition holds.
    alarm_note: Option<String>,
}

/// The capped live-instance pool. Generic over [`LiveBackend`] so policy
/// tests substitute a fake and never pay a rust-analyzer prime (plan T3).
pub struct Pool<B: LiveBackend> {
    config: ScipdConfig,
    spawn: SpawnFn<B>,
    now: NowFn,
    inner: Mutex<Inner<B>>,
}

impl<B: LiveBackend> Pool<B> {
    /// Pool on the real clock. `spawn` runs under the pool lock — the spawn
    /// returns while the instance is still Warming (SPEC-A2 §2), so this
    /// never holds the lock across a prime.
    pub fn new(config: ScipdConfig, spawn: impl Fn(&Path) -> Result<B> + Send + Sync + 'static) -> Self {
        Self::with_clock(config, spawn, Instant::now)
    }

    /// Test seam: inject the clock every policy decision reads.
    pub fn with_clock(
        config: ScipdConfig,
        spawn: impl Fn(&Path) -> Result<B> + Send + Sync + 'static,
        now: impl Fn() -> Instant + Send + Sync + 'static,
    ) -> Self {
        Pool {
            config,
            spawn: Box::new(spawn),
            now: Box::new(now),
            inner: Mutex::new(Inner {
                instances: HashMap::new(),
                ring: VecDeque::new(),
                red_notes: Vec::new(),
                alarm_note: None,
            }),
        }
    }

    /// Get the live instance for `worktree` (keyed by canonical root),
    /// spawning one if absent or dead — evicting the LRU instance first when
    /// at `pool_cap`. The returned handle outlives pool internals; lock it
    /// per request.
    pub fn get_or_spawn(&self, worktree: &Path) -> Result<Arc<Mutex<B>>> {
        if self.config.pool_cap == 0 {
            bail!("pool_cap is 0 — refusing to spawn any live instance (check scipd.toml)");
        }
        let root = worktree
            .canonicalize()
            .with_context(|| format!("canonicalizing worktree root {}", worktree.display()))?;
        let now = (self.now)();
        let mut inner = self.inner.lock().unwrap();

        if let Some(entry) = inner.instances.get_mut(&root) {
            if entry.backend.lock().unwrap().state() != InstanceState::Dead {
                entry.last_used = now;
                return Ok(Arc::clone(&entry.backend));
            }
            let entry = inner.instances.remove(&root).expect("entry just observed");
            log_transition(
                &mut inner,
                format!("reaped dead instance for {} — respawning", root.display()),
            );
            entry.backend.lock().unwrap().shutdown(); // idempotent cleanup
        }

        while inner.instances.len() >= self.config.pool_cap {
            let victim = inner
                .instances
                .iter()
                .min_by_key(|(path, entry)| (entry.last_used, path.to_path_buf()))
                .map(|(path, _)| path.clone())
                .expect("pool at cap ≥ 1 is non-empty");
            let entry = inner.instances.remove(&victim).expect("victim just selected");
            log_transition(
                &mut inner,
                format!(
                    "evicted {} (LRU; pool at cap {})",
                    victim.display(),
                    self.config.pool_cap
                ),
            );
            entry.backend.lock().unwrap().shutdown();
        }

        match (self.spawn)(&root) {
            Ok(backend) => {
                log_transition(&mut inner, format!("spawned instance for {}", root.display()));
                // Watchdog red notes live until the next SUCCESSFUL spawn.
                inner.red_notes.clear();
                let backend = Arc::new(Mutex::new(backend));
                inner.instances.insert(
                    root,
                    Entry { backend: Arc::clone(&backend), spawned_at: now, last_used: now },
                );
                Ok(backend)
            }
            Err(e) => {
                log_transition(
                    &mut inner,
                    format!("spawn FAILED for {}: {e:#}", root.display()),
                );
                Err(e.context(format!("spawning live instance for {}", root.display())))
            }
        }
    }

    /// One policy pass: vanish reap, dead reap, idle-TTL reap, memory
    /// watchdog (worst offender only, post-grace), pool-wide alarm.
    /// Synchronous; the daemon's reaper thread calls this every 30s.
    pub fn sweep(&self) {
        let now = (self.now)();
        let mut inner = self.inner.lock().unwrap();

        // Vanish / dead / idle — collect first, then kill (can't mutate the
        // map mid-iteration).
        let mut doomed: Vec<(PathBuf, String)> = Vec::new();
        for (path, entry) in &inner.instances {
            if !path.exists() {
                doomed.push((
                    path.clone(),
                    format!("vanish-reaped {} (worktree no longer exists)", path.display()),
                ));
                continue;
            }
            if entry.backend.lock().unwrap().state() == InstanceState::Dead {
                doomed.push((
                    path.clone(),
                    format!("reaped dead instance for {}", path.display()),
                ));
                continue;
            }
            let idle = now.duration_since(entry.last_used);
            if idle >= Duration::from_secs(self.config.idle_ttl_secs) {
                doomed.push((
                    path.clone(),
                    format!(
                        "idle-reaped {} (idle {}s ≥ ttl {}s)",
                        path.display(),
                        idle.as_secs(),
                        self.config.idle_ttl_secs
                    ),
                ));
            }
        }
        for (path, msg) in doomed {
            let entry = inner.instances.remove(&path).expect("doomed entry present");
            log_transition(&mut inner, msg);
            entry.backend.lock().unwrap().shutdown();
        }

        // Memory watchdog: among post-grace instances over the limit, kill
        // ONLY the worst offender this sweep (SPEC-A2 §4).
        let grace = Duration::from_secs(self.config.spawn_grace_secs);
        let worst = inner
            .instances
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.spawned_at) >= grace)
            .filter_map(|(path, entry)| {
                entry
                    .backend
                    .lock()
                    .unwrap()
                    .footprint_mb()
                    .filter(|mb| *mb > self.config.mem_limit_mb)
                    .map(|mb| (path.clone(), mb))
            })
            .max_by_key(|(path, mb)| (*mb, path.to_path_buf()));
        if let Some((path, mb)) = worst {
            let entry = inner.instances.remove(&path).expect("offender present");
            let note = format!(
                "mem-watchdog killed {} (footprint {mb}MB > limit {}MB)",
                path.display(),
                self.config.mem_limit_mb
            );
            log_transition(&mut inner, note.clone());
            inner.red_notes.push(note);
            entry.backend.lock().unwrap().shutdown();
        }

        // Pool-wide alarm: log + status note only, never a kill (SPEC-A2 §4).
        let total: u64 = inner
            .instances
            .values()
            .filter_map(|entry| entry.backend.lock().unwrap().footprint_mb())
            .sum();
        if total >= self.config.pool_alarm_mb {
            let note = format!(
                "POOL ALARM: total footprint {total}MB ≥ {}MB across {} instance(s)",
                self.config.pool_alarm_mb,
                inner.instances.len()
            );
            eprintln!("pool: {note}");
            inner.alarm_note = Some(note);
        } else if inner.alarm_note.take().is_some() {
            eprintln!(
                "pool: alarm cleared (total footprint {total}MB < {}MB)",
                self.config.pool_alarm_mb
            );
        }
    }

    /// Ops hatch (`evict` op, SPEC-A2 §3): drop the instance for `worktree`.
    /// Returns whether one existed.
    pub fn evict(&self, worktree: &Path) -> bool {
        // Canonicalize when possible; a vanished dir can't be, so fall back
        // to the raw path (it may still match a stored canonical key).
        let key = worktree
            .canonicalize()
            .unwrap_or_else(|_| worktree.to_path_buf());
        let mut inner = self.inner.lock().unwrap();
        match inner.instances.remove(&key) {
            Some(entry) => {
                log_transition(&mut inner, format!("evicted {} (operator request)", key.display()));
                entry.backend.lock().unwrap().shutdown();
                true
            }
            None => false,
        }
    }

    /// Shut every instance down (daemon SIGTERM path — no orphans).
    pub fn shutdown_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        let paths: Vec<PathBuf> = inner.instances.keys().cloned().collect();
        for path in paths {
            let entry = inner.instances.remove(&path).expect("key just listed");
            log_transition(&mut inner, format!("shut down {} (pool shutdown)", path.display()));
            entry.backend.lock().unwrap().shutdown();
        }
    }

    /// Occupancy snapshot for the `status` op (SPEC-A2 §3/§4): per-instance
    /// state + memory + age/idle, plus loud notes — retained watchdog red
    /// notes first, the pool alarm while active, then the transition ring
    /// (oldest first).
    pub fn status(&self) -> proto::PoolStatus {
        let now = (self.now)();
        let inner = self.inner.lock().unwrap();
        let mut instances: Vec<proto::InstanceStatus> = inner
            .instances
            .iter()
            .map(|(path, entry)| {
                let backend = entry.backend.lock().unwrap();
                proto::InstanceStatus {
                    worktree: path.display().to_string(),
                    state: proto_state(backend.state()),
                    rss_mb: backend.footprint_mb().or_else(|| backend.rss_mb()).unwrap_or(0),
                    age_secs: now.duration_since(entry.spawned_at).as_secs(),
                    idle_secs: now.duration_since(entry.last_used).as_secs(),
                }
            })
            .collect();
        instances.sort_by(|a, b| a.worktree.cmp(&b.worktree));
        let mut notes = inner.red_notes.clone();
        notes.extend(inner.alarm_note.clone());
        notes.extend(inner.ring.iter().cloned());
        proto::PoolStatus { pool_cap: self.config.pool_cap, instances, notes }
    }
}

/// THE transition sink (SPEC-A2 A2-S9): stderr + the in-memory ring that
/// `status` surfaces. Every spawn/evict/reap/kill goes through here.
fn log_transition<B>(inner: &mut Inner<B>, msg: String) {
    eprintln!("pool: {msg}");
    if inner.ring.len() == RING_CAP {
        inner.ring.pop_front();
    }
    inner.ring.push_back(msg);
}

fn proto_state(state: InstanceState) -> proto::InstanceState {
    match state {
        InstanceState::Warming => proto::InstanceState::Warming,
        InstanceState::Ready => proto::InstanceState::Ready,
        InstanceState::Dead => proto::InstanceState::Dead,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{LiveError, LiveResult};
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tempfile::TempDir;

    /// Controllable view of one fake instance, shared with the test.
    #[derive(Debug, Clone)]
    struct FakeHandle {
        state: Arc<Mutex<InstanceState>>,
        footprint: Arc<Mutex<Option<u64>>>,
        shutdowns: Arc<AtomicUsize>,
    }

    impl FakeHandle {
        fn new() -> Self {
            FakeHandle {
                state: Arc::new(Mutex::new(InstanceState::Ready)),
                footprint: Arc::new(Mutex::new(Some(100))),
                shutdowns: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn set_state(&self, s: InstanceState) {
            *self.state.lock().unwrap() = s;
        }

        fn set_footprint(&self, mb: Option<u64>) {
            *self.footprint.lock().unwrap() = mb;
        }

        fn shutdowns(&self) -> usize {
            self.shutdowns.load(Ordering::SeqCst)
        }
    }

    #[derive(Debug)]
    struct FakeBackend {
        h: FakeHandle,
        created: Instant,
    }

    impl LiveBackend for FakeBackend {
        fn state(&self) -> InstanceState {
            *self.h.state.lock().unwrap()
        }

        fn request(&self, _method: &str, _params: Value) -> LiveResult<Value> {
            match self.state() {
                InstanceState::Ready => Ok(Value::Null),
                InstanceState::Warming => Err(LiveError::Warming { elapsed_secs: 0 }),
                InstanceState::Dead => Err(LiveError::Dead { reason: "fake".into() }),
            }
        }

        fn shutdown(&mut self) {
            self.h.shutdowns.fetch_add(1, Ordering::SeqCst);
            self.h.set_state(InstanceState::Dead);
        }

        fn rss_mb(&self) -> Option<u64> {
            // Deliberately useless: the pool must measure via footprint_mb
            // (SPEC-A2 §4 memory metric), only falling back for status.
            None
        }

        fn footprint_mb(&self) -> Option<u64> {
            *self.h.footprint.lock().unwrap()
        }

        fn last_used(&self) -> Instant {
            self.created
        }
    }

    struct Harness {
        pool: Pool<FakeBackend>,
        spawned: Arc<Mutex<Vec<FakeHandle>>>,
        clock: Arc<Mutex<Instant>>,
        fail_spawns: Arc<AtomicBool>,
        _dirs: Vec<TempDir>,
        worktrees: Vec<PathBuf>,
    }

    impl Harness {
        fn tick(&self, secs: u64) {
            *self.clock.lock().unwrap() += Duration::from_secs(secs);
        }

        fn handle(&self, i: usize) -> FakeHandle {
            self.spawned.lock().unwrap()[i].clone()
        }

        fn spawn_count(&self) -> usize {
            self.spawned.lock().unwrap().len()
        }

        fn wt(&self, i: usize) -> &Path {
            &self.worktrees[i]
        }
    }

    /// Pool on a fake clock with `dirs` real (temp) worktree directories.
    fn harness(config: ScipdConfig, dirs: usize) -> Harness {
        let clock = Arc::new(Mutex::new(Instant::now()));
        let spawned: Arc<Mutex<Vec<FakeHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let fail_spawns = Arc::new(AtomicBool::new(false));
        let pool = Pool::with_clock(
            config,
            {
                let spawned = Arc::clone(&spawned);
                let fail = Arc::clone(&fail_spawns);
                move |_root: &Path| {
                    if fail.load(Ordering::SeqCst) {
                        bail!("fake spawn failure (test-injected)");
                    }
                    let h = FakeHandle::new();
                    spawned.lock().unwrap().push(h.clone());
                    Ok(FakeBackend { h, created: Instant::now() })
                }
            },
            {
                let clock = Arc::clone(&clock);
                move || *clock.lock().unwrap()
            },
        );
        let _dirs: Vec<TempDir> = (0..dirs).map(|_| tempfile::tempdir().unwrap()).collect();
        let worktrees = _dirs.iter().map(|d| d.path().canonicalize().unwrap()).collect();
        Harness { pool, spawned, clock, fail_spawns, _dirs, worktrees }
    }

    fn config(cap: usize) -> ScipdConfig {
        ScipdConfig {
            pool_cap: cap,
            idle_ttl_secs: 100,
            mem_limit_mb: 1000,
            pool_alarm_mb: 1500,
            spawn_grace_secs: 180,
            warm_fallback_secs: 240,
        }
    }

    /// Memory-policy tests tick past the 180s grace; a huge idle TTL keeps
    /// the idle reaper from stealing their kills.
    fn mem_config(cap: usize) -> ScipdConfig {
        ScipdConfig { idle_ttl_secs: 1_000_000, ..config(cap) }
    }

    #[test]
    fn spawns_once_then_reuses() {
        let h = harness(config(2), 1);
        let a = h.pool.get_or_spawn(h.wt(0)).unwrap();
        let b = h.pool.get_or_spawn(h.wt(0)).unwrap();
        assert_eq!(h.spawn_count(), 1, "second get must reuse, not respawn");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn keyed_by_canonical_root() {
        let h = harness(config(2), 1);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        // Same directory via a non-canonical spelling: <root>/sub/..
        let sub = h.wt(0).join("sub");
        std::fs::create_dir(&sub).unwrap();
        h.pool.get_or_spawn(&sub.join("..")).unwrap();
        assert_eq!(h.spawn_count(), 1, "path spellings of one root must share an instance");
    }

    #[test]
    fn missing_worktree_is_loud_error() {
        let h = harness(config(2), 0);
        let err = h.pool.get_or_spawn(Path::new("/nonexistent/worktree/xyz")).unwrap_err();
        assert!(format!("{err:#}").contains("/nonexistent/worktree/xyz"));
        assert_eq!(h.spawn_count(), 0);
    }

    #[test]
    fn pool_cap_zero_refuses_loudly() {
        let h = harness(config(0), 1);
        let err = h.pool.get_or_spawn(h.wt(0)).unwrap_err();
        assert!(format!("{err:#}").contains("pool_cap is 0"));
    }

    #[test]
    fn cap_overflow_evicts_least_recently_used() {
        let h = harness(config(2), 3);
        h.pool.get_or_spawn(h.wt(0)).unwrap(); // A
        h.tick(1);
        h.pool.get_or_spawn(h.wt(1)).unwrap(); // B
        h.tick(1);
        h.pool.get_or_spawn(h.wt(0)).unwrap(); // touch A → B is now LRU
        h.tick(1);
        h.pool.get_or_spawn(h.wt(2)).unwrap(); // C → evicts B, not A

        assert_eq!(h.handle(1).shutdowns(), 1, "LRU instance (B) must be shut down");
        assert_eq!(h.handle(0).shutdowns(), 0, "recently-touched A must survive");
        let status = h.pool.status();
        let roots: Vec<&str> = status.instances.iter().map(|i| i.worktree.as_str()).collect();
        assert_eq!(status.instances.len(), 2);
        assert!(roots.contains(&h.wt(0).to_str().unwrap()));
        assert!(roots.contains(&h.wt(2).to_str().unwrap()));
        assert!(
            status.notes.iter().any(|n| n.contains("evicted") && n.contains("LRU")),
            "eviction must be visible in status notes: {:?}",
            status.notes
        );
    }

    #[test]
    fn idle_ttl_reap_via_injected_clock() {
        let h = harness(config(2), 2);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.pool.get_or_spawn(h.wt(1)).unwrap();
        h.tick(99); // ttl is 100
        h.pool.get_or_spawn(h.wt(1)).unwrap(); // touch B only
        h.pool.sweep();
        assert_eq!(h.handle(0).shutdowns(), 0, "99s idle < 100s ttl must survive");

        h.tick(99); // A now idle 198s; B idle 99s
        h.pool.sweep();
        assert_eq!(h.handle(0).shutdowns(), 1, "idle past ttl must be reaped");
        assert_eq!(h.handle(1).shutdowns(), 0, "touched instance must survive");
        let status = h.pool.status();
        assert_eq!(status.instances.len(), 1);
        assert!(
            status.notes.iter().any(|n| n.contains("idle-reaped")),
            "idle reap must be visible in status notes: {:?}",
            status.notes
        );
    }

    #[test]
    fn vanish_reap_kills_instance_for_deleted_worktree() {
        let h = harness(config(2), 2);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.pool.get_or_spawn(h.wt(1)).unwrap();
        std::fs::remove_dir_all(h.wt(0)).unwrap();
        h.pool.sweep();
        assert_eq!(h.handle(0).shutdowns(), 1, "vanished worktree's instance must die");
        assert_eq!(h.handle(1).shutdowns(), 0);
        let status = h.pool.status();
        assert_eq!(status.instances.len(), 1);
        assert!(
            status.notes.iter().any(|n| n.contains("vanish-reaped")),
            "vanish reap must be visible in status notes: {:?}",
            status.notes
        );
    }

    #[test]
    fn mem_watchdog_respects_post_spawn_grace() {
        let h = harness(mem_config(2), 1);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.handle(0).set_footprint(Some(5000)); // way over the 1000MB limit
        h.tick(179); // grace is 180s
        h.pool.sweep();
        assert_eq!(h.handle(0).shutdowns(), 0, "no mem kill within the post-spawn grace");

        h.tick(2); // past grace
        h.pool.sweep();
        assert_eq!(h.handle(0).shutdowns(), 1, "over-limit instance must die after grace");
        let status = h.pool.status();
        assert!(
            status.notes.iter().any(|n| n.contains("mem-watchdog killed")),
            "mem kill must be a red note in status: {:?}",
            status.notes
        );
    }

    #[test]
    fn mem_watchdog_red_note_retained_until_next_successful_spawn() {
        let h = harness(mem_config(2), 2);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.handle(0).set_footprint(Some(5000));
        h.tick(181);
        h.pool.sweep();
        // The red note survives further sweeps...
        h.pool.sweep();
        let red = |s: &proto::PoolStatus| {
            // Red notes are surfaced FIRST, before the ring; the kill also
            // appears once in the ring — count > 1 proves retention.
            s.notes.iter().filter(|n| n.contains("mem-watchdog killed")).count()
        };
        assert!(red(&h.pool.status()) > 1, "red note must persist across sweeps");
        // ...and clears on the next successful spawn (ring copy remains).
        h.pool.get_or_spawn(h.wt(1)).unwrap();
        assert_eq!(red(&h.pool.status()), 1, "successful spawn must clear the red note");
    }

    #[test]
    fn mem_watchdog_kills_worst_offender_only() {
        let mut cfg = mem_config(3);
        cfg.pool_alarm_mb = 100_000; // keep the alarm out of this test
        let h = harness(cfg, 2);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.pool.get_or_spawn(h.wt(1)).unwrap();
        h.handle(0).set_footprint(Some(2000)); // over limit
        h.handle(1).set_footprint(Some(3000)); // worse
        h.tick(181);
        h.pool.sweep();
        assert_eq!(h.handle(1).shutdowns(), 1, "worst offender dies");
        assert_eq!(h.handle(0).shutdowns(), 0, "lesser offender survives this sweep");
        h.pool.sweep();
        assert_eq!(h.handle(0).shutdowns(), 1, "next sweep takes the next offender");
    }

    #[test]
    fn pool_alarm_logs_and_notes_without_killing() {
        let h = harness(mem_config(2), 2);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.pool.get_or_spawn(h.wt(1)).unwrap();
        // Each under the 1000MB limit; total 1600 ≥ 1500 alarm.
        h.handle(0).set_footprint(Some(800));
        h.handle(1).set_footprint(Some(800));
        h.tick(181); // past grace — proves the alarm itself never kills
        h.pool.sweep();
        assert_eq!(h.handle(0).shutdowns() + h.handle(1).shutdowns(), 0, "alarm must not kill");
        let status = h.pool.status();
        assert!(
            status.notes.iter().any(|n| n.contains("POOL ALARM")),
            "alarm must surface in status notes: {:?}",
            status.notes
        );
        // Alarm clears when the total drops back under the threshold.
        h.handle(0).set_footprint(Some(100));
        h.pool.sweep();
        assert!(
            !h.pool.status().notes.iter().any(|n| n.contains("POOL ALARM")),
            "cleared alarm must leave status"
        );
    }

    #[test]
    fn dead_instance_respawns_on_next_get() {
        let h = harness(config(2), 1);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.handle(0).set_state(InstanceState::Dead);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        assert_eq!(h.spawn_count(), 2, "dead instance must be replaced");
        assert_eq!(h.pool.status().instances.len(), 1);
    }

    #[test]
    fn sweep_reaps_dead_instances() {
        let h = harness(config(2), 1);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.handle(0).set_state(InstanceState::Dead);
        h.pool.sweep();
        let status = h.pool.status();
        assert_eq!(status.instances.len(), 0);
        assert!(status.notes.iter().any(|n| n.contains("reaped dead instance")));
    }

    #[test]
    fn evict_op_drops_instance_and_reports_absence() {
        let h = harness(config(2), 1);
        assert!(!h.pool.evict(h.wt(0)), "evicting a non-resident worktree returns false");
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        assert!(h.pool.evict(h.wt(0)));
        assert_eq!(h.handle(0).shutdowns(), 1);
        assert_eq!(h.pool.status().instances.len(), 0);
        assert!(h.pool.status().notes.iter().any(|n| n.contains("operator request")));
    }

    #[test]
    fn spawn_failure_is_loud_error_and_logged() {
        let h = harness(config(2), 1);
        h.fail_spawns.store(true, Ordering::SeqCst);
        let err = h.pool.get_or_spawn(h.wt(0)).unwrap_err();
        assert!(format!("{err:#}").contains("fake spawn failure"));
        assert!(
            h.pool.status().notes.iter().any(|n| n.contains("spawn FAILED")),
            "spawn failure must be visible in status notes"
        );
        // Pool stays usable.
        h.fail_spawns.store(false, Ordering::SeqCst);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        assert_eq!(h.pool.status().instances.len(), 1);
    }

    #[test]
    fn status_reports_state_memory_age_and_idle() {
        let h = harness(config(2), 1);
        h.pool.get_or_spawn(h.wt(0)).unwrap();
        h.handle(0).set_footprint(Some(321));
        h.handle(0).set_state(InstanceState::Warming);
        h.tick(7);
        h.pool.get_or_spawn(h.wt(0)).unwrap(); // touch at t=7
        h.tick(3);
        let status = h.pool.status();
        assert_eq!(status.pool_cap, 2);
        let inst = &status.instances[0];
        assert_eq!(inst.worktree, h.wt(0).to_str().unwrap());
        assert_eq!(inst.state, proto::InstanceState::Warming);
        assert_eq!(inst.rss_mb, 321, "status memory comes from footprint_mb");
        assert_eq!(inst.age_secs, 10);
        assert_eq!(inst.idle_secs, 3);
    }

    #[test]
    fn shutdown_all_kills_everything() {
        let h = harness(config(3), 3);
        for i in 0..3 {
            h.pool.get_or_spawn(h.wt(i)).unwrap();
        }
        h.pool.shutdown_all();
        for i in 0..3 {
            assert_eq!(h.handle(i).shutdowns(), 1, "instance {i} not shut down");
        }
        assert_eq!(h.pool.status().instances.len(), 0);
    }

    #[test]
    fn transition_ring_is_bounded() {
        let h = harness(config(1), 2);
        for _ in 0..60 {
            // Alternating spawns at cap 1 → evict + spawn each round.
            h.pool.get_or_spawn(h.wt(0)).unwrap();
            h.tick(1);
            h.pool.get_or_spawn(h.wt(1)).unwrap();
            h.tick(1);
        }
        let notes = h.pool.status().notes;
        assert!(notes.len() <= RING_CAP, "ring must stay bounded, got {}", notes.len());
        assert!(notes.iter().any(|n| n.contains("evicted")));
    }
}
