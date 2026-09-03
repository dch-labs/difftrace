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
    Zai,
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
    pub batch_files: usize,
    pub max_turns: usize,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            max_findings_per_file: 5,
            batch_files: 4,
            max_turns: 16,
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
    pub fn apply_env_overrides(&mut self) -> Result<(), crate::error::DifftraceError> {
        if let Some(profile) = std::env::var("DIFFTRACE_PROFILE")
            .ok()
            .filter(|value| !value.is_empty())
        {
            self.provider.profile =
                parse_profile(&profile).map_err(crate::error::DifftraceError::Cli)?;
        }
        if let Some(model) = std::env::var("DIFFTRACE_MODEL")
            .ok()
            .filter(|value| !value.is_empty())
        {
            self.provider.model = Some(model);
        }
        Ok(())
    }

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

pub fn parse_profile(raw: &str) -> Result<ProviderProfile, String> {
    match raw {
        "anthropic" => Ok(ProviderProfile::Anthropic),
        "openai" => Ok(ProviderProfile::OpenAi),
        "zai" => Ok(ProviderProfile::Zai),
        "ollama" => Ok(ProviderProfile::Ollama),
        other => Err(format!(
            "unknown provider profile {other:?}; expected anthropic, openai, zai, or ollama"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_the_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let cfg = DifftraceConfig::from_toml_str("")?;
        assert_eq!(cfg, DifftraceConfig::default());
        Ok(())
    }

    #[test]
    fn an_unknown_provider_profile_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "[provider]\nprofile = \"grok\"\n";
        let err = DifftraceConfig::from_toml_str(raw)
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, DifftraceError::ConfigParse { .. }));
        Ok(())
    }

    #[test]
    fn a_partial_provider_section_keeps_other_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "[provider]\nmodel = \"claude-test\"\n";
        let cfg = DifftraceConfig::from_toml_str(raw)?;
        assert_eq!(cfg.provider.model.as_deref(), Some("claude-test"));
        assert_eq!(cfg.provider.profile, ProviderProfile::Anthropic);
        assert_eq!(cfg.review, ReviewSettings::default());
        Ok(())
    }

    #[test]
    fn review_settings_override_their_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "[review]\nbatch_files = 8\n";
        let cfg = DifftraceConfig::from_toml_str(raw)?;
        assert_eq!(cfg.review.batch_files, 8);
        assert_eq!(cfg.review.max_findings_per_file, 5);
        Ok(())
    }

    #[test]
    fn a_full_config_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let cfg = DifftraceConfig::default();
        let raw = toml::to_string(&cfg)?;
        let back = DifftraceConfig::from_toml_str(&raw)?;
        assert_eq!(cfg, back);
        Ok(())
    }

    #[test]
    fn a_missing_config_file_yields_the_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let cfg = DifftraceConfig::load_from(Path::new("/nonexistent-difftrace/config.toml"))?;
        assert_eq!(cfg, DifftraceConfig::default());
        Ok(())
    }

    #[test]
    fn profile_strings_parse_to_their_variants() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(parse_profile("anthropic")?, ProviderProfile::Anthropic);
        assert_eq!(parse_profile("openai")?, ProviderProfile::OpenAi);
        assert_eq!(parse_profile("zai")?, ProviderProfile::Zai);
        assert_eq!(parse_profile("ollama")?, ProviderProfile::Ollama);
        Ok(())
    }

    #[test]
    fn an_unknown_profile_string_names_the_valid_ones() -> Result<(), Box<dyn std::error::Error>> {
        let Err(err) = parse_profile("grok") else {
            return Err("expected an unknown profile to fail".into());
        };
        assert!(err.contains("anthropic"), "error lists the options: {err}");
        assert!(err.contains("zai"), "error lists the options: {err}");
        Ok(())
    }

    #[test]
    fn the_env_profile_override_replaces_the_configured_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_PROFILE"]);
        env.set("DIFFTRACE_PROFILE", "zai");
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides()?;
        assert_eq!(config.provider.profile, ProviderProfile::Zai);
        env.remove("DIFFTRACE_PROFILE");
        Ok(())
    }

    #[test]
    fn an_unset_or_empty_env_override_keeps_the_config() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_PROFILE"]);
        env.remove("DIFFTRACE_PROFILE");
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides()?;
        assert_eq!(config.provider.profile, ProviderProfile::Anthropic);
        env.set("DIFFTRACE_PROFILE", "");
        config.apply_env_overrides()?;
        assert_eq!(config.provider.profile, ProviderProfile::Anthropic);
        Ok(())
    }

    #[test]
    fn the_env_model_override_replaces_the_configured_model()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_MODEL"]);
        env.set("DIFFTRACE_MODEL", "glm-4.8");
        let mut config = DifftraceConfig::default();
        config.provider.model = Some("glm-4.7".to_owned());
        config.apply_env_overrides()?;
        assert_eq!(config.provider.model.as_deref(), Some("glm-4.8"));
        Ok(())
    }

    #[test]
    fn the_env_model_override_supplies_a_model_when_none_is_configured()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_MODEL"]);
        env.set("DIFFTRACE_MODEL", "glm-4.8");
        let mut config = DifftraceConfig::default();
        assert_eq!(config.provider.model, None);
        config.apply_env_overrides()?;
        assert_eq!(config.provider.model.as_deref(), Some("glm-4.8"));
        Ok(())
    }

    #[test]
    fn an_unset_or_empty_env_model_override_keeps_the_config()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_MODEL"]);
        env.set("DIFFTRACE_MODEL", "");
        let mut config = DifftraceConfig::default();
        config.provider.model = Some("glm-4.7".to_owned());
        config.apply_env_overrides()?;
        assert_eq!(config.provider.model.as_deref(), Some("glm-4.7"));
        Ok(())
    }

    #[test]
    fn an_invalid_env_override_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_PROFILE"]);
        env.set("DIFFTRACE_PROFILE", "nonsense");
        let mut config = DifftraceConfig::default();
        let Err(err) = config.apply_env_overrides() else {
            return Err("expected the invalid override to fail".into());
        };
        assert!(err.to_string().contains("nonsense"));
        Ok(())
    }
}
