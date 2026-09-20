//! Issue identity shared by the cross-round state keepers: how two
//! findings raised rounds apart, phrased differently, and anchored a few
//! lines apart are recognized as the same issue rather than minted as
//! duplicates.

use std::collections::BTreeSet;

pub(crate) const LINE_DRIFT_TOLERANCE: u64 = 5;

pub(crate) fn title_tokens(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect()
}

pub(crate) fn shares_any_token(a: &str, b: &str) -> bool {
    title_tokens(a)
        .intersection(&title_tokens(b))
        .next()
        .is_some()
}

pub(crate) fn titles_overlap_enough(a: &str, b: &str) -> bool {
    let left = title_tokens(a);
    let right = title_tokens(b);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let shared = left.intersection(&right).count();
    let total = left.union(&right).count();
    total > 0 && shared.saturating_mul(2) >= total
}

pub(crate) fn lines_within(a: u64, b: u64, span: u64) -> bool {
    a.max(b).saturating_sub(a.min(b)) <= span
}

pub(crate) fn same_spot(line_a: u64, title_a: &str, line_b: u64, title_b: &str) -> bool {
    if line_a == line_b {
        return shares_any_token(title_a, title_b);
    }
    lines_within(line_a, line_b, LINE_DRIFT_TOLERANCE) && titles_overlap_enough(title_a, title_b)
}
