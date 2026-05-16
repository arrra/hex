---
name: hex-checkpoint
description: >
  Non-blocking checkpoint. Quick distill pass, handoff file, todo update, landings refresh. Suggest compact when done.
  Reflection runs in the background via session-reflect.sh — never blocks the session.
---
# sync-safe

# /hex-checkpoint — Checkpoint and Continue

Persist context from the current conversation, then suggest compact if the user wants a fresh context window.

## Usage

Run the checkpoint binary to handle all mechanical steps:

```
hex checkpoint [focus directive]
```

The binary will:
1. Print guidance for the AI-driven distill pass (decisions, people, projects, todo.md)
2. Dispatch background reflection (session-reflect)
3. Write a handoff file to `raw/handoffs/YYYY-MM-DD-HHMMSS.md`
4. Print guidance to update todo.md
5. Append a changelog entry to today's landings file (if it exists)
6. Print the compact suggestion with handoff path

## Arguments

The user may pass a focus directive (what they want to work on next). Pass it as the first argument:

```
hex checkpoint "auth refactor"
```

## AI-driven steps (still required)

After running `hex checkpoint`, complete the steps that require conversation context:

1. **Quick distill pass** — scan the conversation for unpersisted context:
   - Decisions made → `me/decisions/` or `projects/*/decisions/`
   - People mentioned → `people/*/profile.md`
   - Project updates → `projects/*/context.md`
   - Patterns noticed → `evolution/observations.md`
   - Skip if session was very short (< 5 exchanges): note "Distill skipped (short segment)."

2. **Update todo.md** — move completed items, add newly discovered tasks.

3. **Fill in the handoff file** — the binary creates the file at the path it prints; fill in "What We Did", "Key Decisions", "Open Threads", and "Files Modified" sections.

If the session segment was very short with no corrections or pushback, skip reflection entirely and note "Reflection skipped (short segment, no corrections)."
