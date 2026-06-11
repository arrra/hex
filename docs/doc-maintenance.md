# Documentation maintenance record

Routing backbone and verified-claim registry for this repo's docs. Future doc passes read
this first; declined-with-reason items below stand as refutations unless the cited code
changed. Owner: repo maintainer (Mike).

## Source-dir → doc mapping

| Source area | Doc | Audience | Load trigger |
|---|---|---|---|
| repo root (entry) | AGENTS.md (CLAUDE.md = symlink bridge) | agents | always |
| repo root (entry) | README.md | humans | always |
| whole system | docs/architecture.md | both | on-request (pointed from AGENTS.md Quick Start) |
| system/harness/src/memory/ | docs/architecture/memory.md | both | on-request (via docs/architecture/README.md) |
| tests/ | docs/testing.md | both | on-request (pointed from AGENTS.md Q5, README) |
| system/harness modules | docs/module-authoring.md | agents | on-request (via README) |
| release flow | docs/versioning.md | both | on-request (via README) |
| runtime ops (launchd, telemetry, llm config) | docs/hex-ops.md | both | on-request (via architecture.md) |
| iii engine integration | docs/iii-hex.md | agents | on-request (via architecture.md Further Reading) |
| Standing Orders Layer-2 mechanisms | docs/standing-orders.md | agents | on-request (via AGENTS.md §Standing Orders) |
| code intelligence (cq) | docs/code-intel.md (+ SPEC-A1/A2) | agents | on-request (via AGENTS.md §Code intelligence) |
| shipped instance surface | templates/CLAUDE.md, templates/AGENTS.md | instances | install/upgrade copies — edit as SOURCE, never as docs-pass churn |

## Reproducible checks (re-run on each pass)

| Claim | Check |
|---|---|
| BOI spec format in AGENTS.md is v2 | `grep -c 'mode = ' AGENTS.md` → 0; `grep -c 'pipeline\|\[contract\]' AGENTS.md` ≥ 1 |
| CLAUDE.md is a bridge | `test -L CLAUDE.md && readlink CLAUDE.md` → `AGENTS.md` |
| No doc references release.sh/sanitize-check.sh as live | `grep -rn 'release.sh' docs/ AGENTS.md README.md` → only historical/tombstone mentions |
| capabilities-map.md carries frozen-snapshot warning | `grep -c 'Frozen snapshot' docs/capabilities-map.md` = 1 |
| Orphan check | every docs/*.md filename greps ≥1 inbound reference from AGENTS.md, README.md, or another docs/ file |
| Entry-file health | `wc -l AGENTS.md` (budget 50–200; currently over — tracked as backlog C2, not a pass finding) |

## Deferred / declined (standing unless cited code changes)

- **AGENTS.md oversize (610L)** — deferred to in-repo backlog item C2 (AGENTS.md Q3: "decomposition → ≤150-line router"). Do not partially restructure in a docs pass.
- **capabilities-map/experiments/hex-ops multi-subject splits** — deferred pending C2 and a keep-vs-retire call on capabilities-map.
- **Nested AGENTS.md routers (system/harness/, system/skills/, tests/)** — blocked on //! module docs (code work; decision nested-docs-thin-agents-md-routers-2026-06-05).
- **Q5 verify stubs (`hex info repo-mission`, `hex info active-locks`)** — code work, upstream.
- **Core Philosophy wording divergence** (AGENTS.md vs templates/CLAUDE.md, e.g. "You are not a chatbot…") — intentional voice difference, NOT drift. Declined as a finding 2026-06-11.

## UNVERIFIED claims (flagged, not edited)

- AGENTS.md Q2 static answer (C1/C2/C4 statuses, dated 2026-05-16) — currency unverified; PROGRESS.md pointer added instead.
- templates/CLAUDE.md does not reference docs/standing-orders.md while older shipped instances do — template-side decision pending.
