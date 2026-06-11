# Hex Versioning

## Source of Truth

`system/harness/Cargo.toml` (in hex-foundation) is the single source of truth for the hex version. Everything derives from this file:

- **Rust binary** — `build.rs` injects only the git SHA. `main.rs` reads `env!("CARGO_PKG_VERSION")` at compile time. The binary prints `hex 0.13.3 (abc1234)`.
- **git tag** — must match `v$(Cargo.toml version)`. Enforced by `hex release cut`, which bumps Cargo.toml and tags the merge commit in one ceremony.
- **hex VERSIONS file** — `HEX_FOUNDATION_VERSION` is pinned by `/hex-upgrade` from foundation's Cargo.toml at the pulled tag.
- **`.hex/version.txt`** — plain-text semver synced from foundation during `/hex-upgrade`; read by `extension-validate.py` for local tooling checks.
- **`hex version`** — reads the compiled-in `CARGO_PKG_VERSION` + `HEX_GIT_SHA`.
- **`hex --version`** — Clap built-in, reads `CARGO_PKG_VERSION`.

## Version Flow

```
system/harness/Cargo.toml (source of truth)
    ├── env!("CARGO_PKG_VERSION") → embedded in binary at compile time
    ├── git tag must match (enforced by hex release cut)
    └── hex VERSIONS pinned by /hex-upgrade
```

## Releasing a New Version

Releases follow **GitFlow**: feature branches merge to `develop`; a release is cut
from `develop`, merged `--no-ff` to `main`, tagged, and back-merged to `develop`.
Hotfixes cut from `main` directly. The whole ceremony is one command (or one event):

```sh
hex release cut --level patch        # or minor | major, or --version X.Y.Z
hex release cut --hotfix             # hotfix/X.Y.Z from main instead of develop
hex release cut --dry-run            # run the gate battery and stop
```

The agent surface is the `release.requested` event, handled by the `oss-releaser`
worker, which spawns the same ceremony as a detached child.

`hex release cut` will:
1. Take an exclusive lock and pin the `develop` SHA (`main` for `--hotfix`)
2. Run the full gate battery (clean tree, Docker E2E, sanitize, codex-parity, autonomy)
3. Compute the next semver (refusing anything not greater than the latest tag)
4. Branch `release/X.Y.Z`, bump `system/harness/Cargo.toml` + `system/version.txt`,
   rebuild so `Cargo.lock` updates, commit `bump: vX.Y.Z`
5. Merge `--no-ff` to `main`, tag `vX.Y.Z`, back-merge to `develop`
6. Push `main`, `develop`, and the tag (with post-push verification), then create
   the GitHub release if the repo profile enables it

If `develop` does not exist yet, the command refuses and prints the bootstrap
one-liner: `git branch develop main && git push origin develop`.

**Other repos:** the hex-foundation profile is built in. Additional repos load
from `$HEX_DIR/.hex/config/releases.toml` — see `system/templates/releases.toml.example`
(includes a `boi` profile) and the module docs in `system/harness/src/release.rs`.
Repos with no matching profile are refused.

Then in hex, run `/hex-upgrade` to pull the new release and pin VERSIONS.

## hex-doctor version-sync check

`hex-doctor` includes a `version-sync` check that asserts:
- Compiled binary version == Cargo.toml version (FAIL if mismatch)
- Latest git tag == Cargo.toml version (WARN if mismatch — Cargo.toml may be ahead between bumps)
