# PROGRESS.md Schema v0.1

Defines the structure for `PROGRESS.md` — the single machine-readable session state file.
Replaces the disconnected trio of `todo.md`, `landings/`, and `raw/handoffs/` (see audit C3).

---

## Structure (YAML frontmatter + markdown body)

```yaml
---
session: SESSION_ID          # e.g. S2C48, or a short slug
started_at: ISO8601          # e.g. 2026-05-16T09:00:00-07:00
last_updated: ISO8601        # updated on every checkpoint
focus: short text            # one-liner: what this session is driving toward
status: ACTIVE | CHECKPOINT | DONE
---
```

---

## Section: In Flight

Tasks actively being worked. Each line is one unit of work with acceptance criteria.

```markdown
## In Flight
- [ ] task description (acceptance criteria one-liner)
- [ ] another task (criterion)
```

---

## Section: Completed This Session

Tasks finished since `started_at`. Include outcome for handoff legibility.

```markdown
## Completed This Session
- [x] task — outcome / artifact produced
```

---

## Section: Open Threads (carry across sessions)

Items that span session boundaries — long-running context a future agent needs.

```markdown
## Open Threads (carry across sessions)
- thread name — current state — next-action
```

---

## Section: Decisions Made

Key decisions with links to decision files (in `me/decisions/`).

```markdown
## Decisions Made
- decision summary — [link to file](path/to/decision.md)
```

---

## Section: Files Modified

Changed files with one-line change summaries. Aids handoff and audit.

```markdown
## Files Modified
- path/to/file — change summary
```

---

## Usage Rules

1. **Read on session start** — before any work, read PROGRESS.md to orient.
2. **Write on checkpoint** — update `last_updated`, move completed tasks, add open threads.
3. **Single source of truth** — do not duplicate this state into `landings/` or `raw/handoffs/`.
4. **Status discipline** — only `DONE` when ALL in-flight tasks are complete and verified.

---

## Why This Exists (Audit Reference)

Harness engineering audit 2026-05-15 (C3 / Lecture 05 gap):

> "No PROGRESS.md. Handoff files exist but are freeform; no schema enforcement;
> not read at session start."

Session continuity is impossible without structured, schema-enforced state.
This file is the fix.
