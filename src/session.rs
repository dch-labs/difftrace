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

pub struct LocalSession {
    pub config: DifftraceConfig,
    pub client: Arc<DifftraceClient>,
}

fn load_config(
    config_path: Option<&Path>,
) -> Result<(DifftraceConfig, std::collections::BTreeMap<String, String>), DifftraceError> {
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
    let env_file = crate::config::load_env_file();
    config.apply_env_overrides_with(&env_file)?;
    tracing::debug!(
        target: "difftrace::session",
        profile = ?config.provider.profile,
        model = ?config.provider.model,
        max_tokens = ?config.provider.max_tokens,
        log_level = ?config.log_level,
        env_file_entries = env_file.len(),
        "session resolved"
    );
    Ok((config, env_file))
}

pub fn open_local_session(config_path: Option<&Path>) -> Result<LocalSession, DifftraceError> {
    let (config, env_file) = load_config(config_path)?;
    let client = Arc::new(crate::provider::build_client_with(&config, &env_file)?);
    Ok(LocalSession { config, client })
}

pub fn open_session(repo: &str, config_path: Option<&Path>) -> Result<Session, DifftraceError> {
    let repo = crate::cli::parse_repo(repo).map_err(DifftraceError::Cli)?;
    let (config, env_file) = load_config(config_path)?;
    let client = Arc::new(crate::provider::build_client_with(&config, &env_file)?);
    let token = crate::config::layered_lookup("GITHUB_TOKEN", &env_file).ok_or(
        DifftraceError::MissingApiKey {
            env_var: "GITHUB_TOKEN",
        },
    )?;
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
