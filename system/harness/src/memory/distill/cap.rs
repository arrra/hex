//! Slice cap for distill.
//!
//! `cap_span` returns the byte length of the prefix of `span` to process in
//! one LLM call so that the estimated input token count stays under
//! `budget_tokens`. Token estimate is `ceil(chars / 3.5)` — a cheap,
//! tokenizer-free heuristic that overestimates slightly for ASCII-heavy
//! prose, which is the safe side for an input-cap.
//!
//! Preference order for the cut point:
//!   1. A markdown heading boundary (line starting with `#`) within budget.
//!   2. A blank-line paragraph boundary within budget.
//!   3. Degenerate fallback: hard-cap at the budget on a UTF-8 char boundary
//!      (never blind byte slicing).
//!
//! If the whole span already fits under budget, returns `span.len()`.

/// Return the byte length of the prefix of `span` to process so that the
/// estimated input token count stays under `budget_tokens`.
///
/// See the module docs for the boundary preference order and the token
/// estimate (`ceil(chars / 3.5)`).
pub fn cap_span(span: &str, budget_tokens: u32) -> usize {
    if span.is_empty() {
        return 0;
    }

    // Budget in chars. ceil(chars / 3.5) <= budget  <=>  chars <= floor(budget * 3.5).
    let max_chars = (budget_tokens as f64 * 3.5).floor() as usize;
    if max_chars == 0 {
        return 0;
    }

    // Cheap fast-path: if the whole span fits, no cap needed.
    let total_chars = span.chars().count();
    if total_chars <= max_chars {
        return span.len();
    }

    // Find `limit` = largest byte index whose prefix has at most `max_chars`
    // chars. char_indices guarantees `limit` is a UTF-8 char boundary.
    let mut limit = span.len();
    let mut char_count: usize = 0;
    for (i, _c) in span.char_indices() {
        if char_count == max_chars {
            limit = i;
            break;
        }
        char_count += 1;
    }

    // Scan for the latest heading or blank-line boundary at byte <= limit.
    // Heading boundary: index i where bytes[i] == b'#' and (i == 0 or
    //   bytes[i-1] == b'\n'). Cut = i; prefix is everything before the
    //   heading. We require i > 0 so that we always make progress.
    // Blank-line boundary: cut = i where bytes[i-1] == b'\n' and
    //   bytes[i-2] == b'\n'. Prefix ends with the "\n\n".
    let bytes = span.as_bytes();
    let mut best: usize = 0;
    let scan_end = limit.min(bytes.len());
    for i in 1..=scan_end {
        if i < bytes.len() && bytes[i] == b'#' && bytes[i - 1] == b'\n' {
            best = i;
        }
        if i >= 2 && bytes[i - 1] == b'\n' && bytes[i - 2] == b'\n' {
            best = i;
        }
    }

    if best > 0 {
        return best;
    }
    // No boundary fits — hard-cap at the budget. `limit` is a char boundary.
    limit
}

#[cfg(test)]
mod tests {
    use super::cap_span;

    // ~3.5 chars per estimated token. budget_tokens = 100 -> ~350 char budget.

    #[test]
    fn cap_span_under_budget_returns_full_length() {
        let span = "short content under any budget";
        assert_eq!(cap_span(span, 1000), span.len());
    }

    #[test]
    fn cap_span_respects_heading_boundary() {
        // Two sections; budget large enough for first but not both.
        // Boundary should fall at the second heading line.
        let s = "## A\n\nfirst section body line one\nline two\n\n## B\n\nsecond section body that overflows the budget\n";
        let budget = 20u32; // ~70 char budget — fits "## A\n\nfirst section..." partially
        let cut = cap_span(s, budget);
        // The cut must land at a heading-start byte index or a paragraph
        // boundary, and must be <= byte length.
        assert!(cut > 0 && cut <= s.len(), "cut out of range: {}", cut);
        // The prefix should end exactly at a boundary (heading or paragraph).
        let prefix = &s[..cut];
        let ends_at_heading = prefix.ends_with("\n") && s[cut..].starts_with("##");
        let ends_at_blankline = prefix.ends_with("\n\n");
        assert!(
            ends_at_heading || ends_at_blankline,
            "cut should land on a boundary; prefix tail = {:?}",
            &prefix[prefix.len().saturating_sub(8)..]
        );
    }

    #[test]
    fn cap_span_respects_budget() {
        // 1000 chars of "a", no boundaries within budget except the start.
        // budget_tokens = 50 -> ~175 char budget. cut must be <= 175.
        let s = "a".repeat(1000);
        let budget = 50u32;
        let cut = cap_span(&s, budget);
        let est_tokens = ((cut as f64) / 3.5).ceil() as u32;
        assert!(
            est_tokens <= budget,
            "cut={} produces est_tokens={} > budget={}",
            cut,
            est_tokens,
            budget
        );
        assert!(cut > 0, "must process at least some bytes");
    }

    #[test]
    fn cap_span_degenerate_hard_split_no_boundary() {
        // One unbroken token with no boundaries. Must hard-cap at budget.
        let s = "x".repeat(500);
        let budget = 20u32; // ~70 char budget
        let cut = cap_span(&s, budget);
        assert!(cut > 0 && cut < s.len(), "must hard-split when no boundary");
        let est_tokens = ((cut as f64) / 3.5).ceil() as u32;
        assert!(est_tokens <= budget, "hard split must respect budget");
    }

    #[test]
    fn cap_span_utf8_multibyte_safe() {
        // Multibyte chars; budget forces a hard split. Cut must land on a
        // UTF-8 char boundary so &s[..cut] is valid UTF-8.
        // Each "🦀" is 4 bytes, 1 char.
        let s = "🦀".repeat(200); // 800 bytes, 200 chars
        let budget = 10u32; // ~35 char budget
        let cut = cap_span(&s, budget);
        assert!(cut > 0 && cut <= s.len());
        // Must be a valid char boundary — slicing must not panic.
        let _prefix = &s[..cut];
        assert!(s.is_char_boundary(cut), "cut={} not a char boundary", cut);
    }

    #[test]
    fn cap_span_paragraph_boundary_blank_line() {
        let s = "para one line one\npara one line two\n\npara two line one\npara two line two\n\npara three overflowing budget content here\n";
        let budget = 15u32; // ~52 char budget
        let cut = cap_span(s, budget);
        assert!(cut > 0 && cut <= s.len());
        let prefix = &s[..cut];
        // Must end at a blank-line paragraph boundary or a heading boundary.
        let ends_at_blankline = prefix.ends_with("\n\n");
        let ends_at_heading = prefix.ends_with("\n") && s[cut..].starts_with('#');
        assert!(
            ends_at_blankline || ends_at_heading,
            "expected paragraph or heading boundary; prefix tail = {:?}",
            &prefix[prefix.len().saturating_sub(8)..]
        );
    }
}
