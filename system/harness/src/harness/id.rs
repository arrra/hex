//! Crockford base32 id minting for messages and prompts (house style, e.g. Sgzffh520).
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Mint a 9-char Crockford base32 id. Uniqueness comes from nanos-since-epoch
/// XOR a process-lifetime counter, so two calls in the same nanosecond differ.
pub fn mint() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = nanos ^ (COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E3779B97F4A7C15));
    let mut v = n;
    let mut out = [0u8; 9];
    for slot in out.iter_mut() {
        *slot = CROCKFORD[(v & 0x1f) as usize];
        v >>= 5;
    }
    String::from_utf8(out.to_vec()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_is_nonempty_crockford_and_unique() {
        let a = mint();
        let b = mint();
        assert_eq!(a.len(), 9, "id should be 9 chars");
        assert!(
            a.chars()
                .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c)),
            "crockford alphabet only: {a}"
        );
        assert_ne!(a, b, "two mints must differ");
    }
}
