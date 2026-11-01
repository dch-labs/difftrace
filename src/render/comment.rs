//! The hosted sink: the shared sections plus the run-range framing.

use super::{RenderOptions, sections};
use crate::contracts::RoundVerdict;

#[must_use]
pub fn render(verdict: &RoundVerdict, options: &RenderOptions) -> String {
    let body = sections(verdict, options);
    format!(
        "{body}<sub>difftrace review of {}..{}</sub>\n",
        verdict.range.base_sha, verdict.range.head_sha
    )
}
