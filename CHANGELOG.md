# Changelog

All notable changes to hex-foundation will be documented in this file.

## [Unreleased] — spend guardrail: `hex usage burn` + `hex-burn-guard` worker

Credit-burn P0, decision 2026-06-12 (threshold Mike's: $100/hr). `hex usage burn` (the `hex usage` namespace is the home for all usage/metrics
tracking going forward — Mike 2026-06-12) computes the trailing-60m burn rate over ALL Claude Code transcripts —
**recursive scan** (subagent transcripts under `<session>/subagents/` were the
2026-06-12 root-cause blind spot), requestId-deduped, priced at current list
rates (Opus $5/$25, Fable $10/$50; unknown claude models priced at top tier,
never $0). Above threshold → shared loud-alert pathway (stderr + telemetry +
macOS notification, 6h dedupe). Observe-and-alert only — no silent caps (S6).
Recurring cadence via the `hex-burn-guard` harness worker (every 10m), not a
launchd plist. Regression gate: `burn::tests` incl. synthetic-spike red/green,
nested-subagent counting, and requestId dedupe.

## [Unreleased] — recall injection tax cut (credit-burn P0)

Per-prompt memory injection (`hex hook user-prompt-submit` → `memory::recall`)
is transcript ballast: each injected block is cache-re-read on every later turn
until compaction. Measured on June logs (compaction-aware): 4,368 injections,
median ~1.5k tok each, ~3-6% of per-turn cache-read volume ≈ $300-400/mo incl.
writes, plus a 1.6× cache-bust lift on injection-bearing prompts and injections
fired on machine-generated prompts. (An earlier estimate of $1,755/mo / 37%
ignored compaction and was retracted same-day.) This fix does NOT flatten the
cost ∝ turns^1.48 super-linearity — that is transcript accumulation generally,
addressed by the session-length-cap workstream.

- **Context budget 10k → 3k chars** (`MAX_CONTEXT_CHARS`): facts render first
  (cheap, dense); at most **2 chunk snippets** (`MAX_CHUNKS_RENDERED`) at
  **400 chars** each (`CHUNK_SNIPPET_CHARS`, was 5×600).
- **Machine-prompt gate:** recall now gates harness-injected prompts
  (`<task-notification>`, `<local-command-*>`, `<command-name>`,
  `<command-message>`, `<system-reminder>`, `<task-reminder>`) — the hook fires
  on those too, and was burning injections on background task notifications.
- **Regression gate:** `memory::recall::injection_tax_tests` — budget cap,
  chunk-render cap, and machine-prompt gating are asserted in the suite
  (red on the old behavior, green now).

## [Unreleased] — memory pipeline holistic fix

One consolidated pass over the memory pipeline (reference: personal-instance assessment
2026-06-11 / FIX-007…FIX-011):

- **Findings≠failure exit semantics:** consolidate findings are reported, not
  fatal — exit codes reflect operational errors only.
- **Full-consolidate lock wait + alerts:** nightly full waits for the lock (45m)
  and alerts on timeout; quick lock-skips record `skipped-lock`, not ok.
- **Quick-tick budget + cron offset:** 10-min wall-clock budget on the quick
  transcript backstop; quick cron offset from :00 — ticks can no longer starve
  the nightly lock.
- **Doctor nightly-full-liveness check** (26h) + 48h audit window.
- **Deduped alert pathway:** one alert helper (stderr + telemetry + osascript),
  deduplicated.
- **Stdin-first Stop-hook capture:** stdin `transcript_path` is authoritative;
  inline copy; every failure path loud (stderr + telemetry).
- **Distill-child reaper:** pidfile-tracked distill children; serve startup
  kills orphans. Before any kill the reaper verifies process identity
  (`ps -o command=` must mention `claude`) so a recycled PID in a stale
  pidfile never takes out an innocent process group; kill delivery is
  verified (killpg rc checked, plain-kill fallback) — a failed kill is a
  telemetry error, never recorded as success.
- **Drain-timeout telemetry:** harness drain timeouts hit telemetry instead of
  passing silently.
- **`hex backup`:** online sqlite snapshots (memory/events/ledger) with 7-day
  rotation — the 04:00Z cron finally has a target.
- **Vector backfill + stats gaps + KNN floor:** index backfills missing vectors;
  `hex memory stats` reports embedding/orphan gaps; KNN distance floor.
- **`hex memory maintain` weekly:** orphan sweep, FTS optimize,
  transcript_files hygiene, VACUUM — on weekly cron. Hygiene canonicalizes
  transcript_files to the ABSOLUTE path the live backstop writes
  (`<hex_dir>/raw/transcripts/*.md`); relative rows and stale absolute
  prefixes fold into it keeping the furthest watermark — live watermarks are
  never purged (purging them forced a weekly full-corpus re-distillation).
- **Facts semantic recall:** facts_vec populated via maintain backfill;
  `hex memory search` embeds the query once and fuses FTS + KNN arms (RRF)
  for chunks AND facts, and now surfaces a Facts section. The per-prompt
  hook recall path stays FTS-only by design (no embedder cold-load inside
  the UserPromptSubmit latency budget).

## [2026-06-11] — sanitize gate: banned-strings category + idiom re-purge

The sunset third-party session manager's name has crept back into the tree
four times since its removal. The release-cut sanitize gate is the chokepoint
that makes the purge permanent.

### Added
- **`banned string: sunset session-manager name`** category in
  `system/harness/src/sanitize.rs::registry()`: case-insensitive substring
  match over every file type — docs and tests deliberately included (the
  recurring hits live there) — with only the common exclude dirs and filters.
  The pattern encodes the word via a regex hex escape so the scanner source
  never contains the literal it bans; test fixtures build it by concatenation
  for the same reason.

### Changed
- Re-purged the seven idiom hits current on develop: the recurring "…-path"
  idiom reworded to "success path" in docs (`docs/code-intel/SPEC-A2.md`, two
  superpowers plans), code comments (`system/code-intel/src/daemon.rs`,
  `tests/golden_live.rs`), and test fn names (`system/code-intel/tests/cli.rs`
  `def_*`/`other_verbs_*` renamed to the `success_path_exit_0` form, with the
  matching plan snippet updated to keep doc↔code consistent).
- `COMMON_EXCLUDE_DIRS` now prunes `.fastembed_cache/` (gitignored local
  embedding-model cache; its tokenizer vocab blobs contain arbitrary English
  words, which extension-less full-tree checks would false-positive on).
- `docs/architecture/README.md` de-personalized one instance-name reference
  (new arrival on develop; flagged by the existing identifier category).

## [2026-06-11] — remove session title-nudge hook (third-party mobile session manager sunset 2026-06-09)

The third-party mobile session manager (mobile control plane) was sunset
2026-06-09; hex uses native Claude Code Remote Control. The harness still
shipped the manager's session-title nudge, which told sessions to call a
now-removed MCP titling tool and `create_dir_all`'d the manager's state dir on
every user prompt.

### Removed
- **`title_nudge` hook** (`system/harness/src/hook/title_nudge.rs`, added
  v0.19.4): deleted, along with its module declaration and its call site in the
  `UserPromptSubmit` hook. Memory-recall injection behavior is unchanged (same
  JSON output shape, same fail-open semantics).
- `filetime`/`tempfile` dev-deps retained — both still used by other tests
  (`memory/embed.rs` and 30+ files respectively).

## [2026-06-05] — harness lifecycle adopts daemon-green (v0.31.0)

The hex harness no longer hand-rolls launchctl/plist logic. `hex harness
start/stop/status/restart/logs` now route through the standalone **daemon-green**
crate (github.com/mrap/daemon-green, pinned), which renders the gui-domain
LaunchAgent (NO SessionCreate), bootstraps `gui/<uid>` with the wait-out-bootout +
retry + asuser robustness, and on Linux drives `systemd --user`. Same crate now
backs `boi daemon` too — one cross-platform, SSH-safe, sudo-free service manager.

### Changed
- `harness_start/stop/status` reimplemented via `daemon_green::native()`; added
  `hex harness restart` + `hex harness logs`.
### Removed
- Hand-rolled `render_harness_plist` / `harness_plist_path` / `gui_domain` helpers.

## [2026-06-05] — harness plist: remove SessionCreate (it BLOCKS the login keychain) (v0.30.4)

Empirically verified (keychain-launchagent-test on the live box): a gui LaunchAgent
WITHOUT `SessionCreate` reads the login keychain (rc=0); WITH it the read is BLOCKED
(rc=36 errSecAuthFailed). `SessionCreate=true` spawns the job into a NEW audit
session, detaching it from the Aqua session that holds the unlocked login keychain
(per `man launchd.plist`). The v0.30.2 harness plist set it, defeating the very
keychain access it was meant to grant. Removed it; harness_cli_test now asserts its
ABSENCE. (Deployed plists need a re-render + re-bootstrap to pick this up.)

## [2026-06-05] — harness runs as a gui LaunchAgent, not a system daemon (v0.30.2)

Reverses the short-lived LaunchDaemon form from v0.30.0. The harness runs per-task
reasoning inside `claude`, and Claude Code auth lives in the macOS **login
keychain** — unreachable from a system daemon (no login session). So `com.hex.harness`
is now a per-user **gui LaunchAgent** with `<key>SessionCreate</key>` (bridges the
launchd process into a security session for keychain access).

### Changed
- `harness.plist` → gui LaunchAgent + `SessionCreate` (dropped `UserName`).
- `hex harness start|stop` → user gui domain (`launchctl bootstrap gui/<uid>`), no
  sudo, no `/Library/LaunchDaemons`.
- doctor `iii-engine-health` + `hex upgrade` restart path → gui LaunchAgent.

(`hex harness start` template-path fix from v0.30.1 retained: reads the deployed
`.hex/templates/`, falls back to the repo layout.)

## [2026-06-05] — iii engine baked into hex; at-most-once harness daemon (v0.30.0)

The iii engine is now compiled INTO the `hex` binary (forked `mrap/hex-iii`,
pinned in lockstep with `iii-sdk`) and hosted in-process by `hex harness serve` —
there is no separate `iii` binary or `com.hex.iii-engine` service.

### Changed
- **Engine baked in.** `hex harness serve` boots the engine in a tokio task
  (`EngineBuilder::default_config().build().serve()`), connects the worker
  runtime over loopback WS, and runs the whole stack as one process.
- **Workers are typed Rust** in `hex::workers::registry()`, hosted by the single
  `hex harness` process. At-most-once delivery with graceful drain on SIGTERM and
  a durable shutdown-deferral outbox replayed EXACTLY ONCE on restart. Proven by
  a vendorless container E2E (`tests/harness-e2e/`), wired into core-e2e.
- **`hex harness` deploys as a system LaunchDaemon** (`UserName`), not a gui
  LaunchAgent, so it survives logout / no-login-session. `start` stages the plist
  and prints the privileged `launchctl bootstrap system …` sequence.

### Removed
- The declarative YAML worker host (`src/iii_worker.rs`) and the `hex worker` CLI.
- Obsolete deploy artifacts: `com.hex.iii-engine.plist`, `iii-worker.plist`,
  `scripts/iii-engine.sh`.

## [Unreleased] — Collapse hex to Claude Code + BOI

Collapsed hex to Claude Code + BOI: removed the hex-events policy engine + daemon + `hex events` CLI, the SSE/HTTP server + assets + extensions + boi-web, telemetry (`hex telemetry`/`hex metrics`), the workspace picker, the `hex-event`/`hex-agents` skills, and event emission from hooks/memory. Automation moves to OS-level (launchd) per-job scheduling; there is no event bus. SSE/hex-ui is fully removed.

## [2026-06-02] — Demolish the agent fleet, OKRs, and initiatives (v0.21.0)

Large teardown: hex is now a lean single-operator system (BOI delegation + a small
set of personal-cadence hex-events automations). The multi-agent fleet, the OKR
system, the initiatives system, the inbox/messaging layer, and the agent-routed BOI
failure-revive protocol are all removed. Reviewed by a 3-agent release team (harness
correctness + live coherence PASS; silent-failure hunt cleared). Decision records in
`me/decisions/demolish-agent-fleet-2026-06-02.md` (+ rebuild notes for BOI failure
handling and the alert mechanism).

### Removed
- **Harness (Rust):** `agent_spawn`, `agent_evolution`, `wake`, `charter`,
  `charter_triggers`, `messaging`, `message`, `initiative` modules; the
  `hex agent|message|initiative|synthesis|alert|router` CLI; the events-engine
  agent-wake scheduler (`CharterLoader`/`dispatch_agent_wakes`); the `/fleet` HTTP
  route; `doctor` checks `agent_fleet`/`agent_liveness`/`goal_alignment` and the
  `Category::Fleet` variant; the `hex health check-fleet-pulse|check-stalled-initiatives|fleet-scorecard|check-failure-routing` subcommands; and the dead BOI
  failure-revive surface (`build-failure-brief`, `spec-owner-resolver`).
- **Also removed (unused):** `hex workspace` (tmux launcher) and `hex path-map`
  command surface (`path_map.rs` kept — `upgrade.rs` uses `detect_layout`).
- **Policies/docs/reference:** fleet/OKR/initiative/c3 policy templates, the
  `core-agents/` reference set, `docs/multi-agent.md`, `docs/initiative-driven-operations.md`, `docs/charter-as-policy-source.md`, `docs/failure-revive-protocol.md`.
- **CLAUDE template:** Multi-Agent / OKR / Inbox sections + the agent-wake standing order.

### Changed
- `hex` binary `about` string: "Hex multi-agent harness" → "Hex harness".
- Skills (`hex-upgrade`/`debrief`/`decide`/`save`/`reflect`/`doctor`): documented
  `python3 .../memory_index.py|memory_search.py` (removed) → `hex memory index|search`.

### Cleanup
- Pruned dead `*.legacy.{sh,py}` files (Rust-port leftovers).

### Kept intact
- BOI (boi-pm, boi-web, spec-tool verify-claims), release pipeline, memory, doctor,
  startup, events policy engine + hot-reload, integration, telemetry, mcp, session,
  upgrade, hooks. Both default and `--features personal` builds green; full test suite
  green (300+ tests).

## [2026-05-27] — OBS-027 fix: action_log writes restored after 11-day silent drop (v0.20.1)

### Fixed
- **`system/harness/src/events.rs`**: 9 days of `action_log` INSERTs were silently dropped (2026-05-16 to 2026-05-27) because the Rust daemon's writer used new column names (`policy_name`, `rule_name`, `error`, `created_at`) against a Python-era table schema (`recipe`, `error_message`, `executed_at`). The `CREATE TABLE IF NOT EXISTS` was a no-op on existing DBs and the 4 `db.execute()` call sites used `let _ =`, swallowing the column-mismatch error.
  - Added `ALTER TABLE` migrations in `init_schema()` to rename Python-era columns to the Rust schema (`recipe` → `policy_name`, `error_message` → `error`, `executed_at` → `created_at`, plus adding `rule_name`). Renames are idempotent.
  - Added startup column-fingerprint check that fails loud (`return Err`) if any required column is missing — catches future schema drift before it silently corrupts the audit stream.
  - Converted 4 `let _ = db.execute()` call sites (shell, emit, notify, update-file action types) to `if let Err(e) = ... { eprintln!(...) }` per Standing Order S6 (no quiet failures).
  - Verified: `action_log.id` went from frozen at 347265 (2026-05-16 20:35:04) to actively writing again (347304 within seconds of migration completing).

## [2026-05-26] — C3 baseline instrumentation: 5 observability scripts + policies (v0.20.0)

### Added
- **C3 mirror-sink producer** (`system/scripts/c3-mirror-sink.py`, `system/hex-events/policies/c3-mirror-sink.yaml`): drains `action_log` past a forward-only watermark into `~/.hex-events/mirror/YYYY-MM-DD.jsonl`. Second-precision UTC timestamps, controlled `error_class` vocabulary, day-boundary rollover, key-substring redaction, S6 loud-fail on all error paths. Rate-limited to 2000 events/60 min.
- **C3 audit-completeness checker** (`system/scripts/c3-audit-completeness.py`, `system/hex-events/policies/c3-audit-completeness-daily.yaml`): daily scan computing agent wake completion ratio from `actions.jsonl` streams (main + per-agent worktree mirrors). Writes findings to `telemetry/c3-ttd-state.json`.
- **C3 TTD tracker** (`system/scripts/c3-ttd-tracker.py`, `system/hex-events/policies/c3-ttd-tracker.yaml`): tracks time-to-detect (TTD) metrics for quiet failures across the fleet. Correlates `hex.policy.*.failed` events against expected completion windows.
- **C3 orphan scan** (`system/scripts/c3-orphan-scan.py`, `system/hex-events/policies/c3-orphan-scan-daily.yaml`): daily scan for orphaned audit records — policy events with no corresponding agent wake, or wakes with no audit trail. Detects silent drops in the observability pipeline.
- **C3 quiet-failure snapshot** (`system/scripts/c3-quiet-failure-snapshot.py`, `system/hex-events/policies/c3-quiet-failure-weekly-snapshot.yaml`): weekly snapshot of quiet-failure candidates — agents/policies that emit no failure signal despite anomalous gap patterns.
- **Telemetry source-tree home** (`system/telemetry/README.md`, `system/telemetry/migrations/002_c3_views.sql`): C3 baseline VIEWs (orphan candidates, TTD summary, mirror health) defined as SQL migrations. `install.sh` now copies `system/telemetry/migrations/` to the install target.
- **Test suite** (`tests/c3/`): 36 pytest tests covering all 5 C3 scripts — mirror-sink field schema, audit-completeness bucketing, TTD tracker correlation, orphan detection, quiet-failure snapshot logic.

### Fixed
- **`install.sh` refactor guard** (`system/scripts/install.sh`): adds `system/telemetry/migrations/` to the bulk `system/` copy so C3 VIEW migrations land in the install target.
- **`hex-emit.sh` HEX_ROOT export** (`system/scripts/hex-emit.sh`): `HEX_ROOT` was set but not exported, so child processes (including the Python telemetry emitter) could not see it. Changed to `export HEX_ROOT=...`. Synced upstream from a deployed hex instance per Standing Order S1.
- **Sanitize false-positives** (`system/scripts/c3-orphan-scan.py`, `system/scripts/c3-audit-completeness.py`): doc-comment references to path patterns updated to avoid sanitize-check false positives.

## [2026-05-24] — Docker OOM fix for memory indexing (v0.19.8)

### Fixed
- **OBS-019 Docker OOM** (`system/harness/src/memory/{embed,index}.rs`, `tests/Dockerfile`, `tests/Dockerfile.env`): Memory indexing OOMed in the 4 GB Docker E2E container. Chunked the `embed_documents` call to `EMBED_BATCH=8` to bound per-call working set, and capped `ORT_NUM_THREADS=1` / `OMP_NUM_THREADS=1` at container startup so ONNX Runtime sees the limits at dylib load (env-var-from-binary is too late). Added a Linux `rss_mb()` probe for future memory diagnosis.

## [2026-05-24] — BOI v2 path cutover, audit-closure hardcode sweep, E2E fix (v0.19.7)

### Changed
- **BOI v2 path cutover** (`system/scripts/`, `templates/CLAUDE.md`, `system/skills/boi-delegation/SKILL.md`): all active scripts and templates migrated from v1 `.boi` paths to v2 canonical paths. `boi-delegation` SKILL.md rewritten for v2 contract.
- **Charter docs** (`system/reference/core-agents/`): `quality-antagonist` and `boi-optimizer` updated for v2 surface.
- **Harness hardcode parameterization** (`src/budget_reset.rs`, `src/doctor/legacy.rs`, `src/upgrade.rs`): 2 remaining personal hardcoded paths replaced with `$HOME`/env-var resolution. `hex-integration` personal-path fallback replaced with `$HOME/hex`.
- **`.gitignore`**: added `.serena/` to prevent IDE metadata commits.

### Removed
- **Dead health scripts** (`system/scripts/health/`): `surfaces.yaml`, `test-write.txt`, `test2.txt` removed — directory was dead after Rust daemon cutover.
- **Dead events tree** (`system/events/`): stale Python events remnant removed.

### Fixed
- **Docker E2E suites** (`tests/Dockerfile`, `tests/Dockerfile.env`, `tests/test-doctor-events-coverage.sh`): unblocked both Docker test suites; `bump-version` now syncs `system/version.txt` correctly.

---

## [2026-05-24] — title_nudge hook, HEX_DIR path sweep, harness cleanup (v0.19.4)

### Added
- **`title_nudge` hook** (`system/harness/src/hook/title_nudge.rs`): `UserPromptSubmit` hook that emits `hex.session.title.set` when the session title stamp is stale. Prompts Claude to apply a meaningful session title so transcripts are identifiable without manual labeling.

### Fixed
- **`title_nudge` session_id source** (`src/hook/user_prompt_submit.rs`): hook now reads `session_id` from the hook's stdin JSON payload instead of `CLAUDE_SESSION_ID` env var, which Claude Code does not set. Hook now fires correctly in production.
- **Harness mod refs** (`src/`): removed dead `mod` declarations pointing to deleted personal integration files. Eliminates compile-time dead-code warnings.

### Changed
- **HEX_DIR path sweep** (`src/main.rs`, `src/charter.rs`, `src/events.rs`, `src/backup_session.rs`, `build.rs`, `comments-service/server.py`, `system/scripts/meeting-prep.sh`): 7+ hardcoded absolute workspace paths replaced with `get_hex_dir()` / `$HEX_DIR` resolver. `charter.rs` test fallback chain: `MRAP_HEX_PROJECTS` → `HEX_DIR` → `$HOME/hex`.
- **`sanitize-check.sh` hardening**: scan now includes `*.rs` and `*.toml` files in addition to shell scripts — catches hardcoded paths in Rust source before they land.

---

## [2026-05-24] — Wave 2c/3b: dead script dirs + personal skill/command purge (v0.19.3)

### Removed
- **Wave-2c dead script subdirs** (`system/scripts/`): 5 wholesale-dead subdirs (D3 finding) removed — pulse-dashboard v1, `.legacy.*` remnants, Mike-batch dead scripts, 3.5MB binary asset, and `__pycache__` junk. Continuation of wave-2 surface reduction.
- **Wave-3b personal skill/command dirs** (`system/skills/`, `system/commands/`): 5 personal skill dirs, `bet-status.md`, `hex-scout.md` personal commands, and `e2e-guard` personal scripts directory removed. Personal content does not belong in foundation — these lived only in `hex-mrap`.

---

## [2026-05-24] — Wave 2 dead-code purge + Rust daemon canonical (v0.19.3)

### Removed
- **Python events engine** (`system/events/`): entire Python hex_eventd/hex_emit/hex_events_cli tree deleted — 16,589 lines. Rust daemon (`hex_eventd` binary) is now the sole canonical implementation.
- **Orphaned Rust modules** (`src/capture.rs`, `src/health/`, `src/route.rs`): 1,586 lines of dead modules removed post wave-2a call-site strip.
- **Dead call sites** (`src/comments.rs`, `src/lib.rs`, `src/main.rs`, `src/messaging.rs`): 156 lines of dispatch arms for capture::, health::, route:: removed; Health::BudgetReset correctly delegates to budget_reset::.

### Fixed
- **`install.sh`**: stopped copying `system/events/` to target (Python daemon no longer ships); added `templates/hex-events-policies/.gitkeep` placeholder.

---

## [2026-05-24] — Capability lifecycle: callable_by/unprompted, boi-web improvements, memory sanitize (v0.19.3)

### Added
- **`capability_add` `callable_by` field** (`src/wake.rs`): agents can now specify which agents may call a registered capability. If omitted, defaults to the registry allowlist. Prompt docs updated.
- **`capability_add` `unprompted` field** (`src/wake.rs`, `src/prompt.rs`): agents declare whether a capability registration was self-initiated (`unprompted: true`) or explicitly directed by a task. Enables fleet-level audit of autonomous capability growth.
- **Capability lifecycle test suite** (`tests/capability_lifecycle_test.rs`): 235+ lines covering `capability_add` → `capability_call` roundtrip, `callable_by` enforcement, `unprompted` flag propagation.

### Changed
- **`boi_web.rs`** (`src/boi_web.rs`): SSE server improvements — 116-line delta covering connection handling, status streaming, and stability.
- **`charter.rs` test guard**: `all_live_charters_parse` now skips gracefully with `eprintln!` if no charter files found in test environment instead of failing.
- **`memory/provider.rs`**: `hex_root()` fallback changed from hardcoded `/Users/mrap/hex` to `$HOME/hex` (or `/tmp/hex` last resort). Fixes LOW-V16-3 category finding.
- **`memory/recall.rs`**: test fixtures anonymized — `person:whitney` / `'Mike's wife'` renamed to `person:alice` / `'a sample person'`. Slug-boost examples updated to `alice-johnson`.

## [2026-05-24] — Python→Rust ports, paths helper, capability prompt, memory Plan 2 runtime fix (v0.19.2)

### Added
- **`paths.rs`** (`src/paths.rs`): centralized `hex_dir()` resolver — reads `$HEX_DIR`, falls back to `$HOME/hex`. Wired into `main.rs` as `mod paths`. Eliminates per-callsite `$HEX_DIR` assumptions; Wave 0 foundation for D4 hardcode sweep.
- **`capability_call` / `capability_add` trail entry types** (`src/prompt.rs`): agents can now register and call fleet-level capabilities via structured trail entries. Harness routes calls, captures results, appends to `calls.jsonl`. Prompt instructions updated to distinguish `capability_call` from `act`.

### Changed
- **`session_reflect.rs`** (`src/session_reflect.rs`): session-delta eval now written natively via `rusqlite` — eliminates the Rust→Python shellout to `templates/eval/session-delta.py`. `templates/eval/session-delta.py` deleted.
- **`memory/open_db()`** (`src/memory/mod.rs`): Plan 2 migration (`apply_plan2()`) now called at DB open (idempotent DDL) — fixes runtime gap where Plan 2 tables were only created in tests.
- **`distill` max_tokens** bumped to handle longer session transcripts.
- **`.legacy.sh` shellouts restored** (`system/scripts/`): watchdog-heartbeat-check.sh, watchdog-run-full.sh, weekly-synthesis-digest.sh, and others renamed back from `.legacy.sh` for backward compatibility with active callers.
- **SpecTool + Router scripts restored** (`system/scripts/spec-tool/`, `system/scripts/hex-router/`): second batch of `.legacy.py` scripts re-added.
- **PERSONAL-EXTRACT caller pruned** (`src/`): dead Rust caller for `x-oauth2-refresh.sh` removed.

## [2026-05-23] — V2 Memory Plan 2: facts layer, distill pipeline, nightly consolidate (v0.19.1)

### Added
- **Memory facts schema** (`memory/schema.rs`): six new tables — `facts`, `fact_history`, `sessions`, `topics`, `transcript_files`, `vec0`, plus FTS5 index. Persistent structured facts with full history.
- **24-predicate vocabulary** (`memory/predicates.rs`): typed predicates for fact extraction covering preferences, decisions, blockers, goals, and more. Drives LLM extraction fidelity.
- **Distill pipeline** (`memory/distill/`): four-stage pipeline — `extract.rs` (Phase 1 LLM extractor with prompt), `judge.rs` (Phase 2 LLM ADD/UPDATE/NOOP/FLAG decisions), `dedup.rs` (deterministic + embedding deduplication), `watermark.rs` (per-file progress tracking). Triggered by `hex.session.parsed` event.
- **`hex memory distill` CLI**: run distill pipeline on demand over session transcripts.
- **`hex memory consolidate`** (`memory/consolidate.rs`): 6-op consolidation with per-op isolation (contradiction detection, staleness pruning, orphaned-ref cleanup, dedup, topic reorg, summary refresh).
- **Nightly consolidate policy** (`hex-events/policies/memory-consolidate.yaml`): runs `hex memory consolidate` nightly; replaces removed `nightly-consolidation.yaml`.
- **Memory consumption floor policy** (`hex-events/policies/memory-consumption-floor.yaml`): alerts when memory facts drop below configured floor — early warning for silent distill failures.
- **`hex memory distill` event policy** (`hex-events/policies/memory-distill.yaml`): auto-distills new sessions on `hex.session.parsed`.
- **Provider module** (`memory/provider.rs`): extracted from `route.rs`, defer-not-degrade pattern — LLM provider abstraction for distill/judge phases.
- **`facts_injected` telemetry + consumption-floor alert** (`memory/eval.rs`): T17 — facts injected count in memory eval output; consumption-floor alert fires if facts count at zero.
- **`hex-integration-check.sh`**: unified integration health check script covering all companion integrations (BOI, hex-events, MCP, router).
- **`shellout_paths` integration test** (`tests/shellout_paths.rs`): verifies all shell-out targets exist at install paths.
- **Pre-commit hooks** (`.githooks/pre-commit`, `.githooks/pre-commit-ci.sh`): local pre-commit validation and CI-mode variant.
- **Legacy rename guard** (`.github/workflows/legacy-rename-guard.yml`): CI guard against accidental legacy naming patterns.

### Changed
- **`budget_reset.rs`** re-homed from `health/` to top-level module (`src/budget_reset.rs`). Import path updated in `lib.rs`.
- **`memory/recall.rs`** extended with facts arm — recall queries now search the facts table alongside memory entries.
- **`system/scripts/hex-router/router.py`** updated to support additional routing cases.
- **`system/scripts/spec-tool/server.py`** minor updates.
- **README.md**: updated to reflect distill and nightly consolidate in the memory subsystem description.

## [2026-05-23] — Capability system: agent-registered functions with security guard (v0.18.0)

### Added
- **`capability_exec.rs`**: sandbox executor for agent-registered capabilities. Enforces per-wake call caps (`calls_per_wake_cap`), wall-clock timeout, and output byte limits. Returns structured `ExecResult` (stdout, stderr, exit_code, timed_out, output_truncated).
- **`capability_guard.rs`**: static security guard for capability bodies before they are persisted. Hard-denies network egress (`curl`, `wget`, `nc`, `http://`, `https://`), secrets access (`.hex/secrets`), destructive patterns (`rm -rf`), and pipe-to-shell (`| sh`, `| bash`). Also enforces per-agent allowlist and write-once immutability (registered capabilities cannot be overwritten).
- **`registry.rs`**: capability registry — persists `FunctionCapability` and `TriggerCapability` structs to `.hex/registry/`. Manages allowlist (`pilot_agents.json`), build catalog, and atomic add/remove operations. Capabilities are executable scripts with typed input schemas.
- **`doctor/checks/registry_health.rs`**: new doctor check verifying registry directory structure, file integrity, and allowlist consistency.
- **`capability_add` / `capability_call` trail entry types**: agents can now register (`capability_add`) and invoke (`capability_call`) capabilities from trail entries. Gate schema enforces required fields. Wake cycle processes both entry types via `apply_capability_entry()`.
- **Comprehensive test suites**: `capability_exec_test.rs` (328 lines), `capability_guard_test.rs` (203 lines), `capability_lifecycle_test.rs` (371 lines), `gate_test.rs` (135 lines), `registry_test.rs` (429 lines) — 1466 lines of new test coverage.

### Changed
- **`wake.rs`**: wake cycle now handles `capability_add` and `capability_call` trail entries, routing them through the capability guard and executor before persisting results to the audit log.
- **`gate.rs`**: `validate()` extended with schemas for `capability_add` (required: `capability_kind`, `capability_id`, `description`, `wall_hit`, `exec_or_event`) and `capability_call` (required: `capability_id`, `args`).
- **`prompt.rs`**: agent prompt updated to document `capability_add` and `capability_call` trail entry types and their required fields.

## [2026-05-22] — V2 memory command references (v0.17.5)

### Changed
- **Documentation updated**: All operational V1 memory script invocations (`python3 .hex/skills/memory/scripts/memory_search.py`, `memory_index.py`) replaced with V2 `hex` binary subcommands (`hex memory search`, `hex memory recall`, `hex memory index`) across `AGENTS.md`, `GEMINI.md`, `templates/CLAUDE.md`, `templates/AGENTS.md`, `system/commands/`, and `docs/capabilities-map.md`.
- **Capabilities map**: `docs/capabilities-map.md` updated to describe hybrid FTS5+vector retrieval instead of V1 Python scripts. Stale `memory_save.py` sections removed.

## [2026-05-21] — Act evidence verification (v0.17.3)

### Added
- **`act_evidence.rs`**: harness verifies `detail.evidence` on every `act` trail entry claiming a mechanical operation (git push, BOI dispatch, file write, tag creation). Unverifiable claims are recorded as `UNVERIFIED` and do not count as completed work.
- **Evidence types**: `git_tag`, `git_push`, `boi_dispatch`, `file_written` — each matched against observable system state.
- **`tests/act_evidence_test.rs`**: 162-line test suite covering all evidence types, missing-evidence detection, and UNVERIFIED recording.

### Changed
- **Agent prompt hardened** (`prompt.rs`): mechanical act entries now require verifiable `evidence` object. Prompt explicitly states that claims without evidence are recorded as UNVERIFIED. Prevents claim-without-action loops.
- **`types.rs`**: `TrailEntry` gains `evidence` and `verified` fields.
- **`wake.rs`**: post-trail processing calls `act_evidence::verify_trail` — runs after each shift and flags UNVERIFIED entries in the audit log.

## [2026-05-21] — TriggerSpec unification + truncation recovery + harness reliability (v0.17.0)

### Added
- **Bare-string trigger syntax**: `WakeConfig.triggers` now accepts `"timer.tick.6h"` in addition to the full struct form `{ event: { name: "timer.tick.6h" } }`. Both forms are valid in charter YAML. `BlockedItem` gains the same dual-form acceptance. Eliminates boilerplate in every policy file.
- **`hex events emit --source`**: optional `--source <label>` flag tags emitted events with a source identifier, visible in the event log and disk-backed status.
- **Events disk-backed status**: `hex events status` now persists event state to disk, surviving daemon restarts. Daemon status is readable without the daemon running.
- **Doctor check_16**: modernized doctor check for bare-string trigger coverage in active policies.
- **Truncated response recovery (S6)**: harness salvages complete leading elements from truncated JSON agent responses via char-by-char depth scanning. Every truncation emits a loud eprintln warning, an audit entry (`response-truncated`), and a `hex.agent.response.truncated` event — no more silent partial data loss.
- **Non-JSON retry**: harness retries agent wake once when the first response is non-JSON, appending a stern JSON-only reprompt. Prevents permanent failure from one malformed response.
- **Upgrade restarts events daemon**: `hex upgrade` restarts the events daemon after binary swap, ensuring the new binary is immediately active without manual restart.

### Changed
- `TriggerSpec` unified across `WakeConfig.triggers` and `BlockedItem` — single deserialization path handles both bare-string and struct forms.
- Agent wake prompt instructs agents to keep response compact to prevent truncation.
- Trail emitted last in agent response format so truncation always spares actionable fields (messages, queue updates).
- Stale `.hex/scripts/*.sh` refs in commands updated to `hex` subcommands (hex-checkpoint, hex-doctor, hex-save, hex-scout, hex-sync-base, hex-upgrade).

### Fixed
- **Atomic binary install** (vnode-poisoning fix): `hex upgrade` now writes to a temp file, codesigns it, then renames over the destination inode — safe even when the binary is currently executing (mmap'd). Prevents macOS code-signing vnode cache poisoning.
- Stale test updated for bare-string `BlockedItem` deserialization.
- `BOI(S8585)` spec tasks completed (internal).

## [2026-05-17] — Full rustification + S1 skills sync (v0.16.0)

### Added
- `hex events` — native Rust event daemon replacing Python `hex_eventd.py`. Full hot-reload, multi-cadence scheduler, shell/emit/notify/update-file action handlers. No Python runtime required for event processing.
- `hex hook` — Claude Code hook runners ported to Rust: `session-start`, `post-tool-use`, `backup-session`. Shell hook scripts quarantined.
- `hex doctor` — DoctorCheck trait framework with 55+ checks (replaces `doctor.sh`). Codex-CLI checks 51-55 added.
- `hex upgrade` — upgrade pipeline ported from `upgrade.sh` to native Rust.
- `hex agent evolution`, `hex agent reset-periods`, `hex agent optimizer-wake` — harness-native agent lifecycle.
- `hex learnings`, `hex initiative`, `hex route`, `hex validate`, `hex integration`, `hex health`, `hex metrics`, `hex telemetry`, `hex picker`, `hex synthesis`, `hex capture`, `hex session`, `hex env`, `hex mcp`, `hex extension`, `hex workspace`, `hex alert` — Python scripting layer fully ported; 130 Python files reduced to ~22.
- `system/skills/hex-event/`: Policy-based event engine skill — wire reactive automation, event chains, and oneshot notifications.
- `system/skills/hex-save/`: Persist and retrieve named context snapshots across sessions.
- `system/skills/hex-switch/`: Switch active hex topic/project context cleanly.
- `system/skills/imessage/`: iMessage integration skill for sending messages from Claude sessions.
- `system/skills/mirofish/`: Miro board integration for visual brainstorming and diagram workflows.
- `system/skills/remodeling/`: Home remodeling project planning and tracking skill.
- `system/skills/x-twitter/`: X/Twitter post drafting and publishing skill via MCP.
- `system/skills/conjecture-criticism/`: Structured conjecture-criticism-synthesis reasoning framework with agent prompt and cross-critique templates.
- `system/skills/vibe-to-prod/`: Vibe-to-production pipeline skill — assess, characterize, refactor, verify phases with spec templates and metric scripts.
- `system/commands/bet-status.md`: `/bet-status` slash command — surfaces active bets, their confidence levels, and resolution status.

### Changed
- Real-port phase complete: all `.legacy` shim references removed. `upgrade.sh`, `sse-bus/bridge.py`, `agent-evolution.sh`, `doctor.sh`, `capture.sh`, `hex-integration-check-all.sh` are now Rust-native.
- `system/events/` retains policy YAML definitions; daemon/emitter/CLI now live in `hex events` subcommand.
- AGENTS.md rewritten with 5 cold-start questions and verify commands (walkinglabs lecture format).
- PROGRESS.md added as session state schema for Phase 5+ harness refactor tracking.

### Fixed
- Doctor equivalence mismatches resolved: agent-fleet, python, env-sh, me-md, scripts-exec, agent-liveness.
- Routing cluster bug: `hex route` fixes live route-comment regression.
- PATH deduplication: `compose_path` now deduplicates existing PATH entries, not just hex additions.
- Release pipeline: sanitize-check and PII-scan violations resolved in skills and commands.

## [2026-05-12] — Harness messaging + binary resolution fix (v0.15.0)

### Fixed
- `system/harness/src/messaging.rs`: `MessagingHandler::cli_send()` now writes to `.hex/messages/{agent}.jsonl` in addition to `messages.json`. The harness wake cycle reads only from the JSONL inbox (`message::receive()`), so CLI-sent messages were silently dropped — causing agent-to-agent messaging deadlocks (Releaser→Sentinel sign-off blocked for 16+ days; root cause S1499).
- `system/harness/src/claude.rs`: `claude::invoke()` now resolves the `claude` binary via `$CLAUDE_BIN` env var, then `$HOME/.local/bin/claude`, then PATH fallback. LaunchAgent/daemon wakes inherit a restricted PATH that excludes `~/.local/bin`, causing "failed to spawn claude: No such file or directory" in automated wake contexts.

## [2026-05-12] — Slack/cc-connect deprecation + upgrade.sh cleanup (v0.14.0)

### Changed
- `system/scripts/upgrade.sh`: dropped legacy hex-events standalone install path — hex-events is now a subcommand of the `hex` binary (`hex events`) and no longer requires a separate Python install or LaunchAgent. Removes `HEX_EVENTS_REPO/DIR/SRC` variables, `verify_hex_events()`, `install_hex_events()`, v1 rsync block, and misleading "hex-events and BOI" error framing.
- `system/scripts/hex-vitals.py`: removed `--slack` flag and Slack posting (~155 LOC). cc-connect / Slack fully deprecated.
- `system/scripts/hex-doctor`: removed dead `check-slack-alert-roundtrip` module.
- `system/scripts/pulse-dashboard/server.py`: removed `/api/messages` Slack-fetch endpoint and `loadMessages()` frontend poller.
- `system/scripts/pulse/server.py`: removed dead Slack surface entry.
- `system/scripts/telemetry-ratio.py`: removed `slack` surface entry.
- `system/skills/secret-intake/SKILL.md`: removed cc-connect references.
- `system/scripts/integrations/slack-bot.sh`, `secrets-pipeline.sh`: archived to `_archive/` — not removed from history, just retired from active use.

## [2026-05-06] — messaging.receive + wake crash-recovery + health scripts (v0.13.3)

### Added
- `system/harness/src/messaging.rs`: `MessagingHandler.receive()` — fetches agent-type messages for `agent_id` (status=new or in_progress), transitions to `in_progress` atomically so messages survive wake crashes and are re-delivered on next wake.
- `system/scripts/health/check-hex-events-policy-load.sh`: surfaces POLICY LOAD/VALIDATION ERROR entries from the hex-events daemon log (previously sat silently; this closes the gap).
- `system/scripts/health/check-vector-search.sh`: verifies sqlite-vec is loadable and memory.db has vectors; surfaces the silent degradation to FTS-only keyword search.

### Fixed
- `system/harness/src/wake.rs`: use `messaging.receive()` for inbox population (fixes type mismatch between `messaging::Message` and `types::Message`); drain legacy JSONL inbox in parallel; track `wake_succeeded` flag so in_progress messages survive `claude::invoke` errors.
- `system/hooks/scripts/backup_session.sh`, `session-stop-persist.sh`: redirect subshell output to daily log in `.hex/logs/stop-hooks/` instead of discarding; disown to suppress job notifications; 14-day log rotation.
- `tests/test-doctor-events-coverage.sh`: updated to test the new external-script architecture (inline `check_66` was replaced by `check-hex-events-policy-load.sh` in v0.13.2).

## [2026-05-06] — session-start checkpoint resume + integration-check fix + memory leak fix (v0.13.2)

### Fixed
- `system/hooks/scripts/session-start.sh`: channel→topic checkpoint resume — sessions matching `hex-<topic>` pattern now surface `projects/<topic>/checkpoint.md` as additionalContext. Generalized `.hex/state/blockers/*.flag` scan (any flag file surfaces as a blocker). Topic-regex sanitization strips leading `#` from CC_SESSION_KEY.
- `system/scripts/hex-integration-check.sh`: `export _error_raw` bug fix — the prior `VAR=value FAIL_PAYLOAD=$(...)` idiom did not propagate `_error_raw` into the command-substitution subshell, causing 11,948+ events/day with `error: null`. Emit-throttle added for streak>1 fail (heartbeat every 60 consecutive checks prevents log spam without hiding persistent failures).
- `system/skills/memory/scripts/memory_index.py`: cascade-delete `vec_chunks` orphans on re-index — FTS5 chunk deletion did not cascade to the `vec0` virtual table, accumulating 82,377 orphan rows (58% of the vec table). `_delete_vec_for_rowids()` called before every chunk delete.
- `system/skills/memory/scripts/memory_search.py`: `_rrf_merge` documented as FTS-only (KNOWN GAP) — `--hybrid` was paying embedding+vec-query cost without fusing vec results into the score. Log line now honest; TODO surfaced.
- `system/scripts/health/check-career-pipeline.sh`: switched from broken `hex_events_cli.py status` grep to `load_policies`-based check for policy validation.

### Changed
- `system/scripts/hex-doctor`: two new health modules added — Memory Vector Search (surfaces sqlite-vec drift where semantic search silently falls back to FTS) and hex-events Policy Load Errors (surfaces POLICY LOAD/VALIDATION ERROR entries from daemon log that were previously invisible).

## [2026-05-06] — doctor reliability + skip_llm WakeConfig (v0.13.1)

### Fixed
- `system/harness/src/main.rs`: Doctor command switches from `cmd.output()` (buffered) to `cmd.spawn()` with `Stdio::inherit()` — output streams live instead of appearing all-at-once after completion.
- `system/scripts/run-startup-checks.sh`, `run-memory-checks.sh`, `run-landings-workspace-checks.sh`: Stale `CLAUDE_DIR=$HEX_DIR/.claude` path changed to `$HEX_DIR/.hex`. Was causing 5 spurious ERRORs on install paths that follow the `.hex` layout.
- `system/scripts/hex-doctor`: Replace buffered `$()` capture with `tee | tail -n +5` streaming. All PIPESTATUS slots captured so mid-pipeline failures surface explicitly. Combined two EXIT traps into one.
- BOI daemon check in hex-doctor rewritten for LaunchAgent-aware detection (was `pgrep`-based, missed managed processes).

### Added
- `system/harness/src/types.rs`: `WakeConfig.skip_llm` field (`#[serde(default)]` for backwards compat). Allows health-probe agents to exercise wake plumbing without paying for an LLM call.
- `system/harness/src/wake.rs`: When `charter.wake.skip_llm=true`, bypass shift loop and self-assessment phase. Inbox loads, wake-start audit fires, `mark_delivered` runs. Inbox-sourced active queue items drained to prevent `state.json` unbounded growth.
- `system/scripts/health/check-message-roundtrip.sh`: end-to-end validation of skip_llm health-probe wake — sends a message, wakes health-probe agent, verifies mark_delivered, state save, and audit emit.
- `system/scripts/health/check-career-pipeline.sh`: career email pipeline health check — validates draft existence, policy load, and optional dry-run send. Sanitize-clean (env-var paths, example addresses).
- `system/scripts/doctor-checks/boi.sh`: BOI daemon doctor check with LaunchAgent-aware detection.
- `system/scripts/hex-watcher`: minimal tmux BOI status pane (one-shot or `--watch` loop).

## [2026-05-05] — agent performance review + calibration

### Added
- `system/scripts/health/agent-performance-review.py`: per-agent quality/velocity/autonomy scorecard — extracts signals from critic reviews, BOI DB, audit trail, and Mike-pushback messages; composite geometric mean (0.0–1.0); cold-start handling (confidence=low for agents with <5 wakes); outputs markdown scorecard with top/bottom artifacts.
- `system/scripts/health/fleet-scorecard-aggregate.py`: fleet-wide aggregate scorecard — runs agent-performance-review.py for all agents, produces top/bottom 5 performers, biggest movers, Mike-pushback heatmap; sends single coalesced Slack digest to configured Slack channel (no per-agent pings per ergonomics-critic rule).
- `adapter/policy-templates/agent-performance-review-weekly.yaml`: policy template wiring `timer.tick.daily` (Sunday 09:00 ET gate) → `fleet-scorecard-aggregate.py` with 6d rate limit.

## [2026-05-05]

### Added
- `system/scripts/health/check-fleet-pulse.sh`: fleet-pulse watchdog — emits `hex.agent.needs-attention` events for dormant agents; composite liveness score with WARN/ERROR escalation; suppresses when budget-lockout active.
- `system/scripts/health/check-stalled-initiatives.sh`: stalled initiative monitor — detects initiatives with no progress signal in 48h (commit, act trail, KR update), sends drive-or-close directive to owner; anti-spam guard prevents re-fire within 24h.
- `system/scripts/health/check-mike-pending.sh`: Mike-pending board monitor — tier:quiet/digest/direct-ping labels, coalesced per-run alerts, DM fallback to channel if Slack user ID not configured.
- `system/scripts/health/budget-period-reset.py`: budget period auto-reset — rolls cost.current_period.start forward when period expires; 5x runaway safety gate blocks reset and emits ERROR alert instead of silently clearing an out-of-control agent.
- `system/harness/src/wake.rs`: backlog auto-promotion with three safety constraints — proactive_initiatives gate (reactive-only agents never self-assign), per-agent daily wake-budget ceiling at 80% of `charter.budget.usd_per_day`, and a per-wake ceiling of 2 backlog items.
- `adapter/policy-templates/fleet-pulse.yaml`: policy template wiring `timer.tick.1h` → `check-fleet-pulse.sh`.
- `adapter/policy-templates/stalled-initiative-monitor.yaml`: policy template wiring `timer.tick.6h` → `check-stalled-initiatives.sh` with per-initiative rate limiting.
- `adapter/policy-templates/mike-pending-escalator.yaml`: policy template wiring `timer.tick.2h` → `check-mike-pending.sh`.
- `adapter/policy-templates/budget-period-reset.yaml`: policy template wiring `timer.tick.daily` → `budget-period-reset.py`.

## [2026-05-04]

### Changed
- AGENTS.md: Added "Related repos" cross-link section in Quick Start pointing to boi and the local hex workspace, so agents navigating hex-foundation can find the delegation engine and production workspace
- templates/CLAUDE.md: Added Quick Start section with "Related repos" placeholder before the system-managed block
