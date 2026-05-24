#!/usr/bin/env bash
set -uo pipefail

# CI variant: checks PR diff (HEAD vs HEAD~1) instead of git staging area.
# Mirrors logic from pre-commit hook but works on pushed commits.
for f in $(git diff --name-only --diff-filter=R HEAD~1 HEAD); do
  if [[ "$f" =~ \.legacy\.(sh|py)$ ]]; then
    base=$(basename "$f" | sed 's/\.legacy\././')
    if grep -rqE "scripts/${base}([^.]|$)" system/harness/src/; then
      echo "ERROR: ${f} renamed to .legacy but Rust callers still reference ${base}"
      echo "Update all callers in system/harness/src/ before renaming to .legacy.*"
      exit 1
    fi
  fi
done

echo "Legacy rename guard: OK"
