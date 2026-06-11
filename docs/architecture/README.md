# Architecture Deep Dives — Registry & Standard of Practice

`docs/architecture.md` is the ~5-minute system overview. This directory holds the
**per-subsystem deep dives** — the documents an operator or implementer reads before
changing a subsystem. This README is both the registry and the rules that keep these
docs from rotting.

## Registry

The table is a pure index. The **single source of truth** for what a doc covers and
when it was last verified is the machine-readable header inside the doc itself (§1) —
not this table.

| Subsystem | Doc |
|-----------|-----|
| Memory pipeline (index · distill · recall · consolidation · maintain · backup) | [memory.md](memory.md) |

Grandfathered (predates this standard, migrate opportunistically): `docs/code-intel.md`
(+ `docs/code-intel/`). Queued candidates: boi-interface, release-process, telemetry,
hooks, doctor.

## The Standard of Practice

**1. Every deep dive starts with a machine-readable header** (first lines of the file):

```markdown
<!--
verified-against: <commit-sha> (<YYYY-MM-DD>)
source-paths: path/one/, path/two.rs
-->
```

`verified-against` is the commit the doc's claims were checked against. `source-paths`
are the code paths the doc describes — they define the doc's update obligation. This
header is the single authority; nothing else duplicates it.

**2. Docs ride the change.** Any branch, BOI spec, or workflow that modifies a
subsystem's `source-paths` updates that subsystem's deep dive **in the same branch**
and bumps `verified-against` to the new state. A code change without its doc change is
an incomplete change — treat it like a missing test. BOI specs and workflow plans that
touch registered paths must carry a docs task or verification gate.
*Enforcement status: manual — reviewer responsibility.* The designed mechanical form
(a diff-vs-source-paths guard on the legacy-rename-guard pattern, fed by a
machine-readable registry) is a queued follow-up.

**3. New subsystem → new row.** When a new subsystem earns a deep dive (rule of thumb:
it has its own cron jobs, its own storage, or its own failure modes), add the doc, add
the registry row, follow the template below.

**4. Mechanical backstop.** `system/harness/tests/arch_docs_registry.rs` asserts that
every Registry row links a real doc carrying the §1 headers, and **fails on any
registry row with no parseable doc link** (so a malformed row can't silently evade
it). It rides `cargo test` — the standard gate in every BOI spec, workflow battery,
and dev loop. It is **not yet part of the `hex release cut` gate battery** (that
battery is the pinned five gates in `release.rs`); wiring it in as a sixth gate is a
queued follow-up, together with the staleness gate:
`git log <verified-against>..HEAD -- <source-paths>` over a threshold (~15 commits)
⇒ "deep dive for <subsystem> is N commits stale."

**5. Scope.** This registry covers foundation-shipped subsystems (`system/`). Personal
instances receive these docs via `/hex-upgrade`; instance-specific docs (e.g.
mrap-hex's `hex-essence.md`) live in the instance and are not registered here.

## Template

```markdown
<!--
verified-against: <sha> (<date>)
source-paths: <paths>
-->
# <Subsystem> Architecture

> One-paragraph summary: what it is, what feeds it, what reads from it.

## Storage            — files/DBs/tables it owns
## Jobs               — crons/hooks/triggers, with schedules and what each invokes
## Write path         — how data gets in, step by step
## Read path          — how data comes back out
## Failure surfaces   — what can break, how it surfaces (telemetry/alerts/doctor), exit-code semantics
## Operations         — runbook: health checks, recovery levers, maintenance
## Lineage            — key decisions/plans that shaped it (links)
```
