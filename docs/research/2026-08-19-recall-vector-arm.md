# Recall vector (KNN) arm — query-embedding option research

**Date:** 2026-08-19
**Spec:** Sdnap37he · **Task:** Tw0gedxnr (research-embed-option)
**Author:** BOI execute worker (mike@mrap.me)
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
- **Rejected (a) per-invocation ONNX load:** measured cold model load makes
  every `hex memory recall` pay **seconds**, not milliseconds — off the latency
  budget by ≥3× on the *quiet-machine* floor alone. A "small MiniLM int8"
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

Read-only source: `/Users/mrap/.hex-evalsnap/.hex/memory.db` (106 MB), copied
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

Every measurement below was taken on the shared BOI execution host **under
sustained load average 40–54** (dozens of concurrent worker + `cargo` +
`hex memory index` processes). The contention factor is large and directly
observable: the arm-off `hex memory recall` baseline — pure SQLite FTS, no
model — measured a **min of 660 ms** here against a spec-stated quiet baseline
of **~18 ms**, i.e. **~35× inflation**. **Absolute latencies from this host are
upper bounds, not the numbers task 3's adoption gates should be judged
against.**

> **Finding for task 3:** the p50 ≤ 200 ms / p95 ≤ 500 ms adoption gates
> **cannot be validly measured on this saturated shared farm.** They must be
> re-measured on a quiet host (load < ~2). A gate failure observed here would
> be a false negative.

### Numbers (min-of-N, load-stamped)

| What | Path | Samples | Min | Load (1-min) |
|---|---|---|---|---|
| Arm-off recall (BM25 only, **no embed**) | `hex memory recall` | 20 | **660 ms** | ~50 |
| Per-invocation cold (`Embedder::new` + `embed_query` + FTS + KNN) | `hex memory search` | 5 | **≈ 21.9 s** | ~50 |
| Corpus embed (25 files ≈ 300 chunks, batch-8 doc side) | `hex memory index --full` | 1 | **> 3 min (timed out)** | ~43 |

Method: `/usr/bin/time -p` wall clock, `HEX_DIR=/tmp/va-snap`, freshly built
release binary at `/Users/mrap/.boi/v2/cargo-target/release/hex`.

### Interpretation (decision-grade, robust to the contention)

- **Cold-load dominates the per-invocation number by orders of magnitude.**
  The author-documented **~1.6 s** cold-load is the *quiet-machine floor*; the
  contended per-invocation search min was **~22 s**. Even taking the optimistic
  ~1.6 s floor, option (a) pays **≥ 1.6 s on every `hex memory recall`**, which
  is **≥ 3.2× over the 500 ms p95 budget** before a single query is embedded.
  The decision against (a) does **not** depend on resolving the contention.
- **Model-construction + one-embed (compound, measured, contended) — NOT a
  per-query figure:** the cold per-invocation search (`Embedder::new` + **one**
  `embed_query` + FTS + KNN) min was **21.9 s**; subtracting the arm-off recall
  min (**0.66 s**, same host, same window) leaves **≈ 21.2 s** for *model
  construction + one query embed*. **This ≈21.2 s is dominated by model
  construction (the 547 MB ONNX load) by ~3 orders of magnitude — it is NOT the
  per-query embed latency.** The single query embed is the small remainder and
  cannot be isolated on this host (model construction alone exceeds 3 min at
  load 56). **The true per-query figure — a single ONNX forward pass, on the
  order of tens of ms — could not be measured cleanly here and must be measured
  on a quiet host (§6).** Treat ≈21.2 s only as the per-invocation upper bound
  that kills option (a).
- **A single query embed is a lone ONNX forward pass**, orders of magnitude
  cheaper than the 547 MB model construction it currently rides behind. Under
  option (b) the construction is paid once in the resident engine, so the
  recall CLI's marginal cost is *only* that forward pass plus a unix-socket
  round-trip (sub-millisecond, no network). The **isolated** `embed_query`
  figure (model resident, min over 40 single-query embeds) is what task 3 must
  certify on a **quiet** host against the 200 / 500 ms gates; a reproduction
  bench is given inline in §6.

---

## 4. Option comparison

| | (a) per-invocation ONNX | **(b) resident harness socket** | (c) precomputed-only |
|---|---|---|---|
| Query vector in nomic space? | yes | **yes** | no (lexical projection only) |
| Cold-load paid… | every recall (~1.6 s floor, ~22 s here) | **once, in resident engine** | n/a |
| Marginal recall cost | model load + embed | **1 embed + socket RT** | ~0 |
| Fits 200/500 ms budget? | **no** (≥3× over on floor) | **plausibly yes** (needs quiet-host cert) | yes, but wrong answers |
| Reaches zero-overlap paraphrase? | yes | **yes** | **no** — the whole point |
| Extra moving parts | none | resident endpoint + fallback | query projector (unbuilt) |
| Dimensional consistency | 768 (ok) / 384 if MiniLM (breaks) | **768 (ok)** | n/a |

### Why (a) is rejected
1. **Latency:** ≥1.6 s cold-load per invocation on the quiet floor; ~22 s here.
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

## 6. Isolated per-query / cold-start — reproduction bench (for a quiet host)

An isolated micro-bench — `embed_query` timed with the model already resident
(min over 40 single-query embeds), and cold-load timed as `Embedder::new`
alone — is the clean way to separate per-query from cold-load. It could **not**
be produced on this saturated shared host: (1) building it fought the shared
`cargo` target lock (permanently held by sibling BOI builds), and a dedicated
target rebuild of the whole dep tree was abandoned under load 56; (2) even a
one-file `hex memory index` — which loads the same model — **timed out past
3 min** at load 56, so any in-process embed number here would be crushed. **The
isolated numbers must be certified on a quiet host (load < ~2) as part of
task 3's gate validation.**

The bench is a ~40-line `examples/embed_bench.rs` using the real production
`Embedder`; it was used during this research and then removed to keep task 1
from leaving an in-tree build target it has no mandate for (and to avoid an
unverified example gating `cargo test --workspace`). Re-create it on a quiet
host to certify the gates:

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
