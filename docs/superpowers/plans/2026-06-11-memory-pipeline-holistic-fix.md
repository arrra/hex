# Memory Pipeline Holistic Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every memory-pipeline failure loud, fix the Stop-hook capture race, stop the nightly-consolidation silent skip, repair vector-store gaps, add DB maintenance + backups, and give facts semantic recall — closing all 22 confirmed findings from the 2026-06-11 memory-subsystem assessment (mrap-hex `projects/system-improvement/audits/2026-06-11-memory-subsystem-assessment.md`, FIX-007…FIX-011).

**Architecture:** Targeted hardening of the existing pipeline (no rebuild — the assessment confirmed the structure is sound; the failures are all at observability boundaries and edge paths). Three systemic changes ripple through everything: (1) *findings ≠ failure* — workspace lint findings stop driving exit codes; only operational errors do; (2) *every silent path gets a telemetry event + (where it matters) an alert*; (3) *scheduled self-repair* — a weekly `hex memory maintain` job sweeps orphans, optimizes FTS, vacuums, and backfills missing embeddings, so one-off corruption stops being permanent.

**Approaches considered (brainstorm gate):** (a) minimal alerts-only patch — rejected: leaves the findings/exit-code conflation, the lock-skip, orphan rot, and capture race in place; (b) rebuild consolidation as an iii-native pipeline — rejected: high risk, structure verified sound; (c) **targeted hardening across all confirmed findings — chosen.**

**Tech stack:** Rust (hex-harness crate, `system/harness/`), rusqlite 0.31 `bundled-full` (includes the sqlite backup API), serde_json (hooks), fs2 (flock), clap 4, iii worker crons.

**Build/test (from repo root — workspace target trap: artifacts land in `<repo-root>/target/`, NOT `system/harness/target/`):**
```bash
export PATH="/opt/homebrew/bin:$PATH"
cargo test -p hex-harness            # full suite
cargo build --release -p hex-harness # binary at target/release/hex
```

**Line numbers** below are from foundation HEAD `9b70565a` (verified 2026-06-11). If a grep in a task finds the symbol at a different line, trust the grep.

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `system/harness/src/consolidate.rs` | Modify | exit semantics, lock discipline, full-completion stamp |
| `system/harness/src/memory/consolidate.rs` | Modify | backstop wall-clock budget |
| `system/harness/src/memory/distill/mod.rs` | Modify | slice telemetry gains `offset=` |
| `system/harness/src/worker/ctx.rs` | Modify | head+tail detail capture (was tail-only 500) |
| `system/harness/src/doctor/consolidate.rs` | Modify | audit freshness 30d → 48h |
| `system/harness/src/doctor/checks/nightly_full_liveness.rs` | Create | misses of the 03:00Z full run become doctor FAIL + alert |
| `system/harness/src/doctor/runner.rs` | Modify | register new check |
| `system/harness/src/alert.rs` | Create | deduped loud alerts: stderr + ledger + osascript |
| `system/harness/src/hook/capture.rs` | Modify | stdin-first capture, loud failures |
| `system/harness/src/memory/claude_cli.rs` | Modify | pidfile for spawned `claude -p` children |
| `system/harness/src/reaper.rs` | Create | startup sweep of orphaned distill children |
| `system/harness/src/worker/runtime.rs` | Modify | run reaper at serve start; drain-timeout telemetry |
| `system/harness/src/backup.rs` | Create | `hex backup`: sqlite snapshots + rotation |
| `system/harness/src/main.rs` | Modify | register `backup`, `memory maintain` clap commands |
| `system/harness/src/memory/index.rs` | Modify | end-of-run embedding backfill for vectorless chunks |
| `system/harness/src/memory/stats.rs` | Modify | report unembedded chunks + orphan vectors |
| `system/harness/src/memory/vector.rs` | Modify | KNN distance floor |
| `system/harness/src/memory/embed.rs` | Modify | gate `[rss]` logs behind `HEX_RSS_LOG` |
| `system/harness/src/memory/maintain.rs` | Create | orphan sweep, FTS optimize, transcript_files purge, VACUUM, facts backfill |
| `system/harness/src/memory/recall.rs` | Modify | facts gain a KNN arm fused via RRF |
| `system/harness/src/modules/memory_maintenance.worker.rs` | Modify | offset quick cron; add weekly maintain job |
| `system/harness/src/modules/backup.worker.rs` | Keep | already invokes `hex backup` (which Task 7 creates) |
| `CHANGELOG.md` | Modify | one entry for the release train |

Task DAG: 1, 2, 5, 6, 7, 8 are independent. 3 → 4 (same file). 4 → (6 uses alert + stamp). 8 → 9 → 10. 11 last.

---

### Task 1: Telemetry detail quality — slice offsets + head+tail capture

**Files:**
- Modify: `system/harness/src/memory/distill/mod.rs:32-50` (`telemetry_slice`)
- Modify: `system/harness/src/worker/ctx.rs:94-99` (tail closure)

- [ ] **Step 1: Locate both sites**

Run: `grep -n "fn telemetry_slice" system/harness/src/memory/distill/mod.rs && grep -n "saturating_sub(500)" system/harness/src/worker/ctx.rs`
Expected: one hit each (distill/mod.rs:32, ctx.rs:~98).

- [ ] **Step 2: Add `start_offset` to slice telemetry**

`telemetry_slice` currently records `path= bytes= est_tokens= strikes=`. Add a `start_offset: i64` parameter (insert after `path`) and include it in the detail format string:

```rust
fn telemetry_slice(
    path: &str,
    start_offset: i64,
    bytes: i64,
    est_tokens: u32,
    outcome: &str,
    strikes: u32,
) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "memory::distill".into(),
        event: "distill::slice".into(),
        status: outcome.into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(format!(
            "path={} offset={} bytes={} est_tokens={} strikes={}",
            path, start_offset, bytes, est_tokens, strikes
        )),
    });
}
```

Update every caller (grep `telemetry_slice(` in the same file; the per-slice loop has the slice's starting offset in scope as the watermark `offset` variable — pass it as `start_offset as i64`). Without this, poison-skip events are unrecoverable forensically (the 2026-06-10 incident required manual range-chaining to find what was lost).

- [ ] **Step 3: Replace tail-only truncation with head+tail**

In `system/harness/src/worker/ctx.rs`, the closure keeps only the LAST 500 chars of child stderr — that destroyed file paths at the head of error messages during the extract.txt incident. Replace:

```rust
let tail = |bytes: &[u8]| {
    let s = String::from_utf8_lossy(bytes);
    let t = s.trim();
    head_tail(t, 600, 400)
};
```

and add to the same file (module scope):

```rust
/// First `head` + last `tail` chars with an ellipsis marker — error heads
/// carry file paths, tails carry exit reasons; keep both.
fn head_tail(s: &str, head: usize, tail: usize) -> String {
    let flat = s.replace('\n', " ");
    if flat.len() <= head + tail {
        return flat;
    }
    // char-boundary-safe slicing
    let head_end = flat
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= head)
        .last()
        .unwrap_or(0);
    let tail_start = flat
        .char_indices()
        .map(|(i, _)| i)
        .find(|&i| i >= flat.len().saturating_sub(tail))
        .unwrap_or(flat.len());
    format!("{} …[truncated]… {}", &flat[..head_end], &flat[tail_start..])
}
```

- [ ] **Step 4: Unit test for head_tail**

In `ctx.rs` tests module (create `#[cfg(test)] mod tests` if absent):

```rust
#[test]
fn head_tail_keeps_both_ends() {
    let s = format!("/path/to/the/error/file.txt: {}END", "x".repeat(2000));
    let out = head_tail(&s, 600, 400);
    assert!(out.starts_with("/path/to/the/error/file.txt:"));
    assert!(out.ends_with("END"));
    assert!(out.contains("…[truncated]…"));
}

#[test]
fn head_tail_short_passthrough() {
    assert_eq!(head_tail("short", 600, 400), "short");
}
```

- [ ] **Step 5: Test + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t1.log 2>&1; tail -5 /tmp/t1.log`
Expected: `test result: ok`.

```bash
git add system/harness/src/memory/distill/mod.rs system/harness/src/worker/ctx.rs
git commit -m "telemetry: slice events carry start offset; child detail keeps head+tail"
```

---

### Task 2: Exit-code semantics — findings ≠ failure

**Files:**
- Modify: `system/harness/src/consolidate.rs:69-142` (`run`)
- Test: in-file tests `consolidate.rs` + `system/harness/tests/consolidate_orchestrator.rs`

Root cause of "89/89 error completions": Layer 1 (doctor) reports workspace lint findings (broken links, orphan projects — 20 currently in mrap-hex) and `run()` escalates ANY nonzero L1 to exit 1. Every quick tick therefore "fails" forever, drowning real failures.

- [ ] **Step 1: Write the failing test**

In `consolidate.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn l1_findings_do_not_fail_the_run() {
    // exit_code_for is the new pure aggregation fn introduced in Step 2
    assert_eq!(exit_code_for(/*l1_findings=*/ 20, /*any_error=*/ false), 0);
    assert_eq!(exit_code_for(0, true), 1);
    assert_eq!(exit_code_for(5, true), 1);
    assert_eq!(exit_code_for(0, false), 0);
}
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness --lib consolidate::tests::l1_findings_do_not_fail_the_run > /tmp/t2.log 2>&1; tail -3 /tmp/t2.log`
Expected: FAIL (`exit_code_for` not defined).

- [ ] **Step 3: Implement**

In `consolidate.rs`, replace the single `any_fail` flag (line 69) with two tracks plus the pure function:

```rust
/// Exit code policy: workspace FINDINGS (L1 doctor lint) are reported, not
/// fatal. Only OPERATIONAL errors (L2 op failure, DB unopenable, L3 LLM
/// failure, backstop failure) fail the run. 472 consecutive cron "errors"
/// that were really lint findings taught us this (2026-06-11 assessment).
fn exit_code_for(l1_findings: i32, any_error: bool) -> i32 {
    let _ = l1_findings; // reported in summary + artifacts, never exit-fatal
    if any_error { 1 } else { 0 }
}
```

In `run()`:
- `let mut any_error = false;` and `let mut l1_findings: i32 = 0;`
- L1 block (lines 74-79): `l1_findings = l1;` and change the eprintln to `println!("Layer 1: {l1} findings (reported, non-fatal)")` when `l1 != 0`. Do NOT set any_error.
- Backstop `Err` (line 90-93), L2 `report.failed` non-empty (104-106), L2 `Err` (108-111/114-117), L3 `Err` (133-136): set `any_error = true` (rename from any_fail).
- Final lines 140-141:

```rust
    let code = exit_code_for(l1_findings, any_error);
    println!(
        "\n=== consolidate done (exit={code}, findings={l1_findings}, errors={}) ===",
        any_error
    );
    code
```

- [ ] **Step 4: Run tests**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t2b.log 2>&1; tail -5 /tmp/t2b.log`
Expected: `test result: ok`. If existing orchestrator tests asserted exit-1-on-findings, update them to the new policy (they now assert exit 0 + findings count in stdout).

- [ ] **Step 5: Commit**

```bash
git add system/harness/src/consolidate.rs system/harness/tests/
git commit -m "consolidate: findings are reported, not fatal — exit reflects operational errors only"
```

---

### Task 3: Lock discipline — full waits, quick skips loudly

**Files:**
- Modify: `system/harness/src/consolidate.rs:34-67`

The 2026-06-10 nightly was skipped in 55ms (recorded 'ok') because a quick tick held the flock. Full must WAIT (it's the only producer of nightly audit artifacts); quick may skip but must say so in telemetry.

- [ ] **Step 1: Write failing tests (pure policy fn)**

```rust
#[test]
fn lock_policy_full_waits_quick_skips() {
    assert_eq!(lock_wait_budget(Mode::Full), std::time::Duration::from_secs(45 * 60));
    assert_eq!(lock_wait_budget(Mode::Quick), std::time::Duration::ZERO);
}
```

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness --lib consolidate::tests::lock_policy_full_waits_quick_skips > /tmp/t3.log 2>&1; tail -3 /tmp/t3.log`
Expected: FAIL (fn not defined). (Check the actual `Mode` enum variant names at the top of consolidate.rs — adjust `Quick` if it's named differently.)

- [ ] **Step 2: Implement**

```rust
const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Full consolidation is the sole producer of the nightly audit; it WAITS for
/// the lock (a quick tick budget is ≤ ~12 min — see op_transcript_backstop —
/// so 45 min covers two stuck ticks). Quick overlaps are normal; skip fast.
fn lock_wait_budget(mode: Mode) -> std::time::Duration {
    match mode {
        Mode::Full => std::time::Duration::from_secs(45 * 60),
        _ => std::time::Duration::ZERO,
    }
}

fn acquire_lock(lock_file: &std::fs::File, budget: std::time::Duration) -> bool {
    use fs2::FileExt;
    let start = std::time::Instant::now();
    loop {
        if lock_file.try_lock_exclusive().is_ok() {
            return true;
        }
        if start.elapsed() >= budget {
            return false;
        }
        std::thread::sleep(LOCK_POLL_INTERVAL.min(budget - start.elapsed()));
    }
}
```

Replace lines 60-66 with:

```rust
    if !acquire_lock(&lock_file, lock_wait_budget(mode)) {
        let (status, code) = match mode {
            Mode::Full => ("lock-timeout", 1), // nightly MISSED — this must be loud
            _ => ("skipped-lock", 0),          // overlap is normal for quick
        };
        eprintln!(
            "hex memory consolidate ({mode:?}): lock {} held past budget — {status}",
            lock_path.display()
        );
        crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
            source: "memory::consolidate".into(),
            event: format!("consolidate::{mode:?}").to_lowercase(),
            status: status.into(),
            duration_ms: None,
            exit_code: Some(code),
            detail: Some(format!("lock={}", lock_path.display())),
        });
        if matches!(mode, Mode::Full) {
            crate::alert::notify(
                "consolidate-full-lock-timeout",
                "hex nightly consolidation MISSED",
                "full consolidate could not acquire memory-consolidate.lock within 45m",
            );
        }
        return code;
    }
```

Note: `crate::alert` lands in Task 4 — implement Tasks 3+4 on one branch or stub `alert::notify` first (Task 4 defines it; if building task-by-task, do Task 4 Step 1 before this step compiles).

- [ ] **Step 3: Verify TelemetryEvent field types**

Run: `grep -n -A8 "pub struct TelemetryEvent" system/harness/src/telemetry/mod.rs`
Expected: fields `source/event/status: String`-likes, `duration_ms: Option<i64>`, `exit_code: Option<i32>`, `detail: Option<String>`. Adjust `.into()`/`format!` calls to match exactly.

- [ ] **Step 4: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t3b.log 2>&1; tail -5 /tmp/t3b.log`
Expected: `test result: ok`.

```bash
git add system/harness/src/consolidate.rs
git commit -m "consolidate: full waits for the lock (45m) and alerts on timeout; quick lock-skips record skipped-lock, not ok"
```

---

### Task 4: Alert helper + full-completion stamp + doctor nightly-liveness + 48h audit window

**Files:**
- Create: `system/harness/src/alert.rs`
- Modify: `system/harness/src/lib.rs` or `main.rs` module list (add `pub mod alert;` wherever `pub mod telemetry;` lives)
- Modify: `system/harness/src/consolidate.rs` (stamp on full completion)
- Modify: `system/harness/src/doctor/consolidate.rs` (30d → 48h)
- Create: `system/harness/src/doctor/checks/nightly_full_liveness.rs`
- Modify: `system/harness/src/doctor/runner.rs:42-85` (register)

- [ ] **Step 1: alert.rs — deduped, never-fatal, three surfaces**

```rust
//! Loud alert pathway: stderr + telemetry row + macOS notification.
//! Deduped per key via a stamp file so a 15-min cron can call this every
//! tick without producing notification spam. Never fails the caller (S6:
//! observe loudly, never break the observed job).

use std::path::Path;
use std::time::{Duration, SystemTime};

const DEDUPE_WINDOW: Duration = Duration::from_secs(6 * 3600);

/// Returns true if the alert fired (not suppressed by dedupe).
pub fn notify(key: &str, title: &str, msg: &str) -> bool {
    let hex_dir = match std::env::var("HEX_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => {
            eprintln!("ALERT [{key}] {title}: {msg} (HEX_DIR unset — stderr only)");
            return true;
        }
    };
    notify_at(&hex_dir, key, title, msg)
}

/// Inner, testable form.
pub fn notify_at(hex_dir: &Path, key: &str, title: &str, msg: &str) -> bool {
    if suppressed(hex_dir, key) {
        return false;
    }
    eprintln!("ALERT [{key}] {title}: {msg}");
    let _ = crate::telemetry::record(&crate::telemetry::TelemetryEvent {
        source: "alert".into(),
        event: key.into(),
        status: "alert".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(format!("{title}: {msg}")),
    });
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            msg.replace('"', "'"),
            title.replace('"', "'")
        );
        if let Err(e) = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .status()
        {
            eprintln!("alert [{key}]: osascript failed: {e}");
        }
    }
    stamp(hex_dir, key);
    true
}

fn stamp_path(hex_dir: &Path, key: &str) -> std::path::PathBuf {
    hex_dir.join(".hex/run/alerts").join(format!("{key}.last"))
}

fn suppressed(hex_dir: &Path, key: &str) -> bool {
    stamp_path(hex_dir, key)
        .metadata()
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or(Duration::MAX) < DEDUPE_WINDOW)
        .unwrap_or(false)
}

fn stamp(hex_dir: &Path, key: &str) {
    let p = stamp_path(hex_dir, key);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&p, b"") {
        eprintln!("alert [{key}]: stamp write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dedupe_suppresses_within_window() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(notify_at(tmp.path(), "test-key", "t", "m"));
        assert!(!notify_at(tmp.path(), "test-key", "t", "m")); // suppressed
    }
}
```

- [ ] **Step 2: Stamp full-run completion**

In `consolidate.rs`, after the Layer-3 block, when `matches!(mode, Mode::Full) && !any_error`, write metadata key `last_full_consolidated`:

```rust
    if matches!(mode, Mode::Full) && !any_error {
        if let Ok(conn) = crate::memory::open_db(&crate::memory::db_path(hex_dir)) {
            if let Err(e) = conn.execute(
                "INSERT INTO metadata(key, value) VALUES('last_full_consolidated', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![chrono::Local::now().to_rfc3339()],
            ) {
                eprintln!("consolidate: failed to stamp last_full_consolidated: {e}");
            }
        }
    }
```

- [ ] **Step 3: Tighten audit freshness 30d → 48h**

`system/harness/src/doctor/consolidate.rs` (~line 302): replace `let thirty_days_secs: u64 = 30 * 24 * 3600;` with `let fresh_window_secs: u64 = 48 * 3600;` (rename uses). A nightly artifact checked with a 30-day window masked the 2026-06-10 miss for what would have been weeks.

- [ ] **Step 4: New doctor check — nightly_full_liveness.rs**

Mirror `system/harness/src/doctor/checks/reflection_liveness.rs` (same metadata-table read pattern; it reads `last_consolidated` with a 48h threshold — this check reads `last_full_consolidated` with a 26h threshold). **Read that file first and copy its exact `Context`/`CheckResult`/`Category` usage** — the body below marks the one region to adapt:

```rust
//! FAIL when the nightly full consolidation hasn't completed in >26h —
//! catches lock-timeouts, harness-down nights, and kills-in-flight that the
//! per-run telemetry can't see (the run never finished to record anything).
use super::super::check::{Category, CheckResult, Context, DoctorCheck};

const THRESHOLD_SECS: i64 = 26 * 3600;
const KEY: &str = "last_full_consolidated";

pub struct NightlyFullLiveness;

impl DoctorCheck for NightlyFullLiveness {
    fn name(&self) -> &str { "nightly-full-liveness" }
    fn category(&self) -> Category { Category::Health } // ← verify variant against reflection_liveness.rs
    fn run(&self, ctx: &Context) -> CheckResult {
        // ADAPT FROM reflection_liveness.rs: open memory.db via ctx, read
        // metadata[KEY], parse RFC3339, age = now - stamp.
        //   missing key      -> Fail("nightly full consolidation has never completed")
        //   age > THRESHOLD  -> Fail(format!("last full consolidation {age_h}h ago (>26h)"))
        //                       + crate::alert::notify("nightly-full-liveness",
        //                           "hex nightly consolidation missed", <same msg>);
        //   else             -> Pass
        unimplemented!("copy reflection_liveness.rs body, swap key+threshold, add alert on Fail")
    }
}
```

The committed file must contain the real adapted body (the `unimplemented!` is a placeholder in THIS PLAN only, because the exact `CheckResult` constructors live in reflection_liveness.rs — the worker reads it and writes real code). Add an in-file unit test following reflection_liveness.rs's test pattern: fresh stamp → Pass; stale/missing → Fail (tempdir DB).

- [ ] **Step 5: Register the check**

`system/harness/src/doctor/runner.rs` registry vec: add `Box::new(checks::nightly_full_liveness::NightlyFullLiveness),` after the `ReflectionLogFresh` entry; add `pub mod nightly_full_liveness;` to `doctor/checks/mod.rs`.

- [ ] **Step 6: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t4.log 2>&1; tail -5 /tmp/t4.log`
Expected: `test result: ok`.

```bash
git add system/harness/src/alert.rs system/harness/src/doctor/ system/harness/src/consolidate.rs system/harness/src/lib.rs
git commit -m "doctor: nightly-full-liveness check (26h) + 48h audit window; deduped alert pathway (stderr+telemetry+osascript)"
```

---

### Task 5: Quick-tick wall-clock budget

**Files:**
- Modify: `system/harness/src/memory/consolidate.rs:117-158` (`op_transcript_backstop`)

Quick ticks ran 65-99 minutes (54-file backlog × one slice each) — holding the flock across the nightly window. Cap the backstop loop.

- [ ] **Step 1: Failing test**

In `memory/consolidate.rs` tests:

```rust
#[test]
fn backstop_budget_constant_is_ten_minutes() {
    assert_eq!(BACKSTOP_BUDGET, std::time::Duration::from_secs(10 * 60));
    let fresh = std::time::Instant::now();
    assert!(!backstop_over_budget(fresh));
}
```

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness --lib memory::consolidate > /tmp/t5.log 2>&1; tail -3 /tmp/t5.log` → FAIL (symbols undefined).

- [ ] **Step 2: Implement**

```rust
/// One quick tick may not hold the consolidate lock indefinitely — the
/// nightly full run needs it (lock_wait_budget = 45m). 10 minutes processes
/// ~10-20 slices; the 15-min cron picks the remainder up next tick.
pub(crate) const BACKSTOP_BUDGET: std::time::Duration = std::time::Duration::from_secs(10 * 60);

pub(crate) fn backstop_over_budget(start: std::time::Instant) -> bool {
    start.elapsed() >= BACKSTOP_BUDGET
}
```

In the `op_transcript_backstop` file loop (the `for` walking registered transcript files calling `distill::run_on_file`), collect the file list to a `Vec` first (so `remaining` is computable), add `let loop_start = std::time::Instant::now();` before the loop, and at the top of each iteration:

```rust
        if backstop_over_budget(loop_start) {
            let remaining = files.len() - i;
            let msg = format!(
                "backstop budget ({:?}) reached — {remaining} file(s) deferred to next tick",
                BACKSTOP_BUDGET
            );
            println!("{msg}");
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "memory::consolidate".into(),
                event: "backstop::budget-stop".into(),
                status: "ok".into(),
                duration_ms: Some(loop_start.elapsed().as_millis() as i64),
                exit_code: None,
                detail: Some(msg),
            });
            break;
        }
```

- [ ] **Step 3: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t5b.log 2>&1; tail -5 /tmp/t5b.log` → ok.

```bash
git add system/harness/src/memory/consolidate.rs
git commit -m "consolidate: 10-min wall-clock budget on the transcript backstop — quick ticks can no longer starve the nightly lock"
```

---

### Task 6: Stop-hook capture — stdin-first, loud failures

**Files:**
- Modify: `system/harness/src/hook/capture.rs` (rewrite `run`; keep `find_latest_jsonl`/`fast_path_source` + their tests)

Claude Code passes `{"session_id":..., "transcript_path":..., "hook_event_name":"Stop", ...}` on stdin. Current code ignores stdin, keys on `CLAUDE_SESSION_ID` (never set for hooks) → always falls into a newest-mtime scan that copies the *busiest concurrent session*, not the stopping one. Verified live 2026-06-11: 5 concurrent sessions, the stopping session's transcript was never captured.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn stdin_transcript_path_wins() {
    let tmp = tempfile::TempDir::new().unwrap();
    let t = tmp.path().join("abc.jsonl");
    std::fs::write(&t, b"{}").unwrap();
    let raw = format!(r#"{{"session_id":"abc","transcript_path":"{}","hook_event_name":"Stop"}}"#, t.display());
    assert_eq!(source_from_stdin(&raw), Some(t));
}

#[test]
fn stdin_rejects_missing_file_and_garbage() {
    assert_eq!(source_from_stdin(r#"{"transcript_path":"/nonexistent/x.jsonl"}"#), None);
    assert_eq!(source_from_stdin("not json"), None);
    assert_eq!(source_from_stdin(r#"{"session_id":"abc"}"#), None);
}
```

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness --lib hook::capture > /tmp/t6.log 2>&1; tail -3 /tmp/t6.log` → FAIL.

- [ ] **Step 2: Implement**

```rust
/// Stop-hook stdin payload → transcript path, validated to exist.
pub fn source_from_stdin(raw: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let p = v.get("transcript_path").and_then(|t| t.as_str())?;
    let path = PathBuf::from(p);
    path.is_file().then_some(path)
}
```

Rewrite `run()` + add `run_inner(raw, hex_dir)` (param form — testable without env races):

```rust
pub fn run() {
    let mut raw = String::new();
    use std::io::Read;
    let _ = std::io::stdin().read_to_string(&mut raw);
    let hex_dir = std::env::var("HEX_DIR")
        .ok()
        .or_else(|| std::env::var("CLAUDE_PROJECT_DIR").ok())
        .map(PathBuf::from);
    let Some(hex_dir) = hex_dir else {
        fail("HEX_DIR and CLAUDE_PROJECT_DIR both unset");
        return;
    };
    run_inner(&raw, &hex_dir);
}

fn run_inner(raw: &str, hex_dir: &Path) {
    let Some(home) = std::env::var("HOME").ok().map(PathBuf::from) else {
        fail("HOME unset");
        return;
    };
    let projects_dir = home.join(".claude/projects");
    let backup_dir = hex_dir.join("raw/transcripts");

    // Priority: stdin payload (authoritative) → env fast path → newest scan.
    let source = source_from_stdin(raw)
        .or_else(|| {
            let sid = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();
            let pd = std::env::var("CLAUDE_PROJECT_DIR").unwrap_or_default();
            (!sid.is_empty() && !pd.is_empty())
                .then(|| fast_path_source(&projects_dir, &pd, &sid))
                .filter(|p| p.is_file())
        })
        .or_else(|| {
            eprintln!("hex hook capture: no transcript_path on stdin — falling back to newest-jsonl scan (race-prone)");
            find_latest_jsonl(&projects_dir)
        });

    let Some(source) = source else {
        fail("no transcript source found (stdin, env, and scan all empty)");
        return;
    };
    let Some(basename) = source.file_name().map(|n| n.to_os_string()) else {
        fail(&format!("source has no basename: {}", source.display()));
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&backup_dir) {
        fail(&format!("create {}: {e}", backup_dir.display()));
        return;
    }
    let dest = backup_dir.join(&basename);
    match std::fs::copy(&source, &dest) {
        Ok(bytes) => {
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "hook::capture".into(),
                event: "capture".into(),
                status: "ok".into(),
                duration_ms: None,
                exit_code: Some(0),
                detail: Some(format!("{} ({bytes} bytes)", dest.display())),
            });
        }
        Err(e) => fail(&format!("copy {} -> {}: {e}", source.display(), dest.display())),
    }
}

/// Loud but never blocking: a failed backup must not disrupt the session, so
/// the hook process always exits 0 — loudness lives in stderr + telemetry.
fn fail(msg: &str) {
    eprintln!("hex hook capture: {msg}");
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "hook::capture".into(),
        event: "capture".into(),
        status: "error".into(),
        duration_ms: None,
        exit_code: Some(0),
        detail: Some(msg.to_string()),
    });
}
```

Note the inline `fs::copy` replaces the fire-and-forget `cp` spawn — a few-MB copy is <100ms and we get a real error.

- [ ] **Step 3: End-to-end test with stdin payload**

```rust
#[test]
fn run_inner_copies_stdin_transcript() {
    let tmp = tempfile::TempDir::new().unwrap();
    let hex = tmp.path().join("hex");
    std::fs::create_dir_all(&hex).unwrap();
    let t = tmp.path().join("sess.jsonl");
    std::fs::write(&t, b"line1").unwrap();
    let raw = format!(r#"{{"transcript_path":"{}"}}"#, t.display());
    run_inner(&raw, &hex);
    assert!(hex.join("raw/transcripts/sess.jsonl").is_file());
}
```

- [ ] **Step 4: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t6b.log 2>&1; tail -5 /tmp/t6b.log` → ok.

```bash
git add system/harness/src/hook/capture.rs
git commit -m "hook capture: stdin transcript_path is authoritative; inline copy; every failure path loud (stderr+telemetry)"
```

---

### Task 7: `hex backup` — the cron's missing subcommand

**Files:**
- Create: `system/harness/src/backup.rs`
- Modify: `system/harness/src/main.rs` (clap command + dispatch; grep the `Subcommand` enum for the pattern other top-level commands use)
- Modify: module list (`pub mod backup;`)

`backup.worker.rs` fires `hex backup` daily at 04:00Z — the subcommand has never existed (broken since wiring; FIX-010).

- [ ] **Step 1: Failing test**

In `backup.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_snapshots_and_prunes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        std::fs::create_dir_all(hex.join(".hex")).unwrap();
        let conn = rusqlite::Connection::open(hex.join(".hex/memory.db")).unwrap();
        conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);").unwrap();
        drop(conn);
        for i in 1..=9 {
            std::fs::create_dir_all(hex.join(format!(".hex/backups/2026-01-0{i}"))).unwrap();
        }
        assert_eq!(run(hex), 0);
        let dirs: Vec<_> = std::fs::read_dir(hex.join(".hex/backups")).unwrap().collect();
        assert_eq!(dirs.len(), KEEP_DAYS);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let snap = rusqlite::Connection::open(hex.join(format!(".hex/backups/{today}/memory.db"))).unwrap();
        let n: i64 = snap.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
```

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness --lib backup > /tmp/t7.log 2>&1; tail -3 /tmp/t7.log` → FAIL.

- [ ] **Step 2: Implement**

```rust
//! `hex backup` — daily sqlite snapshots with rotation. Invoked by the
//! hex-backup cron worker (modules/backup.worker.rs, 04:00Z) which existed
//! and fired for weeks before this subcommand did (FIX-010).

use std::path::Path;

pub const KEEP_DAYS: usize = 7;
const SOURCES: &[&str] = &[
    ".hex/memory.db",
    ".hex/telemetry/events.db",
    ".hex/ledger/ledger.db",
];

pub fn run(hex_dir: &Path) -> i32 {
    let stamp = chrono::Local::now().format("%Y-%m-%d").to_string();
    let out_dir = hex_dir.join(".hex/backups").join(&stamp);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("hex backup: create {}: {e}", out_dir.display());
        return 1;
    }
    let mut failures = 0;
    for rel in SOURCES {
        let src = hex_dir.join(rel);
        if !src.is_file() {
            println!("hex backup: {rel} absent — skipped");
            continue;
        }
        let dst = out_dir.join(src.file_name().unwrap());
        match snapshot(&src, &dst) {
            Ok(()) => println!("hex backup: {rel} -> {}", dst.display()),
            Err(e) => {
                eprintln!("hex backup: {rel} FAILED: {e}");
                failures += 1;
            }
        }
    }
    prune(&hex_dir.join(".hex/backups"));
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "backup".into(),
        event: "backup::daily".into(),
        status: if failures == 0 { "ok".into() } else { "error".into() },
        duration_ms: None,
        exit_code: Some(if failures == 0 { 0 } else { 1 }),
        detail: Some(format!("dir={} failures={failures}", out_dir.display())),
    });
    if failures == 0 { 0 } else { 1 }
}

/// Online-safe snapshot via the sqlite backup API (correct under WAL with
/// live writers — a plain fs::copy of a hot WAL db can capture a torn state).
fn snapshot(src: &Path, dst: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let src_conn = rusqlite::Connection::open_with_flags(
        src,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut dst_conn = rusqlite::Connection::open(dst)?;
    let bk = rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)?;
    bk.run_to_completion(100, std::time::Duration::from_millis(50), None)?;
    Ok(())
}

fn prune(backups_root: &Path) {
    let Ok(entries) = std::fs::read_dir(backups_root) else { return };
    let mut dirs: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    dirs.sort(); // YYYY-MM-DD names sort chronologically
    while dirs.len() > KEEP_DAYS {
        let victim = dirs.remove(0);
        match std::fs::remove_dir_all(&victim) {
            Ok(()) => println!("hex backup: pruned {}", victim.display()),
            Err(e) => eprintln!("hex backup: prune {} FAILED: {e}", victim.display()),
        }
    }
}
```

(`rusqlite::backup` requires the `backup` feature — `bundled-full` includes it; if the compiler disagrees, add `"backup"` to the rusqlite features list in `system/harness/Cargo.toml:35`.)

Wire into main.rs clap: add a `Backup` variant to the top-level command enum, mirroring how sibling commands declare + dispatch + resolve hex_dir (grep the enum and an existing dispatch arm; copy exactly).

- [ ] **Step 3: Tests + cron registry test + smoke + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t7b.log 2>&1; tail -5 /tmp/t7b.log` → ok (incl. `workers_registry_backup_is_cron_worker`).
Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo run -p hex-harness --bin hex -- backup --help > /tmp/t7c.log 2>&1; grep -ci backup /tmp/t7c.log` → ≥1.

```bash
git add system/harness/src/backup.rs system/harness/src/main.rs
git commit -m "feat: hex backup — online sqlite snapshots (memory/events/ledger) + 7-day rotation; the 04:00Z cron finally has a target"
```

---

### Task 8: Distill child pidfiles + startup reaper + drain telemetry

**Files:**
- Modify: `system/harness/src/memory/claude_cli.rs` (pidfile around spawn, lines ~129-131 + timeout path ~277-332)
- Create: `system/harness/src/reaper.rs`
- Modify: `system/harness/src/worker/runtime.rs` (reaper at serve start; drain-timeout telemetry ~lines 240-269)

Mechanism verified live: `claude -p` children get `process_group(0)` (claude_cli.rs:216-220, needed for timeout-kill), so when launchd kills the harness service's process group the grandchild survives, reparented to PID 1, and the parent-enforced timeout dies with the parent (orphan PID 14882 ran 2h+ burning tokens).

- [ ] **Step 1: Failing tests for reaper policy**

In `reaper.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_pidfile_name() {
        assert_eq!(pid_from_filename("distill-12345.pid"), Some(12345));
        assert_eq!(pid_from_filename("garbage.txt"), None);
    }
    #[test]
    fn sweep_removes_stale_pidfiles() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join(".hex/run/distill");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("distill-999999.pid"), b"").unwrap(); // dead pid
        let report = sweep(tmp.path());
        assert_eq!(report.removed_stale, 1);
        assert_eq!(report.killed, 0);
        assert!(std::fs::read_dir(&run_dir).unwrap().next().is_none());
    }
}
```

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness --lib reaper > /tmp/t8.log 2>&1; tail -3 /tmp/t8.log` → FAIL.

- [ ] **Step 2: Implement reaper.rs**

```rust
//! Startup sweep for orphaned distill children. `claude -p` extract calls run
//! in their own process group (claude_cli.rs — needed for timeout-kill), so a
//! launchd kill of the harness group orphans them to PID 1 where the
//! parent-enforced timeout no longer exists (observed: PID 14882 alive 2h+
//! after its parent died, 2026-06-11). Pidfiles make them findable; serve
//! startup reaps them.

use std::path::{Path, PathBuf};

pub struct SweepReport {
    pub killed: u32,
    pub removed_stale: u32,
}

pub fn run_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex/run/distill")
}

pub fn pid_from_filename(name: &str) -> Option<i32> {
    name.strip_prefix("distill-")?
        .strip_suffix(".pid")?
        .parse()
        .ok()
}

fn alive(pid: i32) -> bool {
    // kill(pid, 0): signal 0 probes existence without sending anything
    unsafe { libc::kill(pid, 0) == 0 }
}

fn orphaned(pid: i32) -> bool {
    // macOS: ppid via `ps`; ppid==1 means reparented to launchd
    std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i32>().ok())
        .map(|ppid| ppid == 1)
        .unwrap_or(false)
}

pub fn sweep(hex_dir: &Path) -> SweepReport {
    let mut report = SweepReport { killed: 0, removed_stale: 0 };
    let dir = run_dir(hex_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else { return report };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = pid_from_filename(&name) else { continue };
        if alive(pid) && orphaned(pid) {
            eprintln!("reaper: killing orphaned distill child pid={pid} (pgid kill)");
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
            report.killed += 1;
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "reaper".into(),
                event: "reaper::killed-orphan".into(),
                status: "ok".into(),
                duration_ms: None,
                exit_code: None,
                detail: Some(format!("pid={pid}")),
            });
        } else if !alive(pid) {
            report.removed_stale += 1;
        } else {
            continue; // alive with a live parent — in-flight, leave it alone
        }
        let _ = std::fs::remove_file(entry.path());
    }
    report
}
```

(`libc` is already used by claude_cli.rs — verify `grep -n '^libc' system/harness/Cargo.toml`; add `libc = "0.2"` if it's a transitive-only dep.)

- [ ] **Step 3: Pidfile RAII guard in claude_cli.rs**

Right after `spawn()` succeeds (claude_cli.rs:~129-131), create a guard so EVERY exit path (success, timeout-kill, error) removes the file:

```rust
struct PidfileGuard(Option<std::path::PathBuf>);

impl PidfileGuard {
    fn new(child_pid: u32) -> Self {
        let p = std::env::var("HEX_DIR").ok().map(|d| {
            let dir = std::path::Path::new(&d).join(".hex/run/distill");
            let _ = std::fs::create_dir_all(&dir);
            let p = dir.join(format!("distill-{child_pid}.pid"));
            if let Err(e) = std::fs::write(&p, b"") {
                eprintln!("claude_cli: pidfile write failed ({}): {e}", p.display());
            }
            p
        });
        PidfileGuard(p)
    }
}

impl Drop for PidfileGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}
```

Usage: `let _pidfile = PidfileGuard::new(child.id());` immediately after spawn.

- [ ] **Step 4: Wire reaper into serve + drain telemetry**

`worker/runtime.rs`: at serve startup (before the engine task spawns, near the "registered N handler(s)" print):

```rust
    if let Ok(hex_dir) = std::env::var("HEX_DIR") {
        let report = crate::reaper::sweep(std::path::Path::new(&hex_dir));
        if report.killed > 0 || report.removed_stale > 0 {
            eprintln!(
                "hex harness serve: reaper killed {} orphan(s), cleared {} stale pidfile(s)",
                report.killed, report.removed_stale
            );
        }
    }
```

And the `DrainOutcome::TimedOut(n)` arm (runtime.rs:~252-260) gets a telemetry record:

```rust
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
```

- [ ] **Step 5: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t8b.log 2>&1; tail -5 /tmp/t8b.log` → ok.

```bash
git add system/harness/src/reaper.rs system/harness/src/memory/claude_cli.rs system/harness/src/worker/runtime.rs system/harness/src/lib.rs
git commit -m "reaper: pidfile-tracked distill children; serve startup kills orphans; drain timeouts hit telemetry"
```

---

### Task 9: Vector backfill + stats surfacing + KNN floor + RSS gating

**Files:**
- Modify: `system/harness/src/memory/index.rs` (end-of-run backfill, after the per-file loop in `run_index`; reuse the embed block at :557-610)
- Modify: `system/harness/src/memory/stats.rs:53-143` (`gather` + print)
- Modify: `system/harness/src/memory/vector.rs:65-74` (distance floor)
- Modify: `system/harness/src/memory/embed.rs:33-38` (gate log_rss)

1,060 chunks (7.1%) have no vectors and stay FTS5-only forever (re-embed only fires when the file changes); stats says nothing; KNN returns confident top-k for garbage queries.

- [ ] **Step 1: Failing test (vector.rs)**

```rust
#[test]
fn distance_floor_filters() {
    let hits = vec![(1i64, 0.4f64), (2, 0.9), (3, 1.4)];
    assert_eq!(filter_by_distance(hits, KNN_MAX_DISTANCE), vec![(1, 0.4), (2, 0.9)]);
}
```

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness --lib memory::vector > /tmp/t9.log 2>&1; tail -3 /tmp/t9.log` → FAIL.

- [ ] **Step 2: KNN floor (vector.rs)**

```rust
/// vec0 FLOAT[768] MATCH distance is L2; fastembed nomic vectors are
/// normalized, so d² = 2(1-cos): d=1.0 ≈ cos 0.5, d=1.15 ≈ cos 0.34.
/// Beyond 1.15 a "neighbor" shares almost nothing with the query — garbage
/// and empty-ish queries previously returned confident top-k (assessment
/// finding: no relevance floor). Tune with HEX_KNN_MAX_DISTANCE if needed.
pub const KNN_MAX_DISTANCE: f64 = 1.15;

pub fn filter_by_distance(hits: Vec<(i64, f64)>, max: f64) -> Vec<(i64, f64)> {
    hits.into_iter().filter(|(_, d)| *d <= max).collect()
}

fn max_distance() -> f64 {
    std::env::var("HEX_KNN_MAX_DISTANCE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(KNN_MAX_DISTANCE)
}
```

Apply inside `knn()` so every caller gets the floor: `Ok(filter_by_distance(hits, max_distance()))`.

- [ ] **Step 3: Index end-of-run embedding backfill (index.rs)**

After the per-file indexing loop in `run_index` (before the final summary print):

```rust
    // Backfill: chunks whose embed failed in an earlier run stay FTS5-only
    // FOREVER unless their file changes (assessment: 1,060 chunks / 7.1%
    // invisible to semantic recall). Re-embed up to a per-run cap here.
    const BACKFILL_CAP: usize = 500;
    match backfill_missing_vectors(&conn, &embedder, BACKFILL_CAP) {
        Ok(0) => {}
        Ok(n) => println!("index: backfilled {n} missing chunk vector(s)"),
        Err(e) => eprintln!("index: vector backfill FAILED: {e}"),
    }
```

```rust
fn backfill_missing_vectors(
    conn: &rusqlite::Connection,
    embedder: &super::embed::Embedder,
    cap: usize,
) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT c.rowid, c.content FROM chunks c
         WHERE c.rowid NOT IN (SELECT rowid FROM vec_chunks)
         LIMIT ?1",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([cap as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let mut done = 0;
    for batch in rows.chunks(8) {
        let texts: Vec<&str> = batch.iter().map(|(_, c)| c.as_str()).collect();
        match embedder.embed_batch(&texts) {
            Ok(vecs) if vecs.len() == batch.len() => {
                for ((rowid, _), vec) in batch.iter().zip(vecs) {
                    super::vector::insert_vec(conn, *rowid, &vec)?;
                    done += 1;
                }
            }
            Ok(v) => eprintln!("index backfill: batch len mismatch ({} != {})", v.len(), batch.len()),
            Err(e) => eprintln!("index backfill: embed batch failed: {e}"),
        }
    }
    Ok(done)
}
```

(Verify the embedder batch method name + `insert_vec` signature against the existing per-file embed block at index.rs:557-610 and reuse EXACTLY those calls — the `embedder` is already in scope in `run_index`; if it's constructed per-file, construct once here the same way.)

- [ ] **Step 4: Stats surfacing (stats.rs)**

In `gather()`:

```rust
    let unembedded_chunks: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE rowid NOT IN (SELECT rowid FROM vec_chunks)",
        [], |r| r.get(0),
    ).unwrap_or(-1);
    let orphan_vectors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vec_chunks WHERE rowid NOT IN (SELECT rowid FROM chunks)",
        [], |r| r.get(0),
    ).unwrap_or(-1);
```

Print after "Files indexed":

```rust
    println!("Unembedded chunks: {unembedded_chunks}   (semantic recall misses these — index backfills ≤500/run)");
    println!("Orphan vectors:    {orphan_vectors}   (swept by hex memory maintain)");
```

`-1` (query failed) printing is intentional — a broken gap-query must be visible, not a silent zero. Add a stats test against a tempdir DB (follow the existing test fixtures in stats.rs/index.rs): 3 chunks, 1 vector, 1 orphan vector → expects `2` and `1`.

- [ ] **Step 5: Gate the `[rss]` log lines (embed.rs:33-38)**

```rust
pub fn log_rss(label: &str) {
    if std::env::var_os("HEX_RSS_LOG").is_none() {
        return; // diagnostic noise on every search otherwise — opt-in only
    }
    match rss_mb() {
        Some(mb) => eprintln!("[rss] {label}: {mb} MB"),
        None => eprintln!("[rss] {label}: (unavailable on this platform)"),
    }
}
```

- [ ] **Step 6: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t9b.log 2>&1; tail -5 /tmp/t9b.log` → ok.

```bash
git add system/harness/src/memory/
git commit -m "memory: index backfills missing vectors; stats reports gaps; KNN distance floor; [rss] logs opt-in"
```

---

### Task 10: `hex memory maintain` + cron reschedule

**Files:**
- Create: `system/harness/src/memory/maintain.rs`
- Modify: `system/harness/src/memory/mod.rs` (module + `MemoryCommands` clap variant — grep for the enum)
- Modify: `system/harness/src/main.rs` (dispatch arm if memory subcommands dispatch there)
- Modify: `system/harness/src/modules/memory_maintenance.worker.rs` (quick-cron offset + weekly maintain job)
- Modify: `system/harness/tests/workers_registry_test.rs` (cover the new job)

- [ ] **Step 1: Failing test**

In `maintain.rs`, write a REAL test (build the fixture with the schema helpers used in index.rs tests — no pseudo-code in the committed file): tmp DB containing (a) 1 orphan vector, (b) `transcript_files` rows `me/learnings.md` (foreign), `raw/transcripts/a.md` (offset 10), `/abs/prefix/raw/transcripts/a.md` (dupe, offset 99). After `run_maintain(&conn, false)`: orphan vectors == 0; `transcript_files` has exactly one row `raw/transcripts/a.md` with `last_offset == 99`.

Run targeted test → FAIL (module absent).

- [ ] **Step 2: Implement maintain.rs**

```rust
//! `hex memory maintain` — scheduled self-repair for memory.db.
//! Weekly cron (modules/memory_maintenance.worker.rs) + on-demand CLI.
//! One-off corruption must never be permanent: orphan vectors, FTS bloat,
//! foreign transcript_files rows, and dead pages all get swept here.

use std::path::Path;

pub fn run(hex_dir: &Path, vacuum: bool, backfill_facts: bool) -> i32 {
    let db_path = super::db_path(hex_dir);
    let conn = match super::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex memory maintain: cannot open {}: {e}", db_path.display());
            return 1;
        }
    };
    let mut failures = 0;

    // 1. Orphan vector sweep (vec rows whose chunk was deleted pre-fix).
    match conn.execute(
        "DELETE FROM vec_chunks WHERE rowid NOT IN (SELECT rowid FROM chunks)",
        [],
    ) {
        Ok(n) => println!("maintain: swept {n} orphan vector(s)"),
        Err(e) => { eprintln!("maintain: orphan sweep FAILED: {e}"); failures += 1; }
    }

    // 2. FTS5 segment optimize (assessment: ~52MB segment bloat).
    for fts in ["chunks", "facts_fts"] {
        match conn.execute(&format!("INSERT INTO {fts}({fts}) VALUES('optimize')"), []) {
            Ok(_) => println!("maintain: optimized {fts}"),
            Err(e) => { eprintln!("maintain: optimize {fts} FAILED: {e}"); failures += 1; }
        }
    }

    // 3. transcript_files hygiene: only relative raw/transcripts/*.md rows are
    //    legitimate. Foreign rows (me/*.md etc.) and absolute-path duplicates
    //    polluted the table (assessment, medium): fold dupes into the relative
    //    row keeping the furthest watermark, then purge everything foreign.
    let fold = conn.execute_batch(
        "UPDATE transcript_files AS rel
           SET last_offset = MAX(rel.last_offset,
               COALESCE((SELECT MAX(abs.last_offset) FROM transcript_files abs
                          WHERE abs.path LIKE '%/' || rel.path
                            AND abs.path != rel.path), 0))
         WHERE rel.path LIKE 'raw/transcripts/%.md';
         DELETE FROM transcript_files
          WHERE path NOT LIKE 'raw/transcripts/%.md'
             OR path LIKE '/%';",
    );
    match fold {
        Ok(()) => println!("maintain: transcript_files canonicalized"),
        Err(e) => { eprintln!("maintain: transcript_files purge FAILED: {e}"); failures += 1; }
    }

    if backfill_facts {
        match super::maintain_facts::backfill(&conn, hex_dir) {
            Ok(n) => println!("maintain: embedded {n} fact(s)"),
            Err(e) => { eprintln!("maintain: facts backfill FAILED: {e}"); failures += 1; }
        }
    }

    // 4. VACUUM last (rebuilds the file: dead vec slots + freelist reclaimed;
    //    assessment: 305MB file, ~100MB live).
    if vacuum {
        let before = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
        match conn.execute("VACUUM", []) {
            Ok(_) => {
                let after = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
                println!("maintain: VACUUM {before} -> {after} bytes");
            }
            Err(e) => { eprintln!("maintain: VACUUM FAILED: {e}"); failures += 1; }
        }
    }

    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "memory::maintain".into(),
        event: "maintain".into(),
        status: if failures == 0 { "ok".into() } else { "error".into() },
        duration_ms: None,
        exit_code: Some(if failures == 0 { 0 } else { 1 }),
        detail: Some(format!("vacuum={vacuum} backfill_facts={backfill_facts} failures={failures}")),
    });
    if failures == 0 { 0 } else { 1 }
}
```

Extract the conn-level core as `run_maintain(conn: &rusqlite::Connection, backfill_facts: bool) -> Result<(), rusqlite::Error>` so Step 1's test can drive it without a hex_dir. Until Task 11 lands, `maintain_facts::backfill` is stubbed as `Ok(0)` with a `// real impl: Task 11` marker — the `--backfill-facts` flag must parse so the cron line is stable.

Clap: add `Maintain { #[arg(long)] vacuum: bool, #[arg(long)] backfill_facts: bool }` to `MemoryCommands` + dispatch.

- [ ] **Step 3: Cron changes (memory_maintenance.worker.rs)**

```rust
/// Quick consolidation — offset from :00 so it never collides with the
/// 03:00:00Z full run (2026-06-10: full lock-skipped behind a quick tick
/// that fired the same second).
pub const CRON_CONSOLIDATE_QUICK: &str = "0 5,20,35,50 * * * * *";

/// Weekly self-repair — Sunday 04:30Z (after the 04:00Z backup).
pub const CRON_MAINTAIN: &str = "0 30 4 * * SUN *";
pub const ARGV_MAINTAIN: &[&str] = &["hex", "memory", "maintain", "--vacuum", "--backfill-facts"];

fn run_maintain(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_MAINTAIN.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}
```

Register `.on_cron(CRON_MAINTAIN, run_maintain)` in `worker()`. Check the cron dialect's DOW tokens: find an existing expression with a day-of-week value in the codebase or iii dep docs; if `SUN` isn't accepted use `0`. Update `workers_registry_test.rs` to assert the worker now has the maintain handler and (if the test framework parses crons) that all expressions parse.

- [ ] **Step 4: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t10.log 2>&1; tail -5 /tmp/t10.log` → ok.

```bash
git add system/harness/src/memory/ system/harness/src/main.rs system/harness/src/modules/memory_maintenance.worker.rs system/harness/tests/
git commit -m "feat: hex memory maintain (orphan sweep, FTS optimize, transcript_files hygiene, VACUUM) on weekly cron; quick cron offset from :00"
```

---

### Task 11: Facts semantic recall (populate facts_vec, KNN arm in recall)

**Files:**
- Create: `system/harness/src/memory/maintain_facts.rs`
- Modify: `system/harness/src/memory/recall.rs:47-117` (`facts_recall` gains vector arm)
- Modify: `system/harness/src/memory/mod.rs` (module)
- Modify: `system/harness/src/memory/maintain.rs` (un-stub `--backfill-facts`)

`facts_vec` (vec0, `fact_id TEXT PK, embedding FLOAT[768]`, schema.rs:69-74) has been dead schema since Plan 2 — 0 rows, facts recall is FTS-keyword-only. Populate it and fuse.

- [ ] **Step 1: Failing test**

In `maintain_facts.rs`: tempdir DB with 2 live facts + 1 tombstoned; after `backfill`, `SELECT COUNT(*) FROM facts_vec` == 2; re-running backfills 0 (idempotent). Build the fixture with the schema helpers. The test embeds with the real embedder (model cache at `.fastembed_cache`); if the environment lacks the cache, mark `#[ignore]` with a comment and rely on the post-deploy live verification.

- [ ] **Step 2: Implement backfill**

```rust
//! Fact embeddings: facts_vec was created by Plan 2 and never written
//! (assessment: dead schema; facts recall keyword-only). Embed the canonical
//! "subject predicate object" rendering — that's what recall queries match.

pub fn backfill(conn: &rusqlite::Connection, hex_dir: &std::path::Path) -> anyhow::Result<usize> {
    // tombstoned facts must leave the index first
    conn.execute(
        "DELETE FROM facts_vec WHERE fact_id NOT IN
            (SELECT CAST(id AS TEXT) FROM facts WHERE tombstone = 0)",
        [],
    )?;
    let mut stmt = conn.prepare(
        "SELECT f.id, f.subject || ' ' || f.predicate || ' ' || f.object
           FROM facts f
          WHERE f.tombstone = 0
            AND CAST(f.id AS TEXT) NOT IN (SELECT fact_id FROM facts_vec)",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    if rows.is_empty() {
        return Ok(0);
    }
    let embedder = super::embed::Embedder::new(hex_dir)?;
    let mut done = 0;
    for batch in rows.chunks(8) {
        let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();
        let vecs = embedder.embed_batch(&texts)?; // maintenance ctx: fail loud
        for ((id, _), vec) in batch.iter().zip(vecs) {
            // serialize the embedding EXACTLY as vector::insert_vec does for
            // vec_chunks — reuse its helper, do not write a second serializer
            super::vector::insert_fact_vec(conn, &id.to_string(), &vec)?;
            done += 1;
        }
    }
    Ok(done)
}
```

Add `insert_fact_vec(conn, fact_id: &str, vec: &[f32])` to `vector.rs` next to `insert_vec`, reusing the same blob serialization against `facts_vec(fact_id, embedding)`.

- [ ] **Step 3: Vector arm in facts_recall (recall.rs)**

The chunk path already embeds the query once — hoist that query vector so facts reuse it (no second cold-load). Change `facts_recall` to accept `query_vec: Option<&[f32]>`; when `Some`, add a KNN arm and fuse:

```rust
    let knn_ids: Vec<i64> = match query_vec {
        Some(qv) => {
            // same MATCH-query shape as vector::knn, against facts_vec;
            // same KNN_MAX_DISTANCE floor; map fact_id TEXT -> i64
            super::vector::knn_facts(conn, qv, k.max(20))
                .map(|hits| hits.into_iter().map(|(id, _)| id).collect())
                .unwrap_or_else(|e| {
                    eprintln!("facts vector arm failed: {e}");
                    vec![]
                })
        }
        None => vec![],
    };
    let fused = super::rrf::rrf_fuse(&[fts_ids, knn_ids], super::rrf::RRF_K);
    // fetch facts in fused order; keep the existing slug-boost + importance
    // tiebreak applied AFTER fusion (same post-processing as today)
```

Add `knn_facts` to vector.rs (mirror `knn`, table facts_vec, parse fact_id to i64, apply `filter_by_distance`). Follow the chunk-side fusion at search.rs:348-376 as the reference. `None` ⇒ exactly today's FTS-only behavior (callers without an embedder lose nothing).

- [ ] **Step 4: Un-stub maintain** — replace Task 10's `Ok(0)` stub with the real `maintain_facts::backfill` call.

- [ ] **Step 5: Tests + commit**

Run: `export PATH="/opt/homebrew/bin:$PATH" && cargo test -p hex-harness > /tmp/t11.log 2>&1; tail -5 /tmp/t11.log` → ok.

```bash
git add system/harness/src/memory/
git commit -m "feat: facts semantic recall — facts_vec populated via maintain backfill; recall fuses FTS + KNN arms (RRF)"
```

---

### Task 12: CHANGELOG

**Files:**
- Modify: `CHANGELOG.md` (top, follow the existing entry format)

- [ ] **Step 1:** Add one consolidated entry under an Unreleased/next-version heading listing: findings≠failure exit semantics; full-consolidate lock wait + alerts; quick-tick budget + cron offset; nightly-full-liveness doctor check + 48h audit window; deduped alert pathway; stdin-first Stop-hook capture; distill-child reaper; drain-timeout telemetry; `hex backup`; vector backfill + stats gaps + KNN floor; `hex memory maintain` weekly; facts semantic recall. Reference: mrap-hex assessment 2026-06-11 / FIX-007…FIX-011.

- [ ] **Step 2:** `git add CHANGELOG.md && git commit -m "changelog: memory pipeline holistic fix"`

Do NOT bump `Cargo.toml` version or create tags — the release pipeline owns versioning.

---

## Appendix: mrap-hex local operations (NOT part of the foundation work — run after `/hex-upgrade` deploys this)

These are data operations on the live instance; BOI workers must not touch `~/hex`.

1. **Deploy:** `/hex-upgrade` (or `hex upgrade --local`) once the foundation work merges; verify `hex memory maintain --help` and `hex backup --help` exist post-deploy (decoy-binary check: the deployed binary builds from repo-root `target/release/hex`, never `system/harness/target/`).
2. **Orphan:** the reaper kills PID 14882 on first harness restart — verify with `ps -p 14882` (expect gone). It has no pidfile (predates them) — if still alive, kill manually: `kill -9 14882`.
3. **Stray DB:** `rm /Users/mrap/hex/.hex/memory/memory.db` after confirming `stat -f%z` reports 0 bytes.
4. **One-time reclaim:** `hex memory maintain --vacuum --backfill-facts` (expect ~305MB → ~100-150MB; ~649 facts embedded; orphan vectors swept).
5. **Watermark rewind verification (data recovery for the ~480 poison-skipped slices):** old skip events lack `offset=` (Task 1 adds it going forward) — reconstruct per-file: for each path in `SELECT detail FROM events WHERE event='distill::slice' AND status='skipped'` (telemetry events.db), range-chain `bytes=` of prior ok/skipped events from offset 0 to compute where skipping began; if that offset < current `transcript_files.last_offset`, the span was never re-extracted → `UPDATE transcript_files SET last_offset = <first_skip_offset>, consecutive_failures = 0 WHERE path = '<path>'`. Dedup/judge absorbs re-extraction (verified in the 2026-06-10 fix; ≈ $0.15/96MB on deepseek). Cross-check against the Jun-10 rewind — only rewind files it missed.
6. **Stop-hook smoke test:** end a session turn, then `ls -la raw/transcripts/<this-session-id>.jsonl` — mtime seconds old; `sqlite3 .hex/telemetry/events.db "SELECT * FROM events WHERE source='hook::capture' ORDER BY id DESC LIMIT 3"` shows ok rows.
7. **A week later:** `hex memory stats` unembedded/orphan lines ≈ 0; `.hex/backups/` has 7 dated dirs; Sunday maintain telemetry row ok.

## Self-review notes

- Spec coverage: FIX-007 → Tasks 1-5, 8; FIX-008 → Tasks 9, 11; FIX-009 → Task 10 + local op 4; FIX-010 → Task 7; FIX-011 → Task 6; assessment mediums (transcript_files pollution, RSS noise, KNN floor, facts_vec, stray DB, orphan claude -p, drain silence) → Tasks 8-11 + local ops.
- Known intentional read-instructions (not TBDs): Task 4 Step 4 (copy reflection_liveness.rs body — the CheckResult constructors live there), Task 9 Step 3 / Task 11 Step 2 (reuse the exact embed/insert_vec call shapes from index.rs:557-610). Workers MUST read the named file before writing.
- Type consistency: `TelemetryEvent` field shapes verified once (Task 3 Step 3), reused identically in Tasks 4-10; `Mode` variants verified Task 3 Step 1; `filter_by_distance`/`KNN_MAX_DISTANCE` defined Task 9, reused Task 11.
