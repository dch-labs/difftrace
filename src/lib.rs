//! An AI pull-request reviewer for `GitHub`, built on loopctl.
//!
//! difftrace fetches a pull request's diff and changed files, drives an LLM
//! agent loop over them, and posts a single review whose inline findings are
//! validated against the diff before submission.
//!
//! # Module Overview
//!
//! - **[`config`]** — Configuration loading and validation.
//! - **[`error`]** — The error enum ([`error::DifftraceError`]) for all
//!   difftrace operations.
//! - **[`provider`]** — Factory from config to the loopctl API client
//!   ([`provider::DifftraceClient`]).

// Relax strict lints in test code. The crate enforces a strict no-panic /
// no-unwrap policy in production code, but test code legitimately uses
// assertions, unwrap, indexing, etc. for readability.
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::missing_panics_doc,
        clippy::missing_errors_doc,
        clippy::unnecessary_wraps,
        clippy::clone_on_ref_ptr,
        clippy::doc_markdown,
        clippy::field_reassign_with_default,
        clippy::used_underscore_items,
        clippy::wildcard_imports,
    )
)]

pub mod config;
pub mod error;
pub mod provider;
