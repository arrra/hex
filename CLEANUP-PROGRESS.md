# Cleanup campaign — hex-foundation (2026-07-22)

**Skill:** repo-cleanup (first live campaign = acceptance test)
**Base:** develop `120b48b1` · worktree `cleanup/audit-2026-07`
**Consumes:** boi-hex audit 2026-07-16 (mrap-hex `projects/hex-ops/analysis/boi-hex-audit-2026-07-16/`)
**Drift check:** audit was cut at `f59df59f`; the S4mewqp4c/Sn7drgcyk spec wave already fixed the recall CRITICAL, release Tests gate, VERSIONS pin, scipd race, and the docs batch — those audit items are CLOSED, not re-derived. Surviving items re-verified against live code below.

## Quality bar
Personal-production infrastructure running unattended (launchd/cron) against real data. The bar: mechanical hygiene with zero behavior change, plus surgical S6 (no-silent-failure) fixes from the audit's surviving Lows. No architecture work, no new abstractions, no test-suite restructuring.

## Baseline (Phase 0/1)
- verify.sh gate: PASS (build+test; clippy clean, report-only) — /tmp/campaign-baseline.log
- Tests: 978 passing across hex-harness + scipd
- rustfmt drift: **901 diff hunks** (concentrated in system/code-intel/) — the campaign's before-metric
- LOC snapshot: scc/tokei unavailable (noted, tool gap); drift-hunk count is the primary mechanical metric

## Candidates

| # | Item | Category | Status |
|---|---|---|---|
| C1 | rustfmt drift, system/code-intel (901 hunks) | mechanical/format | pending → B1 |
| C2 | rustfmt drift, system/harness (if any after B1 measure) | mechanical/format | pending → B2 |
| C3 | failures.rs:126-127 silently drops malformed event rows (.ok() filter_map) — audit hex:23, S6 | semantic (small) | pending → B3 |
| C4 | Direct rusqlite opens without busy_timeout (ledger.db reads, main.rs load_outcome_rows) — audit hex:24 | semantic (small) | pending → B4 |
| C5 | Dead-code sweep (Rust) | dead code | ZERO-CANDIDATE: compiler dead_code + clippy already clean; cargo-machete/udeps not installed — deferred to tooling follow-up |
| C6 | Duplication sweep | duplication | SKIPPED LOUDLY: no detector installed (jscpd/PMD CPD absent) — deferred to tooling follow-up |
| C7 | 185 shell scripts dead-reference inventory | dead code | DEFERRED: scripts are dynamically invoked (launchd/cron/installer) — deletion protocol requires runtime evidence; inventory-only this campaign |
| C8 | .hex/.upgrade-cache duplicate harness copy | dead code | OUT OF SCOPE: untracked instance artifact, not in git |

## Do-NOT-fix (weighed, rejected this campaign)
- Stop-hook O(n²) transcript copy, telemetry auto-prune, HEX_DIR resolver sprawl, code-intel mutex-poisoning refactor — all already tracked in FIX-019 backlog as designed follow-ups; pulling them into a hygiene campaign mixes semantic risk classes.
- Tool installs (cargo-machete, scc, jscpd) — environment mutation belongs in its own dispatched task, not mid-campaign.

## Batches
- B1: `cargo fmt` scoped to scipd (code-intel) — isolated commit + create .git-blame-ignore-revs
- B2: `cargo fmt` harness residue if measure shows any — same treatment
- B3: failures.rs loud-drop fix + test (S6)
- B4: busy_timeout on ledger.db opens + test

## Changelog
- 2026-07-22 — Phase 0/1 complete: worktree, baseline PASS, ledger written.
