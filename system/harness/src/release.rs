use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct ReleaseArgs {
    pub version: Option<String>,
    pub skip_e2e: bool,
    pub dry_run: bool,
}

fn read_cargo_version(hex_dir: &Path) -> Result<String, String> {
    let cargo_toml = hex_dir.join("system/harness/Cargo.toml");
    let text = std::fs::read_to_string(&cargo_toml)
        .map_err(|e| format!("cannot read {}: {e}", cargo_toml.display()))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("version = ") {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Ok(v.to_string());
            }
        }
    }
    Err(format!("cannot find version in {}", cargo_toml.display()))
}

fn run_script(hex_dir: &Path, args: &[&str], env_vars: &[(&str, &str)]) -> i32 {
    let script = hex_dir.join("system/scripts/release.sh");
    if !script.exists() {
        eprintln!("[hex release] FAILED: script not found: {}", script.display());
        return 1;
    }
    let mut cmd = Command::new("bash");
    cmd.arg(&script);
    for a in args {
        cmd.arg(a);
    }
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("[hex release] FAILED: could not exec release.sh: {e}");
            1
        }
    }
}

fn tag_on_origin(hex_dir: &Path, version: &str) -> bool {
    let output = Command::new("git")
        .args(["ls-remote", "--tags", "origin", &format!("v{version}")])
        .current_dir(hex_dir)
        .output();
    match output {
        Ok(o) => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        Err(_) => false,
    }
}

fn push_tag(hex_dir: &Path, version: &str) -> bool {
    println!("[hex release] Pushing tag v{version} to origin...");
    let status = Command::new("git")
        .args(["push", "origin", &format!("v{version}")])
        .current_dir(hex_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("[hex release] FAILED: git push tag: {e}");
            false
        }
    }
}

fn gh_release_exists(hex_dir: &Path, version: &str) -> bool {
    let output = Command::new("gh")
        .args(["release", "view", &format!("v{version}"), "--repo", "mrap/hex-foundation"])
        .current_dir(hex_dir)
        .output();
    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

fn create_gh_release(hex_dir: &Path, version: &str) -> bool {
    println!("[hex release] Creating GitHub release v{version}...");
    let status = Command::new("gh")
        .args([
            "release", "create",
            &format!("v{version}"),
            "--repo", "mrap/hex-foundation",
            "--title", &format!("v{version}"),
            "--generate-notes",
        ])
        .current_dir(hex_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("[hex release] FAILED: gh release create: {e}");
            false
        }
    }
}

pub fn run(hex_dir: &Path, args: ReleaseArgs) -> i32 {
    let release_sh_dir = hex_dir.join("system/scripts/release.sh");
    if !release_sh_dir.exists() {
        eprintln!("[hex release] FAILED: release.sh not found at {}", release_sh_dir.display());
        return 1;
    }

    // Determine target version
    let current_version = match read_cargo_version(hex_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[hex release] FAILED: {e}");
            return 1;
        }
    };

    let target_version = match &args.version {
        Some(v) => v.clone(),
        None => current_version.clone(),
    };

    // Run bump-version if the requested version differs from Cargo.toml
    if let Some(ref v) = args.version {
        if *v != current_version {
            println!("[hex release] Bumping version {} → {v}...", current_version);
            let bump_code = run_script(
                hex_dir,
                &["bump-version", v],
                &[("HEX_RELEASE_PIPELINE", "1")],
            );
            if bump_code != 0 {
                eprintln!("[hex release] FAILED: bump-version exited {bump_code}");
                return bump_code;
            }
        }
    }

    // Build release.sh args
    let mut sh_args: Vec<&str> = Vec::new();
    if args.dry_run {
        sh_args.push("--dry-run");
    }
    if args.skip_e2e {
        sh_args.push("--skip-e2e");
    }

    println!("[hex release] Running release pipeline (version {target_version})...");
    let script_code = run_script(
        hex_dir,
        &sh_args,
        &[("HEX_RELEASE_PIPELINE", "1")],
    );

    if script_code != 0 {
        eprintln!("[hex release] FAILED: release.sh exited {script_code}");
        return script_code;
    }

    // Dry-run: gates passed, nothing pushed — done
    if args.dry_run {
        println!("[hex release] Dry run complete. No commits, tags, or pushes performed.");
        return 0;
    }

    // Verify/push tag to origin
    if !tag_on_origin(hex_dir, &target_version) {
        if !push_tag(hex_dir, &target_version) {
            eprintln!("[hex release] FAILED: could not push tag v{target_version} to origin");
            return 1;
        }
    } else {
        println!("[hex release] Tag v{target_version} already on origin ✓");
    }

    // Create GitHub release (idempotent)
    if gh_release_exists(hex_dir, &target_version) {
        println!("[hex release] GitHub release v{target_version} already exists ✓");
    } else if !create_gh_release(hex_dir, &target_version) {
        eprintln!("[hex release] FAILED: could not create GitHub release v{target_version}");
        return 1;
    }

    // Final ground-truth verification
    if !tag_on_origin(hex_dir, &target_version) {
        eprintln!("[hex release] FAILED: tag v{target_version} not found on origin after push");
        return 1;
    }

    println!("[hex release] Release v{target_version} complete ✓");
    0
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_cargo_version_finds_version() {
        // Build a minimal fake repo root in a temp dir so the test is
        // path-independent (env!("CARGO_MANIFEST_DIR") is unreliable in
        // cargo test --release due to temp-dir compilation paths).
        let tmp = std::env::temp_dir().join(format!("hex-release-test-{}", std::process::id()));
        let harness_dir = tmp.join("system/harness");
        fs::create_dir_all(&harness_dir).unwrap();
        let cargo_toml = harness_dir.join("Cargo.toml");
        fs::write(&cargo_toml, "[package]\nversion = \"1.2.3\"\n").unwrap();

        let v = read_cargo_version(&tmp);
        assert!(v.is_ok(), "should find version: {:?}", v);
        let ver = v.unwrap();
        assert_eq!(ver, "1.2.3");

        fs::remove_dir_all(&tmp).ok();
    }
}
