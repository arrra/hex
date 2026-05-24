/// Port of .hex/scripts/boi-pm/install.sh
/// Installs the BOI Process Manager as a macOS LaunchAgent.
use std::path::PathBuf;
use std::process::Command;

const PLIST_NAME: &str = "com.hex.boi-pm.plist";
const PLIST_REL: &str = ".hex/scripts/boi-pm/com.hex.boi-pm.plist";

pub fn run_install(hex_dir: &PathBuf) {
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("ERROR: HOME is not set");
        std::process::exit(1);
    });

    let boi_pm = PathBuf::from(&home).join(".boi/pm");
    std::fs::create_dir_all(&boi_pm).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot create {}: {e}", boi_pm.display());
        std::process::exit(1);
    });

    let plist_src = hex_dir.join(PLIST_REL);
    if !plist_src.exists() {
        eprintln!("ERROR: plist not found at {}", plist_src.display());
        std::process::exit(1);
    }

    let launch_agents = PathBuf::from(&home).join("Library/LaunchAgents");
    std::fs::create_dir_all(&launch_agents).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot create {}: {e}", launch_agents.display());
        std::process::exit(1);
    });

    let plist_dst = launch_agents.join(PLIST_NAME);
    std::fs::copy(&plist_src, &plist_dst).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot copy plist to {}: {e}", plist_dst.display());
        std::process::exit(1);
    });

    let status = Command::new("launchctl")
        .arg("load")
        .arg(&plist_dst)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("ERROR: launchctl load failed: {e}");
            std::process::exit(1);
        });

    if !status.success() {
        eprintln!("ERROR: launchctl load exited with {status}");
        std::process::exit(status.code().unwrap_or(1));
    }

    println!("BOI PM installed and running");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_path_construction() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let expected = hex_dir.join(PLIST_REL);
        assert_eq!(
            expected.to_str().unwrap(),
            "/Users/test/hex/.hex/scripts/boi-pm/com.hex.boi-pm.plist"
        );
    }

    #[test]
    fn plist_dst_construction() {
        let home = "/Users/test";
        let dst = PathBuf::from(home).join("Library/LaunchAgents").join(PLIST_NAME);
        assert_eq!(
            dst.to_str().unwrap(),
            "/Users/test/Library/LaunchAgents/com.hex.boi-pm.plist"
        );
    }
}
