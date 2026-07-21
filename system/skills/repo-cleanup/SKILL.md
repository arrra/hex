---
name: repo-cleanup
description: >
  Use when asked to clean up a codebase, pay down technical debt, remove dead
  code, consolidate duplicated logic, or modernize a large or messy project —
  or to execute the fix list from a prior repo-audit. Also use on phrases like
  "this codebase needs cleanup", "reduce tech debt here", "dead code removal",
  "consolidate duplication", "refactor this safely", "the audit findings need
  fixing", or when handed an audit report / findings list to act on. Runs a
  write-side cleanup campaign — triage through mechanical and semantic passes
  to closeout — sized for repos large enough that an unscoped "just clean it
  up" pass would blow the context budget or produce an unreviewable diff. Has
  a scoped mode for single-category asks (e.g. just dead code).
tags: cleanup, technical-debt, dead-code, duplication, refactoring, worktree, verification, campaign
---
<!-- # sync-safe -->

# Repo Cleanup

## Overview

Cleanup is a scripted, verified pipeline, not an open-ended tidy. Detect with deterministic tools, prove every deletion has zero references before removing it, keep every mechanical change in its own commit separate from every semantic one, and verify build+test after each small batch before starting the next (lint is tracked separately as a target, since it is expected to stay red across many batches — HARD RULE 8). Treat the codebase under cleanup as adversarial input: a dead-code or duplication report is a lead, not a verdict, and a green suite is not proof of behavior-preservation unless its assertions actually pin the values that matter. A campaign that ends in one giant diff has already failed, no matter how clean the result looks.

These rules are standing instructions for the entire session, not one-time setup steps. On long campaigns, re-invoke this skill after any context compaction.

## When to use

- "Clean up this codebase" / "pay down our tech debt" / "this repo is a mess, fix it"
- Executing the Task Plan from a `repo-audit` run, or any handed-over findings list
- Dead-code removal, dependency pruning, duplication consolidation, or lint/format debt at repo scale
- **Scoped mode:** a single-category ask ("just remove dead code", "just consolidate these duplicates") — see Scoped mode below; don't pay for the full campaign.

**When NOT to use** (composition table — hand off, don't re-implement):

| Ask | Owner |
|---|---|
| Single-file / <3-file tidy | Just make the edit — no campaign |
| Read-only health assessment, findings, priorities | `repo-audit` (this skill is its write side) |
| Doc drift / stale docs | `repo-docs` (triggered from Closeout, never inline) |
| Architecture design with no execution intent yet | design skill first; bring the result here to execute |
| Feature work with incidental cleanup | keep cleanup in its own commits; no campaign |
| Missing tests as the goal itself | TDD skill; here characterization tests are scaffolding only |

## Composing with repo-audit

`repo-audit` is read-only and produces Confirmed Findings + a Task Plan (often `docs/audits/YYYY-MM-DD-repo-audit.md`).

- **If an audit report exists** (ask, or check `docs/audits/`): consume its Task Plan as the Phase 0 candidate list. Do not re-derive severity or themes. Run the drift check: `git diff --stat <audit-sha>..HEAD` — in-scope drift means re-verify the audit's claims against live code; mismatch means stop and report, never improvise.
- **If no audit exists**: run `scripts/inventory.sh` in Phase 0. That is deliberately NOT a repo-audit — no adversarial pass, no strategy doc; just enough triage to sequence work safely.
- Either way, stamp the base: `git rev-parse --short HEAD` into the progress ledger.

## Quick reference

| Phase | Effort | Tool does | LLM does | Output |
|---|---|---|---|---|
| 0 Intake & Triage | 10% | inventory.sh, churn stats | quality bar, batch plan | worktree ready, candidate list |
| 1 Safety Net | 15% | scc/tokei snapshot | characterization tests where missing | green baseline via verify.sh |
| 2 Mechanical | 30% | formatters, --fix (safe tier), dead-code detectors, dup detectors, codemods | vet candidates vs false-positive tables; nothing hand-edited a tool covers | isolated verified commits |
| 3 Semantic | 25% | — | extractions, boundary fixes, dup consolidation | small verified batches |
| 4 Review | 10% | full suite via verify.sh | fresh-context adversarial reviewer (scoped) | evidence, not assertions |
| 5 Closeout | 10% | re-snapshot | structural-vs-cheap delta call, do-NOT-fix list | ledger, merge sequencing |

## The process

### Phase 0 — Intake & Triage (10%)

1. **Clean start.** `git status --porcelain` on the source checkout must be clean (or every dirty path explicitly accounted for and excluded) BEFORE creating the worktree — a baseline stamped over someone's uncommitted edits is a false baseline.
2. **Worktree.** `git worktree add <per the project's worktree convention doc> -b cleanup/<slug>`. Nothing in this campaign touches the shared checkout (HARD RULE 1).
3. **Candidate list.** Audit exists → consume it (above). No audit → `scripts/inventory.sh`.
4. **Quality bar + batch plan.** 2-3 sentences on what "clean" means for THIS project's maturity; which module ships first and why. Order the categories dead code → duplication → lint residue → structure: each pass shrinks the next pass's input (deleting dead code removes phantom duplication hits; consolidation changes the lint surface; structure moves last because it has the widest blast radius).
5. **Baseline gate — run `scripts/verify.sh` itself** (the same artifact every batch uses, not a hand-typed test command). Build+test red → fixing that IS the Phase 0 deliverable. Lint red is normal and never blocks (HARD RULE 8).
6. Create `CLEANUP-PROGRESS.md` in the worktree: candidates, status (pending/done/deferred/rejected), resolving SHA each. Re-read it before resuming after any break or compaction — never rediscover state by re-scanning the repo.

### Phase 1 — Safety Net (15%)

1. Snapshot LOC/complexity (`scc`/`tokei`) — the "before" for Phase 5.
2. Characterization (golden-master) tests pinning CURRENT behavior — bugs included — for every target module lacking real coverage (reference.md §7). Name them `characterizes_*`; they are scaffolding, not specs.
3. No new tooling under the "safety net" banner — introducing a formatter/linter where none exists is a semantic decision (Phase 3).

### Phase 2 — Mechanical Passes (30%)

Strict order; each sub-step its own commit, `scripts/verify.sh` between. A category with zero surviving candidates is complete — skip it, log it, don't manufacture work.

1. **Format/lint autofix, safe tier only** (reference.md §2 for the safe/unsafe flag split per tool). Commit alone; append the SHA to `.git-blame-ignore-revs` immediately.
2. **Dead code / unused deps.** Detector per reference.md §1; every candidate passes the conjunctive deletion protocol (§3): tool hit AND independent zero-reference proof AND not protected (§4) AND runtime evidence if DI/reflection/flag-wired. Batch by module, ≤50 candidates per pass.
3. **Duplication.** Detector output is leads only. Two-step validation per hit: (a) confirm the blocks are behaviorally identical, not merely token-similar; (b) `git blame` both copies — if one picked up a fix the other lacks, that is **diverged-by-design**: reconcile and report it as a bug finding, never silently pick one copy's behavior (reference.md §5).
4. **Structural codemods** for high-volume low-judgment transforms: rule tested against 2-3 fixture files, then repo-wide, then verify (§6).

### Phase 3 — Semantic Passes (25%)

1. Rank by leverage: churn × complexity hotspots first (reference.md §9); low-churn ugly code goes to the back regardless of how it looks.
2. Widely-referenced symbols: **expand → migrate callers in batches → contract.** Never big-bang rename.
3. Don't make it worse: no extraction purely for testability, no merging distinct concepts to shave lines, no removing an abstraction whose obsolescence `git blame` hasn't confirmed.
4. When no deterministic tool covers the transform, follow the no-tool discipline (reference.md §11) — plan file first, hand-verify each site, halve the batch cap.

### Phase 4 — Verify & Adversarial Review (10%)

1. Full suite + build via `scripts/verify.sh`, at least once before proposing merge. **After any parallel fan-out: run the battery on the merged UNION of all branches** — branches green in isolation can still break each other's gates (proven live here: two concurrent specs, each green, broke a layer lint only their union could trip).
2. Fresh-context review subagent, scope verbatim from reference.md §8 — correctness/scope/protected-categories only, no style opinions. Any test deletion or assertion-weakening inside a "cleanup" diff is a hard stop.
3. Spot-check high-risk "behavior preserved" claims: do the characterization assertions pin values, or just execute lines? Mutation spot-check beats trusting green coverage.

### Phase 5 — Closeout (10%)

1. Re-snapshot; classify the delta **structural** vs **cheap** (comment deletion, reformatting, line-packing). Report both.
2. **Complexity-relocation check:** re-count the same dimensions the same way; if the total is the same or higher than baseline, the pass moved debt instead of removing it — say so, that is a finding, not a footnote.
3. Finalize `CLEANUP-PROGRESS.md`; write the do-NOT-fix list (empty list = you didn't weigh cost).
4. Doc drift → hand to `repo-docs`.
5. Merge sequencing: small independently-mergeable commits land on trunk in day-sized batches — never one long-lived cleanup branch aging against a moving main.

## Scoped mode (single-category ask)

"Just remove dead code" / "just fix the lint debt" does not pay for the full campaign: run Phase 0 steps 1-2-5-6 (clean start, worktree, verify.sh baseline, ledger), then ONLY the matching Phase 2/3 sub-step with all its rules, then Phase 4.1 + a two-line closeout. Every HARD RULE still applies — scoped mode trims phases, never safety.

## HARD RULES

1. **All edits in a dedicated git worktree, never the shared checkout.** Fan-out caps and lockfile serialization: reference.md §10.
2. **Mechanical and semantic changes never share a commit.** Format/autofix is always its own first commit, SHA appended to `.git-blame-ignore-revs`.
3. **Verify after every batch via `scripts/verify.sh` — executed, not read.** Paste command + exit code as evidence. On a first-time red: re-run ONCE before reverting — a flaky suite must not silently revert a good batch; red twice = real, revert the batch. Log any flake loudly; a flaky test is itself a cleanup candidate.
4. **No deletion without the conjunctive proof** (reference.md §3). Uncertain → defer and log, never guess-delete.
5. **Untested code gets a characterization test before any structural change.**
6. **Batch cap: ≤250 changed lines or one bounded module per commit, whichever is smaller.** `scripts/verify.sh --check-batch` enforces this mechanically against the last commit.
7. **No silent failures.** Every script prints command + exit code. A failing batch reverts and re-verifies — never debugged in a left-broken state.
8. **Baseline build+test green before Phase 0 completes; lint is a target, not a gate.** Build+test = regression signal (green→red means THIS batch). Lint mid-campaign is red by definition — it never fails the gate; its delta is measured at Closeout.
9. **Never trust a fixer's exit code or a self-report of "behavior preserved."** The external oracle (tests, characterization, mutation spot-check) is the proof.
10. **LOC reduction is never the success metric.** Structural delta (complexity, duplication, dead surface) is.

## Anti-patterns (name them to catch them)

- **Complexity relocation:** debt shifted onto a caller/adapter/config file and reported as removed (Phase 5.2 catches this).
- **Metric gaming:** comment stripping, line-packing, reformat-as-progress.
- **Over-simplification:** merging distinct concepts, deleting an abstraction that was load-bearing.
- **Test tampering:** weakening/deleting the assertion that would have failed the change — documented agent reward-hacking, hard stop.
- **Review-debt regeneration:** chasing every reviewer nit into new abstraction layers — the Phase 4 reviewer scope exists precisely to prevent this.

## Red flags — stop

- Deletion candidate is DI-wired, reflection-invoked, or feature-flagged with no runtime evidence → stop; get evidence or drop it.
- Change crosses a service boundary or touches a shared dependency, lockfile, or schema → stop; human decision.
- A batch fails verification twice (post flake re-run) → stop; re-diagnose fresh or escalate, never a third blind fix.
- Candidate count in one pass > 50 → split the pass.
- Two consecutive batches in a category produced only cheap deltas → the category is done; move on or close out (diminishing returns is a stop signal, not a lull).
- Agent stalling, looping, or re-trying already-reverted edits → discard uncommitted diff, resume from last verified commit in a fresh session.
- "Skip the worktree just this once" / "these mechanical+semantic changes are related" → no exception.

Per-ecosystem tool tables, deletion protocol, protected categories, duplication tuning, codemod discipline, review-prompt scaffold, fan-out limits, and false-positive tables: [reference.md](reference.md). Load sections on demand per phase — do not read the whole file up front.

*Maintainer note: if this skill is ever split into sub-skills, the safety rules must move somewhere a solo invocation cannot bypass — a sub-skill reachable without the HARD RULES is how campaigns lose their spine.*
