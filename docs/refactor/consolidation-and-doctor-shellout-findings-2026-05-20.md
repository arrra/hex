# Findings: Out-of-Scope Breakage — 2026-05-20

Discovered during consolidation-subsystem-repair (spec S151A). Two clusters
of confirmed breakage left unaddressed; work order for a follow-up spec.

---

## A. Four broken `hex doctor` subcommands

**Location:** `system/harness/src/main.rs` lines 2407–2427

`DoctorCommands::Introspect`, `TechScout`, `GoalAlignment`, and
`CleanupProjects` all call `exec_script` against non-existent paths:

| Subcommand | `exec_script` target | Actual file |
|---|---|---|
| `Introspect` | `system/scripts/system-introspection.sh` | `system-introspection.legacy.sh` |
| `TechScout` | `system/scripts/tech-scout.sh` | `tech-scout.legacy.sh` |
| `GoalAlignment` | `system/scripts/goal-alignment.sh` | `goal-alignment.legacy.sh` |
| `CleanupProjects` | `system/scripts/cleanup-project-jsonl.sh` | `cleanup-project-jsonl.legacy.sh` |

Same bug class as `consolidate` (fixed in S151A/T3AAE): rustification renamed
scripts to `*.legacy.sh` but the harness still execs the pre-rename names.
All four commands exit 127 silently.

**Additional issue:** The sibling policies in `system/events/policies/` invoke
a *third* wrong path prefix (`.claude/scripts/` instead of `system/scripts/`):
- `system-introspection.yaml` — calls `.claude/scripts/system-introspection.sh`
- `tech-scout-daily.yaml` — calls `.claude/scripts/tech-scout.sh`

**Fix needed:** Port all four to native Rust modules under
`system/harness/src/doctor/`, following the same pattern as
`doctor/consolidate.rs`. Update the policies to invoke the harness binary.

---

## B. Orphaned policy directory

`system/policies/` contains four policies never deployed to new installs:

- `quality-gaming-alert.yaml`
- `quality-kr-check.yaml`
- `quality-spec-audit.yaml`
- `quality-sweep.yaml`

`install.sh` only deploys `system/events/policies/`. These four policies are
dead on any machine installed after the `events/policies/` migration.

**Fix needed:** Audit each policy — migrate live ones to
`system/events/policies/`, retire obsolete ones.
