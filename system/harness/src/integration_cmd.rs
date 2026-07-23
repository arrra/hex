/// Native Rust ports of integration Python commands:
/// list_cmd.py, status.py, probe.py, rotate.py, validate.py, update.py
///
/// State files live at: HEX_DIR/projects/integrations/_state/<name>.json
/// Bundle dirs live at:  HEX_DIR/integrations/<name>/
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

fn state_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join("projects/integrations/_state")
}

/// Resolve the integration-check harness script, preferring the DEPLOYED layout
/// (`.hex/scripts/`) and falling back to the foundation source layout
/// (`system/scripts/`). Hardcoding `system/scripts/` broke every probe/check on
/// deployed instances, where the script lives under `.hex/scripts/`.
pub fn harness_script(hex_dir: &Path) -> PathBuf {
    let deployed = hex_dir.join(".hex/scripts/hex-integration-check.sh");
    if deployed.exists() {
        deployed
    } else {
        hex_dir.join("system/scripts/hex-integration-check.sh")
    }
}

fn bundles_dir(hex_dir: &Path) -> PathBuf {
    hex_dir.join("integrations")
}

fn read_state(hex_dir: &Path, name: &str) -> Option<Value> {
    let path = state_dir(hex_dir).join(format!("{name}.json"));
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_state_atomic(hex_dir: &Path, name: &str, data: &Value) -> bool {
    let dir = state_dir(hex_dir);
    if fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = dir.join(format!("{name}.json"));
    let tmp_path = dir.join(format!("{name}.json.tmp"));
    let Ok(content) = serde_json::to_string_pretty(data) else {
        return false;
    };
    let content = content + "\n";
    if fs::write(&tmp_path, content).is_err() {
        return false;
    }
    fs::rename(&tmp_path, &path).is_ok()
}

fn is_installed(hex_dir: &Path, name: &str) -> bool {
    state_dir(hex_dir).join(format!("{name}.json")).exists()
}

fn list_installed(hex_dir: &Path) -> Vec<String> {
    let dir = state_dir(hex_dir);
    let mut names = vec![];
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let n = entry.file_name().to_string_lossy().to_string();
            if n.ends_with(".json") && !n.starts_with('.') && !n.starts_with('_') {
                // Boundary proof: ".json" is a 5-byte ASCII suffix (verified by
                // ends_with), so n.len() - 5 lands exactly at its start, a char
                // boundary.
                #[allow(clippy::string_slice)]
                let stem = n[..n.len() - 5].to_string();
                names.push(stem);
            }
        }
    }
    names.sort();
    names
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn read_manifest_yaml(bundle_dir: &Path) -> Result<Value, String> {
    let path = bundle_dir.join("integration.yaml");
    let content =
        fs::read_to_string(&path).map_err(|e| format!("Cannot read integration.yaml: {e}"))?;
    serde_yaml::from_str(&content).map_err(|e| format!("Invalid YAML in integration.yaml: {e}"))
}

fn manifest_hash(bundle_dir: &Path) -> String {
    let path = bundle_dir.join("integration.yaml");
    let content = fs::read(&path).unwrap_or_default();
    let hash = Sha256::digest(content);
    format!("{hash:x}")
}

/// Port of lib/integration/commands/list_cmd.py
/// Lists available bundles and their installed/available status.
pub fn list(hex_dir: &Path, json_out: bool) -> i32 {
    let bundles = bundles_dir(hex_dir);
    let mut available: Vec<String> = vec![];
    if let Ok(entries) = fs::read_dir(&bundles) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let yaml_path = bundles.join(&name).join("integration.yaml");
            if entry.path().is_dir() && yaml_path.exists() {
                available.push(name);
            }
        }
    }
    available.sort();

    let installed_names: std::collections::HashSet<String> =
        list_installed(hex_dir).into_iter().collect();

    let mut rows: Vec<Value> = vec![];
    for name in &available {
        let installed = installed_names.contains(name);
        let status = if installed { "installed" } else { "available" };
        let (tier, last_probed) = if installed {
            let st = read_state(hex_dir, name).unwrap_or_default();
            (
                st.get("tier")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                st.get("last_probed")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_string(),
            )
        } else {
            let tier = read_manifest_yaml(&bundles.join(name))
                .ok()
                .and_then(|m| {
                    m.get("tier")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "?".to_string());
            (tier, "-".to_string())
        };
        rows.push(json!({
            "name": name,
            "status": status,
            "tier": tier,
            "last_probed": last_probed,
        }));
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    } else {
        let header = format!(
            "{:<30} {:<12} {:<10} {}",
            "NAME", "STATUS", "TIER", "LAST_PROBED"
        );
        println!("{header}");
        println!("{}", "-".repeat(header.len()));
        for r in &rows {
            println!(
                "{:<30} {:<12} {:<10} {}",
                r["name"].as_str().unwrap_or(""),
                r["status"].as_str().unwrap_or(""),
                r["tier"].as_str().unwrap_or(""),
                r["last_probed"].as_str().unwrap_or(""),
            );
        }
    }
    0
}

/// Port of lib/integration/commands/status.py
/// Shows state for one or all installed bundles.
pub fn status(hex_dir: &Path, name: Option<&str>, json_out: bool) -> i32 {
    if let Some(name) = name {
        let st = match read_state(hex_dir, name) {
            None => {
                eprintln!("[status] '{name}' is not installed");
                return 1;
            }
            Some(v) => v,
        };
        if json_out {
            println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default());
        } else {
            println!("Integration: {name}");
            println!(
                "  tier:       {}",
                st.get("tier").and_then(|v| v.as_str()).unwrap_or("?")
            );
            println!(
                "  installed:  {}",
                st.get("installed_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
            );
            println!(
                "  version:    {}",
                st.get("version").and_then(|v| v.as_str()).unwrap_or("?")
            );
            let policies = st
                .get("compiled_policies")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!("  policies:   {policies}");
            println!(
                "  last probe: {}",
                st.get("last_probed")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
            );
        }
    } else {
        let installed = list_installed(hex_dir);
        if installed.is_empty() {
            println!("[status] No integrations installed");
            return 0;
        }
        let mut rows: Vec<Value> = vec![];
        for n in &installed {
            let st = read_state(hex_dir, n).unwrap_or_default();
            let policies = st
                .get("compiled_policies")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            rows.push(json!({
                "name": n,
                "tier": st.get("tier").and_then(|v| v.as_str()).unwrap_or("?"),
                "installed_at": st.get("installed_at").and_then(|v| v.as_str()).unwrap_or("?"),
                "version": st.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
                "policies": policies,
                "last_probed": st.get("last_probed").and_then(|v| v.as_str()).unwrap_or("-"),
            }));
        }
        if json_out {
            println!(
                "{}",
                serde_json::to_string_pretty(&rows).unwrap_or_default()
            );
        } else {
            let hdr = format!(
                "{:<30} {:<10} {:<22} {:<10} {}",
                "NAME", "TIER", "INSTALLED", "VERSION", "POLICIES"
            );
            println!("{hdr}");
            println!("{}", "-".repeat(hdr.len()));
            for r in &rows {
                println!(
                    "{:<30} {:<10} {:<22} {:<10} {}",
                    r["name"].as_str().unwrap_or(""),
                    r["tier"].as_str().unwrap_or(""),
                    r["installed_at"].as_str().unwrap_or(""),
                    r["version"].as_str().unwrap_or(""),
                    r["policies"].as_u64().unwrap_or(0),
                );
            }
        }
    }
    0
}

/// Port of lib/integration/commands/probe.py
/// Runs the integration check harness for a bundle, updates last_probed state.
pub fn probe(hex_dir: &Path, name: &str, json_out: bool, quiet: bool) -> i32 {
    if !is_installed(hex_dir, name) {
        eprintln!("[probe] ERROR: '{name}' is not installed");
        return 1;
    }

    let harness = harness_script(hex_dir);
    if !harness.exists() {
        eprintln!("[probe] ERROR: harness not found: {}", harness.display());
        return 1;
    }

    if !quiet {
        eprintln!("[probe] Running probe for {name}");
    }

    let result = std::process::Command::new("bash")
        .arg(&harness)
        .arg(name)
        .env("HEX_ROOT", hex_dir)
        .output();

    let (rc, stdout, stderr) = match result {
        Ok(r) => {
            let rc = r.status.code().unwrap_or(1);
            let stdout = String::from_utf8_lossy(&r.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&r.stderr).trim().to_string();
            (rc, stdout, stderr)
        }
        Err(e) => {
            eprintln!("[probe] FAIL: {e}");
            return 1;
        }
    };

    let mut st = read_state(hex_dir, name).unwrap_or_else(|| json!({}));
    st["last_probed"] = json!(now_iso());
    st["last_probe_rc"] = json!(rc);
    write_state_atomic(hex_dir, name, &st);

    let ok = rc <= 1; // 0=pass, 1=degraded are both acceptable per Python impl
    if json_out {
        println!(
            "{}",
            json!({"name": name, "rc": rc, "ok": ok, "stdout": stdout, "stderr": stderr})
        );
    } else {
        let status_str = if rc == 0 {
            "PASS"
        } else if rc == 1 {
            "DEGRADED"
        } else {
            "FAIL"
        };
        println!("[probe] {name}: {status_str} (exit {rc})");
        if !stdout.is_empty() {
            println!("{stdout}");
        }
        if !stderr.is_empty() && !quiet {
            eprintln!("{stderr}");
        }
    }

    if ok {
        0
    } else {
        rc
    }
}

/// Port of lib/integration/commands/rotate.py
/// Runs the bundle's maintenance/rotate.sh script.
pub fn rotate(hex_dir: &Path, name: &str, json_out: bool, quiet: bool) -> i32 {
    if !is_installed(hex_dir, name) {
        eprintln!("[rotate] ERROR: '{name}' is not installed");
        return 1;
    }

    let rotate_script = bundles_dir(hex_dir)
        .join(name)
        .join("maintenance/rotate.sh");
    if !rotate_script.exists() {
        if json_out {
            println!(
                "{}",
                json!({"name": name, "ok": false, "reason": "no_rotation_defined"})
            );
        } else {
            eprintln!("[rotate] no rotation defined for '{name}'");
        }
        return 5;
    }

    if !quiet {
        eprintln!(
            "[rotate] Running rotate for {name}: {}",
            rotate_script.display()
        );
    }

    let result = std::process::Command::new("bash")
        .arg(&rotate_script)
        .env("HEX_ROOT", hex_dir)
        .output();

    let (rc, stdout, stderr) = match result {
        Ok(r) => {
            let rc = r.status.code().unwrap_or(1);
            let stdout = String::from_utf8_lossy(&r.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&r.stderr).trim().to_string();
            (rc, stdout, stderr)
        }
        Err(e) => {
            eprintln!("[rotate] FAIL: {e}");
            return 1;
        }
    };

    let ok = rc == 0;
    if json_out {
        println!(
            "{}",
            json!({"name": name, "rc": rc, "ok": ok, "stdout": stdout, "stderr": stderr})
        );
    } else {
        if ok {
            println!("[rotate] {name}: rotation complete");
        } else {
            eprintln!("[rotate] {name}: rotation failed (exit {rc})");
        }
        if !stdout.is_empty() {
            println!("{stdout}");
        }
        if !stderr.is_empty() && !quiet {
            eprintln!("{stderr}");
        }
    }
    rc
}

/// Port of lib/integration/commands/validate.py
/// Dry-run schema and file check for a bundle.
pub fn validate(hex_dir: &Path, name: &str, json_out: bool, quiet: bool) -> i32 {
    let bundle_dir = bundles_dir(hex_dir).join(name);
    let mut errors: Vec<String> = vec![];

    macro_rules! log {
        ($msg:expr) => {
            if !quiet {
                eprintln!("[validate] {}", $msg);
            }
        };
    }

    if !bundle_dir.is_dir() {
        let msg = format!(
            "bundle '{name}' not found in {}",
            bundles_dir(hex_dir).display()
        );
        errors.push(msg.clone());
        if json_out {
            println!("{}", json!({"name": name, "ok": false, "errors": errors}));
        } else {
            eprintln!("[validate] FAIL: {msg}");
        }
        return 1;
    }

    let manifest = match read_manifest_yaml(&bundle_dir) {
        Ok(m) => m,
        Err(e) => {
            errors.push(e.clone());
            if json_out {
                println!("{}", json!({"name": name, "ok": false, "errors": errors}));
            } else {
                eprintln!("[validate] FAIL: {e}");
            }
            return 1;
        }
    };

    // Check probe script
    let probe_script = manifest
        .get("probe")
        .and_then(|p| p.get("script"))
        .and_then(|v| v.as_str())
        .unwrap_or("probe.sh");
    if !bundle_dir.join(probe_script).exists() {
        errors.push(format!("probe script '{probe_script}' not found"));
    }

    // events/ dir (warn only, non-fatal)
    if !bundle_dir.join("events").is_dir() {
        log!("events/ directory missing (ok for template-style bundles)");
    }

    // Maintenance scripts must exist if listed
    if let Some(maintenance) = manifest.get("maintenance").and_then(|m| m.as_array()) {
        for item in maintenance {
            if let Some(script) = item.get("script").and_then(|v| v.as_str()) {
                if !bundle_dir.join(script).exists() {
                    errors.push(format!("maintenance script '{script}' not found"));
                }
            }
        }
    }

    let ok = errors.is_empty();
    if json_out {
        println!("{}", json!({"name": name, "ok": ok, "errors": errors}));
    } else if ok {
        println!("[validate] OK: {name}");
    } else {
        eprintln!("[validate] FAIL: {name}");
        for err in &errors {
            eprintln!("  - {err}");
        }
    }
    if ok {
        0
    } else {
        1
    }
}

/// Port of lib/integration/commands/update.py
/// Re-compiles policies, refreshes symlink, and rewrites state.
/// Note: policy compilation (compile_mod.compile_policies) is not ported here —
/// the compiled_policies list is preserved from existing state.
pub fn update(
    hex_dir: &Path,
    name: &str,
    json_out: bool,
    dry_run: bool,
    force: bool,
    quiet: bool,
) -> i32 {
    let bundle_dir = bundles_dir(hex_dir).join(name);

    let existing_state = match read_state(hex_dir, name) {
        None => {
            eprintln!(
                "[update] ERROR: '{name}' is not installed — run: hex integration install {name}"
            );
            return 4;
        }
        Some(s) => s,
    };

    if !bundle_dir.is_dir() {
        eprintln!(
            "[update] ERROR: bundle dir not found: {}",
            bundle_dir.display()
        );
        return 1;
    }

    let current_hash = manifest_hash(&bundle_dir);
    let old_hash = existing_state
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !force && current_hash == old_hash {
        if !quiet {
            eprintln!("[update] {name}: no changes detected");
        }
        return 0;
    }

    if dry_run {
        if !quiet {
            // Boundary proof: current_hash is a SHA-256 hex digest
            // (format!("{hash:x}"), all ASCII), so byte offset 8 is a char boundary.
            #[allow(clippy::string_slice)]
            let short = &current_hash[..8.min(current_hash.len())];
            eprintln!("[update] [DRY RUN] Would update {name} (hash {short})");
        }
        return 0;
    }

    let manifest = match read_manifest_yaml(&bundle_dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[update] ERROR: {e}");
            return 1;
        }
    };

    // Refresh probe symlink
    let symlinks_dir = hex_dir.join(".hex/scripts/integrations");
    let symlink_path = symlinks_dir.join(format!("{name}.sh"));
    let expected_target = format!("../../../../integrations/{name}/probe.sh");
    if fs::create_dir_all(&symlinks_dir).is_ok() {
        if symlink_path.exists() || symlink_path.is_symlink() {
            let _ = fs::remove_file(&symlink_path);
        }
        match std::os::unix::fs::symlink(&expected_target, &symlink_path) {
            Ok(_) => {
                if !quiet {
                    eprintln!(
                        "[update] Symlink: {} -> {expected_target}",
                        symlink_path.display()
                    );
                }
            }
            Err(e) => {
                if !quiet {
                    eprintln!("[update] Warning: symlink failed: {e}");
                }
            }
        }
    }

    let tier = manifest
        .get("tier")
        .and_then(|v| v.as_str())
        .or_else(|| existing_state.get("tier").and_then(|v| v.as_str()))
        .unwrap_or("standard")
        .to_string();

    let state_data = json!({
        "name": name,
        "tier": tier,
        "installed_at": existing_state.get("installed_at").cloned().unwrap_or(json!(now_iso())),
        "updated_at": now_iso(),
        "bundle_path": bundle_dir.to_string_lossy(),
        "compiled_policies": existing_state.get("compiled_policies").cloned().unwrap_or(json!([])),
        "version": current_hash,
    });

    if !write_state_atomic(hex_dir, name, &state_data) {
        eprintln!("[update] ERROR: failed to write state");
        return 1;
    }

    if json_out {
        println!(
            "{}",
            json!({"name": name, "ok": true, "version": current_hash})
        );
    } else {
        println!("[update] Updated {name} successfully");
    }
    0
}
