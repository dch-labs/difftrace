//! Configuration loading and validation.
//!
//! difftrace reads a single TOML file at `~/.difftrace/config.toml`. Every
//! field has a default, so an absent file is a valid configuration; unknown
//! fields and malformed values are rejected with typed errors. Secrets
//! (provider API keys, the `GitHub` token) never live in this file — they
//! are read from the environment by the components that need them.
//!
//! # Examples
//!
//! ```
//! use difftrace::config::{DifftraceConfig, ProviderProfile};
//!
//! let cfg = DifftraceConfig::from_toml_str("").unwrap();
//! assert_eq!(cfg.provider.profile, ProviderProfile::Anthropic);
//! ```

use std::path::Path;

use crate::error::DifftraceError;

/// Which LLM provider a review run talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProfile {
    /// Anthropic Claude — the default profile.
    #[default]
    Anthropic,
    /// Any OpenAI-compatible endpoint, optionally with a custom `base_url`.
    OpenAi,
    /// A local Ollama server; requires an explicit `model`.
    Ollama,
}

/// Provider selection and model tuning.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Which provider client to build.
    ///
    /// Selects the loopctl client implementation the provider factory
    /// constructs. Defaults to [`ProviderProfile::Anthropic`].
    pub profile: ProviderProfile,

    /// Model identifier override.
    ///
    /// Passed to the provider client verbatim. `None` keeps the provider's
    /// own default model, except for [`ProviderProfile::Ollama`], where a
    /// model is required.
    pub model: Option<String>,

    /// Base URL override for OpenAI-compatible endpoints.
    ///
    /// `None` uses the provider's canonical endpoint. Ignored by the
    /// [`ProviderProfile::Anthropic`] profile.
    pub base_url: Option<String>,
}

/// `GitHub` endpoint configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    /// Base URL of the `GitHub` REST API.
    ///
    /// `None` targets `github.com`. Set this to a GitHub Enterprise URL
    /// (for example `https://github.example.com/api/v3`) to review pull
    /// requests there instead.
    pub api_base_url: Option<String>,
}

/// Review behaviour tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ReviewSettings {
    /// Cap on findings accepted per file in one batch review.
    ///
    /// Keeps a single pathological file from flooding the posted review.
    /// Defaults to 5.
    pub max_findings_per_file: usize,

    /// Context lines shown around each hunk when a file's diff is
    /// presented to the model.
    ///
    /// Larger values give the model more surrounding code at the cost of
    /// context budget. Defaults to 3.
    pub context_lines: usize,

    /// Changed files reviewed per agent run.
    ///
    /// Batching bounds the context each run carries. Defaults to 4.
    pub batch_files: usize,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            max_findings_per_file: 5,
            context_lines: 3,
            batch_files: 4,
        }
    }
}

/// The complete difftrace configuration.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DifftraceConfig {
    /// LLM provider selection and tuning.
    pub provider: ProviderConfig,

    /// `GitHub` endpoint configuration.
    pub github: GitHubConfig,

    /// Review behaviour tuning.
    pub review: ReviewSettings,
}

impl DifftraceConfig {
    /// Load the configuration from `~/.difftrace/config.toml`.
    ///
    /// A missing file yields the default configuration rather than an
    /// error, so difftrace runs with no setup beyond provider credentials
    /// in the environment.
    ///
    /// # Errors
    ///
    /// Returns [`DifftraceError::NoHomeDir`] when the home directory
    /// cannot be determined, and read or parse failures for a config file
    /// that exists but is unusable.
    pub fn load() -> Result<Self, DifftraceError> {
        let home = dirs::home_dir().ok_or(DifftraceError::NoHomeDir)?;
        let path = home.join(".difftrace").join("config.toml");
        Self::load_from(&path)
    }

    /// Load the configuration from an explicit path.
    ///
    /// Behaves like [`load`](Self::load): an absent file is the default
    /// configuration; an unreadable or unparseable file is an error.
    ///
    /// # Errors
    ///
    /// Returns a read or parse failure for a file that exists but is
    /// unusable.
    pub fn load_from(path: &Path) -> Result<Self, DifftraceError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::from_toml_str(&contents),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(DifftraceError::ConfigRead {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Parse a configuration from a TOML string.
    ///
    /// The same validation [`load`](Self::load) applies, exposed for tests
    /// and for embedding config strings.
    ///
    /// # Errors
    ///
    /// Returns [`DifftraceError::ConfigParse`] when the input is not valid
    /// TOML for the config schema.
    pub fn from_toml_str(contents: &str) -> Result<Self, DifftraceError> {
        toml::from_str(contents).map_err(|source| DifftraceError::ConfigParse { source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_input_yields_defaults() {
        let cfg = DifftraceConfig::from_toml_str("").unwrap();
        assert_eq!(cfg, DifftraceConfig::default());
    }

    #[test]
    fn test_unknown_provider_profile_rejected() {
        let raw = "[provider]\nprofile = \"grok\"\n";
        let err = DifftraceConfig::from_toml_str(raw).unwrap_err();
        assert!(matches!(err, DifftraceError::ConfigParse { .. }));
    }

    #[test]
    fn test_partial_provider_section_keeps_other_defaults() {
        let raw = "[provider]\nmodel = \"claude-test\"\n";
        let cfg = DifftraceConfig::from_toml_str(raw).unwrap();
        assert_eq!(cfg.provider.model.as_deref(), Some("claude-test"));
        assert_eq!(cfg.provider.profile, ProviderProfile::Anthropic);
        assert_eq!(cfg.review, ReviewSettings::default());
    }

    #[test]
    fn test_review_settings_override() {
        let raw = "[review]\nbatch_files = 8\n";
        let cfg = DifftraceConfig::from_toml_str(raw).unwrap();
        assert_eq!(cfg.review.batch_files, 8);
        assert_eq!(cfg.review.max_findings_per_file, 5);
    }

    #[test]
    fn test_full_config_round_trips() {
        let cfg = DifftraceConfig::default();
        let raw = toml::to_string(&cfg).unwrap();
        let back = DifftraceConfig::from_toml_str(&raw).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn test_load_from_missing_file_yields_defaults() {
        let cfg =
            DifftraceConfig::load_from(Path::new("/nonexistent-difftrace/config.toml")).unwrap();
        assert_eq!(cfg, DifftraceConfig::default());
    }
}
