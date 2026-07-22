pub const PREDICATES: &[&str] = &[
    "is",
    "alternateName",
    "homeLocation",
    "born-on",
    "prefers",
    "dislikes",
    "values",
    "avoids",
    "current-focus",
    "status",
    "blocked-by",
    "available-at",
    "works-on",
    "decided",
    "responsible-for",
    "committed-to",
    "knows",
    "knowsAbout",
    "learned-that",
    "related-to",
    "recipient-of",
    "has",
    "plans-to",
    "_unmapped",
];

#[derive(Debug)]
pub struct UnknownPredicate {
    pub proposed: String,
}

pub fn validate(predicate: &str) -> Result<(), UnknownPredicate> {
    if PREDICATES.contains(&predicate) {
        Ok(())
    } else {
        Err(UnknownPredicate {
            proposed: predicate.to_string(),
        })
    }
}

pub fn vocab_for_prompt() -> String {
    PREDICATES
        .iter()
        .filter(|&&p| p != "_unmapped")
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn count_is_24() {
        assert_eq!(PREDICATES.len(), 24);
    }

    #[test]
    fn all_entries_present() {
        let expected = [
            "is",
            "alternateName",
            "homeLocation",
            "born-on",
            "prefers",
            "dislikes",
            "values",
            "avoids",
            "current-focus",
            "status",
            "blocked-by",
            "available-at",
            "works-on",
            "decided",
            "responsible-for",
            "committed-to",
            "knows",
            "knowsAbout",
            "learned-that",
            "related-to",
            "recipient-of",
            "has",
            "plans-to",
            "_unmapped",
        ];
        for e in &expected {
            assert!(PREDICATES.contains(e), "missing predicate: {}", e);
        }
    }

    #[test]
    fn validate_accepts_known() {
        assert!(validate("is").is_ok());
        assert!(validate("prefers").is_ok());
        assert!(validate("_unmapped").is_ok());
    }

    #[test]
    fn validate_rejects_unknown_with_proposed() {
        let err = validate("invented-predicate").unwrap_err();
        assert_eq!(err.proposed, "invented-predicate");
    }

    #[test]
    fn no_duplicates() {
        let set: HashSet<&&str> = PREDICATES.iter().collect();
        assert_eq!(set.len(), PREDICATES.len());
    }
}
