#!/usr/bin/env bash
# sync-safe
set -euo pipefail

# hex install — Creates a hex instance on the user's machine.
# Usage: bash install.sh [target_dir]
#
# hex is an all-or-nothing package. BOI (parallel workers) is integral —
# there are no flags to skip it.
#
# The repo is the installer, not the workspace. This script creates a
# separate instance directory. The repo is disposable after install.

VERSION=$(cat "$(dirname "${BASH_SOURCE[0]}")/system/version.txt" 2>/dev/null || echo "0.1.0")
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR=""

for arg in "$@"; do
    case "$arg" in
        --help|-h)   echo "Usage: bash install.sh [target_dir]"; exit 0 ;;
        -*)          echo "Unknown flag: $arg"; exit 1 ;;
        *)           TARGET_DIR="$arg" ;;
    esac
done

TARGET_DIR="${TARGET_DIR:-$HOME/hex}"
TARGET_DIR="${TARGET_DIR/#\~/$HOME}"

echo "hex v${VERSION} installer"
echo "========================"
echo ""

# ── Phase 1: Validate environment ──────────────────────────────────

echo "Checking prerequisites..."

if ! command -v python3 &>/dev/null; then
    echo "ERROR: Python 3 is required but not found."
    echo "  Install: https://www.python.org/downloads/"
    exit 1
fi

PY_VERSION=$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')
PY_MAJOR=$(echo "$PY_VERSION" | cut -d. -f1)
PY_MINOR=$(echo "$PY_VERSION" | cut -d. -f2)
if [ "$PY_MAJOR" -lt 3 ] || { [ "$PY_MAJOR" -eq 3 ] && [ "$PY_MINOR" -lt 9 ]; }; then
    echo "ERROR: Python 3.9+ required (found $PY_VERSION)."
    echo "  Install: https://www.python.org/downloads/"
    exit 1
fi

if ! command -v git &>/dev/null; then
    echo "ERROR: git is required but not found."
    echo "  Install: https://git-scm.com/downloads"
    exit 1
fi

if ! command -v claude &>/dev/null; then
    echo "NOTE: Claude Code CLI not found. Install it to use hex:"
    echo "  npm install -g @anthropic-ai/claude-code"
    echo ""
fi

if [ -d "$TARGET_DIR" ]; then
    echo "ERROR: $TARGET_DIR already exists."
    echo "  To upgrade:   bash \"$TARGET_DIR/.hex/scripts/upgrade.sh\""
    echo "  To reinstall: rm -rf \"$TARGET_DIR\" && bash install.sh"
    exit 1
fi

echo "  Python $PY_VERSION  ✓"
echo "  git               ✓"
echo ""

# ── ZONES — Core vs user-space ─────────────────────────────────────
#
# CORE (overwritten by hex upgrade):
#   $TARGET_DIR/.hex/           ← installed from system/ in hex-foundation repo
#
# USER SPACE (never touched by hex upgrade):
#   $TARGET_DIR/.hex/extensions/  ← user-installed extensions
#   $TARGET_DIR/projects/
#   $TARGET_DIR/me/
#   $TARGET_DIR/evolution/
#   $TARGET_DIR/templates/
#   $TARGET_DIR/integrations/
#   $TARGET_DIR/extensions/
#
# hex upgrade writes only to the core zone. User space is preserved.
# See ZONES.md in the hex-foundation repo for the full boundary spec.

# ── Phase 2: Create instance directory structure ───────────────────

echo "Creating hex instance at $TARGET_DIR..."

mkdir -p "$TARGET_DIR"/{me/decisions,projects/_archive,people}
mkdir -p "$TARGET_DIR"/evolution
mkdir -p "$TARGET_DIR"/landings/weekly
mkdir -p "$TARGET_DIR"/raw/{transcripts,handoffs}
mkdir -p "$TARGET_DIR"/specs/_archive

# Copy system files → .hex/   (CORE zone)
# This bulk cp covers EVERY system/* path including system/telemetry/migrations/
# (the C3 baseline VIEW migrations) and system/scripts/, system/skills/, etc.
# If you ever break this bulk cp into per-subdir copies, system/telemetry/migrations
# MUST remain covered — it carries the C3 metric VIEW DDL applied by
# telemetry-init.sh. Refactor guard: keep the literal string
# `system/telemetry/migrations` mentioned here so OBS-025 / Plan A v4-final's
# install.sh verify-only check stays green across future refactors.
cp -r "$SCRIPT_DIR/system" "$TARGET_DIR/.hex"

# Create user-space extensions directory (never overwritten by hex upgrade)
mkdir -p "$TARGET_DIR/.hex/extensions"

# Create memory directory for markdown-format memories
mkdir -p "$TARGET_DIR/.hex/memory"

# Copy root templates
cp "$SCRIPT_DIR/templates/CLAUDE.md"  "$TARGET_DIR/CLAUDE.md"
cp "$SCRIPT_DIR/templates/AGENTS.md"  "$TARGET_DIR/AGENTS.md"
cp "$SCRIPT_DIR/templates/todo.md"    "$TARGET_DIR/todo.md"

# Copy user data templates
cp "$SCRIPT_DIR/templates/me.md"            "$TARGET_DIR/me/me.md"
cp "$SCRIPT_DIR/templates/learnings.md"     "$TARGET_DIR/me/learnings.md"
cp "$SCRIPT_DIR/templates/observations.md"  "$TARGET_DIR/evolution/observations.md"
cp "$SCRIPT_DIR/templates/suggestions.md"   "$TARGET_DIR/evolution/suggestions.md"
cp "$SCRIPT_DIR/templates/changelog.md"     "$TARGET_DIR/evolution/changelog.md"

# Create evolution/eval dir (session-delta.py was ported to Rust in
# session_reflect.rs — commit a819261f / BOI S8785 — and the template
# was deleted. Dir kept for downstream tools that expect it.)
mkdir -p "$TARGET_DIR/evolution/eval"

# Copy tests
if [ -d "$SCRIPT_DIR/tests" ]; then
    cp -r "$SCRIPT_DIR/tests" "$TARGET_DIR/tests"
fi

# Copy commands to both .claude/commands/ (Claude Code) and .hex/commands/ (doctor/tooling)
if [ -d "$SCRIPT_DIR/system/commands" ]; then
    mkdir -p "$TARGET_DIR/.claude/commands"
    cp "$SCRIPT_DIR/system/commands/"*.md "$TARGET_DIR/.claude/commands/"
    mkdir -p "$TARGET_DIR/.hex/commands"
    cp "$SCRIPT_DIR/system/commands/"*.md "$TARGET_DIR/.hex/commands/"
fi

# Symlink .agents/skills/ → .hex/skills/ so tools that look in .agents/ find the same skill set
mkdir -p "$TARGET_DIR/.agents"
ln -sfn ../.hex/skills "$TARGET_DIR/.agents/skills"

# Seed optional configs doctor expects. Defaults are safe and overridable later.
echo '{}' > "$TARGET_DIR/.hex/settings.json"

# Copy hook scripts and configure Claude Code hooks in .claude/settings.json
HOOKS_MANIFEST="$SCRIPT_DIR/system/hooks/required-hooks.json"
if [ -d "$SCRIPT_DIR/system/hooks/scripts" ]; then
    mkdir -p "$TARGET_DIR/.hex/hooks/scripts"
    cp "$SCRIPT_DIR/system/hooks/scripts/"* "$TARGET_DIR/.hex/hooks/scripts/" 2>/dev/null || true
    chmod +x "$TARGET_DIR/.hex/hooks/scripts/"*.sh 2>/dev/null || true
fi
if [ -f "$HOOKS_MANIFEST" ]; then
    mkdir -p "$TARGET_DIR/.claude"
    MANIFEST_PATH="$HOOKS_MANIFEST" SETTINGS_PATH="$TARGET_DIR/.claude/settings.json" python3 << 'PYEOF'
import json, os

manifest_path = os.environ['MANIFEST_PATH']
settings_path = os.environ['SETTINGS_PATH']

with open(manifest_path) as f:
    manifest = json.load(f)

if os.path.exists(settings_path):
    with open(settings_path) as f:
        try:
            settings = json.load(f)
        except json.JSONDecodeError:
            settings = {}
else:
    settings = {}

if 'hooks' not in settings:
    settings['hooks'] = {}

hooks_section = settings['hooks']

for event_type, hook_defs in manifest.items():
    if event_type not in hooks_section:
        hooks_section[event_type] = []
    event_hooks = hooks_section[event_type]
    for hook_def in hook_defs:
        matcher = hook_def.get('matcher', '')
        if 'command' in hook_def:
            hook_command = hook_def['command']
            is_present = any(
                any(h.get('command', '') == hook_command for h in entry.get('hooks', []))
                for entry in event_hooks
            )
        else:
            script_rel = hook_def['script']
            script_name = os.path.basename(script_rel)
            hook_command = f'bash "$CLAUDE_PROJECT_DIR/{script_rel}"'
            is_present = any(
                any(script_name in h.get('command', '') for h in entry.get('hooks', []))
                for entry in event_hooks
            )
        if not is_present:
            event_hooks.append({
                'matcher': matcher,
                'hooks': [{'type': 'command', 'command': hook_command}]
            })

tmp = settings_path + '.tmp'
os.makedirs(os.path.dirname(tmp), exist_ok=True)
with open(tmp, 'w') as f:
    json.dump(settings, f, indent=2)
os.replace(tmp, settings_path)
PYEOF
    echo "  Claude Code hooks   ✓"
fi

# env.sh is already copied from system/scripts/env.sh via the cp -r above.
# Make it executable.
chmod +x "$TARGET_DIR/.hex/scripts/env.sh"
echo "  env.sh              ✓"
if [ -L /etc/localtime ]; then
    # /etc/localtime → /var/db/timezone/zoneinfo/America/Los_Angeles → America/Los_Angeles
    readlink /etc/localtime 2>/dev/null | sed 's|.*zoneinfo/||' > "$TARGET_DIR/.hex/timezone"
fi
# If detection failed or produced empty, leave the file absent (doctor will warn but not error)
if [ -f "$TARGET_DIR/.hex/timezone" ] && [ ! -s "$TARGET_DIR/.hex/timezone" ]; then
    rm -f "$TARGET_DIR/.hex/timezone"
fi

# Initialize the instance as a git repo so decision logs, landings, and
# me/ evolve with history. Quiet failure mode: skip if git init fails.
( cd "$TARGET_DIR" && git init -q 2>/dev/null && git add -A 2>/dev/null && \
    git -c user.email=hex@local -c user.name=hex commit -q -m "hex v${VERSION} initial install" 2>/dev/null ) || true

echo "  Directory structure  ✓"

# ── Phase 3: Initialize memory ─────────────────────────────────────

echo "Initializing memory database..."

python3 -c "
import sqlite3, os
db = os.path.join('$TARGET_DIR', '.hex', 'memory.db')
conn = sqlite3.connect(db)
conn.executescript('''
    CREATE TABLE IF NOT EXISTS memories (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        content TEXT NOT NULL,
        tags TEXT DEFAULT \"\",
        source TEXT DEFAULT \"\",
        created_at TEXT NOT NULL
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
        content, tags, source,
        content=memories, content_rowid=id,
        tokenize=\"unicode61\"
    );
    CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memories_fts(rowid, content, tags, source)
        VALUES (new.id, new.content, new.tags, new.source);
    END;
    CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, content, tags, source)
        VALUES (\"delete\", old.id, old.content, old.tags, old.source);
    END;
    CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
        source_path, heading, chunk_index, content,
        tokenize=\"unicode61\"
    );
    CREATE TABLE IF NOT EXISTS files (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT UNIQUE NOT NULL,
        mtime REAL NOT NULL,
        content_hash TEXT NOT NULL DEFAULT \"\",
        indexed_at TEXT NOT NULL,
        chunk_count INTEGER DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS metadata (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
''')
conn.commit()
conn.close()
"

echo "  Memory database     ✓"

# ── Phase 4: Create standing-orders reference ──────────────────────

mkdir -p "$TARGET_DIR/.hex/standing-orders"
cat > "$TARGET_DIR/.hex/standing-orders/README.md" << 'SOEOF'
# Standing Orders

The 20 core rules, 10 situational rules, and 6 product judgment rules are
defined in CLAUDE.md (system zone). This directory holds extended reference
copies with examples and context for each rule.

See CLAUDE.md → Standing Orders for the working copy.
SOEOF

echo "  Standing orders     ✓"

# ── Phase 5: Install companions ────────────────────────────────────

echo "Installing companions..."

# Memory hybrid-search deps (optional — FTS5-only mode if pip fails)
MEMORY_REQS="$SCRIPT_DIR/system/skills/memory/requirements.txt"
if [ -f "$MEMORY_REQS" ]; then
    if python3 -m pip install -q -r "$MEMORY_REQS" 2>/dev/null; then
        echo "  Memory hybrid deps  ✓"
    else
        echo "  ⚠️  Memory hybrid deps skipped — memory will use FTS5-only mode"
    fi
fi

# Read pinned versions from VERSIONS file (keeps install.sh in lock-step with
# tested boi releases). Fork-friendly: the HEX_BOI_REPO env var overrides the
# default source.
VERSIONS_FILE="$SCRIPT_DIR/VERSIONS"
if [ ! -f "$VERSIONS_FILE" ]; then
    echo "ERROR: $VERSIONS_FILE not found — this hex-foundation checkout is incomplete."
    exit 1
fi
BOI_VERSION=$(grep "^BOI_VERSION=" "$VERSIONS_FILE" | cut -d= -f2)
HARNESS_VERSION=$(grep "^HARNESS_VERSION=" "$VERSIONS_FILE" | cut -d= -f2 || true)
BOI_REPO="${HEX_BOI_REPO:-https://github.com/mrap/boi.git}"

# BOI — parallel worker dispatch (boi-v2: the canonical TOML engine).
# Builds in a MACHINE-OWNED clone under ~/.boi/src/boi and never touches a
# developer checkout (e.g. ~/github.com/mrap/boi). The previous version ran
# `checkout -f $TAG` + `checkout -B main` against the developer repo, which
# force-reset its main to the pinned tag on every install/upgrade/test run —
# silently eating merged work 4x (OBS-033, 2026-06-10). The build
# checkout stays detached at the pinned tag: it is an artifact cache, not a
# working repo, so there is no branch to leave behind.

# boi.sh wrapper for shell aliases — lives next to the binary, not in any repo.
write_boi_wrapper() {
    cat > "$HOME/.boi/bin/boi.sh" << 'BOISH'
#!/bin/bash
if [ -x "$HOME/.boi/bin/boi" ]; then
    exec "$HOME/.boi/bin/boi" "$@"
fi
echo "error: BOI binary not found at ~/.boi/bin/boi"
exit 1
BOISH
    chmod +x "$HOME/.boi/bin/boi.sh"
}

install_or_upgrade_boi() {
    local boi_build="$HOME/.boi/src/boi"
    local boi_bin="$HOME/.boi/bin/boi"
    mkdir -p "$HOME/.boi/bin" "$HOME/.boi/pids" "$HOME/.boi/logs" \
             "$HOME/.boi/worktrees" "$HOME/.boi/src"

    # TRIPWIRE (2026-06-05): record who triggers the boi rebuild/symlink loop.
    # Kept: it identified the OBS-033 resetter (codex-parity tests → install.sh).
    {
        echo "[$(date '+%F %T')] install_or_upgrade_boi BOI_VERSION=$BOI_VERSION pid=$$ ppid=$PPID"
        ps -o pid,ppid,command -p "$PPID" 2>/dev/null || true
        echo "  args: $0 $*"
    } >> "$HOME/.boi/install-tripwire.log" 2>&1 || true

    # Fast path: the machine-owned build already provides the pinned version.
    # (Also makes repeated install.sh runs — e.g. from test suites — no-ops.)
    # `|| true` inside the substitution: a present-but-unrunnable binary (e.g.
    # interrupted build) must fall through to the rebuild below, not errexit
    # the whole installer.
    if [ -x "$boi_bin" ] && \
       [ "$(readlink "$boi_bin" 2>/dev/null)" = "$boi_build/target/release/boi" ]; then
        local current
        current="v$("$boi_bin" --version 2>/dev/null | awk '/^boi /{print $2}' | tail -1 || true)"
        if [ "$current" = "$BOI_VERSION" ]; then
            echo "  BOI $BOI_VERSION already installed  ✓"
            write_boi_wrapper
            return
        fi
    fi

    # Update the machine-owned build checkout (detached at the tag). A repo
    # that cannot reach the pin (corrupt clone, force-moved tag) self-heals by
    # re-cloning fresh — never build a stale checkout and call it $BOI_VERSION.
    # (fetch failure alone is tolerated: the pinned tag may already be local.)
    if [ -d "$boi_build/.git" ]; then
        echo "  BOI build repo exists — fetching $BOI_VERSION..."
        if ! ( cd "$boi_build" && { git fetch --tags origin 2>/dev/null || true; } && \
               git checkout -f --detach "$BOI_VERSION" 2>/dev/null ); then
            echo "  BOI: build repo cannot reach $BOI_VERSION — re-cloning fresh" >&2
            rm -rf "$boi_build"
        fi
    fi
    if [ ! -d "$boi_build/.git" ]; then
        echo "  Cloning BOI build repo (machine-owned, ~/.boi/src/boi)..."
        git clone "$BOI_REPO" "$boi_build" 2>/dev/null || {
            echo "  BOI: failed to clone $BOI_REPO — keeping currently installed binary" >&2
            return
        }
        ( cd "$boi_build" && git checkout -f --detach "$BOI_VERSION" 2>/dev/null ) || {
            echo "  BOI: tag $BOI_VERSION not found in $BOI_REPO — keeping currently installed binary" >&2
            return
        }
    fi

    # Build the Rust binary (full log kept — a swallowed compiler error makes
    # failures undiagnosable, S6)
    if command -v cargo &>/dev/null; then
        echo "  Building BOI binary..."
        local build_log="$HOME/.boi/logs/boi-build.log"
        ( cd "$boi_build" && cargo build --release ) > "$build_log" 2>&1 || {
            echo "  BOI: cargo build failed — last 20 lines of $build_log:" >&2
            tail -20 "$build_log" >&2 || true
            return
        }
        # Symlink binary
        ln -sf "$boi_build/target/release/boi" "$boi_bin"
        echo "  BOI $BOI_VERSION built and linked  ✓"
    else
        echo "  ⚠️  Rust/cargo not found — cannot build BOI binary"
        echo "     Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        return
    fi

    write_boi_wrapper
}
install_or_upgrade_boi

# ── Phase 5: Register install ──────────────────────────────────────

python3 -c "
import json, os
from datetime import datetime, timezone
info = {
    'install_path': '$TARGET_DIR',
    'install_date': datetime.now(timezone.utc).isoformat(),
    'version': '$VERSION'
}
with open(os.path.expanduser('~/.hex-install.json'), 'w') as f:
    json.dump(info, f, indent=2)
"

# Seed optional configs (llm-preference, codex config) via doctor's --fix path.
# HEX_DIR must be set explicitly so doctor.sh doesn't auto-detect the caller's cwd.
# Silent; any failure is non-fatal.
HEX_DIR="$TARGET_DIR" bash "$TARGET_DIR/.hex/scripts/doctor.sh" --fix --quiet >/dev/null 2>&1 || true

# ── Phase 7: Install hex binary (unified harness + server) ────────

echo "Installing hex binary..."

mkdir -p "$TARGET_DIR/.hex/bin"
mkdir -p "$TARGET_DIR/.hex/data"
mkdir -p "$TARGET_DIR/.hex/sse/topics"

# Migration: remove old standalone hex-agent binary (replaced by symlink)
if [ -f "$TARGET_DIR/.hex/bin/hex-agent" ] && [ ! -L "$TARGET_DIR/.hex/bin/hex-agent" ]; then
    echo "  Migrating: replacing old hex-agent binary with hex + symlink..."
    rm -f "$TARGET_DIR/.hex/bin/hex-agent"
fi

_harness_build_from_source() {
    echo "  Building hex from source..."
    ( cd "$SCRIPT_DIR/system/harness" && cargo build --release 2>&1 ) || return 1
    # When system/harness is a member of a workspace (root Cargo.toml), cargo
    # emits to the workspace-root target dir, NOT system/harness/target. Probe
    # both so the cp doesn't silently fail and fall back to a network download.
    local built=""
    local candidate
    for candidate in \
        "$SCRIPT_DIR/system/harness/target/release/hex" \
        "$SCRIPT_DIR/target/release/hex"; do
        if [ -x "$candidate" ]; then built="$candidate"; break; fi
    done
    if [ -z "$built" ]; then
        echo "  hex binary not found after build (checked system/harness/target and workspace target)" >&2
        return 1
    fi
    cp "$built" "$TARGET_DIR/.hex/bin/hex"
    chmod +x "$TARGET_DIR/.hex/bin/hex"
    ln -sf hex "$TARGET_DIR/.hex/bin/hex-agent"
}

_harness_download_prebuilt() {
    local arch os harness_url
    arch=$(uname -m)
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    harness_url="https://github.com/mrap/hex-foundation/releases/download/${HARNESS_VERSION}/hex-${os}-${arch}"
    echo "  Downloading hex from ${harness_url}..."
    curl -fSL "$harness_url" -o "$TARGET_DIR/.hex/bin/hex" && chmod +x "$TARGET_DIR/.hex/bin/hex"
    ln -sf hex "$TARGET_DIR/.hex/bin/hex-agent"
}

_harness_warn_missing() {
    echo ""
    echo "WARNING: hex binary could not be built or downloaded."
    echo "  Install Rust (https://rustup.rs) and re-run to build the hex binary."
    echo "  Core shell functionality (BOI, memory scripts) still works without it."
    echo ""
}

if command -v cargo &>/dev/null; then
    _harness_build_from_source || {
        echo "  Build failed — trying pre-built binary download..."
        if command -v curl &>/dev/null; then
            _harness_download_prebuilt || _harness_warn_missing
        else
            echo "  curl not found — skipping pre-built download"
            _harness_warn_missing
        fi
    }
elif command -v curl &>/dev/null; then
    echo "  cargo not found — trying pre-built binary download..."
    _harness_download_prebuilt || _harness_warn_missing
else
    echo "  cargo and curl not found — skipping binary install"
    _harness_warn_missing
fi

# Copy SSE topic manifests
if [ -d "$SCRIPT_DIR/system/sse/topics" ]; then
    cp -R "$SCRIPT_DIR/system/sse/topics/"*.yaml "$TARGET_DIR/.hex/sse/topics/" 2>/dev/null || true
fi

# Copy CLI helpers
for helper in hex-asset hex-comment-respond.sh hex-sse-publish hex-sse-listen; do
    if [ -f "$SCRIPT_DIR/system/scripts/bin/$helper" ]; then
        cp "$SCRIPT_DIR/system/scripts/bin/$helper" "$TARGET_DIR/.hex/bin/$helper"
        chmod +x "$TARGET_DIR/.hex/bin/$helper"
    fi
done

if [ -x "$TARGET_DIR/.hex/bin/hex" ]; then
    if ! "$TARGET_DIR/.hex/bin/hex" version &>/dev/null; then
        echo "WARNING: hex binary installed but failed to execute. Re-run install to retry."
    else
        hex_ver=$("$TARGET_DIR/.hex/bin/hex" version 2>/dev/null || echo "unknown")
        echo "  hex binary          ✓ ($hex_ver)"
        # Verify symlink works
        if [ -L "$TARGET_DIR/.hex/bin/hex-agent" ]; then
            echo "  hex-agent symlink   ✓"
        else
            echo "  hex-agent symlink   ⚠ (creating...)"
            ln -sf hex "$TARGET_DIR/.hex/bin/hex-agent"
        fi
    fi
else
    echo "  hex binary          ⚠ (install Rust to enable agent fleet + server)"
fi

# ── Phase 8: Shell environment setup ─────────────────────────────

SHELL_RC=""
if [[ -n "${ZSH_VERSION:-}" ]] || [[ "$SHELL" == */zsh ]]; then
    SHELL_RC="$HOME/.zshrc"
elif [[ -n "${BASH_VERSION:-}" ]] || [[ "$SHELL" == */bash ]]; then
    SHELL_RC="$HOME/.bashrc"
fi

if [[ -n "$SHELL_RC" ]]; then
    NEEDS_WRITE=false
    if ! grep -q 'export HEX_DIR=' "$SHELL_RC" 2>/dev/null; then
        NEEDS_WRITE=true
    fi

    if $NEEDS_WRITE; then
        echo "Setting up shell environment in $SHELL_RC..."
        cat >> "$SHELL_RC" << RCEOF

# =====================
# Hex Agent
# =====================
export HEX_DIR="$TARGET_DIR"
export AGENT_DIR="\$HEX_DIR"  # deprecated alias — use HEX_DIR
export PATH="\$HEX_DIR/.hex/bin:\$PATH"
RCEOF
        echo "  HEX_DIR, AGENT_DIR (deprecated alias), PATH added to $SHELL_RC ✓"
        echo "  Run 'source $SHELL_RC' or restart your terminal to activate."
    else
        echo "  HEX_DIR already in $SHELL_RC ✓"
    fi

    # Shell completions — sourced from the binary so they always match the
    # installed version. Self-contained (no fpath/compinit ordering deps).
    if ! grep -q 'hex completions' "$SHELL_RC" 2>/dev/null; then
        if [[ "$SHELL_RC" == *.bashrc ]]; then COMP_SHELL="bash"; else COMP_SHELL="zsh"; fi
        cat >> "$SHELL_RC" << RCEOF

# hex shell completions
command -v hex >/dev/null 2>&1 && source <(hex completions $COMP_SHELL)
RCEOF
        echo "  hex completions ($COMP_SHELL) added to $SHELL_RC ✓"
    else
        echo "  hex completions already in $SHELL_RC ✓"
    fi
else
    echo ""
    echo "Add these to your shell rc file:"
    echo "  export HEX_DIR=\"$TARGET_DIR\""
    echo "  export AGENT_DIR=\"\$HEX_DIR\"  # deprecated alias — use HEX_DIR"
    echo "  export PATH=\"\$HEX_DIR/.hex/bin:\$PATH\""
fi

echo ""
echo "========================================="
echo " hex installed at $TARGET_DIR"
echo "========================================="
echo ""
echo "Start your first session:"
echo "  cd $TARGET_DIR && claude"
echo ""
echo "Your agent will walk you through setup."
