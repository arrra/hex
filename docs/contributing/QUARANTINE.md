# Quarantine Checklist — Renaming Scripts to `.legacy.*`

When a shell script or Python file is superseded by a Rust implementation,
it must be quarantined (renamed to `.legacy.{sh,py}`) rather than deleted
immediately. This checklist ensures callers are updated before the rename.

## When to Use This Checklist

Use this checklist any time you rename `scripts/foo.sh` → `scripts/foo.legacy.sh`
or `scripts/foo.py` → `scripts/foo.legacy.py`.

## Pre-Rename Checklist

- [ ] **Identify all callers** — grep `system/harness/src/` for references:
  ```bash
  grep -rn "scripts/foo.sh" system/harness/src/
  ```
- [ ] **Update all Rust callers** — replace hardcoded script paths with the
  Rust-native equivalent or the new canonical path.
- [ ] **Verify no remaining references** — re-run the grep above; confirm zero hits.
- [ ] **Run the shellout-paths test** — confirms all shellout targets exist:
  ```bash
  cargo test --manifest-path system/harness/Cargo.toml --release --test shellout_paths
  ```
- [ ] **Stage all caller changes together with the rename** in the same commit
  so `git bisect` stays clean.

## Post-Rename Checklist

- [ ] CI passes (Legacy Rename Guard workflow shows green).
- [ ] The `.legacy.*` file is left in place for at least one release cycle.
- [ ] A follow-up task is filed to delete the `.legacy.*` file after the
  grace period.

## Why `.legacy.*` Instead of Immediate Deletion?

Immediate deletion risks breaking anything that calls the script at runtime
before all callers are updated. The `.legacy.*` convention preserves the file
at its original path under the new name so any missed caller produces a
clear "file not found" error rather than silent wrong behavior.

## Related CI Guardrails

- **`system/harness/tests/shellout_paths.rs`** — Rust integration test that
  walks `system/harness/src/` for shellout paths and asserts every target
  exists on disk. Fails the build if a caller references a missing script.
- **`.githooks/pre-commit`** — Blocks commits where a `.legacy.*` rename is
  staged but callers still reference the pre-legacy name.
- **`.github/workflows/legacy-rename-guard.yml`** — Runs the same check in CI
  on every pull request.

## Installing the Pre-Commit Hook

```bash
git config core.hooksPath .githooks
```

Run once per clone. The hook activates automatically on every subsequent commit.
