//! Per-run session wiring: the config, the provider client, and the
//! GitHub gateway assembled once per command invocation and shared by
//! the review and chat flows.

use std::path::Path;
use std::sync::Arc;

use crate::config::DifftraceConfig;
use crate::error::DifftraceError;
use crate::github::GitHubClient;
use crate::provider::DifftraceClient;

pub struct Session {
    pub config: DifftraceConfig,
    pub client: Arc<DifftraceClient>,
    pub gateway: Arc<GitHubClient>,
}

pub fn open_session(repo: &str, config_path: Option<&Path>) -> Result<Session, DifftraceError> {
    let repo = crate::cli::parse_repo(repo).map_err(DifftraceError::Cli)?;
    let mut config = match config_path {
        Some(path) if !path.is_file() => {
            return Err(DifftraceError::Cli(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        Some(path) => DifftraceConfig::load_from(path)?,
        None => DifftraceConfig::load()?,
    };
    config.apply_env_overrides()?;
    let client = Arc::new(crate::provider::build_client(&config)?);
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(DifftraceError::MissingApiKey {
            env_var: "GITHUB_TOKEN",
        })?;
    let gateway = Arc::new(GitHubClient::new(
        token,
        repo,
        config.github.api_base_url.as_deref(),
    )?);
    Ok(Session {
        config,
        client,
        gateway,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_explicit_config_path_is_rejected_before_any_setup()
    -> Result<(), Box<dyn std::error::Error>> {
        let err = open_session("a/b", Some(std::path::Path::new("/nonexistent/dt.toml")))
            .err()
            .ok_or("expected the missing config path to fail the session")?;
        assert!(
            err.to_string().contains("config file not found"),
            "the failure names the config: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_malformed_repo_is_rejected_before_the_config_is_read()
    -> Result<(), Box<dyn std::error::Error>> {
        let error = open_session("justname", None)
            .err()
            .ok_or("a malformed repo must fail the session")?;
        assert!(error.to_string().contains("owner/repo"));
        Ok(())
    }
}
