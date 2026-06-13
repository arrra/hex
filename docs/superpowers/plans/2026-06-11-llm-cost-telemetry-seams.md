# LLM Cost Telemetry Seams (OBS-024) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture LLM token usage + cost at all three provider seams into events.db, ending the `cost_usd=0.0`-forever silent under-report (OBS-024). ~5 lines per seam — the parse already exists at one of them.

**Architecture:** One tiny `src/llm_cost.rs` helper that writes a standard row shape (source=`llm-cost`, event=`{transport}::{use_case}`, detail=JSON `{in_tokens, out_tokens, cost_usd, model}`), called from: (1) `memory/claude_cli.rs` (already parses usage, currently eprintln-only — the comment literally says "OBS-024 will pick this up later"), (2) `memory/provider.rs::generate_inner` (OpenRouter `usage` object currently dropped), (3) `worker/run.rs` (claude envelope `usage`/`total_cost_usd` currently dropped).

**Tech Stack:** Rust (harness crate at `system/harness/`), serde_json. Tests in-module with `telemetry::test_support::isolate()`.

**Context (verified 2026-06-11):**
- `memory/claude_cli.rs:224-240` — parses `usage.input_tokens`, `usage.output_tokens`, `total_cost_usd` from the claude envelope; eprintlns; has `use_case: &str` in scope.
- `memory/provider.rs:~164-171` (`generate_inner`) — `json["choices"][0]...` extracted, `json["usage"]` (OpenRouter shape: `prompt_tokens`/`completion_tokens`; `cost` when present) dropped. `generate_inner` does NOT currently receive `use_case` — thread it from `generate_for` (find all call sites: `grep -n "generate_inner(" system/harness/src/memory/provider.rs`).
- `worker/run.rs:~193-198` — claude `-p` envelope parsed for `structured_output` only; `usage`/`total_cost_usd` dropped.
- Precedent row shape in live db: agent-infra `nightly-cost` events with JSON detail `{"in_tokens":…,"out_tokens":…,"cost_usd":…}`.
- Known caveat to note in code: BOI/Goose paths often report `tokens_out=0` — cost is an input-side floor (OBS-024 entry in evolution/observations.md).

**Verification baseline:** `cd system/harness && cargo test` green before starting. STOP conditions: code ≠ plan description (report drift); a verification fails twice; a fix wants files beyond the three seams + the helper + docs.

---

### Task 1: `llm_cost.rs` helper

**Files:**
- Create: `system/harness/src/llm_cost.rs`
- Modify: the crate's module-declaration file (`grep -rn "pub mod alert" system/harness/src/` — add `pub mod llm_cost;`)

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_standard_cost_row() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        record_llm_cost("claude-cli", "extract", 1200, 340, 0.0425, Some("claude-sonnet-4-6"));
        let rows = crate::telemetry::recent(5).unwrap();
        let row = rows.iter().find(|r| r.source == "llm-cost").expect("cost row");
        assert_eq!(row.event, "claude-cli::extract");
        assert_eq!(row.status, "ok");
        let d: serde_json::Value = serde_json::from_str(row.detail.as_ref().unwrap()).unwrap();
        assert_eq!(d["in_tokens"], 1200);
        assert_eq!(d["out_tokens"], 340);
        assert_eq!(d["cost_usd"], 0.0425);
        assert_eq!(d["model"], "claude-sonnet-4-6");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cd system/harness && cargo test llm_cost 2>&1 | tail -5` → FAIL (module missing).

- [ ] **Step 3: Implement**

```rust
//! OBS-024: LLM cost capture at the provider seams → events.db.
//! One standard row shape so any consumer can aggregate spend per transport
//! and use-case. Loud-but-not-fatal (record_loud): cost telemetry must never
//! fail the LLM call it observes.
//!
//! Known caveat: some paths (BOI/Goose) report out_tokens=0 — treat recorded
//! cost as an input-side floor, not a total.

/// Record one LLM call's usage. `transport` = claude-cli | openrouter |
/// worker-run; `use_case` = the llm.toml use case or call-site label.
pub fn record_llm_cost(
    transport: &str,
    use_case: &str,
    in_tokens: u64,
    out_tokens: u64,
    cost_usd: f64,
    model: Option<&str>,
) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "llm-cost".into(),
        event: format!("{transport}::{use_case}"),
        status: "ok".into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(
            serde_json::json!({
                "in_tokens": in_tokens,
                "out_tokens": out_tokens,
                "cost_usd": cost_usd,
                "model": model,
            })
            .to_string(),
        ),
    });
}
```

- [ ] **Step 4: Run** — `cargo test llm_cost 2>&1 | tail -5` → PASS.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat(llm-cost): standard cost-row helper (OBS-024)"`

---

### Task 2: Seam 1 — claude_cli.rs (parse already exists)

**Files:**
- Modify: `system/harness/src/memory/claude_cli.rs:224-240`

- [ ] **Step 1: Edit the existing usage block** — keep the eprintln, add the record, update the comment:

```rust
    // Cost telemetry seam (OBS-024 — recorded to events.db via llm_cost).
    if let Some(usage) = envelope.get("usage") {
        let in_tok = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let out_tok = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cost = envelope
            .get("total_cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        eprintln!("claude-cli[{use_case}]: in={in_tok} out={out_tok} cost_usd={cost}");
        crate::llm_cost::record_llm_cost("claude-cli", use_case, in_tok, out_tok, cost, None);
    }
```

(If the model id is available in scope or in the envelope — check `envelope.get("model")` — pass `Some(&model_str)` instead of `None`.)

- [ ] **Step 2: Suite** — `cargo test 2>&1 | tail -5` → PASS.
- [ ] **Step 3: Commit** — `git commit -am "feat(llm-cost): claude-cli seam records cost rows"`

---

### Task 3: Seam 2 — provider.rs generate_inner (thread use_case)

**Files:**
- Modify: `system/harness/src/memory/provider.rs`

- [ ] **Step 1:** `grep -n "generate_inner(" system/harness/src/memory/provider.rs` — find the definition + every call site.

- [ ] **Step 2:** Add `use_case: &str` as the first parameter of `generate_inner`; update every call site to pass the use-case string it already has (e.g. `generate_for` has it; `health_check` passes `"health_check"`). Then add the capture right after the `let json: serde_json::Value = resp.into_json()...` line and BEFORE the content extraction:

```rust
    // OBS-024: OpenRouter-shape usage (prompt_tokens/completion_tokens;
    // `cost` is OpenRouter-specific and absent on other gateways → 0.0).
    if let Some(usage) = json.get("usage") {
        let in_tok = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let out_tok = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cost = usage.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
        crate::llm_cost::record_llm_cost("openrouter", use_case, in_tok, out_tok, cost, Some(model));
    }
```

(`model` is already a parameter of `generate_inner` — pass it through. If the var name differs, match the actual signature.)

- [ ] **Step 3: Suite** — `cargo test 2>&1 | tail -5` → PASS (provider tests exercise the deferred path, not the HTTP path — they should be untouched; if any test constructs `generate_inner` calls, update its arity).

- [ ] **Step 4: Commit** — `git commit -am "feat(llm-cost): openrouter seam records cost rows (use_case threaded)"`

---

### Task 4: Seam 3 — worker/run.rs + final gates

**Files:**
- Modify: `system/harness/src/worker/run.rs:~192-199`

- [ ] **Step 1:** After the envelope parse succeeds (the `let envelope: serde_json::Value = serde_json::from_slice(&out.stdout)...` line) and before `structured_output` extraction, add:

```rust
    // OBS-024: claude envelope usage (same shape as memory/claude_cli.rs).
    if let Some(usage) = envelope.get("usage") {
        let in_tok = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let out_tok = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cost = envelope.get("total_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
        crate::llm_cost::record_llm_cost("worker-run", "question", in_tok, out_tok, cost, None);
    }
```

(If `run.rs` has a use-case/label in scope better than the static `"question"` — check the surrounding fn's params — use it.)

- [ ] **Step 2: Full suite + release build** — `cargo test 2>&1 | tail -3` → PASS; `cargo build --release 2>&1 | tail -3` → compiles.

- [ ] **Step 3: Live smoke (one cheap real call)**

Run: `HEX_DIR=/Users/mrap/hex ./target/release/hex memory search "telemetry" >/dev/null 2>&1; sqlite3 /Users/mrap/hex/.hex/telemetry/events.db "SELECT ts,event,detail FROM events WHERE source='llm-cost' ORDER BY id DESC LIMIT 3;"`
Expected: if that path makes an LLM call, a cost row appears; if it doesn't (pure-index search), note it and instead verify via `cargo test llm_cost` only — do NOT invent an expensive smoke. Either way, record what you observed.

- [ ] **Step 4: Commit + report** — `git commit -am "feat(llm-cost): worker-run seam (OBS-024 complete at all three seams)"`; report branch, test counts, smoke observation, deviations.
