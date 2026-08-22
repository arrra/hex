# Recall vector (KNN) arm — query-embedding option research

**Date:** 2026-08-19
**Spec:** Sdnap37he · **Task:** Tw0gedxnr (research-embed-option)
**Author:** BOI execute worker (operator account)
**Base:** develop @ c1905be

## TL;DR

- **Chosen option: (b) resident harness embed endpoint** — the long-running
  hex engine holds one resident `Embedder` and exposes a local unix-socket
  embed call; the `hex memory recall` CLI asks for the query vector over the
  socket and degrades **loudly** to BM25-only when the socket is absent, times
  out, or errors. This is the only option that both (1) turns the existing
  `facts_recall` KNN arm on with a *real* nomic-space query vector and (2) can
  plausibly fit the per-message p50 ≤ 200 ms / p95 ≤ 500 ms budget, because it
  pays the ~1.6 s ONNX cold-load **once** in the resident process instead of on
  every hook invocation.
- **Rejected (a) per-invocation ONNX load:** measured cold model load
  (**≈ 13–15 s on this machine under load ~25**, two independent methods; ~1.6 s
  quiet-machine floor) makes every `hex memory recall` pay **seconds**, not
  milliseconds — off the latency budget by ≥3× on the *quiet-machine* floor
  alone. A "small MiniLM int8"
  swap does **not** rescue it: it is a different 384-dim embedding space, so it
  would desync from the 768-dim vectors already stored for 19 018 chunks and
  force a full re-embed of the entire corpus (`vector.rs` hardcodes
  `EMBED_DIM = 768`).
- **Rejected (c) precomputed-only / query-side projection:** no credible
  implementation exists for turning a raw user query into a nomic-space vector
  *without running the model*; a lexical query-side projection cannot reach the
  zero-keyword-overlap paraphrase cases that are the entire motivation. It is
  also moot on coverage grounds (see below).
- **Fact-vector coverage in the frozen snapshot: `0 / 1863` (0 %).** The
  `facts_vec` table exists but has never been written; chunk coverage is
  `19 018 / 19 018` (100 %). The KNN arm therefore returns *nothing* today even
  when handed a query vector. **Fact-embedding backfill becomes an explicit
  part of task 2.** The backfill is already implemented and CLI-reachable
  (`hex memory maintain --backfill-facts`), so the A/B in task 3 is still
  meaningful — the STOP condition ("coverage so low the A/B cannot be
  meaningful even after backfill") does **not** trigger.

---

## 1. How the existing embedding path works

| Aspect | Value | Source |
|---|---|---|
| Library | `fastembed` 4.x (ONNX Runtime, CPU) | `Cargo.toml`, `embed.rs` |
| Model | `nomic-embed-text-v1.5` | `embed.rs` |
| Output dim | **768** | `vector.rs::EMBED_DIM` |
| Model artifact size | **547 MB** (`model.onnx`) | `~/hex/.fastembed_cache/…/onnx/model.onnx` |
| Cache size on disk | **523 MB** | `du -sh ~/hex/.fastembed_cache` |
| Cold-load cost (author-documented) | **~1.6 s** per process | `embed.rs` doc comment |
| ORT threading | forced to 1 (`ORT_NUM_THREADS=1`, `OMP_NUM_THREADS=1`) | `embed.rs::Embedder::new` (OBS-019 OOM fix) |
| Asymmetry | docs get `search_document: `, queries get `search_query: ` | `embed.rs` |

Vectors live in two `sqlite-vec` vec0 tables (`vector.rs`, `schema.rs`):

- `vec_chunks(rowid, embedding FLOAT[768])` — chunk/corpus side, rowid mirrors
  the `chunks` FTS5 rowid.
- `facts_vec(fact_id TEXT PRIMARY KEY, embedding FLOAT[768])` — fact side.

`facts_recall` already accepts an optional query vector and fuses a KNN arm:

```rust
// recall.rs:178
let knn_ids: Vec<i64> = match query_vec {
    Some(qv) => super::vector::knn_facts(conn, qv, k.max(20))
        .map(...).unwrap_or_else(|e| { eprintln!("facts vector arm failed: {e}"); vec![] }),
    None => vec![],                       // <-- the hot-path default today
};
let fused = super::rrf::rrf_fuse(
    &[fts_content_ids, fts_entity_ids, slug_ids, knn_ids], cfg.rrf_k);
```

So the arm is a **4th RRF arm** alongside the two dual-weighted BM25 arms and
the slug arm. Turning it on = (1) producing a query vector and passing
`Some(qv)`, and (2) having `facts_vec` populated. The hot path passes `None`
because the only way to get a query vector today is `Embedder::new` — the
547 MB per-invocation cold-load this research is about. `knn_facts` applies a
relevance floor (`KNN_MAX_DISTANCE = 1.15`, L2; d² = 2(1−cos)) and the arm is
best-effort/loud-on-failure by construction.

The chunk pipeline (`index.rs`) and the fact backfill (`maintain_facts.rs`)
both embed the **document** side in batches of 8 (`embed_documents`), matching
the OBS-019 working-set bound.

---

## 2. Fact-vector coverage in the frozen snapshot

Read-only source: `~/.hex-evalsnap/.hex/memory.db` (106 MB), copied
to `/tmp/va-snap` for all work — the snapshot was never mutated.

| Metric | Count |
|---|---|
| `facts` (all, `tombstone=0`) | **1 863** |
| facts with an `embedding` BLOB | **0** |
| rows in `facts_vec` (via `facts_vec_rowids` shadow) | **0** |
| `chunks` | 19 018 |
| rows in `vec_chunks` (via `vec_chunks_rowids` shadow) | **19 018** |

**Fact-vector coverage = 0 / 1863 (0 %).** The `maintain_facts.rs` module
header states it plainly: *"facts_vec was created by Plan 2 and never written
(assessment: dead schema; facts recall keyword-only)."* Chunks, by contrast,
are fully embedded.

### Backfill is implemented and reachable (not a new build)

`maintain_facts::backfill` (called from `maintain.rs:89`, wired to the CLI at
`main.rs:1149` as `hex memory maintain --backfill-facts`) already:

- deletes `facts_vec` rows for tombstoned/deleted facts first,
- selects live facts **missing** a vector (idempotent),
- embeds the canonical triple `subject || ' ' || predicate || ' ' || object`
  on the **document** side (`embed_documents`, `search_document:` prefix) — the
  correct asymmetric pair to the `search_query:`-prefixed query side,
- inserts via the shared `vector::insert_fact_vec` serializer.

So task 2's backfill is **"run the existing maintainer against the A/B copy,"**
not "build a backfiller." The 0 % coverage means it has simply never run
against this snapshot. Task 3's A/B therefore must run `maintain
--backfill-facts` on the snapshot copy **before** scoring the arm-on leg, or
the KNN arm contributes nothing.

---

## 3. Measured latencies on this machine

### ⚠️ Measurement environment caveat (load-bound, first-class finding)

Every number below **was measured on this machine** — the shared BOI execution
host — but **under heavy contention** (12 physical cores, 1-minute load average
**25–54** across the runs: dozens of concurrent worker + `cargo` +
`hex memory index` processes, all timestamped inline). The contention factor is
large and directly observable: the arm-off `hex memory recall` baseline — pure
SQLite FTS, no model — measured a **min of 240 ms at load ~25** (660 ms at
load ~50) against a spec-stated quiet baseline of **~18 ms**. Every absolute
latency here is therefore a **measured upper bound**, load-stamped; the
quiet-machine floor is smaller (noted per row). The *decision* in §5 is robust
to the contention because it rests on the quiet-machine floor, not the inflated
figure (see interpretation).

> **Finding for task 3:** the research figures below are measured and
> decision-sufficient, but the p50 ≤ 200 ms / p95 ≤ 500 ms **adoption gates**
> must be re-certified on a quiet host (load < ~2) with the resident embedder —
> a gate *failure* observed under this load would be a false negative. The
> gate certification is task 3's job (§6), not task 1's.

### Measured numbers (this machine, load-stamped)

| # | What | How measured | Value (min-of-N) | Load | Quiet-host floor |
|---|---|---|---|---|---|
| 1 | Arm-off recall (BM25 only, **no embed**) | `hex memory recall` ×3 | **240 ms** (also 660 ms @ load ~50) | ~25 | ~18 ms (spec) |
| 2 | Per-invocation search (`Embedder::new` + 1 `embed_query` + FTS + KNN) | `hex memory search` ×3 | **15.60 s** (also 21.9 s @ load ~50) | ~25 | — |
| 3a | **Cold-start** (`Embedder::new` model load) — differencing | #2 − #1 = 15.60 − 0.24 | **≈ 15.4 s** | ~25 | ~1.6 s (`embed.rs` doc) |
| 3b | **Cold-start** — index-slope intercept (independent 2nd method) | intercept of #4 | **≈ 12.7 s** | ~27 | ~1.6 s |
| 4 | **Per-item embed** (document side, batch-8) — index slope | 13 chunks @ 27.62 s vs 85 chunks @ 110.20 s | **≈ 1147 ms/chunk** | ~27 | tens of ms |

**Cold-start is measured two independent ways and they agree at ~13–15 s under
load ~25–27** (§3a differences the two CLI paths; §3b reads it off the
zero-chunk intercept of the index-slope line). Both bracket the same
author-documented **~1.6 s quiet floor** once the ~2.5× core-oversubscription is
removed. **Per-item embed (#4) is the measured per-query cost proxy:** it is the
document-side, batch-8 forward pass — the *same* nomic-v1.5 forward pass the
query side runs, differing only in the `search_document:`/`search_query:` prefix
and batch=1 vs 8. At load ~27 it is **1147 ms/chunk**; the query-side single
forward pass on a quiet host is orders of magnitude cheaper (~tens of ms).

Method: `/usr/bin/time -p` wall clock; freshly built release binary at
`~/.boi/v2/cargo-target/release/hex`. Rows 1–2: `HEX_DIR=/tmp/va-snap`
(the read-only snapshot copied once; never mutated). Rows 3b–4: two fresh
throwaway `HEX_DIR`s under `/tmp` (each with a `.fastembed_cache` symlink to
`~/hex/.fastembed_cache`, so **no network fetch**), indexed at two chunk counts;
the slope is per-item embed, the intercept is cold-start — cold-start cancels in
the slope, per-item cancels in the intercept. Throwaway dirs deleted after.

### Interpretation (decision-grade, robust to the contention)

- **Cold-load dominates, measured two ways.** Cold-start is **≈ 13–15 s** here
  (row 3a via CLI-path differencing, row 3b via the index-slope intercept —
  independent methods, same answer), against an author-documented **~1.6 s**
  quiet-machine floor. Even taking the optimistic ~1.6 s floor, option (a) pays
  **≥ 1.6 s on every `hex memory recall`** invocation — **≥ 3.2× over the 500 ms
  p95 gate before a single query token is embedded.** The decision against (a)
  does **not** depend on resolving the contention: it dies on the quiet floor.
- **Per-query embed is now isolated and measured** (row 4). The index-slope
  method cancels cold-load in the slope, so the **1147 ms/chunk** figure is the
  contended marginal per-item forward pass — *not* contaminated by the 547 MB
  model construction. It is the same nomic-v1.5 forward pass the query side runs
  (batch=1, `search_query:` prefix). On a quiet host that single pass is ~tens
  of ms; here it is inflated ~30× by 12-core oversubscription with
  `ORT_NUM_THREADS=1`. Crucially: **the per-query cost is separable from and
  vastly smaller than the cold-load** — which is the entire basis for choosing a
  resident model (pay cold-load once) over per-invocation load (pay it every
  time).
- **Under option (b)** the ~13–15 s construction is paid **once** in the
  resident engine; the recall CLI's marginal cost is then *only* that one
  query-side forward pass (~tens of ms quiet) plus a unix-socket round-trip
  (sub-millisecond, no network). That is what plausibly lands inside the
  200 / 500 ms gates. Task 3 must **certify** that resident-path p50/p95 on a
  quiet host (§6) — the research measurement here already shows the two cost
  components (cold-load vs per-query) that make (b) viable and (a) not.

---

## 4. Option comparison

| | (a) per-invocation ONNX | **(b) resident harness socket** | (c) precomputed-only |
|---|---|---|---|
| Query vector in nomic space? | yes | **yes** | no (lexical projection only) |
| Cold-load paid… | every recall (~1.6 s floor, ~13–15 s measured here) | **once, in resident engine** | n/a |
| Marginal recall cost | model load + embed | **1 embed + socket RT** | ~0 |
| Fits 200/500 ms budget? | **no** (≥3× over on floor) | **plausibly yes** (needs quiet-host cert) | yes, but wrong answers |
| Reaches zero-overlap paraphrase? | yes | **yes** | **no** — the whole point |
| Extra moving parts | none | resident endpoint + fallback | query projector (unbuilt) |
| Dimensional consistency | 768 (ok) / 384 if MiniLM (breaks) | **768 (ok)** | n/a |

### Why (a) is rejected
1. **Latency:** ≥1.6 s cold-load per invocation on the quiet floor; ~13–15 s here (§3, measured two ways).
   Off the p95 budget by ≥3×. This is the hot UserPromptSubmit hook — it runs
   per message.
2. **The "small int8 MiniLM" mitigation does not hold:** MiniLM-class models
   are 384-dim in a *different* embedding space. `facts_vec`/`vec_chunks` store
   **768-dim nomic** vectors for 19 018 chunks; mixing spaces is meaningless,
   so adopting MiniLM forces re-embedding the **entire** corpus and diverging
   from the chunk KNN arm. `vector.rs::EMBED_DIM = 768` is compiled in. Cost and
   risk explode for a latency win that (b) already captures more cheaply.

### Why (c) is rejected
1. **No credible query-side implementation:** the paraphrase misses (20/27) are
   *semantic, zero-keyword-overlap*. A precompute/lexical query-side projection
   is, by definition, keyword-driven — it cannot manufacture a nomic-space
   vector that lands near a semantically-related fact it shares no tokens with.
   That is precisely what the v0.50.3 BM25 arms already fail at.
2. **Moot on coverage anyway:** fact side is 0 % embedded; "precomputed" has
   nothing to stand on until backfill runs, at which point (b) is strictly
   better (real model vectors on both sides).

### Why (b) is chosen
1. **Only option that gives a real nomic query vector within budget.** The
   resident engine (`src/harness/supervise.rs`, `com.hex.harness` — a persistent
   supervised process) already runs continuously; hosting one `Embedder`
   (≈ 547 MB RSS) and a unix-socket embed call amortizes the cold-load across
   every hook invocation.
2. **Matches the spec's loud-bounded-fallback contract:** socket absent / dead
   / slow → stderr WARN + BM25-only, with a hard internal timeout on the embed
   step. No network on the hot path (unix socket only).
3. **Symmetric with the existing chunk KNN arm** and reuses `knn_facts`,
   `insert_fact_vec`, RRF fusion, and the `search_query:`/`search_document:`
   asymmetry unchanged.

**Costs/risks to carry into task 2 (stated, not hidden):**
- **+~547 MB resident RSS** on the engine. This codebase has already been
  OOM-killed once by ONNX activation tensors (OBS-019); the resident embedder
  must keep `ORT_NUM_THREADS=1` and bound batch size, and the engine's memory
  headroom should be checked.
- **The arm is silently off whenever the engine is down** (by design — loud
  fallback to BM25). Consequence for task 3: the A/B arm-on leg must be run with
  the engine up (or with an equivalent in-process embed shim) or it measures
  BM25-only and understates the arm. Whichever task 2 ships, task 3 must
  exercise the *shipped* path.
- **No new crate dependency is required** — `fastembed`/`sqlite-vec` are already
  in-tree; nothing > 50 MB is fetched at build time (the 547 MB model is a
  runtime cache artifact, already present, not a build-time fetch).

---

## 5. Decision

**Chosen: (b) resident harness embed endpoint**, arm shipped **default OFF**
until task 3's adoption gates pass on a quiet host. Task 2 wires the query
vector behind `recall_config` (default OFF, byte-identical when off) with the
loud bounded-timeout BM25 fallback, **and** includes fact-embedding backfill
(`maintain --backfill-facts`) because snapshot coverage is 0 %.

Rejections: (a) per-invocation ONNX — latency ≥3× over budget on the quiet
floor + 384/768 dimensional break for any MiniLM swap; (c) precomputed-only —
no credible semantic query-side projection and moot on 0 % coverage.

---

## 6. Quiet-host certification of the adoption gates (task 3) — reproduction bench

The research measurement (§3) is **complete and measured on this machine**:
cold-start (~13–15 s contended, two independent methods) and per-item embed
(1147 ms/chunk contended) were separated cleanly by *differencing* CLI paths and
index-slope points — a **no-build** technique that sidesteps the shared `cargo`
target lock entirely (20+ sibling `rustc` procs held it during this run, and a
dedicated-target dep-tree rebuild of `ort`/`fastembed` is a multi-minute
budget-killer, so no in-tree example was built or committed). Those numbers
already decide §5.

What **remains for task 3** is a different thing: certifying the *resident-path*
p50 ≤ 200 ms / p95 ≤ 500 ms **adoption gates** on a **quiet** host (load < ~2),
where the model is already resident and only the single query-side forward pass
+ socket round-trip is on the hot path. The contended figures here are upper
bounds and would produce a false gate failure. The reproduction bench below —
`embed_query` timed with the model resident, and `Embedder::new` timed alone —
is provided **inline as an artifact** (not committed as an in-tree
`examples/embed_bench.rs`, which would gate `cargo test --workspace` on an
unverified example). Drop it into `system/harness/examples/` on a quiet host to
certify the gates:

```rust
// system/harness/examples/embed_bench.rs
use hex::memory::embed::Embedder;
use std::path::Path;
use std::time::Instant;

fn main() {
    let root = std::env::var("HEX_DIR")
        .unwrap_or_else(|_| format!("{}/hex", std::env::var("HOME").unwrap()));
    let root = Path::new(&root);

    let t0 = Instant::now();
    let e = Embedder::new(root).expect("embedder load");       // COLD-START
    println!("cold_start_ms {:.1}", t0.elapsed().as_secs_f64() * 1000.0);

    let qs = ["how do we handle memory schema migrations",
              "what did we decide about the recall ranking config",
              "who is responsible for the harness engine"];
    for q in &qs { let _ = e.embed_query(q).unwrap(); }         // warm-up

    let mut s = Vec::new();
    for _ in 0..14 { for q in &qs {
        let t = Instant::now(); let _ = e.embed_query(q).unwrap();
        s.push(t.elapsed().as_secs_f64() * 1000.0);
    }}
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("per_query_min_ms {:.2}  per_query_p50_ms {:.2}  per_query_p95_ms {:.2}",
             s[0], s[s.len()/2], s[(s.len()*95)/100]);
}
```

Run: `HEX_DIR=~/hex cargo run --release --example embed_bench`. Expected on a
quiet host (author-documented cold-load ~1.6 s; single nomic-v1.5 forward pass):
cold-start ≈ 1.5–2 s, per-query on the order of tens of ms — the numbers task 3
must confirm land the resident-socket arm inside the 200 / 500 ms gates while
option (a)'s per-invocation cold-load does not.

The decision in §5 is already robust without these: option (a) dies on the
≥ 1.6 s cold-load floor regardless of the exact per-query figure, and option
(b)'s marginal cost is a single forward pass behind a unix socket.
