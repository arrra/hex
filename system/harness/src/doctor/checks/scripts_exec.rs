use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;
use std::os::unix::fs::PermissionsExt;

/// check_13: All .sh files under .hex/scripts/ are executable.
pub struct ScriptsExecutable;

impl DoctorCheck for ScriptsExecutable {
    fn name(&self) -> &str {
        "scripts-executable"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let scripts_dir = ctx.hex_dir.join(".hex/scripts");
        if !scripts_dir.is_dir() {
            return CheckResult::skip(".hex/scripts/ does not exist");
        }

        let mut non_exec: Vec<String> = Vec::new();
        collect_non_exec(&scripts_dir, &mut non_exec);

        if non_exec.is_empty() {
            return CheckResult::pass(".hex/scripts/ all .sh files are executable");
        }

        if ctx.fix {
            let mut fixed = 0usize;
            for path_str in &non_exec {
                let path = std::path::Path::new(path_str);
                if let Ok(meta) = fs::metadata(path) {
                    let mut perms = meta.permissions();
                    perms.set_mode(perms.mode() | 0o111);
                    if fs::set_permissions(path, perms).is_ok() {
                        fixed += 1;
                    }
                }
            }
            if fixed == non_exec.len() {
                return CheckResult::fixed(format!("Made {} script(s) executable", fixed));
            }
        }

        CheckResult::warn(format!("{} non-executable .sh script(s)", non_exec.len()))
            .with_details(non_exec.join("\n"))
    }
}

fn collect_non_exec(dir: &std::path::Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            collect_non_exec(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("sh") {
            if let Ok(meta) = fs::metadata(&path) {
                let mode = meta.permissions().mode();
                if mode & 0o111 == 0 {
                    out.push(path.display().to_string());
                }
            }
        }
    }
}
