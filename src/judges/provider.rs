//! Profile-named provider clients behind the judge panel.

use loopctl::provider::{self, AnthropicClient, OpenAiClient};

use crate::config::EnvSource;
use crate::error::DtError;

pub enum JudgeClient {
    Zai(AnthropicClient),
    Anthropic(AnthropicClient),
    Local(OpenAiClient),
}

fn required(env: &EnvSource, name: &str) -> Result<String, DtError> {
    env.lookup(name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| DtError::Config(format!("the local profile needs {name}")))
}

pub fn client_for_profile(profile: &str, max_tokens: u32) -> Result<JudgeClient, DtError> {
    let env = EnvSource::live();
    match profile.trim().to_lowercase().as_str() {
        "zai" => {
            let mut builder = provider::zai_builder().with_max_tokens(max_tokens);
            if let Some(key) = env
                .lookup("ZAI_API_KEY")
                .or_else(|| env.lookup("ZHIPUAI_API_KEY"))
            {
                builder = builder.with_api_key(key);
            }
            Ok(JudgeClient::Zai(builder.build().map_err(|err| {
                DtError::Config(format!("the zai profile could not start: {err}"))
            })?))
        }
        "anthropic" => {
            let mut builder = AnthropicClient::builder().with_max_tokens(max_tokens);
            if let Some(key) = env.lookup("ANTHROPIC_API_KEY") {
                builder = builder.with_api_key(key);
            }
            Ok(JudgeClient::Anthropic(builder.build().map_err(|err| {
                DtError::Config(format!("the anthropic profile could not start: {err}"))
            })?))
        }
        "local" => {
            let base_url = required(&env, "DIFFTRACE_LOCAL_BASE_URL")?;
            let model = required(&env, "DIFFTRACE_LOCAL_MODEL")?;
            Ok(JudgeClient::Local(
                provider::self_hosted(&base_url, &model).map_err(|err| {
                    DtError::Config(format!("the local profile could not start: {err}"))
                })?,
            ))
        }
        other => Err(DtError::Config(format!(
            "unknown judge profile {other:?}; expected zai, anthropic, or local"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopctl::testing::EnvGuard;

    #[test]
    fn unknown_profiles_error_naming_the_profile() {
        let outcome = client_for_profile("vibes", 1024);
        assert!(
            matches!(&outcome, Err(err) if err.to_string().contains("vibes")),
            "an unknown profile names itself"
        );
    }

    fn temp_home(name: &str) -> Result<std::path::PathBuf, std::io::Error> {
        let home =
            std::env::temp_dir().join(format!("difftrace-provider-{name}-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".difftrace"))?;
        Ok(home)
    }

    #[test]
    fn a_key_in_the_env_file_builds_when_the_process_env_is_clear() -> Result<(), std::io::Error> {
        let home = temp_home("file-key")?;
        std::fs::write(
            home.join(".difftrace").join("env"),
            "ZAI_API_KEY=file-key\n",
        )?;
        let guard = EnvGuard::acquire(&["HOME", "ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        guard.remove("ZAI_API_KEY");
        guard.remove("ZHIPUAI_API_KEY");
        guard.set("HOME", home.to_string_lossy().as_ref());
        let outcome = client_for_profile("zai", 1024);
        let _unused = std::fs::remove_dir_all(&home);
        assert!(outcome.is_ok(), "the env-file key starts the zai profile");
        Ok(())
    }

    #[test]
    fn with_no_key_anywhere_the_native_error_names_the_variable() -> Result<(), std::io::Error> {
        let home = temp_home("no-key")?;
        let guard = EnvGuard::acquire(&["HOME", "ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        guard.remove("ZAI_API_KEY");
        guard.remove("ZHIPUAI_API_KEY");
        guard.set("HOME", home.to_string_lossy().as_ref());
        let outcome = client_for_profile("zai", 1024);
        let _unused = std::fs::remove_dir_all(&home);
        assert!(
            matches!(&outcome, Err(err) if err.to_string().contains("ZAI_API_KEY")),
            "the builder's own missing-key error survives the layering"
        );
        Ok(())
    }

    #[test]
    fn local_knobs_resolve_from_the_env_file() -> Result<(), std::io::Error> {
        let home = temp_home("local-file")?;
        std::fs::write(
            home.join(".difftrace").join("env"),
            "DIFFTRACE_LOCAL_BASE_URL=http://localhost:11434\nDIFFTRACE_LOCAL_MODEL=qwen\n",
        )?;
        let guard =
            EnvGuard::acquire(&["HOME", "DIFFTRACE_LOCAL_BASE_URL", "DIFFTRACE_LOCAL_MODEL"]);
        guard.remove("DIFFTRACE_LOCAL_BASE_URL");
        guard.remove("DIFFTRACE_LOCAL_MODEL");
        guard.set("HOME", home.to_string_lossy().as_ref());
        let outcome = client_for_profile("local", 1024);
        let _unused = std::fs::remove_dir_all(&home);
        assert!(outcome.is_ok(), "the env file feeds the local profile");
        Ok(())
    }

    #[test]
    fn a_set_but_empty_local_base_url_errors_like_an_unset_one() -> Result<(), std::io::Error> {
        let home = temp_home("empty-base")?;
        let guard =
            EnvGuard::acquire(&["HOME", "DIFFTRACE_LOCAL_BASE_URL", "DIFFTRACE_LOCAL_MODEL"]);
        guard.set("HOME", home.to_string_lossy().as_ref());
        guard.set("DIFFTRACE_LOCAL_MODEL", "m");
        guard.remove("DIFFTRACE_LOCAL_BASE_URL");
        let unset = client_for_profile("local", 1024);
        guard.set("DIFFTRACE_LOCAL_BASE_URL", "");
        let empty = client_for_profile("local", 1024);
        guard.set("DIFFTRACE_LOCAL_BASE_URL", "   ");
        let blank = client_for_profile("local", 1024);
        let _unused = std::fs::remove_dir_all(&home);
        for outcome in [unset, empty, blank] {
            assert!(
                matches!(&outcome, Err(err) if err.to_string().contains("DIFFTRACE_LOCAL_BASE_URL")),
                "an absent, empty, or blank base URL fails at start naming the variable"
            );
        }
        Ok(())
    }
}
