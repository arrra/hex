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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_standard_cost_row() {
        let (_t, _g) = crate::telemetry::test_support::isolate();
        // Pre-existing suite hazard (observed 2026-06-11): llm_config's tests
        // mutate the process-global HEX_DIR under their OWN private mutex (not
        // telemetry::test_support::ENV_LOCK), and `llm_cost` is alphabetically
        // adjacent so libtest schedules them concurrently — HEX_DIR can be
        // repointed between this test's write and read. Re-pin HEX_DIR and
        // retry until one write+read cycle completes uninterfered (the
        // interfering burst is milliseconds long). Values are identical every
        // iteration, so finding any row is sufficient.
        let mut found = None;
        for _ in 0..100 {
            std::env::set_var("HEX_DIR", _t.path());
            record_llm_cost(
                "claude-cli",
                "extract",
                1200,
                340,
                0.0425,
                Some("claude-sonnet-4-6"),
            );
            let rows = crate::telemetry::recent(5).unwrap();
            if let Some(r) = rows.iter().find(|r| r.source == "llm-cost") {
                found = Some(r.clone());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let row = &found.expect("cost row");
        assert_eq!(row.event, "claude-cli::extract");
        assert_eq!(row.status, "ok");
        let d: serde_json::Value = serde_json::from_str(row.detail.as_ref().unwrap()).unwrap();
        assert_eq!(d["in_tokens"], 1200);
        assert_eq!(d["out_tokens"], 340);
        assert_eq!(d["cost_usd"], 0.0425);
        assert_eq!(d["model"], "claude-sonnet-4-6");
    }
}
