//! Reciprocal Rank Fusion — combine independently-ranked id lists into one
//! ranking. Spec §7: k = 60, over the BM25 and vector arms of the same store.

use std::collections::HashMap;

/// The standard RRF constant (spec §7).
pub const RRF_K: f64 = 60.0;

/// Fuse ranked id lists (each best-first). Returns (id, score), best first.
pub fn rrf_fuse(ranked_lists: &[Vec<i64>], k: f64) -> Vec<(i64, f64)> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for list in ranked_lists {
        for (rank, &id) in list.iter().enumerate() {
            *scores.entry(id).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
        }
    }
    let mut fused: Vec<(i64, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_in_both_lists_outranks_item_in_one() {
        // id 1: rank 0 in both arms. id 2: rank 0 in one arm only.
        let fused = rrf_fuse(&[vec![1, 2], vec![1, 3]], RRF_K);
        assert_eq!(fused[0].0, 1);
    }

    #[test]
    fn empty_lists_yield_empty() {
        assert!(rrf_fuse(&[], RRF_K).is_empty());
        assert!(rrf_fuse(&[vec![], vec![]], RRF_K).is_empty());
    }

    #[test]
    fn single_list_preserves_order() {
        let fused = rrf_fuse(&[vec![9, 8, 7]], RRF_K);
        assert_eq!(fused.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![9, 8, 7]);
    }
}
