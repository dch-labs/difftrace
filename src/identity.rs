//! Issue identity shared by the cross-round state keepers: how two
//! findings raised rounds apart, phrased differently, and anchored a few
//! lines apart are recognized as the same issue rather than minted as
//! duplicates.

use std::collections::BTreeSet;

pub const LINE_DRIFT_TOLERANCE: u64 = 5;

#[must_use]
pub fn title_tokens(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect()
}

#[must_use]
pub fn shares_any_token(a: &str, b: &str) -> bool {
    title_tokens(a)
        .intersection(&title_tokens(b))
        .next()
        .is_some()
}

#[must_use]
pub fn titles_overlap_enough(a: &str, b: &str) -> bool {
    let left = title_tokens(a);
    let right = title_tokens(b);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let shared = left.intersection(&right).count();
    let total = left.union(&right).count();
    total > 0 && shared.saturating_mul(2) >= total
}

#[must_use]
pub fn lines_within(a: u64, b: u64, span: u64) -> bool {
    a.max(b).saturating_sub(a.min(b)) <= span
}

#[must_use]
pub fn same_spot(line_a: u64, title_a: &str, line_b: u64, title_b: &str) -> bool {
    if line_a == line_b {
        return shares_any_token(title_a, title_b);
    }
    lines_within(line_a, line_b, LINE_DRIFT_TOLERANCE) && titles_overlap_enough(title_a, title_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rephrase_at_the_same_line_is_the_same_issue() {
        assert!(same_spot(
            40,
            "Lock is dropped too early",
            40,
            "The lock is dropped early"
        ));
    }

    #[test]
    fn a_disjoint_title_at_the_same_line_is_new() {
        assert!(!same_spot(
            40,
            "Lock is dropped too early",
            40,
            "Buffer grows unbounded"
        ));
    }

    #[test]
    fn a_drifted_line_with_half_the_wording_is_the_same_issue() {
        assert!(same_spot(
            40,
            "The clamp breaks the walk",
            43,
            "clamp breaks the walk differently"
        ));
    }

    #[test]
    fn a_far_drift_is_new_even_with_shared_wording() {
        assert!(!same_spot(
            40,
            "The clamp breaks the walk",
            52,
            "clamp breaks the walk differently"
        ));
    }

    #[test]
    fn titles_need_half_the_combined_wording() {
        assert!(titles_overlap_enough("one two three", "three two one"));
        assert!(!titles_overlap_enough("one two three", "three four five"));
        assert!(!titles_overlap_enough("— …", "( …)"));
    }

    #[test]
    fn tokens_are_lowercased_words_with_an_alphanumeric_char() {
        let tokens = title_tokens("Clamp the 42 (…) — guard")
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(
            tokens,
            vec![
                "42".to_owned(),
                "clamp".to_owned(),
                "guard".to_owned(),
                "the".to_owned()
            ]
        );
    }
}
