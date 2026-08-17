//! Folded in from the former `hex memory llm-check` subcommand.
//! Probes LLM provider reachability via memory::provider::health_check().
//! Deferred (no key / not configured / test env) → SKIP; upstream error → WARN.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use crate::memory::provider::{self, ProviderError};

pub struct LlmProviderReachable;

impl DoctorCheck for LlmProviderReachable {
    fn name(&self) -> &str {
        "llm-provider"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, _ctx: &Context) -> CheckResult {
        match provider::health_check() {
            Ok(_) => CheckResult::pass("LLM provider reachable"),
            Err(ProviderError::Deferred(msg)) => {
                CheckResult::skip(format!("LLM provider not configured — {msg}"))
            }
            Err(ProviderError::Upstream(msg)) => {
                CheckResult::warn(format!("LLM provider upstream error — {msg}"))
            }
        }
    }
}
