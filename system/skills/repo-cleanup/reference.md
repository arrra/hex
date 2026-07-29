# Repo Cleanup — Reference

Loaded on demand from `SKILL.md`. Load the section the current phase needs — do not read the whole file up front.

## Table of contents

1. [Per-ecosystem tool table](#1-per-ecosystem-tool-table) — Phase 2
2. [Safe vs. unsafe fix tiers](#2-safe-vs-unsafe-fix-tiers) — Phase 2.1
3. [Dead-code deletion protocol](#3-dead-code-deletion-protocol) — Phase 2.2
4. [Protected-category checklist](#4-protected-category-checklist) — Phase 2.2
5. [Duplication: tuning + two-step validation](#5-duplication-tuning--two-step-validation) — Phase 2.3
6. [Structural codemods](#6-structural-codemods) — Phase 2.4
7. [Characterization / golden-master tests](#7-characterization--golden-master-tests) — Phase 1
8. [Adversarial review subagent prompt](#8-adversarial-review-subagent-prompt) — Phase 4
9. [Hotspot pre-flight](#9-hotspot-pre-flight) — Phase 3
10. [Fan-out and worktree limits](#10-fan-out-and-worktree-limits) — parallel work only
11. [No-tool discipline](#11-no-tool-discipline) — Phase 3.4
12. [Known false-positive classes by tool](#12-known-false-positive-classes-by-tool) — Phase 2.2

## 1. Per-ecosystem tool table

Detect the ecosystem from its manifest, then use this order. A project's own configured tools always beat generic ones.

| Manifest | Dead code / unused deps | Format + lint | Duplication | Structural codemod |
|---|---|---|---|---|
| `package.json` | knip (unified module graph) > depcheck (fallback, noisier on config-only deps) | biome (`--write` safe / `--write --unsafe` opt-in) or eslint `--fix` + prettier | jscpd (raise `minTokens`/`minLines` — §5) | ast-grep or jscodeshift |
| `Cargo.toml` | cargo-machete (fast, text-based; `--with-metadata` for renamed-import FPs) quick pass; cargo-udeps (nightly, compiles) for deep sweeps | `cargo fmt` + `cargo clippy --fix` | PMD CPD | ast-grep |
| `pyproject.toml`/`setup.py` | ruff (F401/F841/F811 — local only) **plus** vulture (`--min-confidence 100` — global reachability); they cover different blind spots | ruff (`check --fix` safe tier; `--unsafe-fixes` opt-in) + ruff format | jscpd / PMD CPD | ast-grep |
| `go.mod` | `go vet` / staticcheck; go.dev deadcode analyzer | `gofmt` + `golangci-lint --fix` | PMD CPD | ast-grep |

No manifest match → `scripts/inventory.sh` generic pass; say explicitly which language-specific step was skipped.

## 2. Safe vs. unsafe fix tiers

Key the auto-apply-vs-review boundary off the tool's OWN designation — don't invent a separate risk judgment:

- **Ruff:** `check --fix` safe by default; `--unsafe-fixes` may change runtime behavior — opt-in, gate behind a full test pass.
- **Biome:** `--write` semantics-preserving; `--write --unsafe` may change semantics and has documented infinite-loop bugs on specific rules — if used at all: one rule, one file, diff-reviewed.
- **ESLint:** `--fix` applies only `fixable`-tagged rules by design; suggestion-only rules need a human pick.
- **Clippy:** `--fix` has confirmed cases of emitting non-compiling or behavior-changed code. Never treat its exit 0 as success — gate on `cargo build && cargo test` after, then `cargo fmt`.
- **cargo-machete:** text/regex-based, imprecise by the maintainer's own description; blind to `build.rs`-generated usage.

Never invoke an unsafe/opt-in tier in an unattended batch without a human decision.

## 3. Dead-code deletion protocol

Conjunctive AND — all four before deletion:

1. Static-analysis hit from the ecosystem tool (§1).
2. Independent zero-reference proof: `grep -rn '<symbol>' --exclude-dir=.git --exclude=.git .` (in a git worktree `.git` is a FILE — `--exclude-dir` alone silently misses it) or LSP `FindReferences(includeDeclaration=false)`. The detector's own report does not count.
3. Not on the protected-category checklist (§4).
4. DI-wired / reflection-invoked (`getattr`, string-driven instantiation, plugin/registry patterns) / feature-flagged → runtime evidence (production hit-count, APM) or explicit human sign-off. Static reachability alone is insufficient for this class.

Any condition fails or is merely uncertain → **do not delete**; log as deferred with the reason. Batch by file/module; a batch that breaks the build gets `git checkout -- <file>`, rebuild, note the false positive — never debug a detector's confidence in a broken tree.

## 4. Protected-category checklist

Never delete without extra scrutiny, even with a clean grep:

- Entry points and barrel/index re-export files.
- Anything tagged `@public`/`@api` or exported from a published package root.
- Symbols referenced only from tests (the test may document intended usage).
- Factory-name patterns (`create*`, `make*`, `*Factory`) resolved dynamically by convention.
- Anything on a project ignore list (`knip` `ignoreDependencies`, `vulture --make-whitelist` output — prefer generated, syntax-checked whitelists over ad hoc ignore strings).

Generate once per repo; append discoveries as the campaign proceeds.

## 5. Duplication: tuning + two-step validation

- jscpd defaults (`minTokens=50`, `minLines=5`) flag trivial near-duplicates — start 3-4× higher, adjust down only if real duplication is missed, record the value used in `CLEANUP-PROGRESS.md`.
- jscpd is pairwise-only; 3+ near-identical blocks produce redundant overlapping pair-reports. PMD CPD reports true n-way groups — prefer it for 3-way-plus.
- PMD CPD exits 5 on a lex failure and that file's duplication silently vanishes from the report — check the exit code.
- **Two-step validation before consolidating any hit:** (a) behaviorally identical, not token-similar — read both blocks against their call sites; (b) `git blame` both copies. One copy carrying a fix the other lacks = **diverged-by-design**: reconcile the divergence and report it as a bug finding; never silently adopt one copy's behavior.
- No tool auto-merges safely. Every consolidation is Phase 3 judgment work (or a fixture-tested ast-grep rule for pure pattern-level duplicates).

## 6. Structural codemods

- Prefer `ast-grep`: matches the real syntax tree, immune to the comment/string-literal false positives of regex sweeps.
- Rule as YAML with `fix:`, unit-tested via `ast-grep test` against fixtures, diff-reviewed on 2-3 real files, only then `ast-grep scan --update-all` repo-wide, then verify.
- semgrep autofix is for narrow hand-verified pattern rules — not a duplication-merging tool.

## 7. Characterization / golden-master tests

- Legacy code = code lacking tests (Feathers), not "old code." Pin CURRENT behavior — bugs included — before structural change.
- Approval/golden-master style (snapshot real output, diff future runs) is the lowest-friction form and mechanically generable at scale.
- Prefix `characterizes_*`; state in the file that these pin observed behavior, not intended design.

## 8. Adversarial review subagent prompt

Use this scope verbatim — a reviewer told to "find gaps" always reports some, and chasing them regenerates the debt:

> Review this diff against these criteria only: (1) does it do only what it claims — no scope creep beyond the stated cleanup target; (2) is behavior preserved — check for silently weakened or deleted assertions/tests, not just "tests still pass"; (3) does every deletion respect the protected-category list. Do not comment on style, naming, or alternative approaches. Flag only findings that would break correctness or violate these three criteria.

Any test deletion or assertion-weakening inside a "cleanup" diff is a hard stop requiring explicit justification — a documented, reproducible agent reward-hacking pattern, not a hypothetical.

## 9. Hotspot pre-flight

Rank Phase 3 targets by churn × (inverse) code health — high-churn/low-health files concentrate defects. Rough proxy: `git log --format= --name-only --since="6 months ago" | sort | uniq -c | sort -rn` crossed with the linter's complexity report. High-churn + high-complexity → smaller, heavier-reviewed batches. Low-churn/low-complexity → back of the queue no matter how ugly — it costs nothing right now.

## 10. Fan-out and worktree limits

- Decompose by module/directory (one unit = discovery + fix + local verify together), never by pipeline phase — phase splits maximize handoffs and context loss.
- Cap concurrent worktrees ~8-10; beyond that coordination eats the parallelism gain.
- Never run two worktrees' dependency/lockfile changes concurrently; serialize those. Renames in a shared namespace also serialize — worktree isolation doesn't catch namespace collisions until merge.
- **Union battery (non-negotiable):** after parallel branches merge, run the full verify battery on the merged union. Branches green in isolation can still break each other's gates — proven live in this workspace: two concurrent specs, each fully green, tripped a layer lint only their union could trip.
- Where a delegation engine exists (in hex: BOI — one task per module, ordering via `blocked_by`), prefer it over ad hoc subagent spawning. Default remains single-agent sequential per module: multi-agent decomposition costs 3-10× the tokens and only pays when modules are demonstrably independent.

## 11. No-tool discipline

When no deterministic tool covers the transform (business-logic consolidation, cross-cutting rename with semantic variants):

1. Write a plan file first: every site, the exact before→after shape, the invariant that must hold. Validate the site list independently (fresh grep, not memory).
2. Halve the batch cap (≤125 lines / half a module).
3. Hand-verify each site against the plan before the batch verify — the plan file, not recollection, is the checklist.
4. Anything that deviates from the planned shape mid-batch → stop, re-plan; deviations are how "mechanical" hand-edits go semantic silently.

## 12. Known false-positive classes by tool

No detector below is ground truth alone — hence the conjunctive protocol (§3):

| Tool | Misses / false-positives on |
|---|---|
| knip | Dynamic imports, plugin-discovered code (mitigate via entry-point config, not by disputing the report) |
| cargo-machete | `build.rs`-generated usage; renamed import vs package name (`--with-metadata`) |
| cargo-udeps | Deps used only in doctests |
| vulture | Reflection/dynamic dispatch, string-driven instantiation, registries |
| rustc `dead_code`/clippy | Fields used only in `Debug` impls; closure-only invocations; trait methods satisfying an untraceable signature |
| depcheck | devDependencies referenced only from config files — fallback-only for this reason |
| jscpd | Permissive defaults; pairwise-only inflation on 3+ way duplication |
