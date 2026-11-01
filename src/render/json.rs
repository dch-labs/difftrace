//! The JSON sink: a pretty dump of the whole verdict.

use crate::contracts::RoundVerdict;

#[must_use]
pub fn render(verdict: &RoundVerdict) -> String {
    let mut json = serde_json::to_string_pretty(verdict).unwrap_or_default();
    json.push('\n');
    json
}
