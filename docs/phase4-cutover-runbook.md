# Phase 4 Cutover Runbook — Python → Rust hex-events Daemon

**Date prepared:** 2026-05-16  
**Author:** BOI S6258 / T9AA7  
**DO NOT EXECUTE** until Mike reviews and approves.

---

## Pre-Cutover Checklist

Before starting, confirm all of the following:

- [ ] Shadow-mode diff (`/tmp/shadow-diff.md`) shows no meaningful divergences
- [ ] `hex events status` returns clean output (no schema errors)
- [ ] `hex events policies` loads all expected policies without validation errors
- [ ] Rust binary at `/Users/mrap/mrap-hex/.hex/bin/hex` is built from latest `rustify` branch
- [ ] Python daemon currently running: `launchctl list com.mrap.hex-eventd | grep PID`

---

## Pre-Cutover: Backup + Snapshot

```bash
# 1. Snapshot the events database (121 MB — this is the authoritative event log)
cp /Users/mrap/.hex-events/events.db \
   /Users/mrap/.hex-events/events.db.pre-cutover-$(date +%Y%m%d-%H%M%S)

# 2. Snapshot daemon log
cp /Users/mrap/.hex-events/daemon.log \
   /Users/mrap/.hex-events/daemon.log.pre-cutover-$(date +%Y%m%d-%H%M%S)

# 3. Backup the current Python plist
cp /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist \
   /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist.python-backup

# 4. Verify Rust binary health
/Users/mrap/mrap-hex/.hex/bin/hex events status
/Users/mrap/mrap-hex/.hex/bin/hex events policies 2>&1 | tail -5
```

---

## Cutover Steps

```bash
# 1. Stop the Python daemon gracefully
launchctl unload /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist

# 2. Confirm the Python process is gone
sleep 3
launchctl list com.mrap.hex-eventd 2>&1
# Expected: "Could not find service" or no PID in output

# 3. Install the Rust plist
cp /tmp/com.mrap.hex-eventd.rust.plist \
   /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist

# 4. Load the Rust daemon
launchctl load /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist

# 5. Confirm the Rust daemon is running
sleep 5
launchctl list com.mrap.hex-eventd
# Expected: PID is populated (not "-")

# 6. Tail the daemon log and confirm heartbeat within 90 seconds
tail -f /Users/mrap/.hex-events/daemon.log | grep -m1 "heartbeat"

# 7. Emit a test event and confirm it is processed
hex emit test.cutover.smoke '{"source": "runbook"}'
sleep 5
hex events trace --event test.cutover.smoke --limit 1

# 8. Confirm minutely tick fires at the next minute boundary
# (watch log for timer.tick.minutely)
tail -f /Users/mrap/.hex-events/daemon.log | grep -m1 "timer.tick.minutely"
```

---

## Rollback Procedure

If the Rust daemon fails (crashes, misses events, wrong actions):

```bash
# 1. Unload the Rust daemon immediately
launchctl unload /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist

# 2. Restore Python plist
cp /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist.python-backup \
   /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist

# 3. Reload Python daemon
launchctl load /Users/mrap/Library/LaunchAgents/com.mrap.hex-eventd.plist

# 4. Confirm Python daemon running
sleep 5
launchctl list com.mrap.hex-eventd

# 5. Tail log to confirm Python daemon is processing events
tail -20 /Users/mrap/.hex-events/daemon.log
```

Rollback is fully reversible — the Python plist backup and all Python source files
remain untouched throughout the cutover.

---

## 24-Hour Monitoring Checklist

Check these every few hours for the first 24 hours:

**Daemon health:**
- [ ] `launchctl list com.mrap.hex-eventd` shows PID (not crashed)
- [ ] `tail -100 /Users/mrap/.hex-events/daemon.log | grep ERROR` — zero errors
- [ ] Heartbeat entries in log are continuous (every 60s)

**Event processing:**
- [ ] `hex events trace --limit 20` shows recent events being matched
- [ ] Timer ticks (`timer.tick.minutely`, `timer.tick.5m`) appearing on schedule
- [ ] `hex events status` reports healthy queue depth

**Action execution:**
- [ ] Shell actions completing (check log for `action:shell` entries)
- [ ] No rate-limit floods (check for repeated `rate-limited` warnings)
- [ ] `notify` actions delivering to macOS notification center

**DB integrity:**
- [ ] `sqlite3 /Users/mrap/.hex-events/events.db "SELECT COUNT(*) FROM events WHERE ts > strftime('%s','now') - 3600;"` — events accumulating

**After 24 hours — if clean:**
```bash
# Quarantine Python source files (don't delete yet)
cd /Users/mrap/.hex-events
for f in *.py; do mv "$f" "${f%.py}.legacy.py"; done
for f in actions/*.py; do mv "$f" "${f%.py}.legacy.py"; done
```

---

## Files Modified by Cutover

| File | Change |
|------|--------|
| `~/Library/LaunchAgents/com.mrap.hex-eventd.plist` | Replaced with Rust plist |
| `~/Library/LaunchAgents/com.mrap.hex-eventd.plist.python-backup` | Created (rollback target) |
| `~/.hex-events/events.db.pre-cutover-*` | Created (snapshot) |
| `~/.hex-events/daemon.log.pre-cutover-*` | Created (snapshot) |

The events.db schema is identical between Python and Rust daemons — no migration needed.
