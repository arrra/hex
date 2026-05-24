# Extensions

This directory is the **foundation scaffold** for extensions shipped with hex-foundation.
Personal or user-specific extensions live in `~/hex/extensions/` (or `$HEX_DIR/.hex/extensions/`),
which are never overwritten by `hex upgrade`.

## Auto-discovery contract

The hex harness scans two directories at startup:

```
$HEX_DIR/extensions/        ← foundation-bundled extensions (this directory)
$HEX_DIR/.hex/extensions/   ← user-installed extensions (never overwritten by upgrade)
```

For a directory to be recognised as an extension it must contain `extension.yaml` at its root.
Directories starting with `.` are ignored.

### extension.yaml

Minimum required fields:

```yaml
name: my-extension
version: "0.1.0"
description: One-line description of what this extension does.
```

### SQLite migrations

If your extension needs persistent state, place numbered `.sql` files in a `migrations/`
subdirectory. The harness runs them in lexicographic order, exactly once, tracked in
`$HEX_DIR/.hex/extensions/ext.db` (table `_ext_migrations`).

```
my-extension/
  extension.yaml
  migrations/
    001_create_table.sql
    002_add_column.sql
```

## Example layout — `hello/`

See `hello/` in this directory for a minimal working skeleton.

## Adding an extension

1. Create `$HEX_DIR/extensions/<name>/extension.yaml` (user-installed extensions go in
   `$HEX_DIR/.hex/extensions/<name>/extension.yaml`).
2. Add `migrations/*.sql` if you need database tables.
3. Restart the hex harness — it will discover and migrate automatically.
