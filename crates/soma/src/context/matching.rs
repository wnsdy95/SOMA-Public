//! Lightweight text matching for correction overrides.
//!
//! This is deliberately conservative: it only suppresses context when the
//! recorded stale claim is a direct substring after normalization, or all
//! meaningful stale-claim tokens appear in the candidate text.

use std::collections::HashSet;

pub(crate) fn stale_claim_matches_text(claim: &str, candidate_text: &str) -> bool {
    let claim_norm = normalize_match_text(claim);
    if claim_norm.len() < 6 {
        return false;
    }
    let candidate_norm = normalize_match_text(candidate_text);
    if candidate_norm.contains(&claim_norm) {
        return true;
    }

    let claim_tokens = match_tokens(claim);
    if claim_tokens.len() < 2 {
        return false;
    }
    let candidate_tokens = match_tokens(candidate_text);
    claim_tokens.iter().all(|token| candidate_tokens.contains(token))
}

fn normalize_match_text(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn match_tokens(s: &str) -> HashSet<String> {
    normalize_match_text(s)
        .split_whitespace()
        .filter(|token| token.chars().count() >= 2)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_match_ignores_case_and_punctuation() {
        assert!(stale_claim_matches_text("Voice is core", "voice is the core product"));
    }

    #[test]
    fn token_match_allows_small_insertions() {
        assert!(stale_claim_matches_text("cargo test", "Open contradiction: cargo test failed"));
    }

    #[test]
    fn short_claims_do_not_match() {
        assert!(!stale_claim_matches_text("go", "go test failed"));
    }
}
