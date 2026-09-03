//! An AI pull-request reviewer for `GitHub`, built on loopctl.
//!
//! Fetches a pull request's diff and changed files, drives an LLM agent
//! loop over them, and posts a single review whose inline findings are
//! validated against the diff before submission.
//!
//! Modules: `config` (configuration), `error` (the error enum),
//! `provider` (loopctl client factory), `github` (the REST layer behind
//! `PrGateway`), `diff` (the diff index and grounding authority),
//! `findings` (the model-output contracts), `tools` (the agent's
//! loopctl `Tool` surface), `review` (the runner that drives batches).

pub mod cli;
pub mod config;
pub mod diff;
pub mod error;
pub mod findings;
pub mod github;
pub mod provider;
pub mod review;
pub mod tools;
