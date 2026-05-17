/// Port of .hex/scripts/integrations/granola-mcp.sh
/// Verifies the Granola MCP integration: credentials file, server binary, and claude.json registration.
use std::path::PathBuf;

const INTEGRATION: &str = "granola-mcp";

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

pub fn run_probe() -> i32 {
    let home = home();

    // Step 1 — Verify supabase.json credentials file exists
    let creds = home.join("Library/Application Support/Granola/supabase.json");
    if !creds.exists() {
        eprintln!(
            "{}: credentials file not found at {}",
            INTEGRATION,
            creds.display()
        );
        return 1;
    }

    // Step 2 — Verify MCP server binary exists
    let server = home.join(".hex/integrations/granola-mcp/dist/index.js");
    if !server.exists() {
        eprintln!(
            "{}: MCP server not found at {}",
            INTEGRATION,
            server.display()
        );
        return 1;
    }

    // Check node is available
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("{}: node not found", INTEGRATION);
        return 1;
    }

    // Step 3 — Verify registered in ~/.claude.json
    let claude_json = home.join(".claude.json");
    match std::fs::read_to_string(&claude_json) {
        Ok(contents) if contents.contains("granola") => {}
        Ok(_) => {
            eprintln!(
                "{}: not registered in {}",
                INTEGRATION,
                claude_json.display()
            );
            return 1;
        }
        Err(_) => {
            eprintln!(
                "{}: not registered in {} (file unreadable)",
                INTEGRATION,
                claude_json.display()
            );
            return 1;
        }
    }

    println!("[{}/probe] OK", INTEGRATION);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_returns_a_path() {
        let h = home();
        assert!(h.to_str().is_some());
    }

    #[test]
    fn constants_match_shell_script() {
        assert_eq!(INTEGRATION, "granola-mcp");
    }
}
