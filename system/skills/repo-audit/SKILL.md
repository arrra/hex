---
name: repo-audit
description: >
  Deep repository audit with adversarial verification: calibrate to the
  project's real quality bar, hunt across risk dimensions, attack your own
  findings before reporting, then deliver a strategy and milestone task plan.
  Use when the user asks to audit a repo, assess codebase health, "upgrade this
  project", find what's wrong with a codebase, or produce an improvement plan.
tags: audit, code-quality, repo, improvement-plan, verification
trigger: >
  User says "audit this repo", "repo audit", "how healthy is this codebase",
  "what should we improve here", "run an audit on <repo>", or asks for a
  prioritized improvement plan for an existing project.
---
<!-- # sync-safe -->

# Repo Audit — Deep Audit with Adversarial Verification

Won a 4-way blind bakeoff on 2026-06-10 (vs. the viral meta_alchemist 4-phase prompt, nud3l's 6-agent /code-audit, and claude-caliper team-mode; judged blind on groundedness / signal / actionability / coverage — see mrap-hex `projects/system-improvement/bakeoffs/repo-audit-prompt/2026-06-10/verdict.md`). The differentiators, in order of measured impact: maturity-calibrated severity, the attack-your-own-findings phase, and process-level coverage (git/CI/deploy state, not just code).

Run the prompt below against the target repository. If a repo isn't specified, ask which one. Analysis is read-only.

---

You are auditing this repository as a principal engineer whose reputation depends on every claim being true. Your output will be used to plan real engineering work, so a false finding costs more than a missed one. Work through the five phases in order.

## Phase 0 — Calibrate (10% of effort)

Before judging anything, establish:
1. **What this is:** purpose, users, maturity (prototype / personal tool / internal service / production / library). Read README, docs, manifests, CI config, and recent git log (`git log --oneline -30`) to see where active development happens.
2. **What "good" means here:** a weekend prototype, a personal automation tool, and a production service have different bars. Write down, in 3 sentences, the quality bar this project should be held to. Every severity rating in later phases must be calibrated against THIS bar, not an abstract enterprise standard.
3. **Where the core is:** identify the ~20% of code that does 80% of the work (entry points, hot paths, the modules git history touches most: `git log --format= --name-only -200 | sort | uniq -c | sort -rn | head -20`). Depth goes there; the periphery gets a lighter pass, and you say so.
4. **Process state, not just code:** check local-vs-origin divergence (`git status`, `git log --oneline @{u}..HEAD` and `HEAD..@{u}` where an upstream exists), recent CI run results if accessible, and whether any deployed artifact matches HEAD. Live operational drift outranks latent code smells.
5. **House conventions:** naming, error-handling idiom, module layout, test style — so recommendations fit the existing culture.

Output: a "Repo Map" — purpose, stack, quality bar statement, architecture sketch, core-vs-periphery split, conventions, and anything surprising.

## Phase 1 — Hunt (40% of effort)

Audit the dimensions below. If you have a subagent/Task tool, run dimensions as parallel subagents, each receiving the Repo Map; otherwise do them sequentially. Spend effort proportional to risk for THIS project — skip or compress dimensions that don't apply.

- **Correctness & error handling:** swallowed errors, unchecked results, race conditions, partial-failure states, resource leaks, missing edge cases on hot paths.
- **Architecture:** boundary violations, god modules, circular deps, abstractions that leak or that nothing uses, scalability cliffs.
- **Security:** secrets in code or history, injection, unsafe deserialization, permissions, auth gaps, dependency CVEs (run the ecosystem's audit tool if installed: `npm audit` / `cargo audit` / `pip-audit` / `govulncheck`).
- **Tests:** what core behavior has NO test, tests that assert nothing, tests coupled to internals, missing failure-path tests.
- **Performance:** only where it matters — hot paths, unbounded growth (queues, files, memory), blocking calls in async contexts.
- **Operability & DevEx:** can a newcomer build and run it from the README alone? CI gaps, lint enforcement, logging quality, silent failure modes.
- **Docs & drift:** docs that contradict code (cite both sides), dead instructions, undocumented critical behavior.

Hard rules:
- Every finding: what, where (file:line), concrete consequence ("if X happens, Y breaks" — not "violates best practice"), severity (Critical/High/Medium/Low) **calibrated to the Phase 0 quality bar**.
- Cite line numbers only from a Read you actually performed; re-check the number after any re-read. Never invent a file:line.
- Label each finding FACT (verified by reading the code) or JUDGMENT (opinion about design).
- When running dimensions as subagents, each must return ONLY structured findings — `severity | file:line | what | concrete consequence` — capped at 15 per dimension, so results merge cleanly.
- Cap: 25 candidate findings max. Prefer 12 that are load-bearing over 25 that pad.
- Record strengths too — what must be preserved.

## Phase 2 — Attack your own findings (15% of effort)

This phase is what separates a useful audit from a plausible-sounding one. For every Critical and High finding (and any finding you'll later build a task around):
1. Re-open the cited file and try to REFUTE the finding. Is there a guard you missed? Is the "dead code" actually called via reflection/config/CLI? Is the "missing test" covered by an integration test elsewhere? Does the "race" actually matter given how the code is invoked?
2. Where possible, verify empirically: run the build, run the test suite, run the linter, grep for callers. Cite the command and its result.
3. Verdict per finding: CONFIRMED (survived attack — say what you checked), DOWNGRADED (real but less severe — explain), or RETRACTED (count these; do not include them in the report body).
4. Tag findings flagged independently by more than one dimension pass/subagent — independent confirmation is a cheap confidence signal; note it on the finding.

Report the attrition: "N candidate findings → M confirmed, K downgraded, J retracted."

## Phase 3 — Strategy (15% of effort)

1. Name the 2–4 root themes that explain most confirmed findings (most repos have a small number of systemic causes, not 30 independent problems).
2. For each theme: target state + the principle behind it.
3. **The do-NOT-fix list:** explicitly name plausible-sounding improvements you are recommending AGAINST for this project, with reasons (effort vs. payoff, maturity, risk). This list is mandatory — an audit with no rejected ideas hasn't thought about cost.
4. Define measurable "done" signals per theme (CI gate exists, suite runs green in <N min, zero Criticals, etc.).

## Phase 4 — Plan (20% of effort)

Convert strategy into tasks an engineer (or coding agent) could pick up cold:
- Each task: title, one-paragraph description, files affected, acceptance criteria (verifiable, ideally a command), effort (S <2h / M half-day / L 1–2 days / XL needs breakdown), risk of the change itself, dependencies on other tasks.
- Milestones: M0 safety net (tests/CI needed before refactoring safely) → M1 correctness & security → M2 leverage (makes future work cheaper) → M3 polish.
- **Quick wins** (high impact, S effort) listed separately, ready to do today.
- For the top 3 tasks: a brief implementation sketch (approach, key steps, gotchas).

## Final deliverable — single document:
1. **Executive Summary** — health grade A–F with one-line justification, top 3 risks, top 3 opportunities, attrition stats from Phase 2. ≤10 sentences.
2. **Repo Map** (Phase 0)
3. **Confirmed Findings** — grouped by theme, sorted by severity; each with file:line, consequence, FACT/JUDGMENT label, and verification note from Phase 2. Then Strengths.
4. **Strategy** — themes, do-NOT-fix list, done signals.
5. **Task Plan** — milestone table + quick wins + top-3 sketches.
6. **Coverage & Open Questions** — which areas got lighter review, what you couldn't verify and why, and decisions that need a human (product intent, deprecation candidates, targets).

## Constraints
- Read-only: do NOT modify code. Running builds/tests/linters/audit tools is allowed and encouraged.
- No padding: a healthy dimension gets one sentence.

## hex integration

- Save the deliverable to `projects/<project>/audits/YYYY-MM-DD-repo-audit.md` (or the workspace path the user prefers); don't leave it only in chat.
- If the user wants the plan executed, convert M0/M1 tasks into a BOI spec (`boi-delegation` skill) — the task format above maps 1:1 onto `[[tasks]]` with `behavior` + `verifications`.
