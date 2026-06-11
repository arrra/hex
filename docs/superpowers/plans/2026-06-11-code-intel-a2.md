# scipd/cq Code Intelligence A2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the live escalation path to A1 — `scipd` daemon with a capped pool of worktree-rooted rust-analyzer instances, cq routing for stale/rename/check, per `docs/code-intel/SPEC-A2.md`.

**Architecture:** New `scipd` bin + `live/` modules in the existing `scipd` crate. UDS newline-JSON protocol. Hand-rolled minimal LSP over stdio (no lsp-types, no tokio — std threads, blocking IO). Pool policy tested through a `LiveBackend` trait seam with a fake; only golden/E2E tests pay a real rust-analyzer prime (the fixture primes in seconds).

**Read first:** `docs/code-intel/SPEC-A2.md` (THE contract — protocol §3, pool policy §4, cq changes §5, success criteria §6). `docs/code-intel/SPEC-A1.md` for the A1 invariants you must not break. When plan and spec disagree, the spec wins.

**Worker protocol (worktree-per-worker, mandatory):** identical to A1's plan: worktree `/tmp/ci-a2-t<N>`, branch `code-intel-a2/t<N>` from `feature/code-intel-a2`, commit there, orchestrator merges. Never touch the primary checkout, `feature/code-intel-a2`, `develop`, or `main`. Gates before reporting: `cargo test -p scipd` green AND `cargo clippy -p scipd --all-targets -- -D warnings` clean.

---

### Task 1: Protocol types, config, daemon skeleton, error taxonomy additions

**Files:** Create `src/daemon.rs`, `src/bin/scipd.rs`, `src/proto.rs`, `src/config.rs`. Modify `src/error.rs`, `src/lib.rs`.

- [ ] TDD `src/proto.rs`: serde types for every request/response in spec §3 (round-trip tests; unknown `op` deserializes to an error reply, never a panic).
- [ ] TDD `src/config.rs`: `ScipdConfig` from `~/.codeintel/scipd.toml` (env override `CODEINTEL_HOME`); defaults per spec §4 (pool_cap=2, idle_ttl_secs=1800, mem_limit_mb=6144 — placeholders; orchestrator will set smoke-#3 values before T9); missing file → defaults; malformed file → loud error (never default-on-parse-failure).
- [ ] TDD `src/error.rs` additions: `LiveUnavailable`→7, `CheckFailed`→8, `RenameAborted`→7, with hints.
- [ ] `src/daemon.rs` + `src/bin/scipd.rs`: UDS listener at `<home>/scipd.sock` (unlink stale socket on bind iff no live daemon — try-connect first; second daemon must refuse loudly), accept loop, per-connection thread, newline-JSON dispatch. Implement `ping` and a stub `status` (empty pool). Socket read/write timeouts 500ms-5s. Integration test: spawn the real `scipd` bin with tempdir home, ping over the socket, second-instance refusal, clean shutdown on SIGTERM.
- [ ] Gates + commit.

### Task 2: LSP client + live instance lifecycle

**Files:** Create `src/live/mod.rs`, `src/live/lsp.rs`, `src/live/instance.rs`. Modify `src/lib.rs`.

- [ ] `live/lsp.rs`: Content-Length framing reader/writer over child stdio (TDD framing against an in-process pipe with canned bytes, incl. split reads); request/notification structs ONLY for: initialize, initialized, shutdown, exit, textDocument/definition, textDocument/references, textDocument/prepareRename+rename, callHierarchy/prepare + callHierarchyItem/incomingCalls, experimental/serverStatus. Client capability `experimental.serverStatusNotification:true`; initializationOptions `{"files":{"watcher":"server"}}`.
- [ ] `live/instance.rs`: `LiveInstance::spawn(worktree_root)` → child rust-analyzer, background reader thread routing responses by id + tracking `quiescent` from serverStatus; states Warming/Ready/Dead; `request(...)` returns `Err(Warming{elapsed})` until quiescent; `shutdown()` graceful (shutdown→exit→wait 2s→SIGKILL); RSS via `ps -o rss= -p`. Define `trait LiveBackend` (spawn/state/request/shutdown/rss/last_used) implemented by `LiveInstance` — the pool (T3) tests against a fake of this trait.
- [ ] Integration test (real rust-analyzer, golden fixture copied to tempdir + git init): spawn → reaches Ready (give it 120s budget) → definition request on `double` call site returns the def position → shutdown clean. Mark `#[ignore]`-NOT; it runs in normal `cargo test` (fixture primes in seconds; A1 already runs real-emit tests).
- [ ] Gates + commit.

### Task 3: Pool policy

**Files:** Create `src/live/pool.rs`.

- [ ] TDD against a `FakeBackend` (controllable state/rss/spawn-count): `get_or_spawn` keyed by canonical worktree root; cap enforcement with LRU evict (spawn 3rd with cap 2 → least-recently-USED is shut down, logged, visible in `PoolStatus`); idle-TTL reap (inject clock or use small ttl + sleep <1s granularity — prefer an injected `now` closure); vanish reap (worktree dir deleted → instance killed on next sweep); memory watchdog (fake rss above limit → kill + red note in status retained until next successful spawn).
- [ ] `sweep()` is called by daemon-owned reaper thread (wire in T5) — pool itself stays synchronous & lock-protected (`Mutex<HashMap<...>>`); every transition (spawn/evict/reap/kill) goes through one `log_transition` fn that writes to stderr + an in-memory ring visible via `status`.
- [ ] Gates + commit.

### Task 4: LSP↔cq translation

**Files:** Create `src/live/translate.rs`.

- [ ] TDD (real instance on the golden fixture, reuse T2's harness): `live_def/live_refs/live_callers(instance, path, line, col)` — input cq-1-based, convert to LSP-0-based-UTF-16 in ONE function pair (`to_lsp_pos`/`from_lsp_pos`, unit-tested incl. multibyte line best-effort note); outputs the A1 `RawResult`-compatible shape (path relative to worktree root, 1-based). `live_rename(instance, path, line, col, new_name)` → normalized edit list per spec §5, each edit carrying `old_text` extracted from the CURRENT file content at the edit range (this is what makes apply-time content assertions possible).
- [ ] Test live answers against known fixture truths: def of `double` from its lib.rs call site; refs set; callers of `double` via callHierarchy ⊇ {top_level_fn, fmt_user} (record whether callHierarchy ALSO finds `macro_caller` — live rust-analyzer may see through macro_rules; assert with a comment either way, don't assume).
- [ ] Gates + commit.

### Task 5: Daemon dispatch — pool + live queries over UDS

**Files:** Modify `src/daemon.rs`, `src/bin/scipd.rs`.

- [ ] Wire real pool into the daemon: `query`/`rename` ops resolve worktree → `pool.get_or_spawn` → Warming ⇒ `{ok:false,warming:{...}}` immediately (spawn happens, reply doesn't wait); Ready ⇒ translate call ⇒ results. `status` returns real `PoolStatus`. `evict` works. Reaper thread (sweep every 30s) + watchdog started by the bin.
- [ ] Integration tests (real scipd bin + real instance on fixture): query-while-warming returns warming ≤2s; re-query after ready returns live def; evict then status shows empty; SIGTERM shuts down children (no orphan rust-analyzer — assert by pid liveness).
- [ ] Gates + commit.

### Task 6: cq routing, rename, check, doctor

**Files:** Modify `src/bin/cq.rs`, `src/respond.rs`, `src/envelope.rs`, `src/doctor.rs`. Create `src/live/client.rs` (UDS client, 500ms connect timeout), `src/check.rs`, `src/rename_apply.rs`.

- [ ] Routing per spec §5 (TDD via real-binary tests like A1's `tests/cli.rs`): stale-intersecting query + daemon up+ready → `source:"live"` answer; daemon warming → index answer + `escalated.warming` + exit per A1 rules; daemon down → index answer + `escalated.daemon-unavailable` (≤500ms added latency, test with a timing bound ~2s); `--live` forces (down ⇒ LIVE_UNAVAILABLE exit 7); `--no-live` never touches the socket.
- [ ] `cq rename FILE:LINE:COL NEW_NAME [--apply]`: spec §5 exactly. Without `--apply`: print edits JSON, exit 0. With: content-assert each edit's `old_text`, all-or-nothing application (write to temp + rename per file AFTER all assertions pass), mismatch ⇒ RENAME_ABORTED exit 7 nothing written (test: corrupt one call site between plan and apply).
- [ ] `cq check [FILE]` per spec §5: `CARGO_TARGET_DIR=<worktree>/target-cq`; parse cargo JSON messages to the normalized diagnostic shape; exits 0/1/8. Tests: clean fixture → 0; fixture with injected type error → 1 + correct path:line; cargo absent from PATH (shim) → 8 loud.
- [ ] `cq doctor`: scipd section per spec §5 (socket ping + status passthrough; launchd-loaded-but-dead detection via `launchctl print gui/$UID/com.hex.scipd` presence vs socket failure — best-effort, absent launchctl in tests is fine: cover with unit on the classification fn).
- [ ] Envelope: `source` plumbed (`"index"` default), optional `escalated`. A1 tests stay green unmodified except where they assert the envelope field-set strictly.
- [ ] Gates + commit.

### Task 7: Golden live suite (A2-S2/S3/S4)

**Files:** Create `tests/golden_live.rs`.

- [ ] Full-stack tests with real scipd + cq binaries (`CARGO_BIN_EXE_*`), hermetic home, golden fixture repo + a worktree of it:
  1. **S3 cross-check:** index fresh, for every A1 golden symbol: `cq def/refs --live` == plain `cq def/refs` (positions + sets equal; envelope sources differ).
  2. **S2 live-on-stale:** in the worktree append `pub fn brand_new() -> i32 { double(7) }` to ops.rs; `cq refs double` (auto-escalates) returns the NEW call site with `source:"live"`; plain `--no-live` flags ops.rs stale and misses it (asserting BOTH sides pins the value of escalation).
  3. **S4 rename:** `cq rename` `double`→`twice` with `--apply` in the worktree: def + all call sites updated (`cargo check` the fixture after — still compiles); abort case: re-setup, hand-edit one call site after computing the plan (use `cq rename` without apply, corrupt, then apply the SAME printed plan via a second invocation? — design the test honestly: simplest is `--apply` with a pre-corrupted `old_text` via direct file edit between two cq calls is racy; instead test the apply engine's unit seam in rename_apply.rs for the abort, and E2E only the happy path).
- [ ] Gates + commit.

### Task 8: E2E extension, launchd, docs (A2-S5/S7/S8)

**Files:** Create `tests/e2e/code-intel-live-e2e.sh`, `system/templates/launchd/com.hex.scipd.plist`. Modify `docs/code-intel.md`, `AGENTS.md`.

- [ ] E2E script (same conventions/hermeticity as A1's): real repo clone, start scipd manually (not launchd) with hermetic home; sections: (S5) edit file in worktree → immediate query ≤2s with warming escalation → poll until live answer (budget 180s) → assert correctness; (S7) kill scipd → full A1 behavior + daemon-unavailable escalation + rename exit 7 + nothing hangs (time-bound every call); (S8) `cq check` clean → inject `let x: i32 = "s";` → structured diagnostic; two-worktree concurrent checks complete without lock contention (time both, assert ≤2x solo time).
- [ ] `com.hex.scipd.plist`: KeepAlive=true, RunAtLoad=true, `scipd` bin placeholder, logs `~/.codeintel/logs/scipd.{out,err}.log`.
- [ ] Docs: `docs/code-intel.md` A2 section (daemon, escalation semantics, rename, check, config knobs, troubleshooting incl. "instance warming" expectations); AGENTS.md cq section: add rename/check verbs + `source`/`escalated` reading guidance.
- [ ] RUN the E2E for real; paste summary. Gates + commit.

### Task 9: Final audit + acceptance record

- [ ] Silent-failure audit of all A2 code (same method as A1's T12).
- [ ] Set pool_cap/mem_limit_mb defaults from smoke test #3 results (`research/smoke-tests/2026-06-11-ra-live-rss.md` in the personal instance; orchestrator will quote values in your prompt).
- [ ] Full gates incl. `cargo test -p hex-harness` + `cargo build --release`. Run BOTH E2E scripts (A1's and A2's).
- [ ] Write "## 8. Acceptance record" into SPEC-A2.md: A2-S1..A2-S10 (S10 = deployment, recorded as "orchestrator step" with the verification commands).
- [ ] Commit.

## Dependency order
T1 ∥ T2 → T3 (needs T2's trait), T4 (needs T2) in parallel → T5 (needs T1+T3+T4) → T6 (needs T5) → T7 ∥ T8 (need T6) → T9.
