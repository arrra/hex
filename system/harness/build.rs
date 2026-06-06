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
#[path = "{d}/release.rs"] mod release;
"#,
        d = personal_dir
    );
    std::fs::write(format!("{}/personal_mods.rs", out_dir), personal_mods).unwrap();

    // ---- hex module discovery: recursive *.worker.rs glob → hex_modules.rs ----
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut roots: Vec<String> = vec![format!("{manifest_dir}/src/modules")];
    // Personal modules root (out-of-crate) only under --features personal.
    if std::env::var("CARGO_FEATURE_PERSONAL").is_ok() {
        let hex_dir = std::env::var("HEX_DIR")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/hex")))
            .expect("HEX_DIR or HOME must be set to locate .hex/modules/");
        roots.push(format!("{hex_dir}/.hex/modules"));
    }

    let mut entries: Vec<(String, String)> = Vec::new(); // (mod_ident, abs_path)
    for root in &roots {
        println!("cargo:rerun-if-changed={root}");
        collect_worker_files(std::path::Path::new(root), std::path::Path::new(root), &mut entries);
    }
    entries.sort();

    // Loud (S6) on ident collision — two files mapping to the same mod ident
    // would otherwise surface as an opaque rustc "defined multiple times" error.
    let mut seen = std::collections::HashSet::new();
    for (ident, path) in &entries {
        if !seen.insert(ident.clone()) {
            panic!("hex module: ident collision on '{ident}' (from '{path}') — rename the file");
        }
    }

    let mut gen = String::new();
    for (ident, path) in &entries {
        gen.push_str(&format!("#[path = \"{path}\"] pub mod {ident};\n"));
    }
    gen.push_str("pub fn module_registry() -> Vec<crate::worker::Worker> {\n    vec![");
    for (ident, _) in &entries {
        gen.push_str(&format!("{ident}::worker(), "));
    }
    gen.push_str("]\n}\n");
    gen.push_str("pub fn module_paths() -> Vec<(String, &'static str)> {\n    vec![");
    for (ident, path) in &entries {
        gen.push_str(&format!("({ident}::worker().name.clone(), \"{path}\"), "));
    }
    gen.push_str("]\n}\n");
    std::fs::write(format!("{out_dir}/hex_modules.rs"), gen).unwrap();
}

/// Recursively collect `*.worker.rs` files under `dir`. `root` is the glob root
/// used to derive a unique snake_case mod ident from the relative path
/// (`trading/kalshi.worker.rs` → `trading_kalshi`). Absent dir = no-op.
fn collect_worker_files(
    dir: &std::path::Path,
    root: &std::path::Path,
    out: &mut Vec<(String, String)>,
) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return, // absent / unreadable → contribute nothing
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_worker_files(&path, root, out);
        } else if path.file_name().and_then(|s| s.to_str())
            .map(|n| n.ends_with(".worker.rs")).unwrap_or(false)
        {
            let rel = path.strip_prefix(root).unwrap();
            let rel_str = rel.to_str().unwrap();
            let stem = rel_str.trim_end_matches(".worker.rs");
            let ident: String = stem
                .chars()
                .map(|c| if c == '/' || c == '.' || c == '-' { '_' } else { c })
                .collect();
            if ident.is_empty()
                || ident.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true)
                || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                panic!(
                    "hex module: '{}' does not yield a valid Rust identifier (got '{}')",
                    path.display(), ident
                );
            }
            // Per-file rerun trigger so edits to a nested module rebuild
            // (the per-root dir watch alone can miss deep-subdir file changes).
            println!("cargo:rerun-if-changed={}", path.display());
            out.push((ident, path.to_str().unwrap().to_string()));
        }
    }
}
