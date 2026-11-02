//! The hosted sink: the shared sections plus the run-range framing.

use super::{RenderOptions, neutralize_mentions, sections};
use crate::contracts::RoundVerdict;

#[must_use]
pub fn render(verdict: &RoundVerdict, options: &RenderOptions) -> String {
    let body = sections(verdict, options);
    neutralize_mentions(&format!(
        "{body}<sub>difftrace review of {}..{}</sub>\n",
        verdict.range.base_sha, verdict.range.head_sha
    ))
}
