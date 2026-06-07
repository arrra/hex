#!/usr/bin/env bash
# Red/green proof for worktree-guard.sh (S7 enforcement / OBS-030).
#
# Replays the OBS-030 incident against a real flagged repo + a real linked
# worktree, asserting the guard DENIES the shared-checkout edit and ALLOWS the
# worktree edit. No mocks of git — actual `git worktree` topology.

set -u
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd -P)
GUARD="$SCRIPT_DIR/../system/hooks/scripts/worktree-guard.sh"

pass=0; fail=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
no()   { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }

# decision <tool> <file> [env] -> prints "allow" | "deny"
decision() {
  local tool="$1" file="$2"
  printf '{"tool_name":"%s","tool_input":{"file_path":"%s"}}' "$tool" "$file" \
    | bash "$GUARD" 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["hookSpecificOutput"]["permissionDecision"])' 2>/dev/null
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# --- build a flagged repo (named hex-foundation) with a shared checkout + worktree ---
FLAGGED="$TMP/hex-foundation"
git init -q "$FLAGGED"
git -C "$FLAGGED" config user.email t@t.t; git -C "$FLAGGED" config user.name t
printf 'readme\n' > "$FLAGGED/README.md"
printf '*.lock\nstate.json\n' > "$FLAGGED/.gitignore"
git -C "$FLAGGED" add -A; git -C "$FLAGGED" commit -qm init
git -C "$FLAGGED" worktree add -q "$TMP/hex-foundation-wt" -b feat 2>/dev/null

# --- build a NON-flagged repo (different name) with a shared checkout ---
OTHER="$TMP/some-app"
git init -q "$OTHER"
git -C "$OTHER" config user.email t@t.t; git -C "$OTHER" config user.name t
printf 'x\n' > "$OTHER/README.md"
git -C "$OTHER" add -A; git -C "$OTHER" commit -qm init

echo "worktree-guard red/green proof (replays OBS-030):"

# 1. THE INCIDENT (red case now caught): edit README in the flagged SHARED checkout.
d=$(decision Edit "$FLAGGED/README.md")
[ "$d" = "deny" ] && ok "OBS-030 incident: Edit README in shared hex-foundation checkout -> DENY" \
                  || no "OBS-030 incident should DENY, got '$d'"

# 2. Write a NEW file in the flagged shared checkout (the module-docs case) -> deny.
d=$(decision Write "$FLAGGED/docs/new-guide.md")
[ "$d" = "deny" ] && ok "Write new file in shared flagged checkout -> DENY" \
                  || no "new file in shared flagged checkout should DENY, got '$d'"

# 3. THE CORRECT PATH (green): same edit, but in a linked worktree -> allow.
d=$(decision Edit "$TMP/hex-foundation-wt/README.md")
[ "$d" = "allow" ] && ok "Edit README in a hex-foundation WORKTREE -> ALLOW" \
                   || no "worktree edit should ALLOW, got '$d'"

# 4. Non-flagged repo's shared checkout -> allow (guard is not over-broad).
d=$(decision Edit "$OTHER/README.md")
[ "$d" = "allow" ] && ok "Edit in non-flagged repo's shared checkout -> ALLOW" \
                   || no "non-flagged repo should ALLOW, got '$d'"

# 5. Ignored runtime file in flagged shared checkout -> allow (tracked-dirty vs runtime).
d=$(decision Write "$FLAGGED/state.json")
[ "$d" = "allow" ] && ok "Ignored runtime file in flagged checkout -> ALLOW" \
                   || no "ignored runtime file should ALLOW, got '$d'"

# 6. Non-mutating tool (Read) on the flagged shared checkout -> allow.
d=$(decision Read "$FLAGGED/README.md")
[ "$d" = "allow" ] && ok "Read (non-mutating) in flagged checkout -> ALLOW" \
                   || no "Read should ALLOW, got '$d'"

# 7. Path outside any git repo -> allow (fail-open).
d=$(decision Edit "$TMP/loose.txt")
[ "$d" = "allow" ] && ok "Edit outside any git repo -> ALLOW (fail-open)" \
                   || no "non-repo path should ALLOW, got '$d'"

echo "----"
echo "passed: $pass  failed: $fail"
[ "$fail" -eq 0 ]
