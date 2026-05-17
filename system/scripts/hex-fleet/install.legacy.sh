#!/bin/bash
# QUARANTINED — ported to `hex fleet install` (system/harness/src/fleet.rs)
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLIST_NAME="com.hex.fleet-manager.plist"
LAUNCH_AGENTS_DIR="$HOME/Library/LaunchAgents"

# Ensure log directory exists
mkdir -p ~/.boi/pm

# Copy plist
cp "$SCRIPT_DIR/$PLIST_NAME" "$LAUNCH_AGENTS_DIR/$PLIST_NAME"

# Load the agent
launchctl load "$LAUNCH_AGENTS_DIR/$PLIST_NAME"

echo "Hex Fleet Manager installed and running"
echo "Logs: ~/.boi/pm/fleet-manager.stdout.log"
