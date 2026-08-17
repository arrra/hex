//! Report distill's readiness to actually extract facts: which prompt source
//! is in effect (embedded default vs instance override) and whether the
//! resolved provider can run at all. Report-only by design — the repo
//! deliberately walked back doctor-creates-placeholder-file patterns (see
//! `doctor_no_longer_creates_llm_preference_placeholder`), and with the
//! embedded prompt defaults a missing instance file is the NORMAL state, not
//! a defect. The one condition worth a warning: http transport with no
//! resolvable API key — distill defers every slice (no data is lost since
//! Deferred never strikes, but no facts are extracted either) until ops land
//! the key.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

pub struct DistillReadiness;

impl DoctorCheck for DistillReadiness {
    fn name(&self) -> &str {
        "distill-readiness"
    }
    fn category(&self) -> Category {
        Category::Config
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let mut detail = String::new();
        for name in ["extract", "judge"] {
            let p = ctx.hex_dir.join(format!(".hex/memory/prompts/{name}.txt"));
            // Mirrors extract.rs/judge.rs resolve_prompt: a whitespace-only
            // instance file does NOT shadow the embedded default.
            let src = match std::fs::read_to_string(&p) {
                Ok(s) if !s.trim().is_empty() => "instance override",
                _ => "embedded default",
            };
            detail.push_str(&format!("  {name} prompt: {src}\n"));
        }

        match crate::llm_config::resolve("memory_extract") {
            Ok(cfg) => {
                detail.push_str(&format!(
                    "  memory_extract: transport={} model={}\n",
                    cfg.transport, cfg.model
                ));
                if cfg.transport == crate::llm_config::TRANSPORT_HTTP
                    && crate::memory::provider::load_api_key(&cfg.api_key_env).is_none()
                {
                    return CheckResult::warn(format!(
                        "distill will DEFER every slice: transport=http and no {} \
                         resolvable (env or .hex/secrets/openrouter.env). Nothing \
                         is lost while deferred, but no facts are extracted until \
                         the key lands",
                        cfg.api_key_env
                    ))
                    .with_details(detail);
                }
            }
            Err(e) => {
                return CheckResult::warn(format!(
                    "llm config unresolvable for memory_extract: {e}"
                ))
                .with_details(detail);
            }
        }
        CheckResult::pass("distill ready (prompts resolved, provider configured)")
            .with_details(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::Status;

    fn ctx_for(dir: &std::path::Path) -> Context {
        Context {
            hex_dir: dir.to_path_buf(),
            home: dir.to_path_buf(),
            fix: false,
        }
    }

    #[test]
    fn warns_when_http_transport_has_no_key() {
        let (td, _g) = crate::telemetry::test_support::isolate();
        std::env::remove_var("OPENROUTER_API_KEY");
        let res = DistillReadiness.run(&ctx_for(td.path()));
        assert_eq!(res.status, Status::Warn, "no key + http must warn: {res:?}");
        assert!(res.message.contains("DEFER"));
    }

    #[test]
    fn passes_on_claude_cli_transport_without_any_key() {
        let (td, _g) = crate::telemetry::test_support::isolate();
        std::env::remove_var("OPENROUTER_API_KEY");
        std::fs::create_dir_all(td.path().join(".hex/config")).unwrap();
        std::fs::write(
            td.path().join(".hex/config/llm.toml"),
            "[use_cases.memory_extract]\ntransport = \"claude-cli\"\n",
        )
        .unwrap();
        let res = DistillReadiness.run(&ctx_for(td.path()));
        assert_eq!(
            res.status,
            Status::Pass,
            "claude-cli transport needs no http key: {res:?}"
        );
        let details = res.details.unwrap_or_default();
        assert!(
            details.contains("embedded default"),
            "with no instance prompt files the embedded default must be reported: {details}"
        );
    }
}
