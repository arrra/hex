#!/usr/bin/env bash
# sanitize-check.sh — Scan for personalization that would break for other users.
# Exits 0 if clean, 1 with a list of violations if any are found.
#
# Usage:
#   bash system/scripts/sanitize-check.sh
#   bash system/scripts/sanitize-check.sh --verbose  # show each matching line

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SELF="$(basename "${BASH_SOURCE[0]}")"
VERBOSE=false
for arg in "$@"; do [[ "$arg" == "--verbose" ]] && VERBOSE=true; done

cd "$REPO_DIR"

red()   { printf '\033[31m%s\033[0m\n' "$*" >&2; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }

VIOLATIONS=()

# run_check LABEL GREP_PATTERN [extra grep args...]
# Runs grep, filters common false positives, records violations.
run_check() {
    local label="$1"
    local pattern="$2"
    shift 2
    local results
    results=$(grep -rn "$pattern" . \
        --exclude-dir=.git \
        "$@" 2>/dev/null \
        | grep -v "/${SELF}:" \
        | grep -v "personalization-audit" \
        | grep -v "Co-Authored" \
        | grep -v -i "# example\|# e\.g\.\|example:\|# Example" \
        || true)
    if [ -n "$results" ]; then
        VIOLATIONS+=("$label")
        COUNT=$(echo "$results" | wc -l | tr -d ' ')
        if $VERBOSE; then
            red "  [$label] $COUNT violation(s):"
            echo "$results" | while IFS= read -r line; do
                printf '    %s\n' "$line" >&2
            done
        else
            red "  [$label] $COUNT violation(s)"
        fi
    fi
}

echo "Scanning for personalization violations..."
echo ""

# Absolute user home paths (hardcoded, not via $HOME or ~)
run_check "hardcoded /Users/ path" "/Users/[a-zA-Z]" \
    --include="*.py" --include="*.sh" --include="*.yaml" \
    --include="*.md" --include="*.json" --include="*.toml"

# mrap-specific identifiers
run_check "mrap-specific identifier" \
    "mrap-hex\|mrap-mrap\|mike@mrap\|mrap\.me\|Mike Rapadas" \
    --include="*.py" --include="*.sh" --include="*.yaml" \
    --include="*.md" --include="*.json"

# Slack-specific channel IDs
run_check "Slack channel IDs" \
    "C0AQZR31EET\|C0AUEAFASQP\|C0B05456Z2L"

# Tailscale hostname/IP specific to this machine
run_check "Tailscale hostname/IP" \
    "tailbd5748\|mac-mini\.tail\|100\.101\.9\."

# macOS LaunchAgent plists tied to com.mrap namespace
run_check "com.mrap. LaunchAgent" \
    "com\.mrap\." \
    --include="*.py" --include="*.sh" --include="*.plist"

# Hardcoded /opt/homebrew when NOT behind an existence guard.
# Legitimate uses (inside "if [ -d /opt/homebrew ]" blocks, macOS VM builders) are excluded.
BREW_VIOLATIONS=$(grep -rn "/opt/homebrew" . \
    --exclude-dir=.git \
    --exclude-dir=eval \
    --exclude-dir=tests \
    --include="*.py" --include="*.sh" 2>/dev/null \
    | grep -v "/${SELF}:" \
    | grep -v 'if.*-d.*opt/homebrew' \
    | grep -v '\[ -d.*opt/homebrew' \
    | grep -v '\[\[ -d.*opt/homebrew' \
    | grep -v 'opt/homebrew.*&&\|&&.*opt/homebrew' \
    | grep -v '_add_to_path' \
    | grep -v "personalization-audit" \
    || true)
if [ -n "$BREW_VIOLATIONS" ]; then
    VIOLATIONS+=("hardcoded /opt/homebrew")
    COUNT=$(echo "$BREW_VIOLATIONS" | wc -l | tr -d ' ')
    if $VERBOSE; then
        red "  [hardcoded /opt/homebrew] $COUNT violation(s):"
        echo "$BREW_VIOLATIONS" | while IFS= read -r line; do
            printf '    %s\n' "$line" >&2
        done
    else
        red "  [hardcoded /opt/homebrew] $COUNT violation(s)"
    fi
fi

# Hardcoded secrets paths with actual credentials (not generic placeholders)
SECRETS_VIOLATIONS=$(grep -rn "secrets/slack-bot-token\|\.hex/secrets/[a-zA-Z][a-zA-Z0-9_-]*\.\(env\|key\)" . \
    --exclude-dir=.git \
    --include="*.py" --include="*.sh" 2>/dev/null \
    | grep -v "/${SELF}:" \
    | grep -v "personalization-audit" \
    | grep -v '<name>\|REPLACE_ME\|YOUR_' \
    || true)
if [ -n "$SECRETS_VIOLATIONS" ]; then
    VIOLATIONS+=("hardcoded secrets path")
    COUNT=$(echo "$SECRETS_VIOLATIONS" | wc -l | tr -d ' ')
    if $VERBOSE; then
        red "  [hardcoded secrets path] $COUNT violation(s):"
        echo "$SECRETS_VIOLATIONS" | while IFS= read -r line; do
            printf '    %s\n' "$line" >&2
        done
    else
        red "  [hardcoded secrets path] $COUNT violation(s)"
    fi
fi

echo ""

if [ ${#VIOLATIONS[@]} -eq 0 ]; then
    green "CLEAN — no personalization violations found"
    exit 0
else
    red "VIOLATIONS FOUND in: ${VIOLATIONS[*]}"
    red "Run with --verbose for details. Fix before pushing."
    exit 1
fi
