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
| C1 | rustfmt drift, system/code-intel (901 hunks) | mechanical/format | DONE ✓ 9a28d305 (byte-identical to pure cargo fmt — reproduced) |
| C2 | rustfmt drift, system/harness (666 hunks) | mechanical/format | DONE ✓ 899999f5 (byte-identical, reproduced) |
| C3 | failures.rs silently drops malformed event rows — audit hex:23, S6 | semantic (small) | DONE ✓ 1b2c3e75 (Report.malformed_rows + loud probe line, test-first) |
| C4 | ledger.db opens without busy_timeout — audit hex:24 | semantic (small) | DONE ✓ e4cc70a9 (open_ledger helper, PRAGMA-asserted 5000ms) |
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

## Closeout (Phase 5)
- **Re-measurement:** rustfmt drift 1,567 hunks → **0** (primary mechanical metric). Tests 978 → 980 (2 new). Every batch gate PASS incl. --check-batch on semantic commits.
- **Structural vs cheap:** the fmt sweep is definitionally the "cheap" delta class — claimed as hygiene (unblocks any future fmt gate; blame preserved via .git-blame-ignore-revs), NOT as complexity reduction. Structural delta = 2 robustness fixes (S6 loud-drop counting; bounded busy_timeout), both test-pinned. No complexity relocation (nothing moved, only counted/bounded).
- **Zero-candidate categories:** Rust dead code (compiler+clippy clean; detectors uninstalled), duplication (no detector). Honest outcome per skill: skipped LOUDLY, not faked.
- **Deferred:** cleanup-toolchain install (cargo-machete/scc/jscpd — own dispatched task); 185-script dead-reference inventory (dynamic invocation ⇒ needs runtime evidence); FIX-019 items stay in backlog.
- **Skill acceptance notes (fold back into repo-cleanup):** (1) fmt-pass batch size necessarily exceeds the 250-line cap — the skill should state explicitly that HARD RULE 6 exempts the isolated format commit (tool-generated, reproducibility-provable) — candidate wording exists in HARD RULE 2 but the interplay is implicit; (2) the byte-identical-reproduction check (restore pre-fmt tree → re-run tool → diff commit == empty) is a stronger no-smuggling proof than diff -w and cheap — worth adding to reference.md §2; (3) tool-poverty on a fresh machine is the common case — inventory.sh should print install hints next to each missing tool.

## Changelog
- 2026-07-22 — Phase 0/1 complete: worktree, baseline PASS, ledger written.
- 2026-07-22 — B1+B2 fmt sweeps landed (1,567 hunks → 0), .git-blame-ignore-revs created; B3 malformed-row counting (S6); B4 ledger busy_timeout. All gates PASS.
- 2026-07-22 — Phase 4: fmt commits proven byte-identical to pure tool output; fresh-context adversarial review dispatched on semantic diff.
