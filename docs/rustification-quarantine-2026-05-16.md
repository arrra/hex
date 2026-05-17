# Rustification Quarantine Log — 2026-05-16

## Summary

Final sweep quarantine pass executed by BOI worker S9D14.

### hex: 99 files quarantined

All files at `.hex/scripts/` that were confirmed duplicates of
hex-foundation canonical copies (or had git port commits) were renamed
to `.legacy.{sh,py}`. This eliminates drift between the personal
instance and hex-foundation.

- 96 tracked via `git mv`
- 3 untracked renamed on disk (`hex-alert.sh`, `check-fleet-pulse.sh`,
  `check-stalled-initiatives.sh`)

### hex-foundation: 0 new quarantines

hex-foundation already has `.legacy.*` counterparts for all confirmed
Rust-ported scripts (`env.sh.legacy.sh`, `hex-agent-spawn.sh.legacy.sh`,
etc.). The live `env.sh` and `hex-agent-spawn.sh` in foundation are thin
shims (intentional wrappers calling `hex env` / `hex agent spawn`) and
must remain.

Scripts still in active use by hex-events policies
(`initiative-watchdog.py`, `hex-experiment.py`, `hex-initiative-loop-v2.py`)
are classified PORT-NOW and are handled by the PORT-NOW work list.

## Next Steps

- See `PORT-NOW` bucket in final sweep report for remaining Rust port work
- Delete `DELETE` bucket files (task TA789)
- Move `MOVE-TO-USER-SPACE` files (task T8D0A)
