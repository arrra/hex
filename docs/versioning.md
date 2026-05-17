# Hex Versioning

## Source of Truth

`system/harness/Cargo.toml` (in hex-foundation) is the single source of truth for the hex version. Everything derives from this file:

- **Rust binary** — `build.rs` injects only the git SHA. `main.rs` reads `env!("CARGO_PKG_VERSION")` at compile time. The binary prints `hex 0.13.3 (abc1234)`.
- **git tag** — must match `v$(Cargo.toml version)`. Enforced by `release.sh bump-version`.
- **mrap-hex VERSIONS file** — `HEX_FOUNDATION_VERSION` is pinned by `/hex-upgrade` from foundation's Cargo.toml at the pulled tag.
- **`.hex/version.txt`** — plain-text semver synced from foundation during `/hex-upgrade`; read by `extension-validate.py` for local tooling checks.
- **`hex version`** — reads the compiled-in `CARGO_PKG_VERSION` + `HEX_GIT_SHA`.
- **`hex --version`** — Clap built-in, reads `CARGO_PKG_VERSION`.

## Version Flow

```
system/harness/Cargo.toml (source of truth)
    ├── env!("CARGO_PKG_VERSION") → embedded in binary at compile time
    ├── git tag must match (enforced by release.sh)
    └── mrap-hex VERSIONS pinned by /hex-upgrade
```

## Releasing a New Version

Use `release.sh bump-version <new-version>` in hex-foundation:

```sh
cd ~/github.com/mrap/hex-foundation
system/scripts/release.sh bump-version 0.12.0
```

This will:
1. Validate semver shape
2. Bump `system/harness/Cargo.toml` version
3. Verify it compiles (`cargo build --release`)
4. Commit and tag `v0.12.0`

Then in mrap-hex, run `/hex-upgrade` to pull the new release and pin VERSIONS.

## hex-doctor version-sync check

`hex-doctor` includes a `version-sync` check that asserts:
- Compiled binary version == Cargo.toml version (FAIL if mismatch)
- Latest git tag == Cargo.toml version (WARN if mismatch — Cargo.toml may be ahead between bumps)
