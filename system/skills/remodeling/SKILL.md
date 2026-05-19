---
name: remodeling
description: >
  Mental map remodeling workflow. Ingests an Excalidraw file, converts to
  canonical markdown, diffs against previous version, runs analysis, and
  facilitates the remodeling conversation. Use when Mike runs a bi-weekly
  remodeling session or invokes /remodel.
version: 1.0.0
---

# /remodel — Mental Map Remodeling Skill

## When This Activates

- User invokes `/remodel`
- User says "let's do a remodel", "remodeling session", or shares an .excalidraw file
- User wants to review, diff, or analyze a mental map

## What This Skill Does

This skill orchestrates the mental map pipeline:

1. **Ingest** — Parse an .excalidraw file into canonical markdown format
2. **Save** — Write to `me/remodeling/YYYY-MM-DD-title.md`
3. **Diff** — Compare against the most recent previous version
4. **Analyze** — Run the analysis engine to surface assumptions, contradictions, and questions
5. **Converse** — Present findings and guide the remodeling conversation
6. **Index** — Rebuild memory index after the session

---

## Step-by-Step Protocol

### Step 1: Get the Excalidraw File

Ask Mike for the file path if not provided:

> "Drop the .excalidraw file path and I'll kick off the remodel."

If Excalidraw Plus MCP is available (future): pull via the API instead.

If Mike provides a file path, proceed. If the file doesn't exist, say so clearly.

### Step 2: Parse and Save

```bash
# Parse the Excalidraw JSON to canonical markdown
python3 ~/.hex/scripts/parse_excalidraw.py "<file_path>"
```

Capture the output. Determine the title from the first `# ` heading in the output.
Save to `me/remodeling/YYYY-MM-DD-<slugified-title>.md` using today's date.

```bash
# Example save path
OUTFILE="me/remodeling/$(date +%Y-%m-%d)-<title-slug>.md"
python3 .hex/scripts/parse_excalidraw.py "<file_path>" > "$OUTFILE.tmp" && mv "$OUTFILE.tmp" "$OUTFILE"
```

Confirm: "Saved to `<path>`."

### Step 3: Diff Against Previous Version

```bash
python3 .hex/scripts/diff_mental_map.py "$OUTFILE"
```

The diff script auto-detects the most recent prior version.

If no prior version exists, say: "No previous version found — this is the first map in this series."

Present the diff summary to Mike. Highlight:
- **New concepts** (added nodes)
- **Dropped concepts** (removed nodes)
- **Open questions** that are new, resolved, or carried forward

### Step 4: Run Analysis

```bash
python3 .hex/scripts/analyze_mental_map.py "$OUTFILE"
```

Present the analysis findings in sections:

1. **Implicit Assumptions** — edges without reasoning. Prompt: "These connections have no stated reason — worth examining?"
2. **Contradictions** — cross-references that don't match learnings.md or todo.md
3. **Unquestioned Assumptions** — nodes unchanged across 3+ versions
4. **Structural Hotspots** — hub nodes, orphans, longest chains
5. **Probing Questions** — the generated list (5–10 questions)

### Step 5: Facilitate Remodeling Conversation

Guide Mike through the map using the analysis as a lens:

- Start with: "Here's what stood out to me. Want to work through these or start somewhere specific?"
- For each flagged assumption: "Is this still true? What would need to change for it to break?"
- For contradictions: "This conflicts with what I've seen in [source]. How do you see it?"
- For unquestioned nodes: "This hasn't changed in [N] versions. Is that because it's solid, or hasn't been challenged?"

Let Mike drive the direction. Take notes on any decisions, reframings, or updates.

### Step 6: Persist Updates

After the conversation:

1. If Mike reframes or updates nodes, ask if he wants to generate an updated map file
2. Write any key insights to `me/learnings.md`
3. Add any action items to `todo.md`
4. Rebuild the memory index:

```bash
bash .hex/scripts/startup.sh --step index
```

Confirm: "Memory index updated — new mental map is now searchable."

---

## Notes

- Scripts live at `~/.hex/scripts/`
- Maps live at `~/hex/me/remodeling/YYYY-MM-DD-title.md`
- Format spec: `~/hex/me/remodeling/FORMAT.md`
- Excalidraw Plus MCP support: add as an input source once the alpha API is available
- Keep conversations focused — the goal is to surface non-obvious things, not exhaustively review every node
