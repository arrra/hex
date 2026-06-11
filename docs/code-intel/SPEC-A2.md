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

| Knob | Default | Behavior |
|---|---|---|
| `pool_cap` | from smoke #3 (2 if unresolved) | LRU evict (SIGTERM→SIGKILL) on overflow; eviction logged + visible in `status` |
| `idle_ttl_secs` | 1800 | reaper kills instances idle past TTL |
| `vanish_reap` | always on | instance whose worktree path no longer exists is killed immediately |
| `mem_limit_mb` | from smoke #3 (6144 if unresolved) | watchdog polls RSS every 30s; over limit → kill + log + `status` red note; NEXT query respawns |
| `max_warm_wait` | n/a | cq never blocks on warming; there is no wait knob by design |

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
| A2-S4 | **Rename:** `cq rename` of a fixture fn updates def + all call sites via `--apply`; content-mismatch abort proven (corrupt one site first → exit 7, zero files modified) | golden_live + unit |
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
