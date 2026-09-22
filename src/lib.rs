//! difftrace v2 — a diff-review agent with a contractual model of
//! truth: the product of a run is a coverage matrix, findings are
//! exceptions confirmed by a judge quorum or mechanical verification,
//! and the verdict is assembled mechanically.

pub mod cli;
pub mod config;
pub mod error;
pub mod identity;

pub mod aspects;
pub mod contracts;
pub mod diff;
pub mod gateway;
pub mod judges;
pub mod oracle;
pub mod orchestrator;
pub mod ratelimit;
pub mod reconcile;
pub mod render;
pub mod reply;
pub mod state;
pub mod tools;
