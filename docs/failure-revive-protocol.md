# BOI Failure-Revive Protocol

> Implemented: 2026-04-29 (spec SC5D5)

When a BOI spec fails, the system now automatically detects the failure, resolves the spec's owner, builds a structured brief, routes it to the owning agent, and surfaces critical failures to `#from-hex`. Owners have three choices: revive, redirect, or abandon.

---

## Lifecycle

```
boi.spec.failed event emitted
        │
        ▼
[boi-failure-route-to-owner policy]
        │
        ├─► spec-owner-resolver.py  ─────► owner agent ID
        │       Resolution order:
        │       1. Spec YAML `agent:` field
        │       2. Spec `initiative:` → initiatives/*.yaml `owner:`
        │       3. Path heuristic (projects/<agent>/**)
        │       4. Title keyword match
        │       5. Default: hex-autonomy + weak-attribution warning
        │
        ├─► build-failure-brief.py  ─────► structured markdown brief
        │
        ├─► hex agent message <owner>  ──► owner's inbox
        │
        └─► hex agent wake <owner>  ─────► owner wakes on inbox.message
                │
                ▼
        [Owner agent reads brief]
                │
                ├─► REVIVE   → revive-spec.sh <id> [--adjustments ...]
                │             (increments revive_count, dispatches new spec)
                ├─► REDIRECT → cancel + dispatch brand-new spec
                └─► ABANDON  → close with one-line reason

boi.spec.failed (if severity=block OR critical initiative)
        │
        └─► [boi-failure-slack-ping policy]
                └─► post to #from-hex: "Spec X failed: <reason>. Routed to <owner>."

timer.tick.daily
        └─► [boi-failure-daily-digest policy]
                └─► 24h aggregate: counts by failure kind, stale unrevived list

boi.spec.failed (3rd+ same kind/title in 24h)
        └─► [boi-failure-three-strike policy]
                └─► emit boi.failure.pattern.detected
                        └─► [boi-failure-pattern-handler policy]
                                └─► route to boi-optimizer
                                    post to #from-hex (severity=warn)
```

---

## Agent Responsibilities

| Agent | Role |
|-------|------|
| **hex-autonomy** | Default owner for un-attributed specs, Context Routing failures |
| **brand** | Failures tagged brand initiative or title mentions brand/lab |
| **career** | Failures touching career/job/profile initiatives |
| **hex-ops** | Infrastructure/pipeline failures |
| **fleet-coordinator** | Multi-agent coordination failures |
| **releaser** | Release/deploy failures |
| **boi-optimizer** | BOI meta-issues + three-strike pattern analysis |

All these agents have a `failure-triage` charter responsibility: on inbox message "Failed spec: ...", read brief, decide, act. Budget: max 3 revive attempts per failure axis.

---

## Budget Rules

### Per-failure revive budget (3-strike)

`revive-spec.sh` tracks `revive_count` on each spec. Once `revive_count >= 3`, the script refuses and escalates via `#from-hex` instead of dispatching another worker.

### Pattern detection (24h window)

`detect-failure-pattern.py` queries the BOI SQLite database for failures in the last 24 hours. If:
- Same `failure_reason.kind` appears **3+ times**, OR
- Same spec title appears **3+ times**

…the policy emits `boi.failure.pattern.detected` → boi-optimizer handles it.

### Daily digest

Every `timer.tick.daily` fires, the digest policy posts a summary of the past 24h to `#from-hex`: failure counts by kind, which owners they were routed to, and any specs with no revive attempt.

---

## Files

| Path | Purpose |
|------|---------|
| `.hex/scripts/spec-owner-resolver.py` | Resolve spec → owner agent ID |
| `.hex/scripts/build-failure-brief.py` | Build structured failure brief from event payload |
| `.hex/scripts/revive-spec.sh` | Owner-callable script to revive a failed spec |
| `.hex/scripts/detect-failure-pattern.py` | Query DB for 3+ repeated failures in 24h |
| `.hex/audit/actions.jsonl` | Append-only log of revive/redirect/abandon actions |
| `~/.hex-events/policies/boi-failure-route-to-owner.yaml` | Route on `boi.spec.failed` |
| `~/.hex-events/policies/boi-failure-slack-ping.yaml` | Ping `#from-hex` on block-severity failures |
| `~/.hex-events/policies/boi-failure-daily-digest.yaml` | Daily 24h aggregate digest |
| `~/.hex-events/policies/boi-failure-three-strike.yaml` | Detect repeated failure patterns |
| `~/.hex-events/policies/boi-failure-pattern-handler.yaml` | Handle `boi.failure.pattern.detected` |

---

## Failure Brief Format

The brief built by `build-failure-brief.py` looks like:

```
## Failure Brief — <spec_id>: <spec_title>

**Reason:** <FailureReason short summary>
**Detail:** <FailureReason detail>
**When:** <failed_at>, after <N> iterations
**Cost:** $<total_cost_usd>
**Owner:** <agent> (resolution: <explicit|initiative|path|keyword|default>)

## Spec
```yaml
<full spec YAML>
```

## Last 30 lines of worker log
```
<tail ~/.boi/logs/<spec_id>-*.log>
```

## Suggested actions (owner picks one)
1. **Revive:** ... Hint: <suggested_fix>
2. **Redirect:** abandon this approach, write a different spec.
3. **Abandon:** close as won't-fix. Requires a one-line reason.
```

Suggested fix hints by failure kind:
- `ProviderRateLimit` → retry with longer backoff
- `VerifyFailed` → loosen or fix the verify command
- `Timeout` → split tasks or bump timeout
- `ToolError` → remove tool dependency
- `WorkerCrash` → investigate worker logs; likely env

---

## Debugging a Failure That Wasn't Routed

**Step 1 — Check the event was emitted**

```bash
sqlite3 ~/.boi/boi-rust.db "SELECT * FROM events WHERE event_type='boi.spec.failed' ORDER BY created_at DESC LIMIT 5;"
```

**Step 2 — Check the hex-events policy fired**

```bash
python3 ~/.hex-events/hex_events_cli.py list-events --filter boi.spec.failed --last 10
```

**Step 3 — Validate the routing policy**

```bash
python3 ~/.hex-events/hex_events_cli.py validate ~/.hex-events/policies/boi-failure-route-to-owner.yaml
```

**Step 4 — Test owner resolution manually**

```bash
python3 ~/hex/.hex/scripts/spec-owner-resolver.py <spec_id>
```

**Step 5 — Build and inspect the brief manually**

```bash
python3 ~/hex/.hex/scripts/build-failure-brief.py <spec_id>
```

**Step 6 — Check hex agent message delivery**

```bash
hex agent inbox hex-autonomy   # or whichever owner
```

**Step 7 — Check the audit log**

```bash
tail -20 ~/hex/.hex/audit/actions.jsonl
```

**Common causes of missing routing:**
- `spec_id` field absent from the `boi.spec.failed` event payload — check BOI version
- Spec not found in DB (cancelled before failure recorded)
- `hex` binary not in PATH when policy shell command runs — check `HEX_DIR` env var
- Owner resolved to a non-existent agent directory — run `--test` on spec-owner-resolver to validate
