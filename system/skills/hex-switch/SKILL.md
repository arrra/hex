---
name: hex-switch
description: >
  Switch between hex workspace contexts. Handles natural language like "switch to X",
  "let's work on X", "go back", and "what are my workspaces". Checkpoints on switch,
  restores on return.
---

# /hex-switch — Workspace Context Switching

Switch between hex workspaces using natural language. Each workspace is a tmux window
with its own Claude session. Switching checkpoints the current context and restores the
target's last state.

## Intent Detection

Match these patterns to determine action:

| User says | Action |
|-----------|--------|
| "switch to X" / "go to X" / "open X" | Switch to workspace named X |
| "let's work on X" / "start X" / "jump to X" | Switch to workspace named X |
| "go back" / "switch back" / "back to previous" | Switch back (`--back` flag) |
| "what are my workspaces" / "list workspaces" / "show contexts" | List workspaces |
| "create workspace X" / "new workspace X" | Create and switch to new workspace X |

## Step 1: Parse intent

Extract the target workspace name from the user's message. Normalize to lowercase,
hyphenated form (e.g. "Pitch Deck" → "pitch-deck").

If the intent is ambiguous, ask: "Which workspace? (e.g. main, pitch-deck, research)"

## Step 2: Execute the switch

### Switch to a named workspace

```bash
bash $HEX_DIR/.hex/scripts/hex-context-switch.sh <workspace-name>
```

### Go back to previous workspace

```bash
bash $HEX_DIR/.hex/scripts/hex-context-switch.sh --back
```

### List workspaces

```bash
source $HEX_DIR/.hex/scripts/hex-context-lib.sh && ctx_list
```

Or read the registry directly:

```bash
python3 -c "
import json, os
path = os.path.join(os.environ.get('HEX_DIR',''), '.claude', 'hex-contexts.json')
data = json.load(open(path))
active = data.get('active','')
for name in sorted(data.get('contexts',{}).keys()):
    marker = ' *' if name == active else ''
    print(f'  {name}{marker}')
"
```

## Step 3: Report result

After switching:

1. **On switch to new workspace:** Say "Switching to [workspace]. Checkpointing current context first." Then confirm: "Now in [workspace] workspace."

2. **On --back:** Say "Going back to [previous workspace]."

3. **On list:** Display workspaces with active marker, e.g.:
   ```
   Workspaces:
     main *
     pitch-deck
     research
   ```

4. **On return to a workspace that has a handoff file:** Read the handoff and summarize:
   ```
   Restored [workspace]. Last checkpoint: [timestamp]

   ## Where we left off
   - [key points from handoff]

   ## Open threads
   - [open items]
   ```

   Handoff files are at `$HEX_DIR/.hex/handoffs/<workspace-name>.md`.

## Notes

- The switch script creates a new tmux window if the workspace doesn't exist yet.
- The `--back` flag pops the context stack (returns to the previous workspace).
- If not inside tmux, the switch script will log but not create windows — inform the user.
