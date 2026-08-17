//! Architecture-docs registry enforcement (docs/architecture/README.md §4).
//!
//! The registry table in docs/architecture/README.md is the index of per-subsystem
//! deep dives. This test makes it unbreakable: every data row must link a real doc
//! carrying the standard's machine-readable headers, and a row whose Doc column has
//! no parseable link FAILS (a malformed row cannot silently evade enforcement).
//! Rides `cargo test` and therefore every BOI/workflow gate battery.

use std::path::PathBuf;

fn arch_docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/architecture")
        .canonicalize()
        .expect("docs/architecture must exist at the repo root")
}

/// Extract the data rows of the `## Registry` table: skip the header and
/// separator rows, stop at the first non-table line after the table started.
fn registry_rows(readme: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut in_section = false;
    let mut table_started = false;
    for line in readme.lines() {
        let t = line.trim();
        if t.starts_with("## ") {
            if in_section && table_started {
                break;
            }
            in_section = t == "## Registry";
            continue;
        }
        if !in_section {
            continue;
        }
        if t.starts_with('|') {
            table_started = true;
            let cells: Vec<&str> = t.trim_matches('|').split('|').collect();
            let is_separator = cells.iter().all(|c| {
                let c = c.trim();
                !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
            });
            let is_header = cells
                .first()
                .map(|c| c.trim().eq_ignore_ascii_case("subsystem"))
                .unwrap_or(false);
            if !is_separator && !is_header {
                rows.push(t.to_string());
            }
        } else if table_started && !t.is_empty() {
            break; // table ended (prose after the table, e.g. the Grandfathered note)
        }
    }
    rows
}

/// Extract a same-directory `*.md` link target from one table row, normalizing
/// `./` prefixes and stripping `#fragment`s. None = no parseable doc link.
fn doc_link(row: &str) -> Option<String> {
    let mut rest = row;
    while let Some(open) = rest.find("](") {
        // `open` + 2 skips the ASCII `](`, so it is always a char boundary;
        // `.get()` keeps the slice panic-free regardless (clippy::string_slice).
        let tail = rest.get(open + 2..).unwrap_or("");
        let close = tail.find(')')?;
        let mut target = tail.get(..close).unwrap_or("").trim();
        if let Some(stripped) = target.strip_prefix("./") {
            target = stripped;
        }
        let target = target.split('#').next().unwrap_or(target);
        if target.ends_with(".md") && !target.contains('/') {
            return Some(target.to_string());
        }
        rest = tail.get(close + 1..).unwrap_or("");
    }
    None
}

#[test]
fn registry_rows_link_real_docs_with_headers() {
    let dir = arch_docs_dir();
    let readme = std::fs::read_to_string(dir.join("README.md"))
        .expect("docs/architecture/README.md must exist");

    let rows = registry_rows(&readme);
    assert!(
        !rows.is_empty(),
        "the Registry table in docs/architecture/README.md has no data rows — \
         the table was removed or reformatted; fix the table or update this parser"
    );

    for row in rows {
        let doc = doc_link(&row).unwrap_or_else(|| {
            panic!(
                "registry row has no parseable same-directory .md link — every row's \
                 Doc column must link its deep dive (standard §4). Row: {row}"
            )
        });
        let path = dir.join(&doc);
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "registry lists {doc} but it is unreadable at {}: {e} — \
                 every registry row needs a real file (standard §3)",
                path.display()
            )
        });
        assert!(
            body.contains("verified-against:"),
            "{doc} is missing the `verified-against:` header (standard §1)"
        );
        assert!(
            body.contains("source-paths:"),
            "{doc} is missing the `source-paths:` header (standard §1)"
        );
    }
}

#[test]
fn parser_extracts_data_rows_and_links() {
    let sample = "\
## Registry

intro prose with a [decoy](decoy.md) link outside the table

| Subsystem | Doc |
|-----------|-----|
| Memory | [memory.md](./memory.md#anchor) |
| Broken row, no link | plain text |

Grandfathered: `docs/code-intel.md`.

## The Standard of Practice

| Other | [other.md](other.md) |";
    let rows = registry_rows(sample);
    assert_eq!(rows.len(), 2, "two data rows expected, got: {rows:?}");
    assert_eq!(doc_link(&rows[0]), Some("memory.md".to_string()));
    assert_eq!(
        doc_link(&rows[1]),
        None,
        "row without a link must parse as None (and fail the main test)"
    );
}
