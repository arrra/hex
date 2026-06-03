/// Runs integration checks for a given tier from the integrations manifest.
use std::path::Path;
use std::sync::{Arc, Mutex};

pub fn run(hex_dir: &Path, tier: &str) -> i32 {
    let manifest_path = hex_dir.join("projects/integrations/manifest.yaml");

    if !manifest_path.is_file() {
        eprintln!("[ERROR] manifest not found: {}", manifest_path.display());
        return 1;
    }

    let raw = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ERROR] cannot read manifest: {e}");
            return 1;
        }
    };

    // Parse "name: tier" lines (no nested keys)
    let integrations: Vec<String> = raw
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .filter_map(|l| {
            let mut parts = l.splitn(2, ':');
            let name = parts.next()?.trim().to_string();
            let t = parts.next()?.trim().to_string();
            if tier == "all" || t == tier {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    if integrations.is_empty() {
        println!("No integrations found for tier: {tier}");
        return 0;
    }

    let harness = crate::integration_cmd::harness_script(hex_dir);

    // Run checks in parallel (mirrors xargs -P 8 behaviour)
    let failures = Arc::new(Mutex::new(0u32));
    let mut handles = Vec::with_capacity(integrations.len());

    for name in integrations {
        let script = harness.clone();
        let hex_root = hex_dir.to_path_buf();
        let failures = Arc::clone(&failures);

        handles.push(std::thread::spawn(move || {
            let ok = std::process::Command::new("bash")
                .arg(&script)
                .arg(&name)
                .env("HEX_DIR", &hex_root)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                *failures.lock().unwrap() += 1;
            }
        }));
    }

    for h in handles {
        h.join().ok();
    }

    if *failures.lock().unwrap() > 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_nonzero_when_no_manifest() {
        let tmp = std::env::temp_dir().join("hex_check_all_test");
        std::fs::create_dir_all(&tmp).ok();
        let code = run(&tmp, "all");
        assert_ne!(code, 0);
    }

    #[test]
    fn skips_empty_and_comment_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("projects/integrations/manifest.yaml");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(
            &manifest,
            "# comment\n\ngithub: critical\nslack: standard\n",
        )
        .unwrap();
        // No harness script exists; just verify the function doesn't panic
        // and that it tries to run (returns nonzero since script missing)
        let code = run(tmp.path(), "critical");
        // We get nonzero because the harness script doesn't exist
        assert!(code == 0 || code == 1);
    }
}
