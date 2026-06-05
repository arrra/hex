# hex on iii — the abstraction layer (primer)

**What this is:** how *hex* uses iii — our command surfaces, our conventions, and the
one canonical producer→consumer flow. **What this is NOT:** a generic iii reference.
For iii itself (functions, triggers, state, queues, SDKs) use the installed `iii-*`
skills — `iii-getting-started`, `iii-functions-and-triggers`, `iii-trigger-schemas`,
`iii-state-reactions`, `iii-queue-processing`, `iii-state-management`. This doc only
covers the hex layer on top.

> Status (2026-06-04): the `hex worker` / `hex triggers` surfaces + the `ops` seam are
> being built (spec `Skt0r3dbg`). The **concepts and conventions** below are stable;
> verify exact CLI flags against `hex worker --help` / `hex triggers --help` once shipped.

---

## The mental model (the part worth memorizing)

iii is three nouns and a set of channels.

| Noun | One line | hex example |
|---|---|---|
| **Function** | a unit of work — "do this" | `hex::landings::reconcile` |
| **Trigger** | a *subscription*: "fire function F when X happens." Registered **once**, when the consumer sets up. Has a **type**. | a `state` trigger watching scope `boi` |
| **Worker** | a process that hosts functions + triggers | `hex worker run <config>` |

A **trigger's type is the channel it listens on:**

| Type | Fires when… | Semantics |
|---|---|---|
| `cron` | the clock hits a schedule | time-driven |
| `state` | a KV key changes | **current value** (last-write-wins) |
| `queue` | a message lands on a named queue | discrete, durable, FIFO |
| `pubsub` | a topic is published | broadcast, fire-and-forget |
| `stream`/`http`/`log` | other sources | — |

**Producing an event = writing to the channel the trigger listens on** (set state /
enqueue / publish). The producer doesn't call the consumer; it writes a channel, and
whatever trigger is listening there fires. That indirection **is** the decoupling —
the whole point of reactive ops ("when X, do Y" without X knowing about Y).

**Static vs dynamic — where attribution lives.** A trigger can carry `metadata`, but
it's **static** (set at registration, same every firing — describes the *subscription*).
Per-event data (who produced it, the payload) is **dynamic** and rides in the **channel
payload**, never on the trigger. So "who emitted this" goes in the event value, not the
trigger.

---

## The hex layer

We do **not** scatter `iii_sdk::` calls across hex. One seam owns iii.

- **`system/harness/src/ops.rs`** — the *only* place (besides the worker host) that calls
  `iii_sdk::`. Exposes hex-native `emit(...)`, state read/write, connect. If iii's API
  changes or we swap substrate, only this file changes.
- **`hex worker run | list | status`** — worker lifecycle, hex-native (was `hex iii worker
  run`; the `iii` is gone from the surface). A worker config is declarative YAML:
  `{ worker_name, jobs: [{ id, command, cron | trigger }] }`, hosted by `hex worker run`.
  The hex binary **is** the worker host — no node, no per-worker binary.
- **`hex triggers emit <event> [--data <json>] [--producer <name>]`** — the producer.
  One `iii.trigger(...)` call under the hood; writes the event onto a channel so reactive
  workers fire. Shell/hook callers use the CLI; Rust callers use `ops::emit(...)` directly
  — same code path, the CLI is just `clap` + the lib.

### Trigger config in a worker YAML

```yaml
worker_name: hex-landings
jobs:
  - id: hex::memory::index
    command: [hex, memory, index]
    cron: "0 */15 * * * * *"          # bare cron = a cron trigger
  - id: hex::landings::reconcile
    command: [hex, landings, reconcile]
    trigger:
      state: { scope: "boi" }          # state | queue (cron also valid here)
```

The fired command receives the trigger event as the **`III_EVENT`** env var (JSON).

---

## Our conventions

- **Channel for events: `state`** (for now). We enrich the value with a producer envelope
  (iii does *not* surface the writer to the consumer — `StateCallRequest` is only
  `{event_type, scope, key, old_value, new_value}`):

  ```jsonc
  // state value at scope=<ns>, key=<event>  →  consumer reads III_EVENT.new_value
  { "event": "boi.spec.complete", "producer": "boi-completion-hook",
    "ts": "2026-06-05T00:31:00Z", "data": { "spec_id": "S5bke4kkf" } }
  ```

- **Decouple on the event *name*, not the function.** Producers emit a fact
  (`boi.spec.complete`); consumers bind a trigger to that scope/key. Neither imports the
  other. Adding a new reactive consumer never edits the producer.

- **Known caveat — state is last-write-wins.** Two emits to the same `scope/key` between
  trigger fires: the first is clobbered. The envelope gives attribution, not delivery
  guarantees. When loss matters for discrete events, **shard the key** (e.g.
  `key = "boi.spec.complete/<spec_id>"`) so distinct events don't collide, or move that
  event to a `queue` trigger (durable, FIFO). Decide per-event; don't blanket-upgrade.

---

## The canonical flow

```
PRODUCER                          CHANNEL              CONSUMER
hex triggers emit  ──writes──►   iii STATE    ──►   TRIGGER (type=state)  ──fires──►  FUNCTION
  boi.spec.complete                key=event                                          hex::landings::reconcile
  {event,producer,ts,data}                                                            reads III_EVENT.new_value
```

- Producer: a hook/wrapper (or `ops::emit` from Rust) — does **not** know about landings.
- Channel: iii state, keyed by the event name.
- Consumer: the landings worker, whose `state` trigger was registered once at startup.

---

## Guardrails (carry from the iii decisions)

- iii is the **additive default** for new mechanisms — not a migration of BOI/launchd/
  memory/harness (`me/decisions/iii-additive-default-substrate-*`).
- **Engine-down must fail loud (S6)**, never silently no-op. iii is a SPOF for anything on it.
- Each iii mechanism is **independently disablable**.
- See also: `me/decisions/build-operations-on-iii-2026-06-04.md`,
  `me/decisions/hex-abstraction-over-iii-2026-06-04.md`.
