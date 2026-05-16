# Harness Engineering Audit — 2026-05-15

---

## Source

**Curriculum:** [Learn Harness Engineering](https://walkinglabs.github.io/learn-harness-engineering/en/)
by WalkingLabs. 12 lectures covering the engineering of AI coding agent harnesses.

**Codebase audited:** hex-foundation at `boi/S2C48` worktree
(representative of `main` as of 2026-05-15).

**Methodology:** WebFetch of all 12 lecture pages → principle extraction →
gap matrix against hex-foundation source → clustering → prioritized backlog.

---

## Executive summary

| Metric | Count |
|--------|-------|
| Lectures distilled | 12 |
| Total principles extracted | 58 |
| **MET** | **4** (7%) |
| **PARTIAL** | **27** (47%) |
| **MISSING** | **27** (47%) |
| HIGH severity gaps | 14 |
| MEDIUM severity gaps | 30 |
| LOW severity gaps | 14 |
| Backlog items | 13 |
| Estimated effort | ~3 months |

**The headline:** Hex has excellent foundational scaffolding (trail schema, event engine,
BOI orchestration, reflection loop) but systematic gaps in three load-bearing areas:
(1) instruction hygiene (563-line CLAUDE.md violates every size principle),
(2) session state continuity (no PROGRESS.md, no five-dimension exit checklist),
and (3) verification enforcement (verify is by convention, not mechanical gate).

### Critical path — items that unblock everything else

1. **`#1 agents-md-verification`** — hours — AGENTS.md cold-start answers + verify commands
2. **`#2 claude-md-decomposition`** — weeks — 563-line CLAUDE.md → ≤150-line router + topic docs
3. **`#3 session-lifecycle-state`** — days — PROGRESS.md schema + five-dimension exit checklist
4. **`#5 trail-audit-implementation`** — days — implement `hex agent audit` (schema exists, command stubbed)
5. **`#4 verify-mechanical-enforcement`** — weeks — BOI daemon enforces verify at DB level before DONE

---

## Methodology

1. **Lecture extraction (Task TF928):** All 12 lecture pages fetched via WebFetch and distilled
   into a structured principles index (`/tmp/harness-principles.md`, 403 lines). Each lecture
   yielded: thesis, 3–7 principles, failure mode addressed, prescribed pattern, and citable quotes.

2. **Gap audit (Task TB483):** Each principle mapped to hex-foundation by reading:
   - `system/harness/src/` — Rust binary source (main.rs, gate.rs, telemetry.rs, events.rs)
   - `system/skills/` — SKILL.md files for every skill
   - `templates/` — CLAUDE.md (563 lines), AGENTS.md (66 lines), decision-template.md
   - Live instance cross-references: `/Users/mrap/mrap-hex/CLAUDE.md`, `~/.hex-events/`,
     `~/.boi/`

   Result: `/tmp/harness-gap-matrix.md` (173 lines) with Status (MET/PARTIAL/MISSING), evidence,
   gap description, severity, and effort estimate per principle.

3. **Clustering and backlog (Task T7325):** Related gaps collapsed into 13 backlog items
   (`/tmp/harness-backlog.md`, 497 lines). Ordered by impact × leverage: HIGH severity and
   cross-cutting items first.

4. **Synthesis (Task TCAD5, this document):** Combined into a self-contained artifact for
   a future agent to dispatch the first refactor spec without re-reading lectures.

**Spot-check confidence:** The gap matrix cites specific file paths and line numbers (e.g.,
`main.rs L676`, `main.rs L1108`, `main.rs L1492–1510`, `gate.rs` trail schema). The two
clearest MISSING findings — 563-line CLAUDE.md and stubbed `hex agent audit` — are verifiable
with `wc -l templates/CLAUDE.md` and running `hex agent audit` respectively.

---

## Lecture-by-lecture summary

### Lecture 01 — Why Capable Agents Still Fail

**Thesis:** Model capability and execution reliability are orthogonal. The harness — everything
outside model weights — determines realized capability.

**Core primitive:** Five-layer failure attribution (task spec → context → environment →
verification → state management).

**Hex status against principles:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Harness-first debugging / five-layer attribution | PARTIAL | `hex-doctor` checks installation health, not failure attribution. No guided triage path for agents when tasks fail. |
| Five defense layers documented | PARTIAL | Layers exist as scattered rules; no enumerated checklist in CLAUDE.md/AGENTS.md. |
| Explicit definition of done | PARTIAL | BOI tasks have `verify:` commands. Interactive hex sessions have **zero** verification gates. |
| AGENTS.md as foundation | PARTIAL | `templates/AGENTS.md` (66 lines) lacks verification commands, tech stack summary, and "how to verify hex works." |
| Diagnostic loop | **MET** | `hex-reflect` implements full loop (issue extract → critic → fix → recurrence tracking). Best-in-class. |

**Net:** 1 MET, 3 PARTIAL, 1 HIGH gap (no verify gates for interactive sessions).

---

### Lecture 02 — What a Harness Actually Is

**Thesis:** A harness is five subsystems, all required: Instruction, Tool, Environment, State, Feedback.

**Core primitive:** Five-subsystem model; feedback subsystem (verify commands in AGENTS.md) has
the highest ROI.

**Hex status:**

| Subsystem | Status | Key finding |
|-----------|--------|-------------|
| Instruction (CLAUDE.md ≤100 lines) | **MISSING** | `templates/CLAUDE.md` is 563 lines — 2–10× the recommended limit. Critical rules buried in the middle. |
| Tool (shell access, least-privilege) | **MET** | Hex CLI, hex-events daemon, BOI workers, MCP tooling — robust and well-structured. |
| Environment (lockfiles, reproducible setup) | PARTIAL | Rust: `Cargo.lock` exists. Python scripts: no `requirements.txt` / `pyproject.toml` in templates. |
| State (PROGRESS.md) | **MISSING** | No PROGRESS.md equivalent. Session state is freeform (`landings/`, `raw/handoffs/`). |
| Feedback (verify commands in AGENTS.md) | PARTIAL | BOI verify commands exist. But AGENTS.md has no "how to verify hex itself" section. |

**Net:** 1 MET, 2 PARTIAL, 2 MISSING including 2 HIGH gaps.

---

### Lecture 03 — Why the Repository Must Become the System of Record

**Thesis:** All project knowledge must live in the repository or it is invisible to the agent.

**Core primitive:** `AGENTS.md` + `PROGRESS.md` + cold-start test (fresh agent answers 5
questions from repo alone).

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Knowledge next to code (module ARCHITECTURE.md) | **MISSING** | `system/harness/src/`, `system/skills/`, `system/scripts/` — no ARCHITECTURE.md in any. Agents reverse-engineer intent. |
| Standardized entry file answers 4 cold-start Qs | PARTIAL | AGENTS.md answers "what" + "how to use." Missing: "how to verify" and "current progress." |
| Cold-start test passes | PARTIAL | Fresh agent cannot answer "how do I verify my changes?" or "what's the current state?" |
| ACID state management | PARTIAL | Git worktrees isolate BOI agents (good). No CI pipeline. Consistency unenforced. |
| Update knowledge with code (coupling mechanism) | **MISSING** | Voluntary only. No commit hook or CI reminder. Documentation decay is real risk. |

**Net:** 0 MET, 3 PARTIAL, 2 MISSING.

---

### Lecture 04 — Why One Giant Instruction File Fails

**Thesis:** Monolithic instruction files bury critical constraints in the "lost in the middle"
zone and inflate context budget consumption.

**Core primitive:** Entry file (50–200 lines, router only) + topic documents (50–150 lines,
on-demand).

**Hex status — this lecture is the most violated:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Entry file 50–200 lines max | **MISSING** | `templates/CLAUDE.md` is **563 lines**. Validated case study showed 600→80 lines improved task success 45%→72%. |
| Topic documents in docs/ | PARTIAL | `system/events/docs/` exists. No topic document architecture for CLAUDE.md overflow. |
| Progressive disclosure | **MISSING** | All CLAUDE.md content is flat; no on-demand loading. |
| Instruction governance (source/applicability/expiry) | **MISSING** | Standing-orders table has rule text + dates, but no applicability condition or expiry condition. |
| Instruction SNR | **MISSING** | Single 563-line doc → SNR approaches 5–10% for simple tasks. Every session reads BOI orchestration docs regardless of task. |

**Net:** 0 MET, 1 PARTIAL, 4 MISSING. All 5 gaps are present-day problems, not future polish.

---

### Lecture 05 — Why Long-Running Tasks Lose Continuity

**Thesis:** Context windows are finite and non-scalable; structured state persistence is the
only reliable mechanism for multi-session continuity.

**Core primitive:** `PROGRESS.md` (schema-enforced), `DECISIONS.md`, and session clock-in/out
routines.

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| PROGRESS.md schema | **MISSING** | No PROGRESS.md. Handoff files exist but are freeform; no schema enforcement; not read at session start. |
| DECISIONS.md schema | PARTIAL | `decision-template.md` is good. Records are per-file in `me/decisions/`, not consolidated. Fresh session must search. |
| Session clock-in/out routines | PARTIAL | `hex-startup` + `hex-shutdown` exist but don't verify system consistency (build/test) on entry/exit. |
| Compaction-safe state ("why" not just "what") | PARTIAL | Decision template captures reasoning. No "rebuild from state" protocol after compaction. |
| Context anxiety management | PARTIAL | WARMING→HOT thresholds (65%/80%) trigger checkpoints. Gap: checkpoint captures "what" not "why was I doing this." |

**Net:** 0 MET, 4 PARTIAL, 1 MISSING (PROGRESS.md — HIGH).

---

### Lecture 06 — Why Initialization Needs Its Own Phase

**Thesis:** Initialization and implementation must be separate phases; mixing them causes
infrastructure debt that accumulates as critical failures in subsequent sessions.

**Core primitive:** Bootstrap contract (4 conditions before implementation) + initialization
acceptance checklist.

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Phase separation | PARTIAL | `hex-startup` has Step 0 (migration check) + Step 1. No explicit "init phase must complete before work begins." |
| Bootstrap contract (4 conditions) | **MISSING** | No bootstrap contract template. `templates/` has warm-start files but no "am I properly initialized?" checklist. |
| Warm start template | **MET** | `templates/` provides 8+ files (CLAUDE.md, AGENTS.md, todo.md, etc.). Better than cold start. |
| Initialization acceptance checklist | **MISSING** | No machine-verifiable "initialization done" state. `hex doctor` checks health but not "can agent orient itself?" |

**Net:** 1 MET, 1 PARTIAL, 2 MISSING.

---

### Lecture 07 — Why Agents Overreach and Under-Finish

**Thesis:** WIP=1 is the essential harness primitive; agents attempting multiple tasks
simultaneously dilute attention below the threshold for any task to complete.

**Core primitive:** WIP=1 rule, four-state feature machine (`not_started → active → blocked →
passing`), Verified Completion Rate monitoring.

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| WIP=1 enforced | PARTIAL | BOI has task dependencies (partial enforcement). CLAUDE.md rule 3 **explicitly permits "2+ simultaneous tasks"** — directly contradicts WIP=1. |
| Completion evidence executable | PARTIAL | BOI verify commands exist. Quality varies: many use `test -f` (existence check) rather than behavioral evidence (API response, test output). |
| Scope surface externalized | PARTIAL | BOI specs are machine-readable (YAML). Interactive work: `todo.md` is markdown-only, not machine-queryable. |
| VCR monitoring (block when VCR < 1.0) | **MISSING** | No Verified Completion Rate metric. No gate blocking new task activation. |

**Net:** 0 MET, 2 PARTIAL, 2 MISSING (VCR — HIGH).

---

### Lecture 08 — Why Feature Lists Are Harness Primitives

**Thesis:** Feature lists are foundational primitives, not memos — the scheduler, verifier,
handoff reporter, and progress tracker all depend on them.

**Core primitive:** JSON feature list with triple structure (behavior + verify + state);
harness-controlled state transitions; single source of truth.

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Triple structure (behavior + verify + state) | PARTIAL | BOI tasks have spec + verify + status. No `feature_list.json` for hex workspace capabilities themselves. |
| State machine (harness controls transitions) | PARTIAL | BOI 3-state machine (PENDING → DONE/FAILED/SKIPPED). Missing "blocked" state. Verify not enforced at DB level. |
| Pass-state gating (verify must succeed) | **MISSING** | "IMPORTANT: run verify before DONE" is an instruction to agent, not a mechanical gate. Nothing stops a worker from skipping verify. |
| Single source of truth | PARTIAL | BOI specs for delegated work. `todo.md`, `landings/`, BOI specs can diverge for the same item. |
| Harness dependency on feature list | **MISSING** | Scheduler (hex-startup), verifier (boi-completion-gate), handoff (hex-checkpoint), progress (hex-startup) all read **different** sources. |

**Net:** 0 MET, 2 PARTIAL, 3 MISSING (pass-state gating — critical correctness gap).

---

### Lecture 09 — Why Agents Declare Victory Too Early

**Thesis:** Agents systematically overestimate completion due to confidence calibration bias;
external, execution-based verification replaces subjective agent confidence.

**Core primitive:** Three-layer verification (syntax → runtime → E2E); worker/checker split;
"Definition of Done" in CLAUDE.md.

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Three-layer verification | PARTIAL | BOI has task verify + post-commit gate (2 layers). No E2E system validation. |
| Worker/checker split | **MET** | BOI has explicit "critic phase" in `boi-delegation/SKILL.md` L68. Well-designed. |
| Completion priority constraint | **MISSING** | No gate preventing refactor/optimization before core features pass verification. |
| Runtime signals (startup success, DB state, etc.) | PARTIAL | `gate.rs` has verify trail type with required fields. Not standardized across signal categories. |
| Actionable error feedback | PARTIAL | "No quiet failures" rule exists. Error format is terse (WHAT only); no WHY or FIX sections. |

**Net:** 1 MET, 2 PARTIAL, 2 MISSING.

---

### Lecture 10 — Why End-to-End Testing Changes Results

**Thesis:** Unit tests alone cannot verify system correctness; only E2E tests detect component
boundary defects, and knowing they will occur changes agent coding behavior.

**Core primitive:** Testing adequacy gradient enforced; architectural boundary checks in CI;
agent-oriented error messages (ERROR/WHY/FIX).

**Hex status — the most uniformly MISSING lecture:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| E2E test mandate | **MISSING** | Hex binary has **one** Rust test (CLI smoke test, `main.rs` L1492–1510). No E2E test suite. Harness correctness is entirely manual. |
| Testing adequacy gradient | **MISSING** | No unit test suite, no integration tests, no E2E tests. Pyramid is inverted. |
| Architectural boundary enforcement (CI) | **MISSING** | No CI pipeline in hex-foundation. No automated architectural invariant enforcement. |
| Agent-oriented error messages | **MISSING** | Errors are terse ("ERROR: HEX_DIR does not contain CLAUDE.md"). No WHY. No FIX. |

**Net:** 0 MET, 0 PARTIAL, 4 MISSING — 3 HIGH, 1 LOW. Entire lecture is a gap.

---

### Lecture 11 — Why Observability Belongs Inside the Harness

**Thesis:** Observability must be architected in from the start; post-hoc addition wastes 30–50%
of session time on redundant diagnosis.

**Core primitive:** Sprint contracts (scope negotiation), evaluator rubrics (structured scoring),
task traces (decision-path records), harness-level signal collection.

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Sprint contracts | **MISSING** | No sprint contract template or skill. Scope is controlled by rules (CLAUDE.md), not negotiated contracts with explicit exclusions. |
| Evaluator rubrics | PARTIAL | `hex-reflect` has structured issue extraction with severity + categories + adversarial critic. Gap: rubric applies to session behavior, not deliverable quality. |
| Task traces as decision-path records | PARTIAL | `gate.rs` trail schema is excellent (decide type requires alternatives + reasoning). `hex agent audit` returns **"not yet implemented"** (`main.rs` L1108). Traces written, never read. |
| Harness-level signal collection | PARTIAL | `telemetry.rs`, event engine, `hex-events` daemon — good foundation. Startup/shutdown/skill invocations emit no events. Memory operations lack telemetry. |
| Observability from design | PARTIAL | Event engine, trail schema, telemetry were designed in from the start (good). Coverage is uneven: events subsystem is well-observable; harness internals are not. |

**Net:** 0 MET, 4 PARTIAL, 1 MISSING. Best "partially implemented" lecture — strong design, uneven execution.

---

### Lecture 12 — Why Every Session Must Leave a Clean State

**Thesis:** Session completion requires both task success AND clean state verification; without
it, entropy accumulates exponentially across sessions.

**Core primitive:** Five-dimension clean state (build + test + progress + artifact + startup);
session exit checklist enforced by harness.

**Hex status:**

| Principle | Status | Key finding |
|-----------|--------|-------------|
| Five-dimension clean state | PARTIAL | `hex-shutdown` covers 2/5 dimensions (progress update + session deregistration). Missing: build/test verification, artifact cleanup. |
| Session integrity as transaction | PARTIAL | Checkpoint is best-effort; no rollback path; no "verify checkpoint integrity" at startup. |
| Quality document (A–C ratings per module) | **MISSING** | No module-level quality scoring. Technical debt accumulation in specific skills is invisible. |
| Session exit checklist | **MISSING** | `hex-shutdown` has no build/test verification step. A session can end with failing tests; harness accepts silently. |
| Idempotent cleanup | PARTIAL | Session deregistration and memory rebuild are idempotent but not explicitly tested or documented as such. |
| Harness simplification reviews (monthly) | **MISSING** | One manual consolidation happened (2026-04-29). No scheduled review cycle. |

**Net:** 0 MET, 3 PARTIAL, 3 MISSING including 2 HIGH gaps.

---

## Cross-cutting gap themes

### Theme A — Instruction Hygiene
**Lectures:** 02, 03, 04  
**Summary:** The 563-line `templates/CLAUDE.md` violates every principle about instruction file
sizing, progressive disclosure, and SNR. This is the single highest-visibility gap — every agent
session is degraded by it. The fix (decomposition into a ≤150-line router + topic documents) is
well-defined and not technically complex, but requires care because agents are trained on the
existing file shape.

**Backlog items:** #1 (AGENTS.md), #2 (CLAUDE.md decomposition), #9 (WIP rule rewrite), #11 (module architecture docs)

---

### Theme B — Session Lifecycle State
**Lectures:** 02, 05, 06, 12  
**Summary:** Hex has three disconnected state artifacts (`todo.md`, `landings/`, `raw/handoffs/`)
instead of one machine-readable `PROGRESS.md`. Session startup reads priorities but doesn't verify
system health. Session shutdown writes context but doesn't verify build/test integrity. Every
session boundary is a continuity risk. Fixing this (PROGRESS.md schema + five-dimension exit
checklist) is the single most impactful correctness improvement.

**Backlog items:** #3 (session lifecycle state), #7 (bootstrap contract), #8 (feature list), #10 (decisions consolidation)

---

### Theme C — Verification Rigor
**Lectures:** 01, 07, 08, 09, 10  
**Summary:** Hex's verification discipline is BOI-only and convention-based. The BOI daemon does
not mechanically enforce verify before DONE transitions. Interactive sessions have no verification
gates. No E2E test suite exists for the hex binary itself. Verification quality varies from
behavioral (good) to existence-only (`test -f file` — not good). This theme contains the highest
density of HIGH severity findings.

**Backlog items:** #4 (verify mechanical enforcement), #6 (hex test suite), #9 (WIP enforcement)

---

### Theme D — Observability Gap
**Lectures:** 01, 11  
**Summary:** Hex has a well-designed observability architecture (trail schema, event engine,
telemetry infrastructure) but the read path is broken: `hex agent audit` is stubbed as "not yet
implemented." All trail data written by agents is currently discarded. Implementing the read side
is a days-scale task (schema is done) and immediately unlocks the entire observability value
proposition.

**Backlog items:** #5 (trail audit), #12 (observability coverage)

---

## Prioritized backlog

*(Full detail for each item follows — ordered by impact × leverage)*

---

### #1 — `agents-md-verification`
**Title:** Add verification commands, tech stack summary, and cold-start answers to AGENTS.md  
**Severity:** HIGH | **Effort:** hours (1–4 hours, no code changes)  
**Depends on:** none

**Principles addressed:**
- L01: AGENTS.md as foundation
- L02: Feedback subsystem (highest-ROI harness investment)
- L03: Standardized entry file (answers only 2/4 cold-start questions)

**Current state:** `templates/AGENTS.md` is 66 lines. It covers directory structure and 6
behavioral rules. A cold-start agent cannot answer "how do I verify my changes work?" or "what
is hex's tech stack?"

**Desired state:** AGENTS.md answers all four cold-start questions. Contains a "Verification
commands" section (`cargo test`, `make test`, `hex doctor`, subsystem checks) and "Tech stack"
section (Rust binary, Python scripts, YAML specs, hex-events). Linked from CLAUDE.md opening.

**Acceptance:**
- [ ] `AGENTS.md` has "Tech stack" section (Rust binary, Python scripts, YAML specs, hex-events)
- [ ] `AGENTS.md` has "Verification commands" section listing `cargo test`, `hex doctor`, subsystem-specific checks
- [ ] `AGENTS.md` has "Current progress" pointer (where to find todo.md, landings/, handoffs/)
- [ ] Fresh agent reading only AGENTS.md can answer all 4 cold-start questions without reading CLAUDE.md
- [ ] CLAUDE.md opening section links to AGENTS.md explicitly

---

### #2 — `claude-md-decomposition`
**Title:** Decompose 563-line CLAUDE.md into a ≤150-line router and topic documents  
**Severity:** HIGH | **Effort:** weeks (1–2 weeks; agent behavior validation required)  
**Depends on:** #1

**Principles addressed:**
- L02: Instruction subsystem (2–10× over limit)
- L04: Entry file limit, topic documents, progressive disclosure, instruction SNR, instruction governance (all MISSING)

**Current state:** 563 lines covering session lifecycle, onboarding, learning engine, improvement
engine, 18 standing orders, landing protocol, hex-events, BOI dispatch, and memory system — all
in one flat document. Every session reads all of it regardless of task type. Middle-of-document
content (lines 100–450) has high probability of effective dismissal.

**Desired state:** CLAUDE.md ≤150 lines: identity, system overview, links to topic documents,
and 5–8 truly universal rules. All remaining content lives in `docs/harness/` topic documents.
Standing-orders table adds `source`, `applies-when`, and `review-by` columns. Instruction SNR
improves from ~5% to ~70% for simple tasks.

**Acceptance:**
- [ ] `templates/CLAUDE.md` is ≤150 lines (target: 80–120)
- [ ] All current content exists verbatim in topic documents under `docs/harness/`
- [ ] CLAUDE.md has explicit links to each topic document
- [ ] Topic documents each have a one-line scope declaration at the top
- [ ] Cold-start agent reading CLAUDE.md + task-relevant topic needs no other topics
- [ ] Standing-orders table has `source`, `applies-when`, `review-by` columns

---

### #3 — `session-lifecycle-state`
**Title:** Introduce PROGRESS.md schema, five-dimension exit checklist, and startup verification  
**Severity:** HIGH | **Effort:** days (3–5 days; skill modifications + template creation)  
**Depends on:** none

**Principles addressed:**
- L02: State subsystem (MISSING)
- L05: PROGRESS.md schema, session clock-in/out (MISSING/PARTIAL)
- L06: Initialization acceptance checklist (MISSING)
- L12: Five-dimension clean state, session exit checklist (PARTIAL/MISSING)

**Current state:** Three disconnected state artifacts: `todo.md` (priorities only), `landings/`
(daily outcomes, freeform), `raw/handoffs/` (freeform session summaries). Startup reads priorities
but doesn't verify build/test status. Shutdown writes context but has no "tests pass" precondition.
Time-to-useful-state after compaction is undefined.

**Desired state:** Schema-enforced `PROGRESS.md` written at session end and read at session start.
Contains: current git commit hash, build status, test status, in-progress items (with blockers),
next items, open decisions. Startup skill reads PROGRESS.md and runs verification before
presenting to user. Shutdown enforces a five-dimension checklist: build passes, tests pass,
progress updated, temp artifacts cleaned, startup path verified.

**Acceptance:**
- [ ] `templates/PROGRESS.md` schema document exists with required fields (commit, build, tests, in-progress, next, decisions)
- [ ] `system/skills/hex-startup/SKILL.md` reads PROGRESS.md and runs at least `cargo build` before presenting state
- [ ] `system/skills/hex-shutdown/SKILL.md` writes PROGRESS.md and verifies build passes before completing
- [ ] Shutdown skill has an explicit five-dimension checklist visible in the SKILL.md
- [ ] After context compaction, fresh session rebuilds state from PROGRESS.md alone in <2 minutes

---

### #4 — `verify-mechanical-enforcement`
**Title:** Make BOI daemon enforce verify-gate at the DB level before accepting DONE status  
**Severity:** HIGH | **Effort:** weeks (2–3 weeks; daemon changes, DB schema migration)  
**Depends on:** #3

**Principles addressed:**
- L01: Explicit definition of done
- L07: VCR monitoring (MISSING)
- L08: Pass-state gating, state machine model (MISSING/PARTIAL)
- L09: Three-layer verification, completion priority constraint

**Current state:** BOI spec says "IMPORTANT: Before marking DONE you MUST run verify commands"
— this is an instruction to the agent, not a mechanical gate. The daemon (`~/.boi/boi.db`)
does not run verify before accepting a DONE transition. Workers can declare DONE without running
verify and nothing stops them.

**Desired state:** BOI daemon runs the task's `verify:` shell command before persisting a DONE
transition. If verify fails, task moves to FAILED (not DONE), with output captured. A Verified
Completion Rate (VCR) metric is tracked per-spec and surfaced in `boi status`. New task activation
is blocked (or flagged) when VCR < 1.0. A "blocked" state is added for tasks where verify fails
and needs human intervention.

**Acceptance:**
- [ ] `~/.boi/` daemon executes `verify:` shell command before writing DONE to `boi.db`
- [ ] If verify exits non-zero, task transitions to FAILED with verify output captured
- [ ] BOI state machine has "blocked" state (PENDING → ACTIVE → DONE | FAILED | BLOCKED)
- [ ] `boi status` shows VCR (verified tasks / total DONE tasks) per spec
- [ ] A worker that skips verify cannot advance task to DONE through the normal path

---

### #5 — `trail-audit-implementation`
**Title:** Implement `hex agent audit` command (currently stubbed as "not yet implemented")  
**Severity:** HIGH | **Effort:** days (3–5 days; schema already designed in `gate.rs`)  
**Depends on:** #1

**Principles addressed:**
- L11: Task traces (PARTIAL — traces written, never read)
- L01: Diagnostic loop (attribution requires readable trace data)
- L11: Harness-level signal collection

**Current state:** `system/harness/src/gate.rs` defines a rich trail schema (types: observe,
find, decide, act, verify, delegate). The `decide` type requires `alternatives` and `reasoning`.
Trails are written per-agent to `~/.hex/agents/<id>/trail.jsonl`. `Commands::Audit` in `main.rs`
L1108 returns "not yet implemented." All trail data is currently discarded.

**Desired state:** `hex agent audit <agent-id>` renders a human-readable summary of an agent's
trail: decisions with alternatives considered, actions taken, verifications run, delegates
spawned. Output is agent-readable (Markdown) and machine-queryable (JSON flag). Hex-startup
automatically runs `hex agent audit --last` and appends a one-paragraph summary to PROGRESS.md.

**Acceptance:**
- [ ] `hex agent audit <id>` produces output (not "not yet implemented")
- [ ] Output shows: session duration, decisions (with alternatives + reasoning), actions, verifications (pass/fail), delegates
- [ ] `hex agent audit --json <id>` produces machine-readable JSON
- [ ] `hex agent audit --last` resolves the most recent completed session
- [ ] `system/skills/hex-startup/SKILL.md` references `hex agent audit --last` in context-loading sequence

---

### #6 — `hex-test-suite`
**Title:** Build E2E and integration test suite for the hex binary against a sample workspace  
**Severity:** HIGH | **Effort:** weeks (2–4 weeks; new code, fixture workspace, CI configuration)  
**Depends on:** #1, #3

**Principles addressed:**
- L10: E2E test mandate (MISSING), testing adequacy gradient (MISSING), architectural boundary enforcement (MISSING)
- L12: Session integrity verification
- L09: Runtime signal capture

**Current state:** One Rust test exists (`main.rs` L1492–1510, CLI smoke test). No integration
tests. No E2E tests. No CI pipeline in hex-foundation. Correctness is verified entirely manually.

**Desired state:** Test suite at `system/harness/tests/` with: (1) ≥10 unit tests for core
functions, (2) ≥5 integration tests against a fixture workspace, (3) one E2E test walking a
complete session lifecycle. GitHub Actions workflow runs `cargo test` on every PR.

**Acceptance:**
- [ ] `cargo test` in `system/harness/` runs ≥10 unit tests
- [ ] `cargo test --test integration` runs ≥5 integration tests against a fixture workspace
- [ ] One E2E test validates: `hex startup` → `hex doctor` passes → `hex shutdown` writes PROGRESS.md → `hex startup` reads it
- [ ] GitHub Actions workflow runs `cargo test` on every push
- [ ] `AGENTS.md` "Verification commands" section updated to include `cargo test`

---

### #7 — `bootstrap-contract`
**Title:** Add bootstrap contract template and initialization acceptance checklist  
**Severity:** HIGH | **Effort:** days (2–3 days; template creation + doctor extension)  
**Depends on:** #1, #3

**Principles addressed:**
- L06: Bootstrap contract (MISSING), initialization acceptance checklist (MISSING)
- L02: Environment subsystem (Python environment not reproducibly specified)

**Current state:** `templates/` has warm-start files but no bootstrap contract. After `hex new`,
no checklist verifies proper initialization. Python scripts have no `requirements.txt`.

**Desired state:** `templates/bootstrap-contract.md` with 4-condition checklist: (1) environment
runnable (`hex doctor` passes), (2) verification commands known and passing, (3) contract filled
out with stack + constraints, (4) tasks broken down in todo.md. `requirements.txt` or
`pyproject.toml` in `system/` for Python reproducibility.

**Acceptance:**
- [ ] `templates/bootstrap-contract.md` exists with 4 sections (env, tests, contract, tasks)
- [ ] `system/` has `requirements.txt` or `pyproject.toml` with pinned Python dependencies
- [ ] `hex doctor` or `hex startup` on fresh workspace walks bootstrap-contract checklist
- [ ] Workspace cannot enter PROGRESS.md-tracked state without bootstrap contract signed off
- [ ] Bootstrap contract documented in AGENTS.md under "Initialization"

---

### #8 — `feature-list-workspace`
**Title:** Introduce feature_list.json as hex workspace's single source of truth for capabilities  
**Severity:** MEDIUM | **Effort:** days (3–5 days; schema + template + skill modifications)  
**Depends on:** #3

**Principles addressed:**
- L07: Scope surface externalization (PARTIAL)
- L08: Triple structure, single source of truth, harness dependency on feature list (PARTIAL/MISSING)

**Current state:** For BOI work, specs serve as a triple-structured feature list. For interactive
work, `todo.md` is the only tracker — markdown-only, not machine-queryable. `todo.md`, `landings/`,
and BOI specs can diverge for the same feature.

**Desired state:** `feature_list.json` in workspace root tracks all capability-level features.
Each entry: `id`, `title`, `behavior`, `verify`, `status` (not_started | active | blocked |
passing), `owner`. Hex-startup reads it; hex-shutdown updates it. `todo.md` becomes a priority
pointer, not a source of truth.

**Acceptance:**
- [ ] `templates/feature_list.json` template exists with schema documented
- [ ] `system/skills/hex-startup/SKILL.md` reads feature_list.json and shows passing/blocked counts
- [ ] `system/skills/hex-shutdown/SKILL.md` updates feature_list.json status for touched features
- [ ] BOI spec format has optional `feature_ref:` field linking to feature_list.json entries
- [ ] Workspace with 5+ features can answer "what is the current passing rate?" automatically

---

### #9 — `wip-enforcement`
**Title:** Enforce WIP=1 in interactive sessions and require completion evidence before new activation  
**Severity:** MEDIUM | **Effort:** days (1–2 days; CLAUDE.md edits + quality doc)  
**Depends on:** #2, #8

**Principles addressed:**
- L07: WIP=1 enforced (CLAUDE.md rule 3 **contradicts** WIP=1), completion evidence quality (PARTIAL)
- L09: Completion priority constraint (MISSING)

**Current state:** CLAUDE.md rule 3 explicitly permits "2+ independent tasks run simultaneously."
This is the only harness document that actively contradicts a core course principle. Verification
quality varies: `test -f` checks are counted as verification.

**Desired state:** CLAUDE.md and AGENTS.md establish WIP=1 for interactive sessions (with explicit
parallel-research exception). Verification quality standard distinguishes behavioral evidence
(command output, API response, test pass) from existence evidence. CLAUDE.md rule 3 is rewritten.

**Acceptance:**
- [ ] CLAUDE.md §"Standing Orders" WIP rule rewritten to enforce WIP=1 with explicit parallel-research exception
- [ ] Verification quality standard document exists: behavioral evidence ≥ existence evidence
- [ ] BOI critic guidance flags `verify: test -f` as low-quality verification
- [ ] `feature_list.json` or PROGRESS.md tracks active item count; startup alerts if >1 item ACTIVE
- [ ] CLAUDE.md rule 3 change rationale-documented in `me/decisions/`

---

### #10 — `decisions-consolidation`
**Title:** Consolidate per-file decision records into a searchable DECISIONS.md index  
**Severity:** MEDIUM | **Effort:** days (1–2 days; template additions + skill modifications)  
**Depends on:** #3

**Principles addressed:**
- L05: DECISIONS.md schema (per-file, not consolidated — PARTIAL)
- L03: ACID state management, cold-start test
- L05: Compaction-safe state

**Current state:** Per-decision files in `me/decisions/`. Good schema in `decision-template.md`
but fragmented. Fresh session after compaction must search file-by-file for architectural
constraints.

**Desired state:** `DECISIONS.md` index at repo root: one row per decision with date, title,
status (active | superseded), link to full per-file record. Hex-startup includes last 5 active
decisions in session brief. Governance metadata (source, applicability, review-by) added to template.

**Acceptance:**
- [ ] `DECISIONS.md` index exists with consolidated table of all active decisions (≥5 rows for mature workspace)
- [ ] `templates/decision-template.md` has added fields: `source`, `applies-when`, `review-by`
- [ ] `system/skills/hex-startup/SKILL.md` reads DECISIONS.md and includes recent decisions in startup brief
- [ ] `system/skills/hex-shutdown/SKILL.md` prompts agent to add new decisions to DECISIONS.md
- [ ] Cold-start agent identifies all current architectural constraints within 2 minutes from DECISIONS.md alone

---

### #11 — `module-architecture-docs`
**Title:** Add ARCHITECTURE.md to each subsystem directory  
**Severity:** MEDIUM | **Effort:** days (2–4 days; writing only, no code changes)  
**Depends on:** #2

**Principles addressed:**
- L03: Knowledge next to code (MISSING), update knowledge with code (MISSING)
- L12: Quality document (MISSING)

**Current state:** `system/harness/src/`, `system/skills/`, `system/scripts/`, `system/commands/`
have no ARCHITECTURE.md. Agents working on subsystems must reverse-engineer intent from code.

**Desired state:** Each top-level subsystem has ARCHITECTURE.md (≤100 lines): what it does, key
data flows, design decisions made and rejected, how to verify. Each file has a quality checklist
(A/B/C rating across: tests exist, docs current, verify command works).

**Acceptance:**
- [ ] `system/harness/ARCHITECTURE.md` covers: binary entry points, state model, trail schema, integration bundle system
- [ ] `system/skills/ARCHITECTURE.md` covers: SKILL.md format, invocation model, harness state access
- [ ] `system/events/ARCHITECTURE.md` exists (expand from existing partial docs)
- [ ] Each ARCHITECTURE.md has quality checklist with A/B/C ratings for tests, docs, verify
- [ ] CLAUDE.md topic document has rule: "Changes to subsystem require ARCHITECTURE.md review"

---

### #12 — `observability-coverage`
**Title:** Expand telemetry and event coverage to harness internals  
**Severity:** MEDIUM | **Effort:** weeks (2–3 weeks; Rust changes + sprint contract template)  
**Depends on:** #5, #3

**Principles addressed:**
- L11: Harness-level signal collection (PARTIAL), observability from design (PARTIAL), sprint contracts (MISSING), evaluator rubrics (PARTIAL)

**Current state:** Good foundation: `telemetry.rs`, event engine, `hex-events`. Gaps: startup,
shutdown, skill invocations emit no structured events. Memory operations lack telemetry. Sprint
contracts don't exist.

**Desired state:** Every harness lifecycle event emits a structured telemetry event: session start,
session end (with five-dimension checklist results), skill invocations, BOI dispatch, checkpoint.
Sprint contract template added for scoped work.

**Acceptance:**
- [ ] `hex startup` emits `session.start` event to hex-events with session ID and config hash
- [ ] `hex shutdown` emits `session.end` event with five-dimension checklist results
- [ ] Skill invocations (hex-startup, hex-shutdown, hex-reflect, boi-delegation) emit `skill.invoked` events
- [ ] `~/.hex-events/events.db` contains session lifecycle events for last 5 sessions
- [ ] `templates/sprint-contract.md` exists with: scope-in, scope-out, acceptance criteria, done-when

---

### #13 — `error-messages-standard`
**Title:** Adopt ERROR/WHY/FIX format for all hex binary error messages  
**Severity:** LOW | **Effort:** days (1–2 days; macro definition + call-site migration)  
**Depends on:** #11

**Principles addressed:**
- L09: Actionable error feedback (PARTIAL)
- L10: Agent-oriented error messages (MISSING)

**Current state:** Hex binary errors are terse (e.g., `"ERROR: HEX_DIR does not contain CLAUDE.md"`)
— WHAT only, no WHY or FIX. Agents must infer remediation from error text.

**Desired state:** All user-visible errors follow ERROR/WHY/FIX structure. A Rust helper macro
ensures format consistency.

**Acceptance:**
- [ ] `hex_error!` macro in `system/harness/src/` formats errors with WHAT/WHY/FIX
- [ ] All `eprintln!("ERROR: ...")` calls in main.rs migrated to new format (≥10 call sites)
- [ ] Each FIX section includes a concrete command or file path
- [ ] `system/harness/ARCHITECTURE.md` documents the error message format standard
- [ ] `cargo test` includes at least one test verifying error message format for a known-bad input

---

### Backlog summary table

| # | ID | Severity | Effort | Depends on |
|---|-----|----------|--------|------------|
| 1 | agents-md-verification | HIGH | hours | — |
| 2 | claude-md-decomposition | HIGH | weeks | #1 |
| 3 | session-lifecycle-state | HIGH | days | — |
| 4 | verify-mechanical-enforcement | HIGH | weeks | #3 |
| 5 | trail-audit-implementation | HIGH | days | #1 |
| 6 | hex-test-suite | HIGH | weeks | #1, #3 |
| 7 | bootstrap-contract | HIGH | days | #1, #3 |
| 8 | feature-list-workspace | MEDIUM | days | #3 |
| 9 | wip-enforcement | MEDIUM | days | #2, #8 |
| 10 | decisions-consolidation | MEDIUM | days | #3 |
| 11 | module-architecture-docs | MEDIUM | days | #2 |
| 12 | observability-coverage | MEDIUM | weeks | #5, #3 |
| 13 | error-messages-standard | LOW | days | #11 |

**Total:** 13 items — 7 HIGH, 5 MEDIUM, 1 LOW  
**Estimated total effort:** ~3 months of focused work (parallelism possible after critical path lands)

---

## Suggested initial BOI specs

*These become the first specs dispatched once Rustification completes.*

---

### Spec A — `agents-md-and-session-state`

```yaml
title: "Fix AGENTS.md cold-start gaps + introduce PROGRESS.md schema"
mode: implement
workspace: /Users/mrap/.boi/worktrees/<new-worktree>
context: |
  Backlog items #1 and #3 — no dependencies, high impact, hours to days.
  Source: docs/refactor/harness-engineering-audit-2026-05-15.md

tasks:
  - id: T001
    title: "Expand AGENTS.md with verification commands, tech stack, progress pointer"
    spec: |
      Edit templates/AGENTS.md to add:
        ## Tech stack
        ## Verification commands (cargo test, hex doctor, subsystem-specific)
        ## Current progress (where to find todo.md, landings/, handoffs/)
      AGENTS.md must answer all 4 cold-start questions without needing CLAUDE.md.
    verify: "wc -l templates/AGENTS.md && grep -q 'Verification commands' templates/AGENTS.md"

  - id: T002
    title: "Create templates/PROGRESS.md schema document"
    spec: |
      Create templates/PROGRESS.md with required fields:
        git commit hash, build status, test status, in-progress items,
        next items, open decisions pointer.
      Update system/skills/hex-startup/SKILL.md to read PROGRESS.md.
      Update system/skills/hex-shutdown/SKILL.md to write PROGRESS.md and
      verify build passes before completing.
    verify: "test -s templates/PROGRESS.md && grep -q 'PROGRESS.md' system/skills/hex-startup/SKILL.md"
    depends: [T001]
```

---

### Spec B — `claude-md-decomposition`

```yaml
title: "Decompose 563-line CLAUDE.md into ≤150-line router + topic documents"
mode: implement
workspace: /Users/mrap/.boi/worktrees/<new-worktree>
context: |
  Backlog item #2 — depends on #1. High impact, 1-2 weeks.
  Source: docs/refactor/harness-engineering-audit-2026-05-15.md
  IMPORTANT: Validate agent behavior after decomposition before shipping.

tasks:
  - id: T001
    title: "Audit CLAUDE.md content and categorize into topic buckets"
    spec: |
      Read templates/CLAUDE.md (563 lines). Categorize each section:
        - Universal (keep in CLAUDE.md)
        - Session lifecycle → docs/harness/session-lifecycle.md
        - BOI dispatch → docs/harness/boi-dispatch.md
        - hex-events → docs/harness/hex-events.md
        - Memory system → docs/harness/memory-system.md
        - Standing orders → docs/harness/standing-orders.md
      Output: /tmp/claude-md-topic-map.md
    verify: "test -s /tmp/claude-md-topic-map.md"

  - id: T002
    title: "Create topic documents and rewrite CLAUDE.md as router"
    spec: |
      Create each topic document per /tmp/claude-md-topic-map.md.
      Rewrite templates/CLAUDE.md to ≤150 lines with links to topic docs.
      Add source/applies-when/review-by columns to standing-orders table.
    verify: |
      wc=$(wc -l < templates/CLAUDE.md)
      [ "$wc" -le 150 ] || { echo "CLAUDE.md is $wc lines, must be ≤150"; exit 1; }
      ls docs/harness/*.md | wc -l
    depends: [T001]
```

---

### Spec C — `trail-audit-and-verify-enforcement`

```yaml
title: "Implement hex agent audit + BOI verify mechanical enforcement"
mode: implement
workspace: /Users/mrap/.boi/worktrees/<new-worktree>
context: |
  Backlog items #5 and #4 — both are load-bearing for reliability.
  #5 is days (schema exists in gate.rs); #4 is weeks (daemon changes).
  Source: docs/refactor/harness-engineering-audit-2026-05-15.md

tasks:
  - id: T001
    title: "Implement hex agent audit subcommand in Rust"
    spec: |
      In system/harness/src/main.rs, implement Commands::Audit.
      Read trail.jsonl from ~/.hex/agents/<id>/trail.jsonl.
      Render: session duration, decisions (alternatives + reasoning),
      actions, verifications (pass/fail), delegates.
      Add --json flag for machine-readable output.
      Add --last flag to resolve most recent completed session.
    verify: |
      hex agent audit --last 2>&1 | grep -v "not yet implemented" || exit 1
      hex agent audit --json --last 2>&1 | python3 -m json.tool > /dev/null
    depends: []

  - id: T002
    title: "Add verify enforcement to BOI daemon before DONE transitions"
    spec: |
      In the BOI daemon (~/.boi/), before persisting DONE to boi.db:
        1. Run the task's verify: shell command
        2. If exits non-zero: transition to FAILED, capture output
        3. Add "blocked" state to state machine for human-intervention cases
        4. Track VCR (verified tasks / total DONE) per spec
        5. Surface VCR in boi status output
    verify: |
      # Create a test spec with a failing verify, attempt DONE transition
      # Confirm task moves to FAILED, not DONE
      boi status | grep -q "VCR"
    depends: [T001]
```

---

### Spec D — `hex-test-suite-foundation`

```yaml
title: "Build initial Rust test suite and CI pipeline for hex-foundation"
mode: implement
workspace: /Users/mrap/.boi/worktrees/<new-worktree>
context: |
  Backlog item #6 — depends on #1 and #3.
  Entire Lecture 10 (E2E testing) is currently MISSING for hex.
  Start with unit + integration; E2E lifecycle test is the milestone.
  Source: docs/refactor/harness-engineering-audit-2026-05-15.md

tasks:
  - id: T001
    title: "Add ≥10 unit tests for core Rust functions"
    spec: |
      In system/harness/src/, add unit tests for:
        - trail entry validation (gate.rs)
        - state transition logic
        - config parsing
        - error message format (hex_error! macro)
    verify: "cargo test 2>&1 | grep 'test result' | grep -v '0 passed'"

  - id: T002
    title: "Add ≥5 integration tests against fixture workspace"
    spec: |
      Create system/harness/tests/fixtures/ with a minimal hex workspace.
      Write integration tests: hex doctor, hex startup (reads PROGRESS.md),
      hex agent audit --last (requires T001 in spec C).
    verify: "cargo test --test integration 2>&1 | grep -E 'test .* ok'"
    depends: [T001]

  - id: T003
    title: "Add GitHub Actions CI workflow"
    spec: |
      Create .github/workflows/test.yml running cargo test on every push.
      Include: toolchain setup, Cargo.lock caching, test run, results.
    verify: "test -f .github/workflows/test.yml && cat .github/workflows/test.yml | grep 'cargo test'"
    depends: [T002]
```

---

## Open questions for Mike

These require decisions only Mike can make — they are scoping, naming, or priority calls
that an agent should not make autonomously:

1. **CLAUDE.md decomposition timing:** The 563-line CLAUDE.md decomposition (#2) requires
   that Mike (or a trusted agent session) validate the decomposed behavior before shipping.
   Should this happen before Rustification completes (risky — dual refactor) or immediately
   after (recommended)? The decomposition should not be batched into Rustification.

2. **BOI daemon verify enforcement scope:** Item #4 (verify mechanical enforcement) changes
   how the BOI daemon works. The safest approach runs verify in the worker session (current
   convention), but the most reliable approach runs it in the daemon process (new code).
   Which end should own the verify gate — worker or daemon?

3. **PROGRESS.md read authority:** Should `hex-startup` block all work until PROGRESS.md
   passes its integrity check (strict), or read it informatively and continue anyway (permissive)?
   Strict is more correct per the course; permissive avoids lockout if PROGRESS.md is malformed.

4. **Feature list scope:** Should `feature_list.json` (#8) track hex workspace capabilities
   (what hex can do) or user project features (what the user's project does)? The course
   describes the latter; hex's current todo.md tracks the former. These are different use cases
   and may need different files.

5. **Rustification handoff:** Which of the 13 backlog items can be incorporated into the
   Rustification work already underway versus which must wait? Item #5 (trail audit) seems
   like a natural Rustification task; item #6 (test suite) must be post-Rustification since
   the binary structure is actively changing.

6. **WIP=1 exception scope:** CLAUDE.md rule 3 currently permits "2+ independent tasks
   simultaneously." The WIP=1 rewrite (#9) needs a clear definition of what constitutes a
   valid parallel-research exception. Mike should define this before the rule is rewritten
   so the exception isn't too permissive (defeating WIP=1) or too strict (blocking legitimate
   parallel subagent use).
