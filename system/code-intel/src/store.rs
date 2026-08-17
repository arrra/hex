//! Generation store — atomic publish, prune, exclusive lock (SPEC-A1 §3, §7).
//!
//! Layout under `<home>/<workspace-id>/`:
//! - `<YYYYMMDDTHHMMSSZ-xxxxxx>/`     immutable published generations
//! - `<name>.tmp/`                    in-flight generation being built
//! - `CURRENT`                        file containing the current generation name
//! - `index.lock`                     flock target for `cq index` exclusivity
//!
//! Publish protocol (spec §3): write into `<name>.tmp/`, fsync, rename to
//! `<name>/`, then atomically rewrite `CURRENT` (write `CURRENT.tmp`, rename).
//! Generations are immutable after publish. Prune keeps the 2 most recent
//! generations and never deletes the one `CURRENT` names.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fs2::FileExt;

/// How many published generations `prune` keeps (spec §3).
const KEEP_GENERATIONS: usize = 2;

/// Handle to one workspace's generation store under `<home>/<workspace-id>/`.
#[derive(Debug, Clone)]
pub struct Store {
    ws_dir: PathBuf,
}

/// An in-flight generation: a `<name>.tmp/` directory being filled before
/// publish. Consumed by [`Store::publish`].
#[derive(Debug)]
pub struct Generation {
    name: String,
    tmp_dir: PathBuf,
}

impl Generation {
    /// The `<name>.tmp/` directory to write `index.sqlite`, `manifest.json`,
    /// emit logs, etc. into.
    pub fn dir(&self) -> &Path {
        &self.tmp_dir
    }

    /// The final generation name this will publish as.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Exclusive advisory lock on `<ws>/index.lock`. Released on drop.
#[derive(Debug)]
pub struct LockGuard {
    file: File,
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Closing the fd releases the flock anyway; report unlock failure
        // loudly rather than swallowing it (Standing Order S6).
        if let Err(e) = fs2::FileExt::unlock(&self.file) {
            eprintln!("warning: failed to unlock {}: {e}", self.path.display());
        }
    }
}

impl Store {
    /// A store rooted at `<home>/<workspace_id>/`. Does not touch the
    /// filesystem until a write operation needs to.
    pub fn new(home: impl AsRef<Path>, workspace_id: &str) -> Self {
        Store {
            ws_dir: home.as_ref().join(workspace_id),
        }
    }

    /// The workspace directory `<home>/<workspace-id>/`.
    pub fn workspace_dir(&self) -> &Path {
        &self.ws_dir
    }

    /// Create a fresh `<name>.tmp/` directory for an in-flight generation.
    pub fn begin_generation(&self) -> Result<Generation> {
        let name = new_generation_name();
        let tmp_dir = self.ws_dir.join(format!("{name}.tmp"));
        fs::create_dir_all(&tmp_dir)
            .with_context(|| format!("creating generation tmp dir {}", tmp_dir.display()))?;
        Ok(Generation { name, tmp_dir })
    }

    /// Atomically publish an in-flight generation: fsync the tmp dir, rename
    /// `<name>.tmp/` → `<name>/`, then rewrite `CURRENT` via `CURRENT.tmp` +
    /// rename. Returns the published generation name.
    pub fn publish(&self, generation: Generation) -> Result<String> {
        let Generation { name, tmp_dir } = generation;
        if !tmp_dir.is_dir() {
            bail!(
                "generation tmp dir vanished before publish: {}",
                tmp_dir.display()
            );
        }
        let final_dir = self.ws_dir.join(&name);
        if final_dir.exists() {
            bail!(
                "generation {} already published at {}",
                name,
                final_dir.display()
            );
        }

        fsync_dir(&tmp_dir)?;
        fs::rename(&tmp_dir, &final_dir).with_context(|| {
            format!(
                "publishing {} -> {}",
                tmp_dir.display(),
                final_dir.display()
            )
        })?;
        fsync_dir(&self.ws_dir)?;

        self.write_current(&name)?;
        Ok(name)
    }

    /// The current generation name, or `None` when no generation has ever
    /// been published. Never panics on a missing/empty store.
    pub fn current(&self) -> Result<Option<String>> {
        let path = self.ws_dir.join("CURRENT");
        match fs::read_to_string(&path) {
            Ok(s) => {
                let name = s.trim().to_string();
                if name.is_empty() {
                    bail!("CURRENT file at {} is empty", path.display());
                }
                Ok(Some(name))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Directory of the current generation. Errors when there is no current
    /// generation or its directory is missing (callers map this to NO_INDEX).
    pub fn current_dir(&self) -> Result<PathBuf> {
        let name = self
            .current()?
            .with_context(|| format!("no published generation under {}", self.ws_dir.display()))?;
        let dir = self.ws_dir.join(&name);
        if !dir.is_dir() {
            bail!(
                "CURRENT names {} but {} does not exist",
                name,
                dir.display()
            );
        }
        Ok(dir)
    }

    /// All published (non-`.tmp`) generation names, newest first
    /// (names sort chronologically).
    pub fn generations(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let entries = match fs::read_dir(&self.ws_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(names),
            Err(e) => return Err(e).with_context(|| format!("listing {}", self.ws_dir.display())),
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("listing {}", self.ws_dir.display()))?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if is_generation_name(name) && entry.path().is_dir() {
                names.push(name.to_string());
            }
        }
        names.sort_unstable_by(|a, b| b.cmp(a));
        Ok(names)
    }

    /// Delete all but the [`KEEP_GENERATIONS`] newest published generations.
    /// The generation named by `CURRENT` is never deleted, regardless of age.
    /// Returns the names that were removed.
    pub fn prune(&self) -> Result<Vec<String>> {
        let current = self.current()?;
        let mut generations = self.generations()?; // newest first
                                                   // Same-second publishes share a timestamp prefix and tie-break by
                                                   // random suffix, which can rank CURRENT below older generations and
                                                   // make prune keep 3. Within a timestamp tie, CURRENT is by
                                                   // definition the newest; distinct timestamps keep pure name order.
        let is_current = |n: &str| Some(n) == current.as_deref();
        let ts = |n: &str| n.split('-').next().unwrap_or(n).to_string();
        generations.sort_by(|a, b| {
            ts(b)
                .cmp(&ts(a))
                .then_with(|| is_current(b).cmp(&is_current(a)))
                .then_with(|| b.cmp(a))
        });
        let mut removed = Vec::new();
        for name in generations.iter().skip(KEEP_GENERATIONS) {
            if Some(name.as_str()) == current.as_deref() {
                continue;
            }
            let dir = self.ws_dir.join(name);
            fs::remove_dir_all(&dir)
                .with_context(|| format!("pruning generation {}", dir.display()))?;
            removed.push(name.clone());
        }
        Ok(removed)
    }

    /// Try to take the exclusive indexer lock (`<ws>/index.lock`). Returns
    /// `None` when another holder has it (the caller reports
    /// `{"skipped":"emit-in-flight"}` — visible, not silent; spec §7).
    pub fn try_lock(&self) -> Result<Option<LockGuard>> {
        fs::create_dir_all(&self.ws_dir)
            .with_context(|| format!("creating {}", self.ws_dir.display()))?;
        let path = self.ws_dir.join("index.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening lock file {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(LockGuard { file, path })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e).with_context(|| format!("locking {}", path.display())),
        }
    }

    /// Atomic `CURRENT` rewrite: write `CURRENT.tmp`, fsync, rename.
    fn write_current(&self, name: &str) -> Result<()> {
        let tmp = self.ws_dir.join("CURRENT.tmp");
        let dst = self.ws_dir.join("CURRENT");
        let mut f = File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(name.as_bytes())
            .and_then(|()| f.write_all(b"\n"))
            .and_then(|()| f.sync_all())
            .with_context(|| format!("writing {}", tmp.display()))?;
        drop(f);
        fs::rename(&tmp, &dst)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), dst.display()))?;
        fsync_dir(&self.ws_dir)
    }
}

/// `YYYYMMDDTHHMMSSZ-<6 random hex>` — lexicographic order == chronological.
fn new_generation_name() -> String {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("{ts}-{}", random_hex6())
}

/// 6 hex chars of entropy without a rand dependency: sha256 over pid,
/// monotonic nanos, and a process-local counter.
fn random_hex6() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(nanos.to_le_bytes());
    hasher.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let digest = hasher.finalize();
    digest[..3].iter().map(|b| format!("{b:02x}")).collect()
}

/// `YYYYMMDDTHHMMSSZ-xxxxxx`, published (no `.tmp` suffix).
// string_slice allow: `ts[..8]`/`ts[9..15]` are reached only after the `&&`
// guards below match ASCII 'T' at byte 8 and 'Z' at byte 15, so bytes 8/9/15
// are proven char boundaries — the slice ends can never split a multi-byte char.
#[allow(clippy::string_slice)]
fn is_generation_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() != 23 || name.ends_with(".tmp") {
        return false;
    }
    // Names come from `read_dir` on an on-disk workspace, so they are
    // arbitrary UTF-8, not just our own generated names: a 23-byte name may
    // have a multi-byte char straddling byte 16. `split_at_checked` returns
    // `None` there instead of panicking.
    let Some((ts, rest)) = name.split_at_checked(16) else {
        return false;
    };
    let Some(hex) = rest.strip_prefix('-') else {
        return false;
    };
    // `ts` is exactly 16 bytes, so index 8 is in bounds. The remaining slices
    // are boundary-safe because `&&` short-circuits: `ts[..8]` is only reached
    // once byte 8 is the ASCII 'T', and `ts[9..15]` only once the final char is
    // the ASCII 'Z' at byte 15 — both ends are then char boundaries.
    ts.as_bytes()[8] == b'T'
        && ts.ends_with('Z')
        && ts[..8].bytes().all(|b| b.is_ascii_digit())
        && ts[9..15].bytes().all(|b| b.is_ascii_digit())
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Best-effort directory fsync (durability of renames within it).
fn fsync_dir(dir: &Path) -> Result<()> {
    let f = File::open(dir).with_context(|| format!("opening {} for fsync", dir.display()))?;
    f.sync_all()
        .with_context(|| format!("fsyncing {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "ab12cd34ef56";

    #[test]
    fn publish_is_atomic_and_current_points_at_it() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(home.path(), WS);
        let generation = store.begin_generation().unwrap();
        assert!(generation.dir().is_dir());
        assert!(generation.dir().to_str().unwrap().ends_with(".tmp"));
        std::fs::write(generation.dir().join("index.sqlite"), b"x").unwrap();
        std::fs::write(generation.dir().join("manifest.json"), b"{}").unwrap();

        let name = store.publish(generation).unwrap();
        assert_eq!(store.current().unwrap().unwrap(), name);
        let dir = store.current_dir().unwrap();
        assert!(dir.join("index.sqlite").exists());
        assert!(dir.join("manifest.json").exists());
        // No .tmp residue.
        assert!(!home.path().join(WS).join(format!("{name}.tmp")).exists());
    }

    #[test]
    fn generation_names_match_spec_format() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(home.path(), WS);
        let generation = store.begin_generation().unwrap();
        let name = generation.name().to_string();
        assert!(is_generation_name(&name), "bad generation name: {name}");
        let name2 = store.begin_generation().unwrap().name().to_string();
        assert_ne!(name, name2, "two generations in flight must not collide");
    }

    #[test]
    fn is_generation_name_rejects_multibyte_name_without_panicking() {
        // 23 bytes total (matches the required length) but a 2-byte UTF-8
        // char (é) straddles byte offset 16, where `split_at(16)` would
        // otherwise panic on a non-char-boundary index.
        let name = "123456789012345\u{e9}abcdez";
        assert_eq!(
            name.len(),
            23,
            "fixture must match generation-name byte length"
        );
        assert!(!is_generation_name(name));
    }

    #[test]
    fn is_generation_name_rejects_multibyte_names_at_every_byte_index() {
        // Each fixture is a 23-byte name that clears `split_at_checked(16)`
        // but puts a multi-byte char on one of the remaining byte indexes
        // (`as_bytes()[8]`, `ts[..8]`, `ts[9..15]`). Every one must return
        // false via the short-circuiting guards, never panic.
        for name in [
            // 'é' straddles bytes 7..9, so `as_bytes()[8]` is a continuation
            // byte (never b'T') and `ts[..8]` is never reached.
            "1234567\u{e9}890123Z-abcdef",
            // 'é' occupies bytes 14..16, so `ts` does not end with 'Z' and
            // `ts[9..15]` (which would split it) is never reached.
            "12345678T90123\u{e9}-abcdef",
        ] {
            assert_eq!(name.len(), 23, "fixture must be 23 bytes: {name}");
            assert!(!is_generation_name(name), "must reject: {name}");
        }
    }

    #[test]
    fn prune_keeps_two_most_recent() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(home.path(), WS);
        let mut names = Vec::new();
        for _ in 0..3 {
            let generation = store.begin_generation().unwrap();
            std::fs::write(generation.dir().join("index.sqlite"), b"x").unwrap();
            names.push(store.publish(generation).unwrap());
        }
        let current = store.current().unwrap().unwrap();
        assert_eq!(current, names[2]);

        store.prune().unwrap();
        let kept = store.generations().unwrap();
        assert_eq!(kept.len(), 2, "prune must keep exactly 2 of 3: {kept:?}");
        assert!(
            kept.contains(&current),
            "CURRENT generation must survive prune"
        );
        assert!(store.current_dir().unwrap().join("index.sqlite").exists());
    }

    #[test]
    fn prune_never_deletes_the_generation_current_names() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(home.path(), WS);
        let ws = home.path().join(WS);
        // Fabricate four published generations with strictly ordered names,
        // and point CURRENT at the OLDEST (e.g. a rolled-back publish).
        let names = [
            "20240101T000000Z-aaaaaa",
            "20240102T000000Z-bbbbbb",
            "20240103T000000Z-cccccc",
            "20240104T000000Z-dddddd",
        ];
        for n in names {
            std::fs::create_dir_all(ws.join(n)).unwrap();
        }
        std::fs::write(ws.join("CURRENT"), format!("{}\n", names[0])).unwrap();

        let removed = store.prune().unwrap();
        assert_eq!(removed, vec![names[1].to_string()]);
        let kept = store.generations().unwrap();
        assert_eq!(
            kept,
            vec![
                names[3].to_string(),
                names[2].to_string(),
                names[0].to_string()
            ],
            "2 newest + CURRENT survive, newest first"
        );
    }

    #[test]
    fn no_current_returns_none_not_panic() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(home.path(), "zz");
        assert!(store.current().unwrap().is_none());
        assert!(store.current_dir().is_err());
        assert!(store.generations().unwrap().is_empty());
    }

    #[test]
    fn tmp_dirs_are_not_listed_as_generations() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(home.path(), WS);
        let _inflight = store.begin_generation().unwrap();
        assert!(store.generations().unwrap().is_empty());
    }

    #[test]
    fn lock_is_exclusive() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(home.path(), WS);
        let guard = store.try_lock().unwrap();
        assert!(guard.is_some(), "first lock must succeed");
        // Second handle (separate fd, same process: flock is per open file
        // description, so this models a second `cq index` invocation).
        assert!(
            store.try_lock().unwrap().is_none(),
            "second lock must be refused"
        );
        drop(guard);
        assert!(
            store.try_lock().unwrap().is_some(),
            "lock must be released on drop"
        );
    }
}
