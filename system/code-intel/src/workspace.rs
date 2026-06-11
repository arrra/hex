//! Workspace identity, registry, worktree resolution (SPEC-A1 §3, plan Task 2).
//!
//! - `workspace-id` = first 12 hex chars of sha256(canonicalized primary
//!   checkout root). The primary root is derived from
//!   `git rev-parse --git-common-dir` (strip the trailing `.git`, which folds
//!   `worktrees/<name>` back to the parent repo).
//! - Worktrees resolve to the parent workspace id but keep their own
//!   `query_root` (the toplevel of the worktree the query runs from).
//! - `Registry` is TOML at `<codeintel_home>/registry.toml`;
//!   `codeintel_home()` honors `$CODEINTEL_HOME` (hermetic tests), defaulting
//!   to `~/.codeintel`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::CqError;

/// A resolved workspace: stable identity plus the root queries run against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// First 12 hex chars of sha256 of the canonicalized primary root.
    pub id: String,
    /// Canonicalized primary checkout root (never a worktree).
    pub primary_root: PathBuf,
    /// Canonicalized toplevel of the worktree `resolve` was called from.
    /// Equals `primary_root` when not in a linked worktree.
    pub query_root: PathBuf,
}

impl Workspace {
    /// Resolve the workspace containing `dir`.
    ///
    /// Runs `git -C dir rev-parse --git-common-dir --show-toplevel`. A linked
    /// worktree's common dir points at the parent repo's `.git`, so worktrees
    /// share the parent's workspace id while keeping their own `query_root`.
    /// Non-git directories are `UnregisteredWorkspace` (spec §5, exit 4).
    pub fn resolve(dir: &Path) -> Result<Workspace, CqError> {
        let unregistered = || CqError::UnregisteredWorkspace {
            cwd: dir.display().to_string(),
        };
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--git-common-dir", "--show-toplevel"])
            .output()
            .map_err(|_| unregistered())?;
        if !out.status.success() {
            return Err(unregistered());
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut lines = stdout.lines();
        let (common_dir, toplevel) = match (lines.next(), lines.next()) {
            (Some(c), Some(t)) => (c.trim(), t.trim()),
            // Inside a .git dir itself there is no toplevel; not a workspace.
            _ => return Err(unregistered()),
        };

        // git emits common-dir relative to `dir` when it is nearby (e.g. ".git").
        let common_dir = if Path::new(common_dir).is_absolute() {
            PathBuf::from(common_dir)
        } else {
            dir.join(common_dir)
        };
        let common_dir = common_dir.canonicalize().map_err(|_| unregistered())?;

        // Primary root = parent of the common `.git` dir. For a linked
        // worktree the common dir is the PARENT repo's `.git`, which is
        // exactly the fold-back the spec requires.
        let primary_root = match common_dir.file_name() {
            Some(name) if name == ".git" => common_dir
                .parent()
                .ok_or_else(unregistered)?
                .to_path_buf(),
            // Bare or exotic layouts (no work tree to index) are unsupported.
            _ => {
                return Err(CqError::UnsupportedWorkspace {
                    reason: format!(
                        "git common dir {} is not a standard .git directory",
                        common_dir.display()
                    ),
                })
            }
        };
        let query_root = PathBuf::from(toplevel)
            .canonicalize()
            .map_err(|_| unregistered())?;

        Ok(Workspace {
            id: workspace_id(&primary_root),
            primary_root,
            query_root,
        })
    }
}

/// First 12 hex chars of sha256 of the canonicalized primary root path.
fn workspace_id(primary_root: &Path) -> String {
    let digest = Sha256::digest(primary_root.to_string_lossy().as_bytes());
    let mut id = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// Resolve the codeintel home: `$CODEINTEL_HOME` override (hermetic tests),
/// else `~/.codeintel`.
pub fn codeintel_home() -> Result<PathBuf, CqError> {
    if let Some(home) = std::env::var_os("CODEINTEL_HOME") {
        return Ok(PathBuf::from(home));
    }
    let user_home = std::env::var_os("HOME").ok_or_else(|| CqError::UnsupportedWorkspace {
        reason: "neither $CODEINTEL_HOME nor $HOME is set".into(),
    })?;
    Ok(PathBuf::from(user_home).join(".codeintel"))
}

/// One registered workspace entry in `registry.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryEntry {
    pub id: String,
    pub root: PathBuf,
    pub registered_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default, rename = "workspace")]
    workspaces: Vec<RegistryEntry>,
}

/// Registered workspaces, persisted as TOML at `<home>/registry.toml`.
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<RegistryEntry>,
}

impl Registry {
    fn path(home: &Path) -> PathBuf {
        home.join("registry.toml")
    }

    /// Load the registry from `<home>/registry.toml`. A missing file is an
    /// empty registry; a malformed file is a loud error (Standing Order S6).
    pub fn load(home: &Path) -> Result<Registry, CqError> {
        let path = Self::path(home);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Registry::default())
            }
            Err(e) => {
                return Err(CqError::UnsupportedWorkspace {
                    reason: format!("cannot read registry {}: {e}", path.display()),
                })
            }
        };
        let file: RegistryFile =
            toml::from_str(&raw).map_err(|e| CqError::UnsupportedWorkspace {
                reason: format!("malformed registry {}: {e}", path.display()),
            })?;
        Ok(Registry {
            entries: file.workspaces,
        })
    }

    /// Persist to `<home>/registry.toml` (atomic: write `.tmp`, rename).
    pub fn save(&self, home: &Path) -> Result<(), CqError> {
        let path = Self::path(home);
        let io_err = |e: std::io::Error| CqError::UnsupportedWorkspace {
            reason: format!("cannot write registry {}: {e}", path.display()),
        };
        std::fs::create_dir_all(home).map_err(io_err)?;
        let file = RegistryFile {
            workspaces: self.entries.clone(),
        };
        let body = toml::to_string_pretty(&file).map_err(|e| CqError::UnsupportedWorkspace {
            reason: format!("cannot serialize registry: {e}"),
        })?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).map_err(io_err)?;
        std::fs::rename(&tmp, &path).map_err(io_err)?;
        Ok(())
    }

    /// Resolve `path` and add its workspace; idempotent on re-register
    /// (refreshes `root`/`registered_at`, keeps one entry per id).
    pub fn register(&mut self, path: &Path) -> Result<RegistryEntry, CqError> {
        let ws = Workspace::resolve(path)?;
        let entry = RegistryEntry {
            id: ws.id.clone(),
            root: ws.primary_root.clone(),
            registered_at: chrono::Utc::now().to_rfc3339(),
        };
        self.entries.retain(|e| e.id != entry.id);
        self.entries.push(entry.clone());
        Ok(entry)
    }

    /// Whether a workspace id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    /// All registered workspaces.
    pub fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }
}

/// `cq register <PATH>` (spec §5): resolve, require `Cargo.toml` at the
/// PRIMARY root (A1 is Rust-only — else `UnsupportedWorkspace`), persist to
/// the registry, return the entry for the CLI to print.
pub fn register_workspace(home: &Path, path: &Path) -> Result<RegistryEntry, CqError> {
    let ws = Workspace::resolve(path)?;
    if !ws.primary_root.join("Cargo.toml").is_file() {
        return Err(CqError::UnsupportedWorkspace {
            reason: format!("no Cargo.toml at primary root {}", ws.primary_root.display()),
        });
    }
    let mut reg = Registry::load(home)?;
    let entry = reg.register(path)?;
    reg.save(home)?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Fresh repo with one commit (and a Cargo.toml so register gates pass).
    fn mkrepo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@test"]);
        run_git(dir.path(), &["config", "user.name", "test"]);
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-m", "init"]);
        dir
    }

    #[test]
    fn workspace_id_is_stable_sha_prefix() {
        let repo = mkrepo();
        let a = Workspace::resolve(repo.path()).unwrap();
        assert_eq!(a.id.len(), 12);
        assert!(a.id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.id, Workspace::resolve(repo.path()).unwrap().id);
    }

    #[test]
    fn primary_resolve_query_root_equals_primary_root() {
        let repo = mkrepo();
        let ws = Workspace::resolve(repo.path()).unwrap();
        assert_eq!(ws.primary_root, repo.path().canonicalize().unwrap());
        assert_eq!(ws.query_root, ws.primary_root);
    }

    #[test]
    fn resolve_from_subdirectory_finds_same_workspace() {
        let repo = mkrepo();
        let sub = repo.path().join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        let a = Workspace::resolve(repo.path()).unwrap();
        let b = Workspace::resolve(&sub).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.query_root, b.query_root);
    }

    #[test]
    fn worktree_resolves_to_parent_workspace() {
        let repo = mkrepo();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt1");
        run_git(repo.path(), &["worktree", "add", wt.to_str().unwrap()]);
        let a = Workspace::resolve(repo.path()).unwrap();
        let b = Workspace::resolve(&wt).unwrap();
        assert_eq!(a.id, b.id); // same workspace identity
        assert_eq!(b.primary_root, a.primary_root);
        assert_eq!(b.query_root, wt.canonicalize().unwrap()); // queries run against the worktree
        assert_ne!(b.query_root, b.primary_root);
    }

    #[test]
    fn non_git_dir_is_unregistered_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            Workspace::resolve(d.path()),
            Err(CqError::UnregisteredWorkspace { .. })
        ));
    }

    #[test]
    fn registry_roundtrip_and_membership() {
        let home = tempfile::tempdir().unwrap();
        let repo = mkrepo();
        let mut reg = Registry::load(home.path()).unwrap(); // empty ok
        assert!(reg.entries().is_empty());
        reg.register(repo.path()).unwrap();
        reg.save(home.path()).unwrap();
        let reg2 = Registry::load(home.path()).unwrap();
        assert!(reg2.contains(&Workspace::resolve(repo.path()).unwrap().id));
    }

    #[test]
    fn register_is_idempotent_one_entry_per_id() {
        let home = tempfile::tempdir().unwrap();
        let repo = mkrepo();
        let mut reg = Registry::load(home.path()).unwrap();
        reg.register(repo.path()).unwrap();
        reg.register(repo.path()).unwrap();
        assert_eq!(reg.entries().len(), 1);
    }

    #[test]
    fn malformed_registry_is_loud_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("registry.toml"), "not [ valid toml").unwrap();
        assert!(Registry::load(home.path()).is_err());
    }

    #[test]
    fn register_workspace_requires_cargo_toml_at_primary_root() {
        let home = tempfile::tempdir().unwrap();
        // git repo WITHOUT Cargo.toml → UnsupportedWorkspace
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "-b", "main"]);
        let err = register_workspace(home.path(), repo.path()).unwrap_err();
        assert!(matches!(err, CqError::UnsupportedWorkspace { .. }));
        assert!(!Registry::load(home.path())
            .unwrap()
            .contains(&Workspace::resolve(repo.path()).unwrap().id));
    }

    #[test]
    fn register_workspace_from_worktree_registers_primary_root() {
        let home = tempfile::tempdir().unwrap();
        let repo = mkrepo();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt2");
        run_git(repo.path(), &["worktree", "add", wt.to_str().unwrap()]);
        let entry = register_workspace(home.path(), &wt).unwrap();
        assert_eq!(entry.root, repo.path().canonicalize().unwrap());
        assert_eq!(entry.id, Workspace::resolve(repo.path()).unwrap().id);
    }

    #[test]
    fn codeintel_home_default_under_home() {
        // Only assert the default shape; $CODEINTEL_HOME override is exercised
        // end-to-end by CLI tests (env mutation in-process is not thread-safe).
        if std::env::var_os("CODEINTEL_HOME").is_none() {
            let home = codeintel_home().unwrap();
            assert!(home.ends_with(".codeintel"));
        }
    }
}
