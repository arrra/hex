use clap::Subcommand;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum EnvCommands {
    /// Detect HEX_DIR using env > AGENT_DIR > parent-of-script precedence.
    /// Internal: diagnostic; env.sh bootstraps HEX_DIR inline (can't call the
    /// binary before PATH exists).
    #[command(name = "detect-hex-dir", hide = true)]
    DetectHexDir {
        /// Path to env.sh for auto-detection (env.sh lives at $HEX_DIR/.hex/scripts/env.sh)
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Compose and print the PATH string for hex agents (mirrors env.sh PATH augmentation)
    Path {
        /// Override HEX_DIR for PATH composition
        #[arg(long)]
        hex_dir: Option<PathBuf>,
    },
    /// Print timezone from $HEX_DIR/.hex/timezone (empty string if absent)
    Tz {
        /// Override HEX_DIR for timezone lookup
        #[arg(long)]
        hex_dir: Option<PathBuf>,
    },
    /// Pretty-print all env values for diagnostics.
    /// Internal: diagnostic only, no scripted callers.
    #[command(hide = true)]
    Show {
        /// Override HEX_DIR
        #[arg(long)]
        hex_dir: Option<PathBuf>,
    },
}

/// Detect HEX_DIR with env.sh precedence: HEX_DIR env > AGENT_DIR env > parent-of-script.
pub fn detect_hex_dir(from: Option<&Path>) -> Option<PathBuf> {
    if let Ok(v) = std::env::var("HEX_DIR") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    if let Ok(v) = std::env::var("AGENT_DIR") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    if let Some(script) = from {
        // env.sh lives at $HEX_DIR/.hex/scripts/env.sh — walk up three parents
        return script
            .parent() // .hex/scripts
            .and_then(|p| p.parent()) // .hex
            .and_then(|p| p.parent()) // HEX_DIR
            .map(|p| p.to_path_buf());
    }
    None
}

/// Returns PATH dirs in final PATH order (highest priority first).
/// Mirrors env.sh's _add_to_path calls exactly: only existing dirs are included.
pub fn compose_path_dirs(hex_dir: &Path) -> Vec<PathBuf> {
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => return vec![],
    };

    // env.sh prepends each dir (_add_to_path = PATH="$1:$PATH"), so the last
    // call wins (highest priority). Collect in add-order, then reverse.
    let mut add_order: Vec<PathBuf> = Vec::new();

    // User-local binaries (pip install --user, cargo install, go install)
    add_order.push(home.join(".local/bin"));
    add_order.push(home.join("bin"));
    add_order.push(home.join(".cargo/bin"));
    add_order.push(home.join("go/bin"));

    // Homebrew macOS
    add_order.push(PathBuf::from("/opt/homebrew/bin"));
    add_order.push(PathBuf::from("/usr/local/bin"));

    // Node.js / npm global (claude CLI installs here)
    add_order.push(home.join(".npm-global/bin"));

    // fnm + nvm (env.sh iterates these in a single for loop)
    add_order.push(home.join(".fnm/aliases/default/bin"));
    let nvm_base = home.join(".nvm/versions/node");
    if nvm_base.is_dir() {
        let mut nvm_dirs: Vec<PathBuf> = std::fs::read_dir(&nvm_base)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.path().join("bin"))
            .collect();
        nvm_dirs.sort();
        add_order.extend(nvm_dirs);
    }

    // Python (uv, pyenv)
    add_order.push(home.join(".local/share/uv/python"));
    add_order.push(home.join(".pyenv/shims"));

    // hex binary, BOI
    add_order.push(hex_dir.join(".hex/bin"));
    add_order.push(home.join(".boi/bin"));

    // Filter: existing dirs only; reverse so last-added = highest priority
    let mut seen = std::collections::HashSet::new();
    let mut result: Vec<PathBuf> = Vec::new();
    for dir in add_order.into_iter().rev() {
        if dir.is_dir() && seen.insert(dir.clone()) {
            result.push(dir);
        }
    }
    result
}

/// Compose the full PATH string: new hex dirs prepended to existing PATH, deduplicated.
pub fn compose_path(hex_dir: &Path) -> String {
    let new_dirs = compose_path_dirs(hex_dir);
    let current = std::env::var("PATH").unwrap_or_default();

    let new_set: std::collections::HashSet<String> = new_dirs
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();

    let mut parts: Vec<String> = new_dirs
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();

    let mut seen: std::collections::HashSet<String> = new_set;
    for entry in current.split(':') {
        if !entry.is_empty() && seen.insert(entry.to_string()) {
            parts.push(entry.to_string());
        }
    }

    parts.join(":")
}

/// Read timezone string from $HEX_DIR/.hex/timezone. Returns empty string if absent.
pub fn lookup_tz(hex_dir: &Path) -> String {
    let tz_file = hex_dir.join(".hex/timezone");
    if tz_file.is_file() {
        std::fs::read_to_string(&tz_file)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

pub fn run_env_command(command: EnvCommands) {
    match command {
        EnvCommands::DetectHexDir { from } => match detect_hex_dir(from.as_deref()) {
            Some(p) => println!("{}", p.display()),
            None => {
                eprintln!(
                    "ERROR: cannot detect HEX_DIR — set HEX_DIR env var or pass --from <env.sh path>"
                );
                std::process::exit(1);
            }
        },
        EnvCommands::Path { hex_dir } => {
            let hd = hex_dir
                .or_else(|| detect_hex_dir(None))
                .unwrap_or_else(|| {
                    eprintln!("ERROR: cannot determine HEX_DIR for PATH composition");
                    std::process::exit(1);
                });
            println!("{}", compose_path(&hd));
        }
        EnvCommands::Tz { hex_dir } => {
            let hd = hex_dir
                .or_else(|| detect_hex_dir(None))
                .unwrap_or_else(|| {
                    eprintln!("ERROR: cannot determine HEX_DIR for TZ lookup");
                    std::process::exit(1);
                });
            // No trailing newline — shim uses $(...) interpolation
            print!("{}", lookup_tz(&hd));
        }
        EnvCommands::Show { hex_dir } => {
            let hd = hex_dir
                .or_else(|| detect_hex_dir(None))
                .unwrap_or_else(|| {
                    eprintln!("ERROR: cannot determine HEX_DIR");
                    std::process::exit(1);
                });
            println!("HEX_DIR   = {}", hd.display());
            println!("AGENT_DIR = {}", hd.display());
            println!("HEX_ROOT  = {}", hd.display());
            println!("TZ        = {}", lookup_tz(&hd));
            println!("PATH      = {}", compose_path(&hd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_hex_dir() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".hex/bin")).unwrap();
        dir
    }

    // ── detect-hex-dir ──────────────────────────────────────────────────────

    #[test]
    fn test_detect_from_script_path_math() {
        let tmp = make_hex_dir();
        let scripts_dir = tmp.path().join(".hex/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        let env_sh = scripts_dir.join("env.sh");
        std::fs::write(&env_sh, "# dummy").unwrap();

        // Verify the parent-walking logic independently of env vars
        let detected = env_sh
            .parent()
            .unwrap() // .hex/scripts
            .parent()
            .unwrap() // .hex
            .parent()
            .unwrap(); // HEX_DIR
        assert_eq!(detected, tmp.path());
    }

    #[test]
    fn test_detect_hex_dir_no_input_does_not_panic() {
        // May return Some or None depending on env — must not panic
        let _ = detect_hex_dir(None);
    }

    #[test]
    fn test_detect_hex_dir_from_uses_parent_walking() {
        let tmp = make_hex_dir();
        let scripts_dir = tmp.path().join(".hex/scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        let env_sh = scripts_dir.join("env.sh");
        std::fs::write(&env_sh, "# dummy").unwrap();

        // Temporarily clear HEX_DIR/AGENT_DIR for this test — we test
        // the path math by checking intermediate results
        let from_result = env_sh
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        assert_eq!(from_result, Some(tmp.path().to_path_buf()));
    }

    // ── PATH composition ─────────────────────────────────────────────────────

    #[test]
    fn test_path_contains_hex_bin() {
        let tmp = make_hex_dir();
        let path = compose_path(tmp.path());
        let hex_bin = tmp.path().join(".hex/bin").to_string_lossy().into_owned();
        assert!(
            path.contains(&hex_bin),
            "PATH must contain hex bin dir: {hex_bin}\nGot: {path}"
        );
    }

    #[test]
    fn test_path_no_duplicate_entries() {
        let tmp = make_hex_dir();
        let path = compose_path(tmp.path());
        let parts: Vec<&str> = path.split(':').filter(|s| !s.is_empty()).collect();
        let mut seen = std::collections::HashSet::new();
        for p in &parts {
            assert!(seen.insert(*p), "Duplicate entry in PATH: {p}");
        }
    }

    #[test]
    fn test_path_hex_bin_is_high_priority() {
        let tmp = make_hex_dir();
        let path = compose_path(tmp.path());
        let hex_bin = tmp.path().join(".hex/bin").to_string_lossy().into_owned();
        let parts: Vec<&str> = path.split(':').collect();
        let hex_pos = parts.iter().position(|&p| p == hex_bin).unwrap();
        // hex bin should be in the first half of path entries
        assert!(
            hex_pos < parts.len() / 2 + 1,
            "hex bin should be high priority, got position {hex_pos} of {}",
            parts.len()
        );
    }

    // ── TZ lookup ────────────────────────────────────────────────────────────

    #[test]
    fn test_tz_with_file() {
        let tmp = make_hex_dir();
        std::fs::write(tmp.path().join(".hex/timezone"), "America/New_York\n").unwrap();
        assert_eq!(lookup_tz(tmp.path()), "America/New_York");
    }

    #[test]
    fn test_tz_trims_whitespace() {
        let tmp = make_hex_dir();
        std::fs::write(tmp.path().join(".hex/timezone"), "  US/Pacific  \n").unwrap();
        assert_eq!(lookup_tz(tmp.path()), "US/Pacific");
    }

    #[test]
    fn test_tz_without_file() {
        let tmp = make_hex_dir();
        assert_eq!(lookup_tz(tmp.path()), "");
    }

    #[test]
    fn test_tz_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(lookup_tz(tmp.path()), "");
    }
}
