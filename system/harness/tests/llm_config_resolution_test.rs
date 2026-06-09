// Red test for llm_config::resolve — see spec Sy23e65a2 task T9jf9hyx2.
//
// Verifies:
//   * Missing llm.toml → built-in defaults (zero behavior change)
//   * [use_cases.X] overrides [defaults]
//   * HEX_LLM_MODEL_<USE_CASE_UPPER> env var beats file
//   * HEX_CONSOLIDATE_MODEL alias still resolves consolidate_audit
//   * Malformed TOML is a hard error (not a silent fallback)
//
// All cases share one #[test] because they mutate process-wide env vars
// (HEX_DIR / HEX_LLM_MODEL_*) and must run sequentially.

use std::fs;

use hex::llm_config;

fn clear_overrides() {
    for k in [
        "HEX_LLM_MODEL_MEMORY_EXTRACT",
        "HEX_LLM_MODEL_MEMORY_JUDGE",
        "HEX_LLM_MODEL_CONSOLIDATE_AUDIT",
        "HEX_LLM_MODEL_HEALTH_CHECK",
        "HEX_CONSOLIDATE_MODEL",
    ] {
        std::env::remove_var(k);
    }
}

#[test]
fn llm_config_resolution_layers() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HEX_DIR", tmp.path());
    clear_overrides();

    // ---- 1. Missing llm.toml → built-in defaults ----------------------------
    let r = llm_config::resolve("memory_extract").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-sonnet-4.5");
    assert_eq!(r.max_tokens, 16384);

    let r = llm_config::resolve("memory_judge").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-sonnet-4.5");
    assert_eq!(r.max_tokens, 256);

    let r = llm_config::resolve("consolidate_audit").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-sonnet-4.5");
    assert_eq!(r.max_tokens, 4096);

    let r = llm_config::resolve("health_check").expect("built-in default");
    assert_eq!(r.model, "anthropic/claude-haiku-4.5");

    // ---- 2. [use_cases.X] overrides [defaults] -----------------------------
    let cfg_dir = tmp.path().join(".hex/config");
    fs::create_dir_all(&cfg_dir).unwrap();
    let cfg_path = cfg_dir.join("llm.toml");
    fs::write(
        &cfg_path,
        r#"
[defaults]
model = "openrouter/default-model"

[use_cases.memory_extract]
model = "anthropic/claude-opus-4.1"
max_tokens = 9999
"#,
    )
    .unwrap();

    let r = llm_config::resolve("memory_extract").expect("file resolve");
    assert_eq!(r.model, "anthropic/claude-opus-4.1");
    assert_eq!(r.max_tokens, 9999);

    // use_case without override picks up [defaults].model, but max_tokens
    // still falls back to the built-in for that use case.
    let r = llm_config::resolve("memory_judge").expect("defaults fallback");
    assert_eq!(r.model, "openrouter/default-model");
    assert_eq!(r.max_tokens, 256);

    // ---- 3. Env var beats file --------------------------------------------
    std::env::set_var(
        "HEX_LLM_MODEL_MEMORY_EXTRACT",
        "anthropic/from-env-extract",
    );
    let r = llm_config::resolve("memory_extract").expect("env override");
    assert_eq!(r.model, "anthropic/from-env-extract");
    std::env::remove_var("HEX_LLM_MODEL_MEMORY_EXTRACT");

    // ---- 4. HEX_CONSOLIDATE_MODEL alias for consolidate_audit -------------
    std::env::set_var("HEX_CONSOLIDATE_MODEL", "anthropic/legacy-alias");
    let r = llm_config::resolve("consolidate_audit").expect("alias env override");
    assert_eq!(r.model, "anthropic/legacy-alias");
    std::env::remove_var("HEX_CONSOLIDATE_MODEL");

    // ---- 5. Malformed TOML is a hard error --------------------------------
    fs::write(&cfg_path, "this is = not valid = toml [[[").unwrap();
    let err = llm_config::resolve("memory_extract");
    assert!(
        err.is_err(),
        "malformed llm.toml must be a loud error, got Ok({:?})",
        err.ok()
    );
}
