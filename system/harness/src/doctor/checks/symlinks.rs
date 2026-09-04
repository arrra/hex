use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// check_5: .agents/skills is a symlink pointing to .hex/skills.
pub struct AgentsSkillsSymlink;

impl DoctorCheck for AgentsSkillsSymlink {
    fn name(&self) -> &str {
        "agents-skills-symlink"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let agents_skills = ctx.hex_dir.join(".agents/skills");
        let target = ctx.hex_dir.join(".hex/skills");

        if agents_skills.is_symlink() {
            // Check it resolves
            if agents_skills.exists() {
                return CheckResult::pass(".agents/skills symlinked correctly");
            } else {
                if ctx.fix {
                    let _ = fs::remove_file(&agents_skills);
                    if std::os::unix::fs::symlink(&target, &agents_skills).is_ok() {
                        return CheckResult::fixed(".agents/skills symlink repaired");
                    }
                }
                return CheckResult::warn(".agents/skills symlink is broken");
            }
        }

        if agents_skills.exists() {
            return CheckResult::warn(".agents/skills exists but is not a symlink");
        }

        // Not present at all
        if ctx.fix {
            if let Some(parent) = agents_skills.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if std::os::unix::fs::symlink(&target, &agents_skills).is_ok() {
                return CheckResult::fixed(".agents/skills symlink created");
            }
        }
        CheckResult::fail(".agents/skills symlink missing — run bootstrap to fix")
    }
}

/// check_12: No broken symlinks under .hex/ or .agents/.
pub struct NoBrokenSymlinks;

impl DoctorCheck for NoBrokenSymlinks {
    fn name(&self) -> &str {
        "no-broken-symlinks"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let dirs = [ctx.hex_dir.join(".hex"), ctx.hex_dir.join(".agents")];
        let mut broken: Vec<String> = Vec::new();

        for dir in &dirs {
            if !dir.is_dir() {
                continue;
            }
            collect_broken_symlinks(dir, &mut broken);
        }

        if broken.is_empty() {
            return CheckResult::pass("no broken symlinks found");
        }

        let count = broken.len();
        if ctx.fix {
            let mut removed = 0usize;
            for path in &broken {
                if fs::remove_file(path).is_ok() {
                    removed += 1;
                }
            }
            if removed == count {
                return CheckResult::fixed(format!("Removed {} broken symlink(s)", count));
            }
            return CheckResult::warn(format!("Removed {}/{} broken symlink(s)", removed, count));
        }

        CheckResult::fail(format!("{} broken symlink(s) found", count))
            .with_details(broken.join("\n"))
    }
}

fn collect_broken_symlinks(dir: &std::path::Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() && !path.exists() {
            out.push(path.display().to_string());
        } else if path.is_dir() && !path.is_symlink() {
            collect_broken_symlinks(&path, out);
        }
    }
}
