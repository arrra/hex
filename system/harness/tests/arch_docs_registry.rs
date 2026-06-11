//! Architecture-docs registry enforcement (docs/architecture/README.md §4).
//!
//! The registry table in docs/architecture/README.md is the source of truth for
//! per-subsystem deep dives. This test makes the registry unbreakable: every doc
//! linked from the table must exist and carry the machine-readable
//! `verified-against:` header the standard requires. It rides `cargo test` and
//! therefore every release-gate battery.

use std::path::PathBuf;

fn arch_docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/architecture")
        .canonicalize()
        .expect("docs/architecture must exist at the repo root")
}

/// Extract `target.md` from every markdown link of the form `[label](target.md)`
/// found in the registry table rows (lines starting with `|`).
fn registry_doc_links(readme: &str) -> Vec<String> {
    let mut links = Vec::new();
    for line in readme.lines().filter(|l| l.trim_start().starts_with('|')) {
        let mut rest = line;
        while let Some(open) = rest.find("](") {
            let tail = &rest[open + 2..];
            if let Some(close) = tail.find(')') {
                let target = &tail[..close];
                if target.ends_with(".md") && !target.contains('/') {
                    links.push(target.to_string());
                }
                rest = &tail[close + 1..];
            } else {
                break;
            }
        }
    }
    links
}

#[test]
fn registry_docs_exist_with_verified_headers() {
    let dir = arch_docs_dir();
    let readme = std::fs::read_to_string(dir.join("README.md"))
        .expect("docs/architecture/README.md must exist");

    let links = registry_doc_links(&readme);
    assert!(
        !links.is_empty(),
        "registry table in docs/architecture/README.md has no doc links — \
         the table was removed or reformatted; fix the table or update this parser"
    );

    for doc in links {
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
            "{doc} is missing the `verified-against:` header (standard §1) — \
             add the machine-readable header block at the top of the file"
        );
        assert!(
            body.contains("source-paths:"),
            "{doc} is missing the `source-paths:` header (standard §1)"
        );
    }
}

#[test]
fn link_parser_handles_table_rows() {
    let sample = "| Memory | [memory.md](memory.md) | `a/`, `b.rs` | `abc` |\n\
                  not a table line [skip.md](skip.md)\n\
                  | Two | [a.md](a.md) and [b.md](b.md) | x | y |";
    assert_eq!(registry_doc_links(sample), vec!["memory.md", "a.md", "b.md"]);
}
