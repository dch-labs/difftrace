//! The review engine: `ReviewRunner` drives one loopctl `BareLoop` per
//! batch of changed files, records findings through the `record_findings`
//! tool, re-emits the review rubric at every turn boundary, captures a
//! trajectory, and summarizes the aggregated findings with structured
//! output.

pub mod batch;
pub mod logging;
pub mod record;
pub mod rubric;
pub mod runner;

pub use batch::ReviewOutcome;
pub use record::RecordFindingsTool;
pub use rubric::ReviewRubric;
pub use runner::ReviewRunner;
