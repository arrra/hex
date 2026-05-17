#!/bin/bash
set -uo pipefail
mkdir -p ~/.boi/pm
cp com.hex.boi-pm.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.hex.boi-pm.plist
echo "BOI PM installed and running"
