#!/usr/bin/env bash
# Red/green proof for `hex hook worktree-guard` (S7 enforcement / OBS-030).
#
# Replays the OBS-030 incident against real git topology: ANY repo's shared
# checkout must DENY a mutating edit; a linked worktree must pass; the one
# exemption is the $HEX_DIR workspace repo. No mocks of git.
#
# Contract under test (deny-only guard):
#   deny  = JSON permissionDecision:"deny" on stdout + exit 2
#   allow = ABSTAIN: no output + exit 0
#
# Binary: $HEX_BIN if set, else the worktree's release build, else `hex` on PATH.

set -u
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd -P)
HEX_BIN="${HEX_BIN:-}"
if [ -z "$HEX_BIN" ]; then
    for cand in "$SCRIPT_DIR/../system/harness/target/release/hex" "$(command -v hex || true)"; do
        [ -n "$cand" ] && [ -x "$cand" ] && HEX_BIN="$cand" && break
    done
fi
if [ -z "$HEX_BIN" ]; then
    echo "SKIP: no hex binary found (set HEX_BIN or build system/harness)"; exit 0
fi

pass=0; fail=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
no()   { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }

# decision <tool> <file> -> prints "allow" | "deny"
# Abstain (empty stdout, exit 0) is the allow path of the deny-only guard.
decision() {
  local tool="$1" file="$2" out
  out=$(printf '{"tool_name":"%s","tool_input":{"file_path":"%s"}}' "$tool" "$file" \
        | "$HEX_BIN" hook worktree-guard 2>/dev/null)
  if [ -z "$out" ]; then echo "allow"; else
    echo "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["hookSpecificOutput"]["permissionDecision"])' 2>/dev/null
  fi
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

mkrepo() { # mkrepo <dir>
  git init -q "$1"
  git -C "$1" config user.email t@t.t; git -C "$1" config user.name t
  printf 'readme\n' > "$1/README.md"
  printf '*.lock\nstate.json\n' > "$1/.gitignore"
  git -C "$1" add -A; git -C "$1" commit -qm init
}

# --- any old repo (no allowlist anymore) with a shared checkout + worktree ---
REPO="$TMP/some-app"
mkrepo "$REPO"
git -C "$REPO" worktree add -q "$TMP/some-app-wt" -b feat 2>/dev/null

# --- a repo standing in for the $HEX_DIR workspace ---
HEXWS="$TMP/hexws"
mkrepo "$HEXWS"

# HEX_DIR points at the workspace stand-in, NOT at $REPO — so $REPO is guarded
# and $HEXWS is exempt.
export HEX_DIR="$HEXWS"

echo "worktree-guard red/green proof (replays OBS-030, any-repo scope):"

# 1. THE INCIDENT (red case now caught): edit README in ANY repo's shared checkout.
d=$(decision Edit "$REPO/README.md")
[ "$d" = "deny" ] && ok "OBS-030 incident: Edit README in a shared checkout -> DENY" \
                  || no "shared-checkout edit should DENY, got '$d'"

# 2. Write a NEW file in the shared checkout (the module-docs case) -> deny.
d=$(decision Write "$REPO/docs/new-guide.md")
[ "$d" = "deny" ] && ok "Write new file in a shared checkout -> DENY" \
                  || no "new file in shared checkout should DENY, got '$d'"

# 3. THE CORRECT PATH (green): same edit, but in a linked worktree -> allow.
d=$(decision Edit "$TMP/some-app-wt/README.md")
[ "$d" = "allow" ] && ok "Edit README in a linked WORKTREE -> ALLOW" \
                   || no "worktree edit should ALLOW, got '$d'"

# 4. The $HEX_DIR workspace repo -> allow (the one exemption).
d=$(decision Edit "$HEXWS/README.md")
[ "$d" = "allow" ] && ok "Edit in the \$HEX_DIR workspace repo -> ALLOW (exempt)" \
                   || no "\$HEX_DIR workspace should ALLOW, got '$d'"

# 5. Ignored runtime file in a shared checkout -> allow (tracked-dirty vs runtime).
d=$(decision Write "$REPO/state.json")
[ "$d" = "allow" ] && ok "Ignored runtime file in shared checkout -> ALLOW" \
                   || no "ignored runtime file should ALLOW, got '$d'"

# 6. Non-mutating tool (Read) on the shared checkout -> allow.
d=$(decision Read "$REPO/README.md")
[ "$d" = "allow" ] && ok "Read (non-mutating) in shared checkout -> ALLOW" \
                   || no "Read should ALLOW, got '$d'"

# 7. Path outside any git repo -> allow (fail-open).
d=$(decision Edit "$TMP/loose.txt")
[ "$d" = "allow" ] && ok "Edit outside any git repo -> ALLOW (fail-open)" \
                   || no "non-repo path should ALLOW, got '$d'"

echo "----"
echo "passed: $pass  failed: $fail"
[ "$fail" -eq 0 ]
