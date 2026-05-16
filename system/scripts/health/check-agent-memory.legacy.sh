#!/usr/bin/env bash
# check-agent-memory.sh — health check for the agent memory system.
#
# The agent memory system is file-based:
#   ~/.claude/shared-memory/SHARED.md  (cross-project shared memory index)
#   ~/.claude/projects/*/memory/       (per-project memory directories)
#
# Verifies:
#   1. Shared memory directory exists and is readable
#   2. SHARED.md index is present and readable
#   3. Claude projects directory is accessible
#
# Exit 0 = healthy, Exit 1 = unhealthy (prints reason to stderr)
set -uo pipefail

python3 - <<'PYEOF'
import sys, os, glob

shared_mem = os.path.expanduser("~/.claude/shared-memory")
projects_dir = os.path.expanduser("~/.claude/projects")

# 1. Shared memory directory must exist
if not os.path.isdir(shared_mem):
    print(f"FAIL: shared memory directory missing: {shared_mem}", file=sys.stderr)
    sys.exit(1)

# 2. SHARED.md (the cross-project memory index) must be readable
shared_index = os.path.join(shared_mem, "SHARED.md")
if not os.path.isfile(shared_index):
    print(f"FAIL: SHARED.md missing: {shared_index}", file=sys.stderr)
    sys.exit(1)

try:
    shared_content = open(shared_index).read()
except Exception as e:
    print(f"FAIL: cannot read SHARED.md: {e}", file=sys.stderr)
    sys.exit(1)

# 3. Projects directory must exist and be accessible
if not os.path.isdir(projects_dir):
    print(f"FAIL: Claude projects directory missing: {projects_dir}", file=sys.stderr)
    sys.exit(1)

# 4. Count accessible memory directories (non-fatal if zero — new installs may have none)
try:
    mem_dirs = [d for d in glob.glob(os.path.join(projects_dir, "*", "memory"))
                if os.path.isdir(d) and os.access(d, os.R_OK)]
except Exception as e:
    print(f"FAIL: error scanning project memory dirs: {e}", file=sys.stderr)
    sys.exit(1)

# Count shared memory files
try:
    shared_files = [f for f in os.listdir(shared_mem) if f.endswith(".md")]
except Exception as e:
    print(f"FAIL: cannot list shared memory directory: {e}", file=sys.stderr)
    sys.exit(1)

print(f"agent-memory: ok (SHARED.md={len(shared_content)}B, "
      f"{len(shared_files)} shared files, {len(mem_dirs)} project memory dirs)")
sys.exit(0)
PYEOF
