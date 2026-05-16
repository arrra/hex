pub mod budget_reset;

/// Port of .hex/scripts/health/check-agent-memory.sh
///
/// Health check for the agent memory system. Verifies:
///   1. Shared memory directory exists and is readable
///   2. SHARED.md index is present and readable
///   3. Claude projects directory is accessible
///   4. Reports counts of shared memory files and project memory dirs
use std::path::PathBuf;

pub fn check_agent_memory() {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("FAIL: HOME environment variable not set");
            std::process::exit(1);
        }
    };

    let shared_mem = PathBuf::from(&home).join(".claude/shared-memory");
    let projects_dir = PathBuf::from(&home).join(".claude/projects");

    // 1. Shared memory directory must exist
    if !shared_mem.is_dir() {
        eprintln!("FAIL: shared memory directory missing: {}", shared_mem.display());
        std::process::exit(1);
    }

    // 2. SHARED.md must be readable
    let shared_index = shared_mem.join("SHARED.md");
    if !shared_index.is_file() {
        eprintln!("FAIL: SHARED.md missing: {}", shared_index.display());
        std::process::exit(1);
    }
    let shared_content = match std::fs::read_to_string(&shared_index) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: cannot read SHARED.md: {e}");
            std::process::exit(1);
        }
    };

    // 3. Projects directory must exist and be accessible
    if !projects_dir.is_dir() {
        eprintln!("FAIL: Claude projects directory missing: {}", projects_dir.display());
        std::process::exit(1);
    }

    // 4. Count accessible memory directories (non-fatal if zero)
    let mem_dirs = count_memory_dirs(&projects_dir);

    // 5. Count shared memory files
    let shared_files = match std::fs::read_dir(&shared_mem) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".md")
            })
            .count(),
        Err(e) => {
            eprintln!("FAIL: cannot list shared memory directory: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "agent-memory: ok (SHARED.md={}B, {} shared files, {} project memory dirs)",
        shared_content.len(),
        shared_files,
        mem_dirs,
    );
}

fn count_memory_dirs(projects_dir: &std::path::Path) -> usize {
    let entries = match std::fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    entries
        .flatten()
        .filter(|e| {
            let mem = e.path().join("memory");
            mem.is_dir()
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_memory_dirs_nonexistent_returns_zero() {
        let count = count_memory_dirs(std::path::Path::new("/nonexistent/path/projects"));
        assert_eq!(count, 0, "nonexistent dir must return 0");
    }
}
