#!/usr/bin/env bash
# Phase 0 lighter inventory for a cleanup campaign — run when no repo-audit
# report exists. Prints a structured markdown report to stdout; nothing here
# mutates the repo. Every step reports what it did or explicitly says it was
# skipped and why — no silent gaps.
#
# Usage: scripts/inventory.sh [path-to-repo]   (default: current directory)

set -uo pipefail  # not -e: keep going and report each check's own status

REPO="${1:-.}"
cd "$REPO" || { echo "ERROR: cannot cd into $REPO" >&2; exit 1; }

echo "# Cleanup inventory — $(date -u +%FT%TZ)"
echo
echo "Repo: $(pwd)"
echo "HEAD: $(git rev-parse --short HEAD 2>/dev/null || echo 'not a git repo — STOP, worktree/commit discipline requires git')"
echo "Dirty paths (must be clean or accounted for before the worktree is cut):"
git status --porcelain 2>/dev/null | head -20
echo

# --- 0. Existing audit report? Consume it instead of duplicating triage. ---
echo "## Existing repo-audit report"
audit_files=$(find . -path ./.git -prune -o -path "*/docs/audits/*repo-audit.md" -print 2>/dev/null)
if [ -n "$audit_files" ]; then
  echo "Found — consume this as the Phase 0 candidate list instead of the sections below:"
  echo "$audit_files"
else
  echo "None found under docs/audits/. Proceeding with a lighter inventory (not a substitute for a full audit)."
fi
echo

# --- 1. Ecosystem / manifest detection ---
echo "## Ecosystem"
for manifest in package.json Cargo.toml pyproject.toml setup.py go.mod; do
  if [ -f "$manifest" ]; then
    echo "- Found $manifest"
  fi
done
echo

# --- 2. LOC / complexity baseline ---
echo "## LOC baseline"
if command -v scc >/dev/null 2>&1; then
  scc --no-cocomo 2>/dev/null | head -30
  echo "(scc's complexity/estimate figures are a RELATIVE ranking index only — never a timeline or cost estimate.)"
elif command -v tokei >/dev/null 2>&1; then
  tokei 2>/dev/null
else
  echo "Neither scc nor tokei found (brew install scc). Raw line count fallback:"
  find . -path ./.git -prune -o -type f -print 2>/dev/null | xargs wc -l 2>/dev/null | tail -1
fi
echo

# --- 3. Churn hotspots ---
echo "## Churn hotspots (last 200 commits, top 20)"
git log --format= --name-only -200 2>/dev/null | sed '/^$/d' | sort | uniq -c | sort -rn | head -20
echo "(Cross-reference with complexity: high churn x low health first — reference.md §9.)"
echo

# --- 4. Baseline gate ---
echo "## Baseline gate"
echo "Run: bash scripts/verify.sh <this-repo>   — the SAME artifact every later batch uses."
echo "Build+test red: fixing that IS the Phase 0 deliverable. Lint red: expected, never blocks (HARD RULE 8)."
echo

# --- 5. Existing lint/dead-code config ---
echo "## Cleanup toolchain availability"
for t in scc jscpd ast-grep cargo-machete; do
  command -v "$t" >/dev/null 2>&1 && echo "- $t: available" || echo "- $t: MISSING (scc/jscpd: brew install; cargo-machete: cargo install cargo-machete; ast-grep: brew install ast-grep) — affected passes must be skipped LOUDLY, not faked"
done
echo

echo "## Existing lint/dead-code config"
for cfg in .eslintrc* biome.json knip.json knip.jsonc .ruff.toml ruff.toml pyproject.toml .golangci.yml .golangci.yaml clippy.toml; do
  [ -e "$cfg" ] && echo "- $cfg present"
done
echo

echo "## Next step"
echo "Write the quality-bar statement (2-3 sentences) and the batch plan into CLEANUP-PROGRESS.md,"
echo "then Phase 1 (Safety Net) per SKILL.md."
