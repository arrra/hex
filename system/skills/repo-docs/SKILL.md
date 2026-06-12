---
name: repo-docs
description: >
  Idempotently updates and restructures a repository's documentation — agent instruction
  files (AGENTS.md / CLAUDE.md, root and nested) and human docs (README, docs/) — converging
  them to a router-plus-topic-doc architecture and fixing doc↔code drift with code-cited
  evidence; re-running on an unchanged repo (prior run merged) produces zero diff. Use when
  asked to "clean up the docs", "restructure CLAUDE.md / AGENTS.md", "split this giant
  instruction file", "docs are stale", "fix doc drift", "set up agent docs for this repo",
  or after an audit confirms documentation findings worth fixing.
tags: docs, agents-md, instruction-files, drift, routing, idempotent
---
<!-- # sync-safe -->

# Repo Docs

## Overview

Update a repository's documentation as an instruction architect whose edits must survive byte-level review: entry files become routers — a small always-loaded core, detail behind one-hop pointers — and every claim left standing is verified against code. Converge to a target state; never regenerate files (regeneration means style drift, hallucination, unreviewable diffs). Unlike repo-audit, this skill WRITES: the deliverable is a branch plus a report.

**Non-negotiables** — stated first, recapped as red flags last; mid-file rules get missed:

1. **Isolate first.** Clean tree required — dirty tree → stop and ask. Work in a worktree (preferred) or fresh branch, never the default branch; record the default branch's SHA before any write. Never push unless asked, and never the default branch.
2. **Converge, don't churn.** Unchanged repo ⇒ zero diff on re-run (measured with the prior run merged); changed code ⇒ drift-driven deltas only. Every diff hunk traces to a CONFIRMED finding. Never reword, reflow, or reorder conforming text; no timestamps; preserve the author's voice.
3. **Drift needs a citation — in both directions.** A statement is drifted only after you read the contradicting code (cite doc line AND code file:line) AND verified the code isn't the regressed side (§5). Never invent target content.
4. **No line vanishes silently.** Every removed line maps to exactly one ledger row (FIX / MOVE / SPLIT / RETIRE).
5. **Provider-neutral structure.** Load-bearing mechanisms: entry files, nested AGENTS.md co-located with subsystems, plain markdown topic docs behind one-line pointers. Tool-specific layers are optional accelerators — except the thin entry-file bridge for tools that can't read AGENTS.md, which is core layout (§3).
6. **Docs-only writes; respect ownership.** Never modify source, CI, or config files in this pass — route those to Upstream proposals. Generated or sync-managed files (§6) get upstream proposals, never in-place edits.

## When to use

- "Clean up / restructure the docs", "split this giant AGENTS.md", stale or drifted docs
- Standing up agent docs in a repo that has none; normalizing divergent tool-specific instruction files into one canonical layout; fixing confirmed docs findings from an audit

**When NOT to use:** assessment with no writes wanted (use repo-audit); a single-document copy edit; generated API-reference output (fix the generator, not the output); pure prose-style polish.

## Quick reference

| Phase | Effort | Output |
|---|---|---|
| 0 Calibrate | 10% | Inventory + ownership map, isolated workspace, before-pass |
| 1 Hunt | 30% | Typed findings, each with a reproducible check |
| 2 Attack | 10% | CONFIRMED / DOWNGRADED / RETRACTED + attrition stats |
| 3 Plan | 15% | Typed ops + ledger (durable out-of-tree file) + do-NOT-touch list |
| 4 Apply | 20% | Batch A moves + link rebases, then Batch B content edits |
| 5 Converge | 15% | In-scope-empty second pass, conservation + churn gate, report |

Thresholds, the placement tree, drift recipes, edge cases, run lifecycle, and formats live in [reference.md](reference.md) — read it before Phase 1.

## The process

If no target repo was specified, ask which one. Work the phases in order.

### Phase 0 — Calibrate

1. **Safety:** clean `git status` or stop. Detect a previous run (`git branch --list`, `git worktree list`, open PRs): resume or clean up — never start a parallel run (§10). Record the default branch SHA; create the worktree/branch before any write.
2. **Inventory** documentation only: entry files, nested instruction files, `docs/`, co-located module docs (`git ls-files '*.md' '*.rst'`; `*.txt` only on request; LICENSE/NOTICE/COPYING, CHANGELOG, manifests, and build files are out of scope or read-only — §6). Record line count, audience, load trigger (always / co-located-lazy / on-request), file type (symlink mode 120000 = bridge — conforming, not a duplicate), and ownership: editable / upstream-managed / generated (markers and sync configs: §6).
3. **Learn the repo:** purpose, build/test/run commands from manifests and CI, module layout, state artifacts (progress / decisions / feature-state docs or equivalents), the ~5 most common task types, and any prior maintenance record (§10) — its declined-with-reason items are standing refutations unless the cited code changed.
4. **Before-pass:** which file answers: what is this · how is it organized / where are the rules for this area · how to run · how to verify · what's the current progress and next step? Note unanswered questions and orphan docs nothing points to.

### Phase 1 — Hunt

Verify claims and assess structure (per-claim recipes: §5; run commands only when cheap and side-effect-free). Every finding — drift or structural — gets a globally unique ID (namespace per hunter/dimension when hunting in parallel — colliding IDs cross-wire later verdicts) and records the exact reproducible check (command, path, threshold) that Phase 5 re-runs verbatim. Types:

- **DRIFT** — doc says X; code at file:line says Y. Quote both sides. Unverifiable claims stay UNVERIFIED and unedited.
- **DEAD** — subject no longer exists or its expiry condition arrived; cite the search proving it.
- **MISPLACED** — wrong layer: load-bearing rule mid-file, area rule in root, encyclopedic detail always-loaded, agent detail cluttering README, content already visible in code.
- **OVERSIZE / GAP** — a file over budget (§1), or a before-pass question (including progress/state) no doc answers.
- **CONFLICT / DUPLICATE** — two files disagree or repeat a rule. Exempt: entry-file bridges (symlink/include) and bottom-of-file recap lines (§1). Nested-override nuance: §3.

**Cross-doc claim sweep — mandatory, never sampled.** Conflicts found only where a reader happens to compare two files are luck, not coverage. Build a claim table over the FULL in-scope inventory: every normative claim (command, path, format/contract, version, threshold, behavior rule) keyed by subject, one row per file stating it. Two files with non-identical statements on one subject = CONFLICT candidate; byte-identical = DUPLICATE candidate. On large doc sets, fan out one reader per doc and merge tables. The table persists in the maintenance record (§10) so future runs diff against it instead of rebuilding from scratch.

A finding without evidence is not a finding — drop it. Cap ~30 candidates — CONFLICTs outrank other types within the cap (a contradiction misleads on every read; a stale line misleads on one); findings beyond the cap are DEFERRED and listed in Open questions — never silently chased. Record what is already good — it must survive untouched.

### Phase 2 — Attack your own findings

Before any finding drives an edit, try to refute it: a "drifted" command may work via a wrapper or another cwd; an "orphan" may be referenced from code, CI, or doc-site config (grep its path); a "duplicate" may serve a different audience; "inferable from code" may encode a deliberate exception. For every DRIFT, run the direction check (§5): if tests, git history, or issues support the doc's claim, the code may be the regressed side → Open questions / upstream proposal, never a doc FIX. Verdict each: CONFIRMED (say what you checked; drift adds "doc-side because: <evidence>") / DOWNGRADED / RETRACTED. Report attrition: "N candidates → M confirmed, K downgraded, J retracted." Only CONFIRMED findings produce edits.

### Phase 3 — Plan

Translate confirmed findings into typed ops. Write the full plan + ledger to a durable file OUTSIDE the repo before the first write (§10); don't pause for approval unless asked. Run the inbound-reference scan on every MOVE / SPLIT / RETIRE source: `git grep` its path, filename, and section anchors repo-wide (code, CI, doc-site configs, manifests); consume the scan output IN FULL — a truncated scan (`head`, first-N) is a missed reference — and every hit blocks the op or becomes a ledgered link-rebase op. Never rename platform well-known files (§6).

- **FIX** — replace a drifted claim with text derived from the cited code, in the file's voice. The ledger row quotes the removed line(s) verbatim.
- **MOVE / SPLIT** — relocate verbatim to the layer the placement tree (§2) assigns; leave a one-line pointer with an applicability condition.
- **RETIRE** — delete with a reason: subject gone; byte-identical duplicate (near-duplicates that differ are CONFLICTs — a code citation picks the survivor); already visible in code; or fails BOTH audience tests — agent ("would removal cause agent mistakes?") and human ("would a reader lose needed information?"). Community / attribution / legal / history sections are exempt (§2).
- **ADD** — only what a fresh agent provably lacks: verified commands, structure map, state docs, missing pointers, the maintenance record (§10). Bootstrap and monorepo recipes: §7.

Close with a **do-NOT-touch list**: conforming-but-imperfect text left alone, protected files with their upstream routing, and declined improvements with reasons. Nothing outside the plan gets edited.

### Phase 4 — Apply

1. **Batch A — mechanical moves** (MOVE/SPLIT + entry-file bridge): content byte-identical at the destination, plus pointer lines and the planned link rebases (relative paths/anchors broken by relocation; inbound references from the Phase 3 scan). Purity check: the diff shows relocation, pointers, and link rebases — zero content alteration. Commit: `docs: restructure — mechanical moves only`.
2. **Batch B — content edits** (FIX/RETIRE/ADD): smallest-scope diffs, matching the surrounding voice. Commit: `docs: fix drift, retire stale content`.
3. Leave clean state: no scratch files in-repo, no half-applied moves, no pointers to paths that don't exist.

### Phase 5 — Converge and prove

1. **Proof pass — read-only, no writes, no commits, fresh eyes:** run it in a context that did not produce the edits (a fresh subagent/session where available) — self-review reliably misses what fresh context catches, including errors the fixes themselves introduced. Re-run the detection and planning of Phases 0–3 against the result using the recorded checks. Convergence = zero re-derived findings WITHIN the scope the plan addressed, plus an explicit "converged within scope; N deferred" statement. In-scope residue → re-enter Phases 3–4 for it and re-prove (max 3 rounds, then stop and report the instability; common causes: §9). Never measure convergence as a clean tree after more commits. Never ship "mostly idempotent".
2. **Churn gate:** every hunk in `git diff <base>..HEAD` traces to a CONFIRMED finding; untraceable hunks → revert. Any non-doc path in the diff → revert it.
3. **Conservation:** cross-check every removed line against the ledger, exactly one row each (command, self-test, and structural-line carve-out: §9).
4. **Reachability & links:** every topic doc one pointer hop from an entry file; every pointer carries an applicability condition; links resolve — run a link checker, or fail closed by grepping every old path and anchor.
4b. **Cohesion gate:** re-derive the claim table over the in-scope doc set (fresh eyes, same recipe as Phase 1). Any subject still carrying two divergent live statements is residue — the repo is not cohesive, and "converged" may not be claimed. Deliberate divergence (e.g. an instance template intentionally differing from the repo's own entry file) must be recorded as declined-with-reason in the maintenance record, or it counts as residue on every future run.
5. **After-pass:** a fresh agent, from repo contents alone, answers all five before-pass questions — including how to verify a change (ordered: unit → integration → e2e) and current progress. Each remaining gap becomes an ADD (if code-verified) or an Open question.
6. **Teardown:** push and open the PR (report file as body) only if asked; remove the worktree (§10).

## Deliverable

The branch (two commit groups) plus the report — drafted out-of-tree, delivered as the PR body; local-only fallback: the final commit message (§10): **Summary** (what changed, why; attrition; every touched path) · **Router map** (before → after: each doc, audience, load trigger, applicability condition) · **Drift fixes** (doc line + code citation + direction evidence) · **Change ledger** · **Convergence proof** (in-scope-empty second pass; conservation + churn-gate output) · **Upstream proposals** · **Open questions** (UNVERIFIED claims, DEFERRED findings, fresh-session gaps) · **Declined changes** (with reasons). The mapping table, reproducible checks, and declined/UNVERIFIED state also land in-repo in the maintenance record (§10).

## Red flags — stop and fix before delivering

- Work begun on a dirty tree, or any write on the default branch → stop; replay the work onto the work branch, hard-reset the default branch to its recorded SHA, verify with `git log`.
- Any write or commit during the proof pass → the proof is void; reset and re-prove.
- An edit inside a protected or sync-managed file, or any source/CI/config file → revert; route upstream.
- A FIX without a code citation and direction evidence → a guess; move it to Open questions.
- A diff hunk not traceable to a CONFIRMED finding → churn; revert it.
- A removed line with no ledger row → restore it or ledger it.
- Moves and content edits mixed in one commit → split them.
- A pointer without an applicability condition → the router can't route; add one.
- An existing entry file longer after restructuring than before → you added instead of routing (exempt: the file was missing or below the §1 budget floor before the pass).
- A tool-specific mechanism doing load-bearing routing (entry-file bridges exempt) → rebuild on the neutral palette.
- A non-empty second-pass plan within addressed scope → not converged; do not deliver. Accelerator commits after the proof (§8) → re-run the proof.
- Zero retractions in Phase 2 on a sizable doc set → you didn't attack hard enough (on a small or genuinely clean set, say so explicitly).
- CONFLICT findings only from incidental reading, no claim table built → cohesion was sampled, not verified; run the sweep before claiming convergence.