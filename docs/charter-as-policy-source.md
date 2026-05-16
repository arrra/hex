# Charter as Policy Source — Agent Authoring Guide

> Status: Phase 1 implemented (2026-05-16)
> Applies to: hex-foundation/system/harness daemon v2+

The hex-events daemon reads agent wake configuration directly from each agent's `charter.yaml`. There is no separate policy file for agent wakes. The charter's `wake:` block is the single source of truth.

---

## The `wake:` Block Schema

```yaml
wake:
  enabled: bool                    # default true; false = hibernated (manual wake still works)
  rate_limit:                      # null = no rate limit; absence = validation error
    max_fires: 4                   # integer, max fires allowed in window
    window: "6h"                   # duration string: "60m", "6h", "24h", "120m", etc.
  command: string | null           # override wake invocation; null = use default
  triggers:                        # required; empty list = validation error
    - event: "timer.tick.daily"    # event name from allowlist
      condition: null              # optional shell expression; null = always fire
    - event: "attention.needed"
      condition: "test -f ~/.hex-events/projects/my-agent/active"
  on_success:                      # events to emit when wake exits 0
    - "hex.agent.my-agent.woke"
  on_failure:                      # events to emit when wake exits non-zero or times out
    - "hex.agent.my-agent.failed"
```

All fields under `wake:` except `enabled` and `triggers` are optional at the schema level. The daemon validates:

- `wake.triggers` must be present and non-empty
- `wake.rate_limit` must be explicitly `null` or a valid `{max_fires, window}` object — omitting it entirely is a validation error
- Trigger event names must appear in the daemon's allowlist
- Validation failure: daemon logs the error, excludes the agent from processing, and writes an entry to `health.json` under `health.agents.<id>`

---

## Special Cases

### Rate-Limited Agent

An agent that should not fire more than 4 times per 6 hours:

```yaml
wake:
  enabled: true
  rate_limit:
    max_fires: 4
    window: "6h"
  triggers:
    - event: "timer.tick.6h"
    - event: "attention.needed"
  on_success:
    - "hex.agent.scout.wake"
  on_failure: []
```

Rate limit state is persisted in SQLite (`agent_wake_fires` table). A daemon restart does **not** reset the window — a rate-limited agent that fired 4 times before restart will still be blocked until the window expires.

For an agent with no rate limit, use `rate_limit: null` explicitly:

```yaml
wake:
  rate_limit: null
  triggers:
    - event: "boi.spec.completed"
```

### Custom Wake Command

Use `command:` when the agent's wake invocation requires a custom script instead of the default `nohup hex agent wake <id> --trigger <event>`:

```yaml
wake:
  enabled: true
  rate_limit: null
  command: "/Users/mrap/mrap-hex/.hex/scripts/boi-optimizer-wake.sh {{ event.type }} {{ event.payload | tojson }}"
  triggers:
    - event: "boi.spec.failed"
    - event: "boi.spec.completed"
  on_failure:
    - "hex.policy.boi-optimizer-agent.failed"
```

`{{ event.* }}` variables use Tera-style template substitution. Available variables:

| Variable | Description |
|----------|-------------|
| `event.type` | The event name that triggered the wake |
| `event.payload` | The full event payload (use `| tojson` to serialize) |
| `event.source` | The event emitter's source identifier |

The command is dispatched via the daemon's nohup-style background shell runner — same infrastructure as the default invocation.

### Conditional Trigger

Use `condition:` when a trigger should only fire if a shell expression exits 0:

```yaml
wake:
  enabled: true
  rate_limit:
    max_fires: 6
    window: "60m"
  triggers:
    - event: "boi.spec.completed"
      condition: "grep -qx \"{{ event.spec_id }}\" ~/.hex-events/projects/hex-autonomy/dispatched-specs.txt"
    - event: "attention.needed"
      condition: null
  on_success:
    - "hex.agent.hex-autonomy.wake"
```

Condition evaluation rules:

- Exit 0 → condition met, proceed with wake
- Non-zero exit → condition failed, skip this wake (logged at DEBUG level)
- Timeout (5 second limit) or referenced file missing → logged at ERROR level, wake skipped, `health.agents.<id>.degraded = true` written to health.json
- `condition: null` → always fires (no guard)

Conditions are evaluated per-trigger, not per-agent. An agent with 3 triggers will evaluate each trigger's condition independently.

---

## Hibernating an Agent

Set `wake.enabled: false` to hibernate an agent. The daemon will not load the agent's triggers or include it in event fan-out:

```yaml
wake:
  enabled: false
  rate_limit:
    max_fires: 4
    window: "24h"
  triggers:
    - event: "timer.tick.6h"
```

Manual wake still works: `hex agent wake <id>` bypasses the daemon's enabled check entirely. Hibernation only suppresses automatic event-driven wakes.

To restore: set `enabled: true` (or remove the field — default is `true`). The daemon picks up the change within its next mtime-polling cycle (≤10 seconds).

---

## Validating a New Charter

Run the doctor check after adding or editing a `wake:` block:

```bash
hex doctor charter-triggers
```

This checks every agent charter for:
- `wake.triggers` present and non-empty
- `wake.rate_limit` explicitly declared (not absent)
- Trigger event names in the daemon allowlist
- `on_success` / `on_failure` format valid

For post-migration validation (when policy dir is empty and all agents are charter-only):

```bash
hex doctor charter-triggers --mode post-migration
```

Expected output: `14/14 PASS` when all 14 agent charters are valid and no per-agent policy files remain.

---

## Adding a New Trigger — Migration Note

If you are an existing agent author and want to add a new trigger to your agent:

**Before Phase 1**: You had to edit two files — `charter.yaml` and `~/.hex-events/policies/<id>-agent.yaml`.

**After Phase 1**: Edit only `charter.yaml`. Add the trigger to `wake.triggers:`. The daemon reads it directly. No policy file change needed. No coordination required.

```yaml
# charter.yaml — this is the only file you touch
wake:
  triggers:
    - event: "timer.tick.daily"
    - event: "attention.needed"
    - event: "my.new.trigger"    # just add it here
```

The daemon picks up charter changes within its mtime-polling window (≤10 seconds, no restart required).

---

## Daemon Behavior Reference

| Situation | Daemon Response |
|-----------|----------------|
| Charter has no `wake:` block | Agent treated as non-wake agent; not loaded |
| `wake.triggers` missing or empty | Validation error; agent excluded; health.json entry written |
| `wake.rate_limit` absent (not null) | Validation error; agent excluded; health.json entry written |
| Trigger event name not in allowlist | Validation error; agent excluded; health.json entry written |
| `wake.enabled: false` | Agent not loaded; no triggers registered |
| Charter edited mid-wake | In-flight wake completes with snapshot at dispatch time; new config takes effect next poll |
| Rate limit exceeded | Wake skipped silently (not an error) |
| Condition evaluation timeout (>5s) | ERROR logged; wake skipped; `health.agents.<id>.degraded = true` |

## Health JSON Location

```
~/.hex-events/health.json
```

Structure:
```json
{
  "agents": {
    "my-agent": {
      "status": "invalid_charter",
      "error": "wake.triggers is required and must be non-empty"
    }
  }
}
```

Check this file when an agent appears not to be waking and `hex doctor charter-triggers` is clean.
