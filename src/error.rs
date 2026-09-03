//! The error enum for all difftrace operations.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum DifftraceError {
    #[error("cannot read config file {path}: {source}")]
    ConfigRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config file: {source}")]
    ConfigParse { source: toml::de::Error },
    #[error("cannot determine the home directory for the config path")]
    NoHomeDir,
    #[error("no API key: set {env_var}")]
    MissingApiKey { env_var: &'static str },
    #[error("ollama profile requires provider.model to be set in the config")]
    OllamaModelMissing,
    #[error("provider client construction failed: {source}")]
    ClientBuild {
        source: loopctl::api::error::ApiError,
    },
    #[error("invalid GitHub API base URL {url}: {source}")]
    InvalidBaseUrl {
        url: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("GitHub client construction failed: {source}")]
    GitHubInit { source: octocrab::Error },
    #[error("GitHub API call failed: {source}")]
    GitHubApi { source: octocrab::Error },
    #[error("{path} is a directory or empty entry, not a readable file")]
    NotAFile { path: String },
    #[error("cannot decode content of {path}: {source}")]
    ContentDecode {
        path: String,
        source: base64::DecodeError,
    },
    #[error("{path} is binary content; only text files can be reviewed")]
    BinaryContent { path: String },
    #[error("{path} is {size} bytes; the contents endpoint returns no content above 1 MB")]
    ContentTooLarge { path: String, size: i64 },
    #[error("cannot parse diff at line {line}: {reason}")]
    DiffParse { line: usize, reason: String },

    #[error("review run failed: {source}")]
    ReviewRun { source: loopctl::error::LoopError },

    #[error("summary generation failed: {source}")]
    Summary {
        source: loopctl::structured::StructuredError,
    },
}
