//! Per-use-case LLM configuration registry.
//!
//! See spec Sy23e65a2. This module is the source of truth for which model,
//! max_tokens, base_url, and api_key_env each LLM-backed hex use case should
//! call. Resolution order (highest wins):
//!   1. env var HEX_LLM_MODEL_<USE_CASE_UPPER> (model only)
//!      - HEX_CONSOLIDATE_MODEL is honored as an alias for consolidate_audit
//!   2. [use_cases.<name>] in $HEX_DIR/.hex/config/llm.toml
//!   3. [defaults]            in $HEX_DIR/.hex/config/llm.toml
//!   4. built-in registry defaults below
//!
//! Missing llm.toml → built-ins (zero behavior change). Malformed llm.toml →
//! LOUD error returned from `resolve()`. Unknown [use_cases.*] table names →
//! warning to stderr but tolerated (per spec).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLlm {
    pub model: String,
    pub max_tokens: u32,
    pub base_url: String,
    pub api_key_env: String,
    /// Optional cap on estimated INPUT tokens per call. Used by distill to
    /// slice oversize spans. None means "no input cap configured".
    pub max_input_tokens: Option<u32>,
}

/// Built-in default for a known use case. These are the values that were
/// hardcoded at call sites before this module existed.
#[derive(Debug, Clone)]
struct BuiltIn {
    model: &'static str,
    max_tokens: u32,
    max_input_tokens: Option<u32>,
}

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_API_KEY_ENV: &str = "OPENROUTER_API_KEY";

fn builtin(use_case: &str) -> Option<BuiltIn> {
    match use_case {
        "memory_extract" => Some(BuiltIn {
            model: "anthropic/claude-sonnet-4.5",
            max_tokens: 16384,
            // 48k input cap leaves comfortable headroom under the model's
            // input limit alongside the 16384-token output cap. Anything
            // larger gets sliced by `memory::distill::cap::cap_span`.
            max_input_tokens: Some(48_000),
        }),
        "memory_judge" => Some(BuiltIn {
            model: "anthropic/claude-sonnet-4.5",
            max_tokens: 256,
            max_input_tokens: None,
        }),
        "consolidate_audit" => Some(BuiltIn {
            model: "anthropic/claude-sonnet-4.5",
            max_tokens: 4096,
            max_input_tokens: None,
        }),
        "health_check" => Some(BuiltIn {
            model: "anthropic/claude-haiku-4.5",
            max_tokens: 64,
            max_input_tokens: None,
        }),
        _ => None,
    }
}

/// All known use cases — used by doctor to list resolved configs.
pub fn known_use_cases() -> &'static [&'static str] {
    &[
        "memory_extract",
        "memory_judge",
        "consolidate_audit",
        "health_check",
    ]
}

#[derive(Debug, Deserialize, Default)]
struct LlmTomlFile {
    #[serde(default)]
    defaults: Option<SectionFields>,
    #[serde(default)]
    use_cases: HashMap<String, SectionFields>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct SectionFields {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    max_input_tokens: Option<u32>,
}

fn config_path() -> PathBuf {
    let hex_dir = std::env::var("HEX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hex")
        });
    hex_dir.join(".hex/config/llm.toml")
}

fn load_file() -> Result<Option<LlmTomlFile>> {
    let path = config_path();
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("reading llm.toml at {}", path.display()))?;
    let parsed: LlmTomlFile = toml::from_str(&body)
        .with_context(|| format!("parsing llm.toml at {}", path.display()))?;

    // Warn (do not fail) on unknown use-case table names.
    for name in parsed.use_cases.keys() {
        if !known_use_cases().contains(&name.as_str()) {
            eprintln!(
                "warning: llm.toml contains unknown [use_cases.{}] — \
                 known use cases are: {}",
                name,
                known_use_cases().join(", ")
            );
        }
    }
    Ok(Some(parsed))
}

fn env_model_for(use_case: &str) -> Option<String> {
    let key = format!("HEX_LLM_MODEL_{}", use_case.to_uppercase());
    if let Ok(v) = std::env::var(&key) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    // Back-compat alias.
    if use_case == "consolidate_audit" {
        if let Ok(v) = std::env::var("HEX_CONSOLIDATE_MODEL") {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

pub fn resolve(use_case: &str) -> Result<ResolvedLlm> {
    let bi = builtin(use_case).with_context(|| {
        format!(
            "unknown LLM use case `{}` — known: {}",
            use_case,
            known_use_cases().join(", ")
        )
    })?;

    let file = load_file()?;

    let defaults = file.as_ref().and_then(|f| f.defaults.clone()).unwrap_or_default();
    let uc = file
        .as_ref()
        .and_then(|f| f.use_cases.get(use_case).cloned())
        .unwrap_or_default();

    // Field resolution: env (model only) > use_case > defaults > built-in.
    let model = env_model_for(use_case)
        .or(uc.model)
        .or(defaults.model.clone())
        .unwrap_or_else(|| bi.model.to_string());

    let max_tokens = uc
        .max_tokens
        .or(defaults.max_tokens)
        .unwrap_or(bi.max_tokens);

    let base_url = uc
        .base_url
        .or(defaults.base_url.clone())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let api_key_env = uc
        .api_key_env
        .or(defaults.api_key_env)
        .unwrap_or_else(|| DEFAULT_API_KEY_ENV.to_string());

    let max_input_tokens = uc
        .max_input_tokens
        .or(defaults.max_input_tokens)
        .or(bi.max_input_tokens);

    Ok(ResolvedLlm {
        model,
        max_tokens,
        base_url,
        api_key_env,
        max_input_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    // Tests mutate process env vars (HEX_DIR, HEX_LLM_MODEL_*,
    // HEX_CONSOLIDATE_MODEL) and must run serially.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const ENV_KEYS: &[&str] = &[
        "HEX_DIR",
        "HEX_LLM_MODEL_MEMORY_EXTRACT",
        "HEX_LLM_MODEL_MEMORY_JUDGE",
        "HEX_LLM_MODEL_CONSOLIDATE_AUDIT",
        "HEX_LLM_MODEL_HEALTH_CHECK",
        "HEX_CONSOLIDATE_MODEL",
    ];

    fn clear_env() {
        for k in ENV_KEYS {
            std::env::remove_var(k);
        }
    }

    fn setup_hex_dir() -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("HEX_DIR", td.path());
        std::fs::create_dir_all(td.path().join(".hex/config")).unwrap();
        td
    }

    fn write_llm_toml(hex_dir: &std::path::Path, body: &str) {
        let p = hex_dir.join(".hex/config/llm.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn missing_file_uses_builtins() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let _td = setup_hex_dir();

        let r = resolve("memory_extract").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-sonnet-4.5");
        assert_eq!(r.max_tokens, 16384);

        let r = resolve("memory_judge").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-sonnet-4.5");
        assert_eq!(r.max_tokens, 256);

        let r = resolve("consolidate_audit").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-sonnet-4.5");
        assert_eq!(r.max_tokens, 4096);

        let r = resolve("health_check").expect("resolve ok");
        assert_eq!(r.model, "anthropic/claude-haiku-4.5");
    }

    #[test]
    fn use_case_overrides_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let td = setup_hex_dir();
        write_llm_toml(
            td.path(),
            r#"
[defaults]
model = "defaults/model"

[use_cases.memory_extract]
model = "uc/extract-model"
max_tokens = 999
"#,
        );

        let r = resolve("memory_extract").expect("resolve ok");
        assert_eq!(r.model, "uc/extract-model");
        assert_eq!(r.max_tokens, 999);

        // memory_judge has no use_cases entry → inherits [defaults] model,
        // but max_tokens falls back to the built-in (256).
        let r = resolve("memory_judge").expect("resolve ok");
        assert_eq!(r.model, "defaults/model");
        assert_eq!(r.max_tokens, 256);
    }

    #[test]
    fn env_var_overrides_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let td = setup_hex_dir();
        write_llm_toml(
            td.path(),
            r#"
[use_cases.memory_extract]
model = "file/model"
"#,
        );
        std::env::set_var("HEX_LLM_MODEL_MEMORY_EXTRACT", "env/model");

        let r = resolve("memory_extract").expect("resolve ok");
        assert_eq!(r.model, "env/model");
    }

    #[test]
    fn hex_consolidate_model_alias_still_works() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let _td = setup_hex_dir();
        std::env::set_var("HEX_CONSOLIDATE_MODEL", "alias/consolidate");

        let r = resolve("consolidate_audit").expect("resolve ok");
        assert_eq!(r.model, "alias/consolidate");
    }

    #[test]
    fn malformed_toml_is_hard_error() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let td = setup_hex_dir();
        write_llm_toml(td.path(), "this is = = not valid toml [[[");

        let err = resolve("memory_extract")
            .err()
            .expect("malformed TOML must be a loud error, not a silent fallback");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("llm.toml") || msg.to_lowercase().contains("toml"),
            "error should mention llm.toml / TOML, got: {msg}"
        );
    }
}
