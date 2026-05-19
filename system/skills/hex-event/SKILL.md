---
name: hex-event
description: Generate and validate hex-events policies for reactive/event-driven automation. Use when wiring "when X happens do Y", event chains, oneshot notifications, or any event-driven behavior.
---

# hex-event — Policy Generator

Use this skill whenever you need reactive or event-driven behavior. Do NOT write shell polling loops, manual `rm -f` cleanup, or ad-hoc cron hacks — use hex-events policies instead.

## When to Use

Invoke when you hear patterns like:
- "when X finishes / completes / succeeds / fails"
- "after Y, do Z"
- "notify me when…"
- "chain this to that"
- "do X once then clean up"
- "schedule a one-off action"
- "react to event…"

## Policy Schema

```yaml
# ~/.hex-events/policies/<name>.yaml

name: string                    # required, unique
description: string             # optional
lifecycle: persistent           # persistent | oneshot-delete | oneshot-disable
                                # oneshot-delete: fires once, deletes file
                                # oneshot-disable: fires once, sets enabled: false
max_fires: int                  # optional: auto-disable after N total fires
enabled: bool                   # false = skip on load (default: true)

provides:
  events: [list of event names this policy emits]

requires:
  events: [list of events this policy depends on — informational]

rate_limit:
  max_fires: int
  window: "30m"                 # duration string: \d+[smhd]

standing_orders: [list]
reflection_ids: [list]

rules:                          # required, non-empty list
  - name: string                # required

    trigger:
      event: "event.name"       # glob patterns supported: "boi.*", "task.*"

    ttl: "24h"                  # optional: skip rule if file is older than this
                                # format: \d+[smhd] (e.g. 60s, 30m, 24h, 7d)
                                # measured from policy file mtime

    conditions:                 # optional list (AND logic)
      - field: "payload.status" # dot-notation into event payload
        op: eq                  # eq | neq | gt | gte | lt | lte | contains | glob | regex
        value: "done"
      - field: "count(event.name, 5m)"  # count() counts matching events in window
        op: gte
        value: 3
      - type: shell             # shell condition: passes if exit code 0
        command: "test -f /tmp/flag"

    actions:                    # required, non-empty list
      - type: shell
        command: "echo '{{ event.status }}'"  # Jinja2 template
        timeout: 60             # seconds (default: 60)
        retries: 3              # (default: 3)
        on_success:             # nested actions list
          - type: emit
            event: "task.done"
        on_failure:
          - type: notify
            message: "Action failed: {{ action.stdout }}"

      - type: emit
        event: "task.started"
        payload:                # dict, supports Jinja2 values
          source: "{{ workflow.name }}"
          job: "compile"
        delay: "5s"             # optional: defer emission
        cancel_group: "grp"     # optional: cancel pending emits with same group
        source: "my-policy"     # optional: sets event source field

      - type: notify
        message: "Build done: {{ event.payload.result }}"
        # delegates to ~/.hex/scripts/hex-notify.sh

      - type: update-file
        target: "/path/to/file.yaml"
        pattern: 'status: \w+'  # regex
        replace: "status: done"  # atomic file update

      - type: dagu
        workflow: "my-dag.yaml"  # triggers a Dagu workflow
```

### Jinja2 Template Context

| Variable | Value |
|---|---|
| `{{ event.field }}` | Any field in the triggering event payload |
| `{{ workflow.name }}` | Current workflow name |
| `{{ workflow.config.X }}` | Workflow config key X |
| `{{ action.stdout }}` | stdout from previous shell action (in on_success/on_failure) |

## Common Patterns

### Oneshot Notification

```yaml
name: notify-on-build-done
lifecycle: oneshot-delete
ttl: 2h

rules:
  - name: notify
    trigger:
      event: "build.completed"
    actions:
      - type: notify
        message: "Build finished: {{ event.status }}"
```

### Event Chain (A → B)

```yaml
name: chain-compile-to-test
lifecycle: oneshot-delete
ttl: 30m

rules:
  - name: start-tests-after-compile
    trigger:
      event: "compile.done"
    conditions:
      - field: "payload.exit_code"
        op: eq
        value: 0
    actions:
      - type: shell
        command: "bash $HEX_DIR/scripts/run-tests.sh"
        on_success:
          - type: emit
            event: "tests.started"
```

### Conditional Gate

```yaml
name: gate-deploy-on-tests
lifecycle: persistent

rules:
  - name: deploy-only-if-tests-pass
    trigger:
      event: "tests.completed"
    conditions:
      - field: "payload.passed"
        op: eq
        value: true
    actions:
      - type: shell
        command: "bash $HEX_DIR/scripts/deploy.sh"
      - type: notify
        message: "Deployed successfully"
```

### Rate-Limited Monitor

```yaml
name: heartbeat-monitor
lifecycle: persistent
rate_limit:
  max_fires: 1
  window: "10m"

rules:
  - name: check-health
    trigger:
      event: "heartbeat"
    actions:
      - type: shell
        command: "python3 $HEX_DIR/scripts/health-check.py"
        on_failure:
          - type: notify
            message: "Health check failed!"
```

## Generation Flow

1. **Identify the trigger event** — what event name fires this rule? Check `~/.hex-events/policies/` for naming conventions.
2. **Choose lifecycle** — oneshot-delete for fire-and-forget, oneshot-disable to keep the file, persistent for recurring.
3. **Set TTL if time-limited** — always set TTL on oneshots to prevent stale policies.
4. **Define conditions** — what must be true on the event payload?
5. **Write actions** — shell, emit, notify, update-file, or dagu.
6. **Wire provides/requires** — document what events this policy emits and depends on.
7. **Write to** `~/.hex-events/policies/<name>.yaml` (or a subdirectory).
8. **Validate** — run the CLI (see below). Fix all errors before considering done.

## Validation

Always validate before marking done:

```bash
python3 ~/.hex-events/hex_events_cli.py validate ~/.hex-events/policies/<name>.yaml
```

Exit 0 = valid. Any output = error to fix.

## Anti-Patterns

| Don't | Do instead |
|---|---|
| `rm -f policy.yaml` after firing | Use `lifecycle: oneshot-delete` |
| Polling loop in shell action | Use hex-events trigger + event chain |
| Hardcoded `~/hex/...` paths | Use `$HEX_DIR` variable |
| Skip TTL on oneshots | Always set TTL (2h–24h typical) |
| Nested shell scripts that emit events manually | Use `type: emit` action |
| Write policy without validating | Always run the validate CLI first |

## Reference

Full schema source of truth: `~/.hex-events/docs/skill-reference.md`
