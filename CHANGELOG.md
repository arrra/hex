# Changelog

All notable changes to hex-foundation will be documented in this file.

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
