#!/usr/bin/env bash
# PreToolUse hook (Bash matcher) — blocks cc-connect cron add.
# All scheduling must go through hex-events policies.
# Fast path: grep raw stdin, skip JSON parsing.

if [[ ! -t 0 ]]; then
  INPUT=$(cat 2>/dev/null) || exit 0
  if echo "$INPUT" | grep -q 'cc-connect cron add'; then
    echo 'BLOCKED: Use hex-events policies at ~/.hex-events/policies/*.yaml — never cc-connect cron add. See CLAUDE.md hex-events section.' >&2
    exit 2
  fi
fi
exit 0
