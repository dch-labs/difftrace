//! Configuration loading: `~/.difftrace/config.toml` into
//! [`DifftraceConfig`]. Every field defaults, so an absent file is valid;
//! secrets never live in the file — they resolve from the environment.

use std::path::Path;

use crate::error::DifftraceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProfile {
    #[default]
    Anthropic,
    OpenAi,
    Ollama,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub profile: ProviderProfile,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    pub api_base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ReviewSettings {
    pub max_findings_per_file: usize,
    pub context_lines: usize,
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

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DifftraceConfig {
    pub provider: ProviderConfig,
    pub github: GitHubConfig,
    pub review: ReviewSettings,
}

impl DifftraceConfig {
    pub fn load() -> Result<Self, DifftraceError> {
        let home = dirs::home_dir().ok_or(DifftraceError::NoHomeDir)?;
        let path = home.join(".difftrace").join("config.toml");
        Self::load_from(&path)
    }

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

    pub fn from_toml_str(contents: &str) -> Result<Self, DifftraceError> {
        toml::from_str(contents).map_err(|source| DifftraceError::ConfigParse { source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_the_defaults() {
        let cfg = DifftraceConfig::from_toml_str("").unwrap();
        assert_eq!(cfg, DifftraceConfig::default());
    }

    #[test]
    fn an_unknown_provider_profile_is_rejected() {
        let raw = "[provider]\nprofile = \"grok\"\n";
        let err = DifftraceConfig::from_toml_str(raw).unwrap_err();
        assert!(matches!(err, DifftraceError::ConfigParse { .. }));
    }

    #[test]
    fn a_partial_provider_section_keeps_other_defaults() {
        let raw = "[provider]\nmodel = \"claude-test\"\n";
        let cfg = DifftraceConfig::from_toml_str(raw).unwrap();
        assert_eq!(cfg.provider.model.as_deref(), Some("claude-test"));
        assert_eq!(cfg.provider.profile, ProviderProfile::Anthropic);
        assert_eq!(cfg.review, ReviewSettings::default());
    }

    #[test]
    fn review_settings_override_their_defaults() {
        let raw = "[review]\nbatch_files = 8\n";
        let cfg = DifftraceConfig::from_toml_str(raw).unwrap();
        assert_eq!(cfg.review.batch_files, 8);
        assert_eq!(cfg.review.max_findings_per_file, 5);
    }

    #[test]
    fn a_full_config_round_trips() {
        let cfg = DifftraceConfig::default();
        let raw = toml::to_string(&cfg).unwrap();
        let back = DifftraceConfig::from_toml_str(&raw).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn a_missing_config_file_yields_the_defaults() {
        let cfg =
            DifftraceConfig::load_from(Path::new("/nonexistent-difftrace/config.toml")).unwrap();
        assert_eq!(cfg, DifftraceConfig::default());
    }
}
