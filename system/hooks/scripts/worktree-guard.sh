#!/usr/bin/env bash
# worktree-guard.sh — PreToolUse guard enforcing Standing Order 7
# ("all work in worktrees, never the shared checkout").
#
# Refuses Write/Edit/NotebookEdit on a FLAGGED repo when the target lives in the
# repo's SHARED checkout (the main working tree) rather than a linked worktree.
# This is the mechanical backstop for the dirty-tree-safety hazard recorded in
# evolution/observations.md OBS-030 (a concurrent agent's reset silently wiped an
# uncommitted edit twice in one day).
#
# Contract (Claude Code PreToolUse):
#   stdin  : JSON { tool_name, tool_input: { file_path, ... }, ... }
#   stdout : JSON { hookSpecificOutput: { hookEventName, permissionDecision,
#                                         permissionDecisionReason } }
#   exit   : 0 on allow / any internal error (fail-open — a broken guard must
#            never wedge the session); 2 on deny (universal PreToolUse block).
#
# Honest framing (mirrors the pre-push hook): this is a FOOTGUN-GUARD, not a
# security boundary. An agent runs as the same user and can bypass it. It makes
# the right path (worktree) the easy path; it does not make the wrong path
# impossible.
#
# Flagged repos: env HEX_WORKTREE_GUARD_REPOS (space/comma list of repo dir
# basenames) overrides the default. Default: hex-foundation. Personal-data repos
# (e.g. ~/hex) are intentionally NOT flagged — the hazard is multi-agent CODE
# repos.

set -u

DEFAULT_FLAGGED="hex-foundation"
FLAGGED="${HEX_WORKTREE_GUARD_REPOS:-$DEFAULT_FLAGGED}"

allow() {
  # $1 = reason (informational; only surfaced for debugging)
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}\n'
  exit 0
}

deny() {
  # $1 = human-readable reason.
  # Block via BOTH channels so the guard holds across Claude Code versions:
  #  - structured JSON permissionDecision=deny on stdout (modern contract), and
  #  - exit code 2 with the reason on stderr (legacy/universal block path).
  # Either alone blocks; together they're robust to contract drift (S6: a guard
  # must never silently no-op).
  local reason="$1"
  local esc
  esc=$(printf '%s' "$reason" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null || printf '%s' "$reason")
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}\n' "$esc"
  printf '%s\n' "$reason" >&2
  exit 2
}

# --- read + parse stdin -------------------------------------------------------
INPUT=$(cat 2>/dev/null || true)
[ -z "$INPUT" ] && allow "no input"

read_field() {
  # $1 = python expression over the parsed object `d`; prints value or empty.
  printf '%s' "$INPUT" | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
except Exception:
    sys.exit(0)
try:
    v=$1
except Exception:
    v=None
print(v if v is not None else '')
" 2>/dev/null
}

TOOL=$(read_field "d.get('tool_name')")
FILE=$(read_field "d.get('tool_input',{}).get('file_path')")

# Only mutating file tools are in scope.
case "$TOOL" in
  Write|Edit|NotebookEdit|MultiEdit) ;;
  *) allow "tool $TOOL not guarded" ;;
esac

[ -z "$FILE" ] && allow "no file_path"

# --- resolve the deepest existing ancestor dir of FILE (it may be a new file) --
dir="$FILE"
[ -d "$dir" ] || dir=$(dirname "$FILE")
while [ ! -d "$dir" ] && [ "$dir" != "/" ] && [ "$dir" != "." ]; do
  dir=$(dirname "$dir")
done
[ -d "$dir" ] || allow "no existing ancestor dir"

# --- is this a git repo at all? ----------------------------------------------
git -C "$dir" rev-parse --git-dir >/dev/null 2>&1 || allow "not a git repo"

# Submodule guard: a submodule also has git-dir != git-common-dir, but it is NOT
# the multi-agent-worktree hazard. Treat submodules as allowed.
if [ -n "$(git -C "$dir" rev-parse --show-superproject-working-tree 2>/dev/null)" ]; then
  allow "inside a submodule"
fi

# rev-parse may return paths RELATIVE to $dir, so resolve them with cwd=$dir.
GIT_DIR_REL=$(git -C "$dir" rev-parse --git-dir 2>/dev/null)
GIT_COMMON_REL=$(git -C "$dir" rev-parse --git-common-dir 2>/dev/null)
GIT_DIR_ABS=$(cd "$dir" 2>/dev/null && cd "$GIT_DIR_REL" 2>/dev/null && pwd -P)
GIT_COMMON_ABS=$(cd "$dir" 2>/dev/null && cd "$GIT_COMMON_REL" 2>/dev/null && pwd -P)
[ -z "$GIT_DIR_ABS" ] && allow "cannot resolve git-dir"
[ -z "$GIT_COMMON_ABS" ] && allow "cannot resolve git-common-dir"

# A linked worktree has git-dir != git-common-dir → the SAFE, isolated case.
if [ "$GIT_DIR_ABS" != "$GIT_COMMON_ABS" ]; then
  allow "in a linked worktree"
fi
# From here: GIT_DIR == GIT_COMMON → the SHARED checkout (main working tree).

# --- is this repo flagged? ----------------------------------------------------
# The repo's canonical name = basename of the dir containing the common .git.
REPO_NAME=$(basename "$(dirname "$GIT_COMMON_ABS")")
flagged=0
for r in $(printf '%s' "$FLAGGED" | tr ',' ' '); do
  [ "$r" = "$REPO_NAME" ] && flagged=1 && break
done
[ "$flagged" -eq 0 ] && allow "repo '$REPO_NAME' not flagged"

# --- ignored runtime files are allowed (tracked-dirty vs ignored-runtime) ------
# Reuse git's own ignore logic: an ignored path is sweepable runtime state, not
# work worth protecting.
if git -C "$dir" check-ignore -q "$FILE" 2>/dev/null; then
  allow "ignored runtime file"
fi

# --- refuse -------------------------------------------------------------------
deny "S7 violation: editing '$FILE' in the SHARED checkout of flagged repo '$REPO_NAME'. All work must happen in a git worktree (concurrent agents in one working tree silently clobber each other's uncommitted edits — see OBS-030). Fix: git -C $(dirname "$GIT_COMMON_ABS") worktree add ../$REPO_NAME-<task> -b feature/<name>  (or use the using-git-worktrees skill), then edit there."
