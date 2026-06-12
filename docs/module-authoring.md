# Authoring hex modules

A **module** is a typed Rust worker that the harness auto-discovers from a
`*.worker.rs` file. You write the worker, drop the file in a module root, and
rebuild — the build script finds it, registers it, and the harness runs its
triggers. No central registry to edit, no YAML.

This guide is for a Rust developer new to hex. It is code-forward: every symbol
below exists in `system/harness/`.

---

## 1. What a module is

- **One file → one module.** A module is a single `*.worker.rs` file that
  exposes exactly one entry point:

  ```rust
  pub fn worker() -> hex::worker::Worker
  ```

- **Single responsibility.** A worker bundles a `name` plus one or more
  `(trigger, handler)` pairs. Keep each file focused on one job (a backup, a
  memory-maintenance schedule, one integration). Multiple triggers in one
  worker are fine when they serve the same responsibility.

- **Author through the `hex` lib, never iii directly.** You build workers with
  the `hex::worker` API (`Worker`, `Ctx`, `Event`). The harness maps your
  declarative triggers onto the underlying iii engine at startup
  (`TriggerSpec::to_register_input`). You never touch `iii_sdk` from a module.

---

## 2. The convention

### File naming → module ident

`build.rs` recursively globs every file ending in `.worker.rs` under each
discovery root. For each match it derives a Rust module identifier:

1. Strip the `.worker.rs` suffix from the path **relative to the root**.
2. Replace each `/`, `.`, and `-` with `_`.

Examples:

| File (relative to root)      | Module ident      |
| ---------------------------- | ----------------- |
| `backup.worker.rs`           | `backup`          |
| `memory_maintenance.worker.rs` | `memory_maintenance` |
| `trading/kalshi.worker.rs`   | `trading_kalshi`  |

The ident must be a valid Rust identifier: it must start with an ASCII letter,
contain only `[A-Za-z0-9_]`, and be non-empty. A file that does not yield a
valid ident **panics the build** (loud by design — S6). If two files map to the
**same** ident, the build also panics:

```
hex module: ident collision on 'trading_kalshi' (from '...') — rename the file
```

### The two discovery roots

`build.rs` globs from two roots:

1. **Core (always):** `system/harness/src/modules/` — in-repo, shipped to
   everyone who installs hex-foundation.
2. **Personal overlay (only under `--features personal`):**
   `$HEX_DIR/.hex/modules/` (falls back to `$HOME/hex/.hex/modules/` if
   `HEX_DIR` is unset) — your machine only, never in the foundation repo.

Both roots are globbed **recursively**, so subdirectories like
`trading/kalshi.worker.rs` are found. An absent root contributes nothing (the
foundation builds clean with no personal overlay present).

### What the build generates

Codegen writes `$OUT_DIR/hex_modules.rs`, containing:

- a `#[path = "..."] pub mod <ident>;` line per discovered file,
- `pub fn module_registry() -> Vec<crate::worker::Worker>` — calls every
  module's `worker()`,
- `pub fn module_paths() -> Vec<(String, &'static str)>` — maps each worker's
  `name` to its source file path (drives `hex module list`'s source column).

`workers/mod.rs::registry()` is built from `module_registry()`. It also asserts
worker **names** are unique at runtime — two files with distinct idents but the
same `Worker::new(name)` will abort.

### rebuild = install

**There is no runtime `add` command.** You install a module by dropping its
`*.worker.rs` file in a root and **rebuilding the harness**. The build script
re-runs whenever a `*.worker.rs` file or a root directory changes (per-file
`cargo:rerun-if-changed` triggers), regenerates `hex_modules.rs`, and the new
binary ships the module.

This is deliberate: modules are typed Rust compiled into the harness, not
plugins loaded at runtime. You get compile-time checking of every trigger and
handler, and there is no dynamic-loading attack surface. The price is a rebuild
per change — cheap, and it keeps the registry honest.

---

## 3. Anatomy of a worker file

A minimal, copy-pasteable worker. Save as
`system/harness/src/modules/hello.worker.rs`:

```rust
//! `hex-hello` — a tiny example worker that emits an event on a schedule.

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression: top of every hour (7-field: sec min hour dom mon dow year).
const CRON_HOURLY: &str = "0 0 * * * * *";

/// The handler. Signature is fixed:
///   Fn(Event, Ctx) -> Result<()> + Send + Sync + 'static
fn say_hello(_e: Event, ctx: Ctx) -> Result<()> {
    ctx.emit("hello.tick", serde_json::json!({ "msg": "hi from hex" }))?;
    Ok(())
}

/// The required entry point. Build a `Worker`, chain `.on_*` triggers.
pub fn worker() -> Worker {
    Worker::new("hex-hello").on_cron(CRON_HOURLY, say_hello)
}
```

Notes:
- The import line `use hex::worker::{ctx::Ctx, event::Event, Result, Worker};`
  is the standard set; it matches the shipped workers.
- `Result<T>` here is `hex::worker::Result<T>` = `std::result::Result<T,
  anyhow::Error>`. Use `?` freely.
- A handler can be a plain `fn` (as above) or any closure satisfying
  `Fn(Event, Ctx) -> Result<()> + Send + Sync + 'static`.

### A real shipped example

`system/harness/src/modules/backup.worker.rs` — the entire file:

```rust
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression for the nightly backup job — 04:00 daily.
pub const CRON_DAILY: &str = "0 0 4 * * * *";

/// Argv for the backup job.
pub const ARGV_BACKUP: &[&str] = &["hex", "backup"];

fn run_backup(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_BACKUP.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-backup` worker.
pub fn worker() -> Worker {
    Worker::new("hex-backup").on_cron(CRON_DAILY, run_backup)
}
```

For a multi-trigger worker, see
`system/harness/src/modules/memory_maintenance.worker.rs`, which chains four
`.on_cron(...)` calls (index / quick-consolidate / parse-transcripts every 15
min, full consolidation at 03:00), each shelling out via `ctx.run(...)`.

---

## 4. Triggers

Each `.on_*` builder pushes one `(TriggerSpec, Handler)` pair and returns
`Self`, so you chain them. All four take a handler with the same
`Fn(Event, Ctx) -> Result<()> + Send + Sync + 'static` signature.

| Builder | Fires when | Use for |
| --- | --- | --- |
| `.on_cron(expr, f)` | the cron schedule matches | periodic jobs (backups, maintenance, reports) |
| `.on_event(event, f)` | a named hex event is emitted | reacting to domain events (`boi.spec.complete`, …) |
| `.on_state(scope, key, f)` | `scope/key` state changes | reacting to a specific KV entry changing |
| `.on_queue(queue, f)` | a job lands on a named queue | durable background work |

```rust
pub fn worker() -> Worker {
    Worker::new("hex-example")
        .on_cron("0 0 3 * * * *", nightly)          // 03:00 daily
        .on_event("boi.spec.complete", on_complete)  // named event
        .on_state("landings", "today", on_landing)   // state change
        .on_queue("ingest", drain_ingest)            // queue job
}
```

**Cron format.** Expressions are 6- or 7-field: `sec min hour dom mon dow
[year]`. `0 */15 * * * * *` = every 15 minutes; `0 0 4 * * * *` = 04:00 daily.

**`on_event` under the hood.** A named event maps to a state trigger with
`scope="events", key=<event>` — the same convention `ops::emit` uses when it
writes an event envelope. You don't need to know this to use it, but it's why
`.on_event("x", ...)` and `.on_state("events", "x", ...)` are equivalent.

---

## 5. The `Ctx` API

`Ctx` is the handle passed as the second handler argument. Three capabilities:

### `emit` — publish a hex event

```rust
ctx.emit("hello.tick", serde_json::json!({ "spec_id": "S123" }))?;
```

Writes a `{event, producer, ts, data}` envelope to hex state, where other
workers' `.on_event(...)` triggers pick it up. (During the harness shutdown
drain window the runtime transparently diverts emits to a durable outbox and
replays them on next init — at-most-once; nothing for you to handle.)

### `run` — shell out

```rust
let out = ctx.run(&["hex".to_string(), "backup".to_string()])?;
```

`run(argv: &[String])` spawns `argv[0]` with the rest as args and returns
`std::process::Output`. **A non-zero exit is an `Err`** carrying the exit code
and a stderr (or stdout) tail — so failing crons land in telemetry with a real
reason instead of a silent `ok`. Empty argv is also an error.

### `state()` — read/write KV state

`ctx.state()` returns a `StateHandle`. Scope and key are your choice; values are
arbitrary `serde_json::Value`.

```rust
let st = ctx.state();
st.set("counters", "ticks", serde_json::json!(1))?;

match st.get("counters", "ticks")? {
    Some(v) => { /* present */ }
    None    => { /* absent — get returns Ok(None), not an error */ }
}

st.delete("counters", "ticks")?;
```

`StateHandle` methods:
- `get(scope, key) -> Result<Option<Value>>` — `Ok(None)` when the key is absent.
- `set(scope, key, value: Value) -> Result<()>`
- `delete(scope, key) -> Result<()>`

---

## 6. Reading event data

The first handler argument is `Event`, a typed view over the
`{event, producer, ts, data}` envelope:

```rust
fn on_complete(e: Event, _ctx: Ctx) -> Result<()> {
    let _name     = e.event();      // -> &str, e.g. "boi.spec.complete"
    let _producer = e.producer();   // -> &str, who emitted it
    let _ts       = e.ts();         // -> &str, ISO timestamp
    let _raw      = e.envelope();   // -> &serde_json::Value, full envelope

    let data = e.data();            // -> Data<'_>
    let spec_id = data.str("spec_id")?;   // typed string, Err if missing / not a string
    let _whole  = data.raw();             // -> &serde_json::Value, the data object
    println!("spec {spec_id} done");
    Ok(())
}
```

`Data` accessors:
- `str(k) -> Result<&str>` — errors if the key is missing or not a string.
- `raw() -> &Value` — the underlying `data` object for anything more complex.

Cron and most periodic handlers ignore the event (`fn run(_e: Event, ...)`).

---

## 7. Installing / verifying a module

```bash
# 1. Drop the file in a module root.
#    core:     system/harness/src/modules/hello.worker.rs
#    personal: $HEX_DIR/.hex/modules/hello.worker.rs   (needs --features personal)

# 2. Rebuild the harness (this is the "install").
cargo build --release                 # core only
cargo build --release --features personal   # to also pick up the overlay

# 3. Confirm it registered. `list` shows name, [source], and triggers.
hex module list
#   hex-hello                    [core]  cron(0 0 * * * * *)
#   hex-backup                   [core]  cron(0 0 4 * * * *)

# 4. Inspect one worker.
hex module status hex-hello
#   name:     hex-hello
#   source:   core
#   file:     .../system/harness/src/modules/hello.worker.rs
#   triggers: cron(0 0 * * * * *)
```

The `[source]` tag is derived from the file path: `core` for
`/src/modules/`, `personal` for `/.hex/modules/`.

Inspect any state your worker reads or writes from the shell:

```bash
hex state get counters ticks         # prints JSON; empty + exit 1 if absent
hex state set counters ticks 1       # value is a JSON literal
hex state delete counters ticks
```

---

## 8. Core vs personal modules

hex follows a base/overlay model. Modules pick their layer by **where the file
lives**:

| | Core | Personal |
| --- | --- | --- |
| Location | `system/harness/src/modules/` (in-repo) | `$HEX_DIR/.hex/modules/` (overlay) |
| Shipped to | everyone who installs hex-foundation | your machine only |
| Build flag | always globbed | only under `--features personal` |
| `hex module status` source | `core` | `personal` |

Put a module in **core** when it belongs in the shipped system (the canonical
backup and memory-maintenance workers live here). Put it in **personal** when
it's specific to your machine, secrets, or accounts — it stays out of the
foundation repo entirely and is compiled in only when you build
`--features personal`. The two roots share the same convention, idents, and
collision rules; they differ only in visibility and the build flag.
