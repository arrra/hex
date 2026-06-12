//! Red test for spec Sbe8m4886 task T0mnyfwa7 — the claude-cli transport's
//! model-name mapping.
//!
//! The registry uses OpenRouter ids (e.g. "anthropic/claude-sonnet-4.5"),
//! but the headless `claude -p` binary wants its own naming scheme
//! ("claude-sonnet-4-5"). The transport module
//! `system/harness/src/memory/claude_cli.rs` MUST expose a pure
//! `map_model_to_cli(&str) -> String` function that:
//!
//!   * strips the leading "anthropic/" prefix, then replaces '.' with '-'
//!     ONLY in the trailing version segment — so "anthropic/claude-sonnet-4.5"
//!     becomes "claude-sonnet-4-5";
//!   * passes models WITHOUT an "anthropic/" prefix through verbatim, so
//!     llm.toml can specify CLI aliases like "sonnet" directly.
//!
//! This file fails to compile until the module and function exist — that is
//! the intended red state. Once the implementation lands the test must turn
//! green without modification.
//!
//! See spec_contract scope, "Model-name mapping" paragraph, for the recipe.

use hex::memory::claude_cli::map_model_to_cli;

#[test]
fn anthropic_prefixed_sonnet_45_maps_to_cli_form() {
    // Both current registry ids must work — see scope: "Unit-test the mapping
    // for both current registry ids."
    assert_eq!(
        map_model_to_cli("anthropic/claude-sonnet-4.5"),
        "claude-sonnet-4-5",
    );
}

#[test]
fn anthropic_prefixed_haiku_45_maps_to_cli_form() {
    assert_eq!(
        map_model_to_cli("anthropic/claude-haiku-4.5"),
        "claude-haiku-4-5",
    );
}

#[test]
fn non_anthropic_prefixed_model_passes_through_verbatim() {
    // Lets llm.toml use CLI aliases like "sonnet" directly without any
    // mangling — verbatim passthrough is the contract.
    assert_eq!(map_model_to_cli("sonnet"), "sonnet");
    assert_eq!(map_model_to_cli("claude-sonnet-4-5"), "claude-sonnet-4-5");
}
