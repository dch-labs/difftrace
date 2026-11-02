//! Shared machinery for the difftrace eval tooling: transcript
//! extraction, fix-commit correlation, the mutation set, and metrics.

pub mod correlate;
pub mod error;
pub mod extract;
pub mod metrics;
pub mod mutations;
pub mod report;
