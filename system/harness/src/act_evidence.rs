use crate::types::ActEvidence;
use std::process::Command;

/// Verify a mechanical act's evidence claim against reality.
/// Returns Ok(()) if verified, Err(reason) if the check fails.
pub fn verify(ev: &ActEvidence) -> Result<(), String> {
    match ev {
        ActEvidence::GitTag { value, repo } => {
            let out = Command::new("git")
                .args(["-C", repo, "tag", "--list", value])
                .output()
                .map_err(|e| format!("git tag --list failed to run: {e}"))?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.trim().is_empty() {
                Err(format!("git tag '{value}' not found in repo '{repo}'"))
            } else {
                Ok(())
            }
        }

        ActEvidence::GitPush { repo, git_ref } => {
            let range = format!("origin/{git_ref}..{git_ref}");
            let out = Command::new("git")
                .args(["-C", repo, "rev-list", &range, "--count"])
                .output()
                .map_err(|e| format!("git rev-list failed to run: {e}"))?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let count = stdout.trim();
            if count == "0" {
                Ok(())
            } else if count.is_empty() || !out.status.success() {
                Err(format!(
                    "git rev-list {range} failed in '{repo}': {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            } else {
                Err(format!(
                    "{count} commit(s) unpushed on '{git_ref}' in '{repo}'"
                ))
            }
        }

        ActEvidence::BoiDispatch { spec_id } => {
            let boi_bin = shellexpand::tilde("~/.boi/bin/boi").to_string();
            let out = Command::new(&boi_bin)
                .args(["status", "--all"])
                .output()
                .map_err(|e| format!("boi status --all failed to run: {e}"))?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains(spec_id.as_str()) {
                Ok(())
            } else {
                Err(format!(
                    "spec_id '{spec_id}' not found in `boi status --all` output"
                ))
            }
        }

        ActEvidence::FileWritten { path } => {
            let p = std::path::Path::new(path);
            if !p.exists() {
                return Err(format!("file '{path}' does not exist"));
            }
            let meta = std::fs::metadata(p).map_err(|e| format!("cannot stat '{path}': {e}"))?;
            if meta.len() == 0 {
                Err(format!("file '{path}' exists but is empty (0 bytes)"))
            } else {
                Ok(())
            }
        }
    }
}
