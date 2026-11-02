//! The console sink: the shared sections printed as-is.

use super::{RenderOptions, sections};
use crate::contracts::RoundVerdict;

#[must_use]
pub fn render(verdict: &RoundVerdict, options: &RenderOptions) -> String {
    sections(verdict, options)
}
