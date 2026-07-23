/// Port of system/scripts/promote-learnings.py
///
/// Scans me/learnings.md (and raw/reflections/*.md) for dated entries,
/// clusters similar ones by category using Jaccard similarity, and writes
/// promotion candidates to evolution/suggestions.md when a pattern appears
/// 3+ times.
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const MIN_CLUSTER_SIZE: usize = 3;
const SIMILARITY_THRESHOLD: f64 = 0.15;

// ---------------------------------------------------------------------------
// Stop words (mirrors the Python set)
// ---------------------------------------------------------------------------
fn stop_words() -> HashSet<&'static str> {
    [
        "a",
        "an",
        "the",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "have",
        "has",
        "had",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "may",
        "might",
        "shall",
        "can",
        "need",
        "must",
        "ought",
        "me",
        "my",
        "we",
        "our",
        "you",
        "your",
        "he",
        "she",
        "it",
        "they",
        "them",
        "their",
        "this",
        "that",
        "these",
        "those",
        "and",
        "but",
        "nor",
        "not",
        "so",
        "if",
        "then",
        "than",
        "too",
        "very",
        "just",
        "about",
        "above",
        "after",
        "again",
        "all",
        "also",
        "any",
        "as",
        "at",
        "because",
        "before",
        "between",
        "both",
        "by",
        "each",
        "for",
        "from",
        "get",
        "got",
        "how",
        "in",
        "into",
        "its",
        "like",
        "more",
        "most",
        "of",
        "off",
        "on",
        "once",
        "only",
        "other",
        "out",
        "over",
        "own",
        "same",
        "some",
        "such",
        "to",
        "up",
        "us",
        "use",
        "used",
        "using",
        "what",
        "when",
        "where",
        "which",
        "while",
        "who",
        "whom",
        "why",
        "with",
        "down",
        "here",
        "there",
        "through",
        "during",
        "under",
        "until",
        "even",
        "still",
        "already",
        "much",
        "many",
        "well",
        "way",
        "don",
        "doesn",
        "didn",
        "won",
        "him",
        "his",
        "her",
        "hers",
        "mine",
        "ours",
        "yours",
        "theirs",
        "agent",
        "always",
        "never",
        "every",
        "often",
        "sometimes",
        "wants",
        "want",
        "make",
        "makes",
        "made",
        "thing",
        "things",
        "something",
        "nothing",
        "everything",
    ]
    .iter()
    .copied()
    .collect()
}

// ---------------------------------------------------------------------------
// Stemmer — mirrors the Python suffix-stripping stemmer
// ---------------------------------------------------------------------------
fn stem(word: &str) -> String {
    if word.len() <= 3 {
        return word.to_string();
    }
    // (suffix, min_word_len)
    let rules: &[(&str, usize)] = &[
        ("ingly", 7),
        ("edly", 6),
        ("ness", 6),
        ("ment", 6),
        ("tion", 6),
        ("sion", 6),
        ("ably", 6),
        ("ibly", 6),
        ("ally", 6),
        ("ful", 5),
        ("ly", 4),
        ("ies", 0),
        ("ing", 5),
        ("est", 5),
        ("er", 4),
        ("ed", 4),
        ("es", 4),
    ];
    for (suffix, min_len) in rules {
        if word.ends_with(suffix) && word.len() > *min_len {
            if *suffix == "ies" {
                // "ies" (3 ASCII bytes) was just matched by ends_with ⇒ len-3 is a char boundary.
                #[allow(clippy::string_slice)]
                return format!("{}y", &word[..word.len() - 3]);
            }
            // `suffix` (all-ASCII) was just matched by ends_with ⇒ len-suffix.len() is a char boundary.
            #[allow(clippy::string_slice)]
            return word[..word.len() - suffix.len()].to_string();
        }
    }
    if word.ends_with('s') && !word.ends_with("ss") && word.len() > 3 {
        // trailing 's' (1 ASCII byte) was just matched by ends_with ⇒ len-1 is a char boundary.
        #[allow(clippy::string_slice)]
        return word[..word.len() - 1].to_string();
    }
    word.to_string()
}

fn tokenize(text: &str, stops: &HashSet<&'static str>) -> HashSet<String> {
    // Strip quoted strings, date annotations, (imported) tag
    let text = regex_lite_strip_quotes(text);
    let text = regex_lite_strip_dates(&text);
    let text = text.replace("(imported)", "");
    let mut tokens = HashSet::new();
    for word in text.split(|c: char| !c.is_alphabetic()) {
        let w = word.to_lowercase();
        if w.len() > 3 && !stops.contains(w.as_str()) {
            tokens.insert(stem(&w));
        }
    }
    tokens
}

fn regex_lite_strip_quotes(s: &str) -> String {
    // Remove "..." and '...' substrings (simple state-machine, no regex dep)
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            while let Some(nc) = chars.next() {
                if nc == '"' {
                    break;
                }
            }
        } else if c == '\'' {
            while let Some(nc) = chars.next() {
                if nc == '\'' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn regex_lite_strip_dates(s: &str) -> String {
    // Remove (YYYY-MM-DD) and (YYYY-MM-DD, YYYY-MM-DD, ...) patterns
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            // look for closing paren that contains only digits, dashes, spaces, commas
            let start = i;
            let mut j = i + 1;
            let mut looks_like_date = false;
            while j < bytes.len() && bytes[j] != b')' {
                j += 1;
            }
            if j < bytes.len() {
                // start indexes b'(' and j indexes b')' (both ASCII byte scans) ⇒ start+1 and j are char boundaries.
                #[allow(clippy::string_slice)]
                let inner = &s[start + 1..j];
                if inner
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '-' || c == ',' || c == ' ')
                {
                    looks_like_date = true;
                }
            }
            if looks_like_date {
                i = j + 1; // skip past the closing paren
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
struct Entry {
    text: String,
    category: String,
    dates: Vec<String>,
    tokens: HashSet<String>,
}

impl Entry {
    fn new(
        text: String,
        category: String,
        dates: Vec<String>,
        stops: &HashSet<&'static str>,
    ) -> Self {
        let tokens = tokenize(&text, stops);
        Entry {
            text,
            category,
            dates,
            tokens,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PendingState {
    candidates: Vec<Candidate>,
    processed_clusters: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Candidate {
    id: String,
    category: String,
    rule: String,
    entries: Vec<String>,
    entry_count: usize,
    dates: Vec<String>,
    status: String,
    created: String,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------
fn parse_learnings(path: &Path, stops: &HashSet<&'static str>) -> Vec<Entry> {
    let Ok(text) = fs::read_to_string(path) else {
        return vec![];
    };
    let mut entries = Vec::new();
    let mut category = String::new();

    for line in text.lines() {
        let stripped = line.trim();
        if stripped.starts_with("## ") {
            // "## " (3 ASCII bytes) was just matched by starts_with ⇒ byte 3 is a char boundary.
            #[allow(clippy::string_slice)]
            let cat = stripped[3..].trim().to_string();
            category = cat;
        } else if stripped.starts_with("- ") && !category.is_empty() {
            // "- " (2 ASCII bytes) was just matched by starts_with ⇒ byte 2 is a char boundary.
            #[allow(clippy::string_slice)]
            let text = stripped[2..].trim().to_string();
            let dates = extract_dates(&text);
            entries.push(Entry::new(text, category.clone(), dates, stops));
        }
    }
    entries
}

fn parse_reflections(dir: &Path, stops: &HashSet<&'static str>) -> Vec<Entry> {
    let Ok(rd) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut entries = Vec::new();

    let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();

    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // `get(..10)` returns None when byte 10 is not a char boundary, so a
        // filename with a multi-byte char straddling that offset yields no
        // date instead of panicking on the slice.
        let date = fname
            .get(..10)
            .filter(|p| p.chars().all(|c| c.is_ascii_digit() || c == '-'))
            .map(|p| p.to_string());

        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut cat = "Reflection".to_string();
        for line in text.lines() {
            let s = line.trim();
            if s.starts_with("## ") {
                // "## " (3 ASCII bytes) was just matched by starts_with ⇒ byte 3 is a char boundary.
                #[allow(clippy::string_slice)]
                let c = s[3..].trim().to_string();
                cat = c;
            } else if s.starts_with("- ") && s.len() > 10 {
                // "- " (2 ASCII bytes) was just matched by starts_with ⇒ byte 2 is a char boundary.
                #[allow(clippy::string_slice)]
                let t = s[2..].trim();
                if t.starts_with('_') || t.starts_with("Auto-generated") {
                    continue;
                }
                let dates = date.as_ref().map(|d| vec![d.clone()]).unwrap_or_default();
                entries.push(Entry::new(t.to_string(), cat.clone(), dates, stops));
            }
        }
    }
    entries
}

fn extract_dates(text: &str) -> Vec<String> {
    let mut dates = Vec::new();
    // Find all YYYY-MM-DD patterns inside the last parenthesised group
    if let Some(open) = text.rfind('(') {
        // open indexes the ASCII '(' found by rfind ⇒ open+1 is a char boundary.
        #[allow(clippy::string_slice)]
        let tail = &text[open + 1..];
        if let Some(close) = tail.find(')') {
            // close indexes the ASCII ')' found by find ⇒ it is a char boundary.
            #[allow(clippy::string_slice)]
            let inner = &tail[..close];
            if inner
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == ',' || c == ' ')
            {
                for part in inner.split(',') {
                    let d = part.trim();
                    if d.len() == 10 {
                        dates.push(d.to_string());
                    }
                }
            }
        }
    }
    dates
}

// ---------------------------------------------------------------------------
// Similarity and clustering (union-find, mirrors Python implementation)
// ---------------------------------------------------------------------------
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    inter as f64 / union as f64
}

fn find(parent: &mut Vec<usize>, x: usize) -> usize {
    if parent[x] != x {
        parent[x] = find(parent, parent[x]);
    }
    parent[x]
}

fn union(parent: &mut Vec<usize>, x: usize, y: usize) {
    let px = find(parent, x);
    let py = find(parent, y);
    if px != py {
        parent[px] = py;
    }
}

fn find_clusters(entries: &[Entry]) -> Vec<Vec<usize>> {
    // Group indices by category, only dated entries
    let mut by_cat: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if !e.dates.is_empty() {
            by_cat.entry(&e.category).or_default().push(i);
        }
    }

    let mut all_clusters = Vec::new();

    for cat_indices in by_cat.values() {
        if cat_indices.len() < MIN_CLUSTER_SIZE {
            continue;
        }
        let n = cat_indices.len();
        let mut parent: Vec<usize> = (0..n).collect();

        for i in 0..n {
            for j in (i + 1)..n {
                let sim = jaccard(
                    &entries[cat_indices[i]].tokens,
                    &entries[cat_indices[j]].tokens,
                );
                if sim >= SIMILARITY_THRESHOLD {
                    union(&mut parent, i, j);
                }
            }
        }

        let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            let root = find(&mut parent, i);
            components.entry(root).or_default().push(cat_indices[i]);
        }

        for cluster in components.into_values() {
            if cluster.len() >= MIN_CLUSTER_SIZE {
                all_clusters.push(cluster);
            }
        }
    }
    all_clusters
}

fn cluster_key(cluster: &[usize], entries: &[Entry]) -> String {
    let mut texts: Vec<String> = cluster
        .iter()
        .map(|&i| entries[i].text.chars().take(50).collect::<String>())
        .collect();
    texts.sort();
    // Simple non-crypto hash (sha256 not available without dep; use a stable hash)
    // We concatenate and take a hex representation of a simple hash.
    let joined = texts.join("|");
    let hash = stable_hash(&joined);
    format!("{hash:016x}").chars().take(12).collect()
}

fn stable_hash(s: &str) -> u64 {
    // FNV-1a 64-bit
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn generate_rule(cluster: &[usize], entries: &[Entry]) -> String {
    let base = cluster
        .iter()
        .min_by_key(|&&i| entries[i].text.len())
        .map(|&i| &entries[i].text)
        .unwrap();
    // Strip trailing date annotation
    let rule = strip_trailing_dates(base);
    rule.trim().to_string()
}

fn strip_trailing_dates(text: &str) -> String {
    // Strip (YYYY-MM-DD) or (YYYY-MM-DD, ...) at end; also strip (imported)
    let mut s = text.trim_end().to_string();
    loop {
        if s.ends_with(')') {
            if let Some(open) = s.rfind('(') {
                // open indexes ASCII '(' (rfind) and s ends_with ')' so s.len()-1 indexes that ASCII byte ⇒ both are char boundaries.
                #[allow(clippy::string_slice)]
                let inner = &s[open + 1..s.len() - 1];
                if inner
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '-' || c == ',' || c == ' ')
                    || inner.trim() == "imported"
                {
                    // open indexes the ASCII '(' found by rfind ⇒ it is a char boundary.
                    #[allow(clippy::string_slice)]
                    let head = s[..open].trim_end().to_string();
                    s = head;
                    continue;
                }
            }
        }
        break;
    }
    s
}

// ---------------------------------------------------------------------------
// Pending state persistence
// ---------------------------------------------------------------------------
fn pending_path(hex_dir: &Path) -> PathBuf {
    hex_dir.join("evolution/.pending-promotions.json")
}

fn load_pending(hex_dir: &Path) -> PendingState {
    let path = pending_path(hex_dir);
    if !path.exists() {
        return PendingState::default();
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return PendingState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_pending(hex_dir: &Path, state: &PendingState) -> Result<(), String> {
    let path = pending_path(hex_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Suggestions file
// ---------------------------------------------------------------------------
fn write_suggestion(hex_dir: &Path, candidate: &Candidate) -> Result<(), String> {
    let path = hex_dir.join("evolution/suggestions.md");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let today = Local::now().format("%Y-%m-%d").to_string();
    let dates_str = candidate
        .dates
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let suggestion = format!(
        "\n## [{today}] Suggestion: Standing order from {category}\n\
         - **What:** Add standing order: \"{rule}\"\n\
         - **Why:** Pattern observed {count} times ({dates_str})\n\
         - **How:** Append to standing orders table in CLAUDE.md\n\
         - **Expected benefit:** Consistent behavior without repeated corrections\n\
         - **Status:** pending-approval (ID: {id})\n",
        today = today,
        category = candidate.category,
        rule = candidate.rule,
        count = candidate.entry_count,
        dates_str = dates_str,
        id = candidate.id,
    );
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, format!("{existing}{suggestion}")).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Main promote pipeline
// ---------------------------------------------------------------------------
pub fn run_promote(hex_dir: &Path, dry_run: bool) {
    let stops = stop_words();

    let learnings_file = hex_dir.join("me/learnings.md");
    let reflections_dir = hex_dir.join("raw/reflections");

    let mut entries = parse_learnings(&learnings_file, &stops);
    let reflection_entries = parse_reflections(&reflections_dir, &stops);
    let refl_count = reflection_entries.len();
    entries.extend(reflection_entries);

    let dated_count = entries.iter().filter(|e| !e.dates.is_empty()).count();
    eprintln!(
        "[promote] {} total entries ({} dated, {} from reflections)",
        entries.len(),
        dated_count,
        refl_count
    );

    if dated_count < MIN_CLUSTER_SIZE {
        eprintln!("[promote] Not enough dated entries for clustering");
        println!("No candidates found (not enough dated entries).");
        return;
    }

    // Find similarity clusters
    let mut clusters: Vec<Vec<usize>> = find_clusters(&entries);

    // Also add single entries with 3+ dates as solo clusters
    for (i, e) in entries.iter().enumerate() {
        if e.dates.len() >= MIN_CLUSTER_SIZE {
            clusters.push(vec![i]);
        }
    }

    eprintln!("[promote] {} cluster(s) found", clusters.len());

    let mut pending = load_pending(hex_dir);
    let mut new_count = 0;

    for cluster in &clusters {
        let key = cluster_key(cluster, &entries);
        if pending.processed_clusters.contains(&key) {
            continue;
        }

        let rule = generate_rule(cluster, &entries);
        let mut all_dates: HashSet<String> = HashSet::new();
        for &i in cluster {
            for d in &entries[i].dates {
                all_dates.insert(d.clone());
            }
        }
        let mut dates: Vec<String> = all_dates.into_iter().collect();
        dates.sort();

        let occurrence_count = cluster.len().max(dates.len());
        let candidate = Candidate {
            id: format!("promo_{key}"),
            category: entries[cluster[0]].category.clone(),
            rule: rule.clone(),
            entries: cluster
                .iter()
                .map(|&i| entries[i].text.chars().take(120).collect())
                .collect(),
            entry_count: occurrence_count,
            dates: dates.clone(),
            status: "pending".to_string(),
            created: Local::now().format("%Y-%m-%d").to_string(),
        };

        if dry_run {
            println!(
                "[dry-run] Would promote: {} ({}x) — {}",
                candidate.id, occurrence_count, rule
            );
        } else {
            if let Err(e) = write_suggestion(hex_dir, &candidate) {
                eprintln!("WARN: could not write suggestion: {e}");
            }
            pending.candidates.push(candidate.clone());
            pending.processed_clusters.push(key);
            println!(
                "Promoted candidate: {} [{}]",
                candidate.id, candidate.category
            );
            new_count += 1;
        }
    }

    if !dry_run && new_count > 0 {
        if let Err(e) = save_pending(hex_dir, &pending) {
            eprintln!("WARN: could not save pending state: {e}");
        }
    }

    println!(
        "Done. {} new candidate(s){}.",
        new_count,
        if dry_run { " (dry-run)" } else { "" }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression test for a char-boundary panic in `parse_reflections`:
    // the date-prefix check used `fname[..10]`, guarded only by
    // `fname.len() >= 10` (a byte-length check). A filename with a
    // multi-byte character straddling byte offset 10 panics on the slice
    // even though the length guard passes. This filename places a 2-byte
    // 'é' (U+00E9, UTF-8 bytes 0xC3 0xA9) at byte offsets 9..11, so byte
    // index 10 lands mid-character.
    #[test]
    fn parse_reflections_handles_multibyte_char_at_byte_10() {
        let dir = tempfile::tempdir().unwrap();
        let fname = "202607-01é-note.md";
        assert!(fname.len() >= 10, "fixture must exercise the len guard");
        assert!(
            !fname.is_char_boundary(10),
            "fixture must place byte 10 mid-character"
        );
        fs::write(
            dir.path().join(fname),
            "## Reflection\n- something happened (2026-07-01)\n",
        )
        .unwrap();

        let stops = stop_words();
        // Must not panic, and since the byte-10 prefix isn't a clean
        // ASCII date, the entry should carry no reflection-filename date.
        let entries = parse_reflections(dir.path(), &stops);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].dates.is_empty());
    }
}
