//! The error enum for all difftrace operations.

use std::path::PathBuf;

/// Errors returned by difftrace.
///
/// Every failure path in the crate funnels into this enum so callers get a
/// typed, displayable cause instead of a stringly-typed message.
#[derive(Debug, thiserror::Error)]
pub enum DifftraceError {
    /// The configuration file exists but could not be read.
    #[error("cannot read config file {path}: {source}")]
    ConfigRead {
        /// The path whose read attempt failed.
        ///
        /// Reported verbatim so the user can see which file is misbehaving.
        path: PathBuf,

        /// The underlying I/O error.
        ///
        /// Preserved for callers that distinguish permission errors from
        /// transient filesystem failures.
        source: std::io::Error,
    },

    /// The configuration file is not valid TOML for the config schema.
    #[error("invalid config file: {source}")]
    ConfigParse {
        /// The underlying TOML deserialization error.
        ///
        /// Carries the line/column position of the offending value.
        source: toml::de::Error,
    },

    /// The user home directory could not be determined.
    #[error("cannot determine the home directory for the config path")]
    NoHomeDir,

    /// A required provider API key is absent from the environment.
    #[error("no API key: set {env_var}")]
    MissingApiKey {
        /// The environment variable expected to carry the key.
        ///
        /// Named in the message so the user knows exactly what to export.
        env_var: &'static str,
    },

    /// The `Ollama` profile was selected without a model.
    #[error("ollama profile requires provider.model to be set in the config")]
    OllamaModelMissing,

    /// A provider client could not be constructed.
    #[error("provider client construction failed: {source}")]
    ClientBuild {
        /// The underlying provider error.
        ///
        /// Typically an invalid `base_url`; key and model problems are
        /// caught earlier with more specific variants.
        source: loopctl::api::error::ApiError,
    },
}
