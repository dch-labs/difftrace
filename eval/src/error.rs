//! Errors for the golden-set extractor.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum DtError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("git in {repo}: {message}")]
    Git { repo: PathBuf, message: String },
    #[error("transcript {run}: {message}")]
    Parse { run: String, message: String },
    #[error("cli: {0}")]
    Cli(String),
}
