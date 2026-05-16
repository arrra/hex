#!/usr/bin/env bash
# hex-agent-spawn.sh — thin shim delegating to `hex agent spawn`
# Portable logic (YAML parsing, validation, template rendering, file creation,
# rate limiting, audit logging) lives in system/harness/src/agent_spawn.rs.
# This shim handles env setup (source env.sh) which requires a shell file.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Auto-detect HEX_DIR before sourcing env.sh (chicken-and-egg: same logic as env.sh)
if [[ -z "${HEX_DIR:-}" ]]; then
  if [[ -n "${AGENT_DIR:-}" ]]; then
    HEX_DIR="$AGENT_DIR"
  else
    HEX_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
  fi
  export HEX_DIR
fi

# Source env.sh to get PATH (including hex binary) and the claude() function
source "$HEX_DIR/.hex/scripts/env.sh"

if [[ $# -lt 1 ]]; then
  echo "Usage: hex-agent-spawn.sh <role-spec-file.yaml>" >&2
  exit 1
fi

exec hex agent spawn "$1"
