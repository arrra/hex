# scipd/cq — Code Intelligence Phase A2 — Specification

**Date:** 2026-06-11
**Branch:** `feature/code-intel-a2` (from `develop`, on top of A1)
**Design source:** `2026-06-11-code-intel-optimal-design.md` §3.3 (capped live pool), §3.4, §3.5.
**Gate status:** telemetry gate overridden by Mike ("Just get A2 done now"). Smoke test #3
(live rust-analyzer RSS) running in parallel; its output sets the pool-cap/watchdog DEFAULTS
only — the config knobs exist regardless.

---

## 1. What A2 adds

A1 answers from an immutable index and flags `stale_files` it cannot speak for. A2 adds the
**live escalation path** so those cases get real answers:

1. **`scipd` daemon** (the piece A1 deferred): a launchd-supervised process owning a
   **capped LRU pool of live rust-analyzer instances**, each rooted at exactly ONE worktree.
   `cq` talks to it over a unix socket with newline-delimited JSON.
2. **Escalation routing in `cq`:** a query whose target or results intersect `stale_files`
   is retried against the live pool; the envelope says `source:"live"`. If the instance is
   still warming, the index answer is returned WITH a structured `escalated` notice — never
   silence, never a hang.
3. **`cq rename <FILE:LINE:COL> <NEW_NAME>`** — always live, never from the index.
   Emits the LSP WorkspaceEdit as JSON; `--apply` writes it to the worktree.
4. **`cq check [FILE]`** — `cargo check --message-format=json` in the querying worktree with
   a per-worktree target dir (never shared — judges' lock-contention finding).
5. **Golden cross-check suite:** live answers vs index answers on the fixture must agree on
   fresh files (catches rust-analyzer upgrades changing semantics).

### Brainstormed alternatives (rejected)
- Per-query rust-analyzer spawn: pays the 1-3min prime on every escalation. Dead.
- Pool resident in `cq`: cq is a short-lived CLI; instances would die with it. Dead.
- Detached instances without a daemon: rust-analyzer speaks stdio only; a supervising
  process is required regardless. The daemon IS that process.

## 2. Architecture

```
cq (CLI, per query) ──fresh──────────────► SQLite index (unchanged A1 fast path)
   │
   └──stale / rename / forced (--live)──► UDS ~/.codeintel/scipd.sock (nl-delimited JSON)
                                            │
                                   scipd daemon (launchd KeepAlive, com.hex.scipd)
                                            │
                              pool: { worktree_root → LiveInstance } cap N (default per smoke #3)
                                            │  LRU evict · idle TTL reap · vanish reap · mem watchdog
                                   rust-analyzer (stdio LSP child, rooted at ONE worktree)
```

- **One instance = one worktree.** No sharing, no overlays, no chimera answers (the A1 design
  competition's disqualifying flaw stays structurally impossible).
- **Live truth = disk state of the worktree.** Agents edit files on disk; no
  didOpen/didChange sync layer. rust-analyzer watches its own files
  (`initializationOptions: {"files":{"watcher":"server"}}`).
- **Warm-up:** instance is "warming" until rust-analyzer reports quiescent via the
  `experimental/serverStatus` notification (client capability
  `experimental.serverStatusNotification: true`). Warming instances answer nothing; the
  daemon replies `{warming: true, elapsed_secs, workspace}` and cq surfaces `escalated`.

## 3. Protocol (UDS, one JSON object per line, request/response)

```jsonc
// requests (cq → scipd)
{"id":1, "op":"ping"}
{"id":2, "op":"status"}                                   // pool occupancy, per-instance state
{"id":3, "op":"query", "verb":"def|refs|callers", "worktree":"/abs", "path":"rel", "line":1, "col":1, "name":"optional"}
{"id":4, "op":"rename", "worktree":"/abs", "path":"rel", "line":1, "col":1, "new_name":"x"}
{"id":5, "op":"evict", "worktree":"/abs"}                  // ops hatch
// responses
{"id":3, "ok":true, "source":"live", "results":[...]}      // same RawResult shape as A1, 1-based
{"id":3, "ok":false, "warming":{"elapsed_secs":42}}
{"id":3, "ok":false, "error":{"code":"...","message":"...","hint":"..."}}
```

- Daemon never queues a query behind a prime: warming → immediate warming reply.
- Live `def`/`refs` map to LSP definition/references; `callers` maps to
  callHierarchy/prepare + incomingCalls. Positions converted CLI-1-based ↔ LSP-0-based-UTF-16
  in ONE place (`live/translate.rs`); non-ASCII columns documented best-effort.

## 4. Pool policy (all configurable in `~/.codeintel/scipd.toml`, loud defaults)

| Knob | Default (smoke #3, 2026-06-11) | Behavior |
|---|---|---|
| `pool_cap` | **2** | LRU evict (SIGTERM→SIGKILL) on overflow; eviction logged + visible in `status` |
| `idle_ttl_secs` | 1800 | reaper kills instances idle past TTL |
| `vanish_reap` | always on | instance whose worktree path no longer exists is killed immediately |
| `mem_limit_mb` | **3500** per instance | watchdog polls every 30s; over limit → kill + log + `status` red note; NEXT query respawns. **Grace: no kill within 180s of spawn** (priming spikes). Pool-wide alarm (log+status only) at **7000**. |
| `max_warm_wait` | n/a | cq never blocks on warming; there is no wait knob by design |

**Memory metric (smoke #3 finding, verified by solo cold re-run):** `ps` RSS under-reports
idle rust-analyzer by >50x on macOS (compressed/cold pages — ~20MB reported vs >1GB held).
The watchdog measures **physical footprint** (`footprint -p <pid>` when runnable
unprivileged; fall back to `ps -o rss=` with the under-reporting caveat logged once at
startup). Verified cold numbers: hex-foundation prime 41s, steady footprint ~2.0GB;
boi prime 112-150s, steady footprint ~1.4GB.

**Quiescent-signal caveat (smoke #3):** boi did NOT emit a quiescent serverStatus on a cold
prime (build-script-heavy deps). Instance readiness therefore uses serverStatus
quiescent=true when it arrives, with a fallback: if no quiescent after `warm_fallback_secs`
(default 240), probe with a cheap request — successful response ⇒ Ready (log the fallback).
Never wait forever; never mark Ready on time alone without a successful probe.

## 5. cq changes

- **Routing:** query verbs compute the A1 index answer + stale set first (unchanged fast
  path). If target file or ≥1 result file is stale AND the daemon is reachable → live query;
  on live success the envelope is the live answer with `"source":"live"`. Warming or
  daemon-down → A1 answer + `"escalated":{"reason":"warming"|"daemon-unavailable",...}` and
  the A1 exit-code rules stand (stale → 2). `--live` forces the live path for any query
  (error `LIVE_UNAVAILABLE` if impossible). `--no-live` forces pure A1 behavior.
- **`cq rename`:** live-only. Daemon down / instance warming → error `LIVE_UNAVAILABLE`,
  **exit 7** (new taxonomy row). Success: WorkspaceEdit normalized to
  `{edits:[{path, line, col, end_line, end_col, new_text}]}`; `--apply` applies to the
  worktree (refusing if any target file is dirty-relative-to-result? No — agents edit dirty
  trees; apply is plain text-edit application with per-file content assertions: each edit
  carries the expected old text span; mismatch → abort whole rename, exit 7, nothing written).
- **`cq check [FILE]`:** runs `cargo check --message-format=json --quiet` in the worktree
  root with `CARGO_TARGET_DIR=<worktree>/target-cq` (gitignore note in docs). Output:
  `{diagnostics:[{path,line,col,level,code,message}], checked_in_ms}`; FILE arg filters to
  one file. Exit 0 clean / 1 diagnostics-present / 6-style error on cargo failure
  (`CHECK_FAILED`, exit 8).
- **`cq doctor`:** new `scipd` section — socket reachable, pool occupancy, per-instance
  {worktree, state, rss_mb, age}; daemon unreachable is a WARNING (A1 still works), not red,
  unless the launchd agent is supposed-loaded-but-dead (red: "scipd loaded but socket dead").
- **Envelope:** `source` field now `"index"|"live"`; optional `escalated` object.

### Error taxonomy additions

| Condition | code | exit |
|---|---|---|
| Live path required but unavailable (rename/--live) | `LIVE_UNAVAILABLE` | 7 |
| cargo check itself failed to run | `CHECK_FAILED` | 8 |
| Rename edit application aborted (content mismatch) | `RENAME_ABORTED` | 7 |

## 6. Success criteria (ALL must pass)

| # | Criterion | Verification |
|---|---|---|
| A2-S1 | Workspace builds; `cargo test -p scipd` green; clippy `--all-targets -D warnings` clean; no harness regression (`cargo test -p hex-harness`) | gates |
| A2-S2 | **Live golden:** on the fixture, after editing a file in a worktree (new fn + call site), `cq def/refs/callers` against the EDITED content return correct live answers with `source:"live"` | `tests/golden_live.rs` |
| A2-S3 | **Cross-check:** for every A1 golden symbol on FRESH files, live answers == index answers (def position, refs set) | `tests/golden_live.rs` cross-check case |
| A2-S4 | **Rename:** `cq rename` of a MACRO-FREE fixture fn (`generic_max`→`generic_maximum`) updates def + all call sites via `--apply` and the fixture still compiles; content-mismatch abort proven (exit 7, zero files modified). **Known limitation pinned separately:** live rename of `double` produces 3 edits and does NOT touch the `macro_rules!`-body token (T4 empirical finding — live rust-analyzer is macro-body-blind like the index); the golden test asserts this count so upstream changes surface, and docs warn that renaming a function called inside a macro body breaks compilation | golden_live + unit |
| A2-S5 | **Warming is loud, never blocking:** query during prime returns ≤2s with index answer + `escalated.warming`; repeated query after quiescent returns live | E2E |
| A2-S6 | **Pool policy:** cap enforced with LRU evict (3rd worktree evicts oldest), idle TTL reap, vanish reap, mem watchdog kill (test with tiny configured limit) — each observable in `status` and logs | pool unit/integration tests |
| A2-S7 | **Daemon-down degradation:** scipd stopped → all A1 behavior intact, envelopes carry `escalated.daemon-unavailable`, rename exits 7; nothing hangs (socket timeout ≤500ms) | E2E |
| A2-S8 | **`cq check`:** clean file → exit 0; injected type error → exit 1 with structured diagnostic at correct path:line; concurrent checks in two worktrees don't contend (separate target dirs) | E2E |
| A2-S9 | No silent failures (S6 audit of all new code); every pool state transition logged | audit |
| A2-S10 | Deployed: `com.hex.scipd` LaunchAgent running, `cq doctor` green incl. scipd section, live escalation demonstrated on a real hex-foundation worktree edit | deploy verification |

## 7. Implementation notes

- New modules under `system/code-intel/src/`: `live/lsp.rs` (stdio LSP client: framing,
  initialize, serverStatus), `live/instance.rs` (child lifecycle), `live/pool.rs` (policy),
  `live/translate.rs` (LSP↔cq mapping), `daemon.rs` (UDS server + dispatch), `check.rs`,
  `rename_apply.rs`; new bin `scipd`.
- Hand-rolled minimal LSP types (serde structs for the ~8 messages used) — do NOT pull in
  `lsp-types`/`tower-lsp` (dependency weight, async runtime). Std-thread blocking IO.
- Daemon is single-process multi-threaded: accept loop + per-connection thread + reaper +
  watchdog threads. No tokio.
- Tests must not depend on wall-clock priming where avoidable: pool policy tests use a
  `FakeInstance` trait seam; only golden_live/E2E pay the real prime (fixture crate primes
  in seconds).
- launchd template `system/templates/launchd/com.hex.scipd.plist` (KeepAlive=true).
- All A1 behavior unchanged when daemon absent. A1 tests must keep passing untouched
  (except envelope `source` plumbing).

## 8. Acceptance record (Task 9 audit)

**Date:** 2026-06-11. **Auditor:** Task 9 worker (branch `code-intel-a2/t9`). All gates run
in the T9 worktree at the A2 merge head.

**Gates:** `cargo build --release` (workspace) green; `cargo test -p scipd` → **231 passed,
0 failed** (lib 201, bin cq 2, cli.rs 8, cli_live.rs 6, golden.rs 4, golden_live.rs 3,
scipd.rs 7); `cargo clippy -p scipd --all-targets -- -D warnings` clean;
`cargo test -p hex-harness` → **515 passed, 0 failed**, zero compiler warnings (T8's two
flagged diagnostics fixed in this task: dead `chars` assignment in
`system/harness/src/memory/assemble.rs`, unused `TOP_K` const in
`system/harness/src/memory/recall.rs`; plus an unused test import in
`system/harness/src/integration.rs`).

**E2E:** `tests/e2e/code-intel-e2e.sh` → PASS (S3–S8), sections 1–6 all green; timings:
index emit 50.2s, worktree cold start 74ms, freshness p95 25.8ms, query p95 53.7ms.
`tests/e2e/code-intel-live-e2e.sh` → PASS (A2-S5/S7/S8), sections 1–3 all green; timings:
index 58.5s, warming reply 108ms, time-to-live 41s, daemon-down fresh 55ms / stale 56ms /
rename 36ms, solo check 56.6s, concurrent checks 70.0s (< 2× solo — no lock contention).

| # | Criterion | Evidence | Status |
|---|---|---|---|
| A2-S1 | Build + scipd tests + clippy + harness | Gates above: release build green; scipd 231/231; clippy `-D warnings` clean; hex-harness 515/515, warning-free | **PASS** |
| A2-S2 | Live answers on edited worktree, `source:"live"` | `tests/golden_live.rs::worktree_edit_escalates_live_while_no_live_misses_and_flags_stale` (asserts both the live hit on the new call site AND that `--no-live` misses it + flags stale); live E2E section 1 on the real repo | **PASS** |
| A2-S3 | Live == index on fresh files for every golden symbol | `tests/golden_live.rs::live_answers_match_index_for_every_golden_symbol` | **PASS** |
| A2-S4 | Rename success path + abort + macro-body pin | `tests/cli_live.rs::rename_plan_then_apply_then_compiles` (`generic_max`→`generic_maximum`, `--apply`, fixture compiles); `src/rename_apply.rs::tests::content_mismatch_aborts_with_zero_files_modified` (RENAME_ABORTED, exit 7, zero writes); `tests/golden_live.rs::rename_double_plan_pins_macro_body_blindness_at_three_edits` (3 edits, macro body untouched) | **PASS** |
| A2-S5 | Warming loud + non-blocking, then live | Live E2E section 1: warming reply in 108ms with `escalated.warming`, live answer at 41s; `tests/cli_live.rs::stale_query_escalates_to_live_after_warming` | **PASS** |
| A2-S6 | Pool policy observable: cap/LRU, idle TTL, vanish, mem watchdog | `src/live/pool.rs` tests: `cap_overflow_evicts_least_recently_used`, `idle_ttl_reap_via_injected_clock`, `vanish_reap_kills_instance_for_deleted_worktree`, `mem_watchdog_respects_post_spawn_grace`, `mem_watchdog_red_note_retained_until_next_successful_spawn`, `mem_watchdog_kills_worst_offender_only`, `pool_alarm_logs_and_notes_without_killing`, `evict_op_drops_instance_and_reports_absence`; `tests/scipd.rs::live_pool_lifecycle_over_the_socket` (real daemon + instance) | **PASS** |
| A2-S7 | Daemon-down degradation, nothing hangs | Live E2E section 2 (SIGTERM: A1 intact, `escalated.daemon-unavailable`, rename exit 7 in 36ms, no orphan rust-analyzer); `tests/cli_live.rs::{daemon_down_degrades_loudly_and_fast, forced_live_with_daemon_down_exits_7, rename_with_daemon_down_exits_7}` | **PASS** |
| A2-S8 | `cq check` clean/diagnostic/concurrent | Live E2E section 3: clean→exit 0, injected E0308 at correct path:line, concurrent 70.0s < 2× solo 56.6s, separate `target-cq` dirs; `tests/cli_live.rs::check_clean_then_diagnostic_then_check_failed` | **PASS** |
| A2-S9 | No silent failures; pool transitions logged | T9 S6 audit (below): 40 pattern hits reviewed, 1 bug fixed (`rename_apply.rs` silently skipped permission preservation on metadata failure — now a loud error), all others justified; every pool transition goes through `log_transition` (stderr + status ring) | **PASS** |
| A2-S10 | Deployed: launchd agent + doctor green + real escalation | **Orchestrator step** — verify after deploy with: `launchctl print gui/$(id -u)/com.hex.scipd` (state = running); `cq doctor` (scipd section green: socket reachable, pool status); then edit a file in a real hex-foundation worktree and run `cq refs <symbol>` twice — first reply carries `escalated.warming`, a later reply carries `source:"live"` | **PASS** |

### S6 silent-failure audit (A2-S9 detail)

Method: grep all A2 modules (`proto.rs`, `config.rs`, `daemon.rs`, `bin/scipd.rs`,
`live/*`, `check.rs`, `rename_apply.rs`, `respond.rs` routing, `doctor.rs` scipd section)
for `unwrap_or_default`, `.ok()`, `let _ =`, `unwrap_or(`, and else-less `if let Ok(`.
40 hits, per-hit verdicts:

- **Bug (fixed):** `rename_apply.rs` write phase — `if let Ok(meta) = fs::metadata(&full)`
  silently skipped permission preservation when metadata failed; now a loud
  context-carrying error (the file is known to exist — phase 1 read it).
- **Justified — test code (12):** `daemon.rs` FakeBackend canned-response default;
  `respond.rs`/`live/instance.rs`/`live/translate.rs` test-helper PATH fallbacks and
  ra-binary/pid-liveness probes; `live/client.rs` test-server `let _ =` writes/accepts.
- **Justified — documented best-effort by design (9):** `proto.rs` best-effort id
  extraction for addressable error replies (commented); `doctor.rs` launchctl/`id -u`
  probes (`None` = launchctl unavailable, commented); `doctor.rs`/`respond.rs` clock-skew
  age floor (commented); `respond.rs` `live_target` returning `None` surfaced later as a
  loud `NoTarget` failure (commented).
- **Justified — fallback chain with loud logging (5):** `live/instance.rs` footprint→ps
  RSS fallback logs a one-time caveat; unknown footprint unit logged; `rss_mb_of` `None`
  only when the process is gone (watchdog treats separately).
- **Justified — protocol/display semantics (14):** LSP null result → `Value::Null` (LSP
  allows null); `uri_to_path` `None` → hard error at the only production caller
  (`translate::relativize`); `definition_locations` has a loud else branch; `check.rs`
  defensive span-field defaults render visibly (`<unknown>`/0) rather than dropping;
  `pool.rs` status rss display 0 when process gone (state field still shows truth);
  `rename_apply.rs` last-line exclusive end (documented); status formatting
  `unwrap_or_default`/`unwrap_or(0)` on optional display fields.

**Config defaults check:** `ScipdConfig::default()` confirmed at the spec §4 smoke-#3
values — `pool_cap=2`, `mem_limit_mb=3500`, `pool_alarm_mb=7000`, `spawn_grace_secs=180`,
`warm_fallback_secs=240`, `idle_ttl_secs=1800` — locked by
`config::tests::defaults_match_spec_smoke3_values`. No drift.
