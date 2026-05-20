---
name: hex-sync-base
description: >
  Sync local fixes to hex. Compare local hex against
  hex and push improvements upstream.
---
# sync-safe

Compare local hex against hex and push improvements upstream.

## Steps

1. **Detect HEX_DIR and BASE_DIR**
   - HEX_DIR: walk up from script to find CLAUDE.md
   - BASE_DIR: `~/github.com/mrap/hex`

2. **Diff shared files**
   Compare these directories between hex and hex:
   - `dot-claude/scripts/` vs `.hex/scripts/`
   - `dot-claude/skills/` vs `.hex/skills/`
   - `dot-claude/commands/` vs `.claude/commands/`
   - `CLAUDE.md` (root)

   For each file that exists in both locations, diff them.
   For files only in hex, flag as "new, consider adding."
   For files only in hex, flag as "missing locally, may need pull."

3. **Classify each diff**
   For each changed file, determine:
   - **Push upstream**: Generic improvement that benefits all hex users
   - **Local only**: user-specific customization (personal data, preferences)
   - **Needs work**: Change is valuable but needs generalization before pushing (e.g., hardcoded timezone)

   Present a table to the user with the classification and a one-line summary of each change.

4. **Apply approved changes**
   For each file the user approves:
   - Copy the file to the corresponding location in hex
   - Note: hex uses `.hex/` but hex base repo uses `dot-claude/` (renamed during install)

5. **Commit and push hex**
   - Stage changed files in hex
   - Create a commit with a descriptive message
   - Push to origin

6. **Update CLAUDE.md if needed**
   For CLAUDE.md changes that are generic (new standing orders, protocol updates), apply them to the hex CLAUDE.md. Skip user-specific content (project references, personal evolution items).

## Path Mapping

| hex location | hex location |
|-------------|----------------------|
| `.hex/scripts/` | `dot-claude/scripts/` |
| `.hex/skills/` | `dot-claude/skills/` |
| `.claude/commands/` | `dot-claude/commands/` | (stays in .claude/) |
| `.hex/hooks/` | `dot-claude/hooks/` | (stays in .claude/) |
| `CLAUDE.md` | `CLAUDE.md` |

## Guards

Two layers prevent personal data from leaking:

1. **Manual review**: Before copying any file, inspect its contents for personal data (names, emails, personal file references, project-specific paths). Do not copy if anything matches.
2. **Pre-commit hook**: hex-foundation has a pre-commit hook (`system/scripts/sanitize-check.sh`) that runs as the last line of defense before commits land upstream.

**Before copying any file**, review it carefully. If in doubt, do not copy it and surface the concern to the user.

## Rules

- Manually review every file for personal data before copying. No exceptions.
- Never push personal data (me/, people/, projects/, landings/, evolution/, todo.md)
- Never push settings.json (contains personal hooks and statusline config)
- Always diff before copying. Show the diff to the user.
- Commit to hex with a clear message about what changed and why.
- After pushing, verify the commit landed with `git log -1` in hex.
