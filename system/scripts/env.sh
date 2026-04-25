#!/usr/bin/env bash
# env.sh — Shared environment for hex agents, workers, and scripts.
#
# Every agent wake script and BOI worker sources this file. It guarantees:
#   1. HEX_DIR and AGENT_DIR point to the hex instance
#   2. PATH includes locations where claude/codex and user tools live
#   3. A claude() wrapper bakes in --dangerously-skip-permissions
#   4. TZ is set from the instance's timezone file
#
# Usage (from wake scripts):
#   source "$HEX_DIR/.hex/scripts/env.sh"
#
# Usage (standalone — auto-detects HEX_DIR from script location):
#   source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

# ── Resolve HEX_DIR ─────────────────────────────────────────────────────────
# Priority: AGENT_DIR env var > HEX_DIR env var > auto-detect from script location
if [[ -z "${HEX_DIR:-}" ]]; then
  if [[ -n "${AGENT_DIR:-}" ]]; then
    HEX_DIR="$AGENT_DIR"
  else
    # Auto-detect: env.sh lives at $HEX_DIR/.hex/scripts/env.sh
    HEX_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  fi
fi
export HEX_DIR
export AGENT_DIR="$HEX_DIR"

# ── PATH: ensure user tools are reachable ────────────────────────────────────
# Agents and workers run in clean environments (launchd, tmux, cron) where the
# user's interactive PATH is not inherited. We add every common install location
# so claude, codex, boi, hex-events, python3, node, and npm are discoverable.
_add_to_path() {
  [[ -d "$1" ]] && [[ ":${PATH}:" != *":$1:"* ]] && export PATH="$1:$PATH"
}

# User-local binaries (pip install --user, cargo install, go install)
_add_to_path "$HOME/.local/bin"
_add_to_path "$HOME/bin"
_add_to_path "$HOME/.cargo/bin"
_add_to_path "$HOME/go/bin"

# Homebrew (macOS)
_add_to_path "/opt/homebrew/bin"
_add_to_path "/usr/local/bin"

# Node.js / npm global (where claude CLI lives after npm install -g)
if [[ -d "$HOME/.npm-global/bin" ]]; then
  _add_to_path "$HOME/.npm-global/bin"
fi
# fnm / nvm managed node
for _ndir in "$HOME/.fnm/aliases/default/bin" "$HOME/.nvm/versions/node"/*/bin; do
  _add_to_path "$_ndir" 2>/dev/null
done

# Python (uv, pyenv)
_add_to_path "$HOME/.local/share/uv/python"
if [[ -d "$HOME/.pyenv/shims" ]]; then
  _add_to_path "$HOME/.pyenv/shims"
fi

# BOI and hex-events bin directories
_add_to_path "$HOME/.boi/bin"
if [[ -d "$HOME/.hex-events/venv/bin" ]]; then
  _add_to_path "$HOME/.hex-events/venv/bin"
fi

unset -f _add_to_path

# ── Timezone ─────────────────────────────────────────────────────────────────
if [[ -z "${TZ:-}" && -f "$HEX_DIR/.hex/timezone" ]]; then
  export TZ
  TZ="$(cat "$HEX_DIR/.hex/timezone")"
fi

# ── claude() wrapper ─────────────────────────────────────────────────────────
# All agent invocations of claude go through this function. It bakes in
# --dangerously-skip-permissions so individual scripts don't need to.
claude() {
  local claude_bin
  claude_bin="$(command -v claude 2>/dev/null)" || {
    echo "ERROR: claude not found on PATH after sourcing env.sh" >&2
    echo "  PATH=$PATH" >&2
    return 127
  }
  "$claude_bin" --dangerously-skip-permissions "$@"
}
export -f claude
