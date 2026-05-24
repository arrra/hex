use std::process::Command;

fn main() {
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=HEX_GIT_SHA={}", git_sha);

    // Generate personal_mods.rs with absolute #[path] declarations for the personal feature.
    // Resolves HEX_DIR (set by env.sh) first; otherwise $HOME/hex (install.sh default).
    // No machine-specific path may live in source — both env vars must be set when building with --features personal.
    let personal_dir = std::env::var("HEX_DIR")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{}/hex", h)))
        .map(|d| format!("{}/.hex/harness-personal", d))
        .expect("HEX_DIR or HOME must be set to locate .hex/harness-personal/");
    println!("cargo:rerun-if-env-changed=HEX_DIR");
    println!("cargo:rerun-if-env-changed=HOME");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let personal_mods = format!(
        r#"
#[path = "{d}/integration_apple_addressbook.rs"] mod integration_apple_addressbook;
#[path = "{d}/integration_tailscale.rs"] mod integration_tailscale;
#[path = "{d}/integration_mcp_exa.rs"] mod integration_mcp_exa;
#[path = "{d}/integration_mcp_excalidraw.rs"] mod integration_mcp_excalidraw;
#[path = "{d}/integration_mcp_plugin_ecc.rs"] mod integration_mcp_plugin_ecc;
#[path = "{d}/integration_x_twitter.rs"] mod integration_x_twitter;
#[path = "{d}/integration_publer.rs"] mod integration_publer;
#[path = "{d}/integration_granola_mcp.rs"] mod integration_granola_mcp;
#[path = "{d}/kalshi.rs"] mod kalshi;
#[path = "{d}/mirofish.rs"] mod mirofish;
#[path = "{d}/release.rs"] mod release;
"#,
        d = personal_dir
    );
    std::fs::write(format!("{}/personal_mods.rs", out_dir), personal_mods).unwrap();
}
