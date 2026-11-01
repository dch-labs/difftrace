//! Profile-named provider clients behind the judge panel.

use loopctl::provider::self_hosted_builder;
use loopctl::provider::{self, AnthropicClient, OpenAiClient};

use crate::config::EnvSource;
use crate::error::DtError;

pub enum JudgeClient {
    Zai(AnthropicClient),
    Anthropic(AnthropicClient),
    Local(OpenAiClient),
}

fn optional(env: &EnvSource, name: &str) -> Option<String> {
    env.lookup(name).filter(|value| !value.trim().is_empty())
}

fn reject_blank(env: &EnvSource, names: &[&str]) -> Result<(), DtError> {
    for name in names {
        if env
            .lookup(name)
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(DtError::Config(format!(
                "{name} is set but whitespace-only — a blank credential would fail every call; unset it or paste the real value"
            )));
        }
    }
    Ok(())
}

fn required(env: &EnvSource, name: &str) -> Result<String, DtError> {
    env.lookup(name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| DtError::Config(format!("the local profile needs {name}")))
}

fn split_profile(profile: &str) -> Result<(&str, Option<&str>), DtError> {
    match profile.split_once(':') {
        Some((name, model)) if !model.trim().is_empty() => Ok((name, Some(model.trim()))),
        Some((_, _)) => Err(DtError::Config(format!(
            "judge profile {profile:?} names no model after the colon"
        ))),
        None => Ok((profile, None)),
    }
}

pub fn client_for_profile(
    profile: &str,
    settings: crate::config::ModelSettings,
) -> Result<JudgeClient, DtError> {
    let env = EnvSource::live();
    let (name, model) = split_profile(profile)?;
    let effort = settings.effort.map(effort_of);
    match name.trim().to_lowercase().as_str() {
        "zai" => {
            reject_blank(&env, &["ZAI_API_KEY", "ZHIPUAI_API_KEY"])?;
            let mut builder = provider::zai_builder().with_max_tokens(settings.max_tokens);
            if let Some(effort) = effort {
                builder = builder.thinking_effort(effort);
            }
            if settings.replay_reasoning {
                builder = builder.replay_reasoning(true);
            }
            match model {
                Some(model) => builder = builder.with_model(model),
                None => {
                    if let Some(model) = optional(&env, "ZAI_MODEL") {
                        builder = builder.with_model(model);
                    }
                }
            }
            if let Some(key) =
                optional(&env, "ZAI_API_KEY").or_else(|| optional(&env, "ZHIPUAI_API_KEY"))
            {
                builder = builder.with_api_key(key);
            }
            Ok(JudgeClient::Zai(builder.build().map_err(|err| {
                DtError::Config(format!("the zai profile could not start: {err}"))
            })?))
        }
        "anthropic" => {
            reject_blank(&env, &["ANTHROPIC_API_KEY"])?;
            let mut builder = AnthropicClient::builder().with_max_tokens(settings.max_tokens);
            if let Some(effort) = effort {
                builder = builder.thinking_effort(effort);
            }
            if settings.replay_reasoning {
                builder = builder.replay_reasoning(true);
            }
            match model {
                Some(model) => builder = builder.with_model(model),
                None => {
                    if let Some(model) = optional(&env, "ANTHROPIC_MODEL") {
                        builder = builder.with_model(model);
                    }
                }
            }
            if let Some(key) = optional(&env, "ANTHROPIC_API_KEY") {
                builder = builder.with_api_key(key);
            }
            Ok(JudgeClient::Anthropic(builder.build().map_err(|err| {
                DtError::Config(format!("the anthropic profile could not start: {err}"))
            })?))
        }
        "local" => {
            if settings.max_tokens != crate::config::DEFAULT_MAX_TOKENS {
                return Err(DtError::Config(format!(
                    "the local profile does not take max_tokens ({} set; the self-hosted endpoint owns its output ceiling) — drop judges.max_tokens or the [[judges.model]] entry for this profile",
                    settings.max_tokens
                )));
            }
            let base_url = required(&env, "DIFFTRACE_LOCAL_BASE_URL")?;
            let model = match model {
                Some(model) => model.to_owned(),
                None => required(&env, "DIFFTRACE_LOCAL_MODEL")?,
            };
            let mut builder = self_hosted_builder(&model).with_base_url(&base_url);
            if let Some(effort) = effort {
                builder = builder.thinking_effort(effort);
            }
            if settings.replay_reasoning {
                builder = builder.replay_reasoning(true);
            }
            Ok(JudgeClient::Local(builder.build().map_err(|err| {
                DtError::Config(format!("the local profile could not start: {err}"))
            })?))
        }
        _ => Err(DtError::Config(format!(
            "unknown judge profile {profile:?}; expected zai, anthropic, or local (each may carry :model)"
        ))),
    }
}

fn effort_of(rung: crate::config::EffortRung) -> crate::loopctl_shim::ThinkingEffort {
    use crate::config::EffortRung;
    match rung {
        EffortRung::None => crate::loopctl_shim::ThinkingEffort::None,
        EffortRung::Minimal => crate::loopctl_shim::ThinkingEffort::Minimal,
        EffortRung::Low => crate::loopctl_shim::ThinkingEffort::Low,
        EffortRung::Medium => crate::loopctl_shim::ThinkingEffort::Medium,
        EffortRung::High => crate::loopctl_shim::ThinkingEffort::High,
        EffortRung::Xhigh => crate::loopctl_shim::ThinkingEffort::Xhigh,
        EffortRung::Max => crate::loopctl_shim::ThinkingEffort::Max,
    }
}

#[cfg(test)]
fn default_settings() -> crate::config::ModelSettings {
    crate::config::ModelSettings {
        effort: None,
        replay_reasoning: false,
        max_tokens: crate::config::DEFAULT_MAX_TOKENS,
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
    }
}

#[cfg(test)]
fn settings_of(max_tokens: u32) -> crate::config::ModelSettings {
    crate::config::ModelSettings {
        effort: None,
        replay_reasoning: false,
        max_tokens,
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopctl::api::ApiClient;
    use loopctl::testing::EnvGuard;

    #[test]
    fn a_local_profile_rejects_a_non_default_max_tokens_loudly() {
        let outcome = client_for_profile("local", settings_of(65_536));
        let err = outcome.err().map(|err| err.to_string()).unwrap_or_default();
        assert!(
            err.contains("max_tokens") && err.contains("65536"),
            "the rejection names the knob and its value: {err}"
        );
    }

    #[test]
    fn unknown_profiles_error_naming_the_profile() {
        let outcome = client_for_profile("vibes", settings_of(1024));
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
        let outcome = client_for_profile("zai", settings_of(1024));
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
        let outcome = client_for_profile("zai", settings_of(1024));
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
        let outcome = client_for_profile("local", default_settings());
        let _unused = std::fs::remove_dir_all(&home);
        assert!(outcome.is_ok(), "the env file feeds the local profile");
        Ok(())
    }

    #[test]
    fn whitespace_only_secrets_and_models_count_as_unset() -> Result<(), std::io::Error> {
        let home = temp_home("blank-key")?;
        let guard = EnvGuard::acquire(&[
            "HOME",
            "ZAI_API_KEY",
            "ZHIPUAI_API_KEY",
            "ZAI_MODEL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_MODEL",
        ]);
        guard.set("HOME", home.to_string_lossy().as_ref());
        guard.set("ZAI_API_KEY", "   ");
        guard.set("ZAI_MODEL", "  ");
        let no_key = client_for_profile("zai", settings_of(1024));
        guard.set("ZAI_API_KEY", "real-key");
        let with_key = client_for_profile("zai", settings_of(1024));
        guard.set("ANTHROPIC_API_KEY", "	");
        let blank_anthropic = client_for_profile("anthropic", settings_of(1024));
        let _unused = std::fs::remove_dir_all(&home);
        let zai_cause = no_key.err().map(|err| err.to_string()).unwrap_or_default();
        assert!(
            zai_cause.contains("ZAI_API_KEY") && zai_cause.contains("whitespace-only"),
            "a whitespace key is a loud configuration error naming the variable: {zai_cause}"
        );
        assert!(
            with_key.is_ok(),
            "a real key beside blank neighbors still starts"
        );
        let anthropic_cause = blank_anthropic
            .err()
            .map(|err| err.to_string())
            .unwrap_or_default();
        assert!(
            anthropic_cause.contains("ANTHROPIC_API_KEY")
                && anthropic_cause.contains("whitespace-only"),
            "the anthropic arm rejects blanks the same way: {anthropic_cause}"
        );
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
        let unset = client_for_profile("local", default_settings());
        guard.set("DIFFTRACE_LOCAL_BASE_URL", "");
        let empty = client_for_profile("local", default_settings());
        guard.set("DIFFTRACE_LOCAL_BASE_URL", "   ");
        let blank = client_for_profile("local", default_settings());
        let _unused = std::fs::remove_dir_all(&home);
        for outcome in [unset, empty, blank] {
            assert!(
                matches!(&outcome, Err(err) if err.to_string().contains("DIFFTRACE_LOCAL_BASE_URL")),
                "an absent, empty, or blank base URL fails at start naming the variable"
            );
        }
        Ok(())
    }

    #[test]
    fn every_default_panel_profile_starts() -> Result<(), std::io::Error> {
        let home = temp_home("panel")?;
        std::fs::write(
            home.join(".difftrace").join("env"),
            "ZAI_API_KEY=file-key\nDIFFTRACE_LOCAL_BASE_URL=http://localhost:11434\nDIFFTRACE_LOCAL_MODEL=qwen\n",
        )?;
        let guard = EnvGuard::acquire(&[
            "HOME",
            "ZAI_API_KEY",
            "ZHIPUAI_API_KEY",
            "DIFFTRACE_LOCAL_BASE_URL",
            "DIFFTRACE_LOCAL_MODEL",
        ]);
        guard.remove("ZAI_API_KEY");
        guard.remove("ZHIPUAI_API_KEY");
        guard.remove("DIFFTRACE_LOCAL_BASE_URL");
        guard.remove("DIFFTRACE_LOCAL_MODEL");
        guard.set("HOME", home.to_string_lossy().as_ref());
        let failures: Vec<String> = crate::config::DEFAULT_JUDGE_PANEL
            .iter()
            .filter_map(|profile| {
                client_for_profile(profile, default_settings())
                    .err()
                    .map(|err| format!("{profile}: {err}"))
            })
            .collect();
        let _unused = std::fs::remove_dir_all(&home);
        assert!(
            failures.is_empty(),
            "every shipped default profile must start: {failures:?}"
        );
        Ok(())
    }

    #[test]
    fn the_profile_model_part_reaches_the_client() -> Result<(), std::io::Error> {
        let home = temp_home("model-part")?;
        std::fs::write(
            home.join(".difftrace").join("env"),
            "ZAI_API_KEY=file-key\n",
        )?;
        let guard = EnvGuard::acquire(&["HOME", "ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        guard.remove("ZAI_API_KEY");
        guard.remove("ZHIPUAI_API_KEY");
        guard.set("HOME", home.to_string_lossy().as_ref());
        let zai = client_for_profile("zai:glm-5.3", settings_of(1024));
        let _unused = std::fs::remove_dir_all(&home);
        let model = match &zai {
            Ok(JudgeClient::Zai(client)) => Some(ApiClient::model(client)),
            _ => None,
        };
        assert_eq!(
            model.as_deref(),
            Some("glm-5.3"),
            "the model part overrides the builder default"
        );
        Ok(())
    }

    #[test]
    fn a_bare_zai_profile_honors_the_env_model() -> Result<(), std::io::Error> {
        let home = temp_home("env-model")?;
        std::fs::write(
            home.join(".difftrace").join("env"),
            "ZAI_API_KEY=file-key\nZAI_MODEL=glm-5.3-flash\n",
        )?;
        let guard = EnvGuard::acquire(&["HOME", "ZAI_API_KEY", "ZHIPUAI_API_KEY", "ZAI_MODEL"]);
        guard.remove("ZAI_API_KEY");
        guard.remove("ZHIPUAI_API_KEY");
        guard.remove("ZAI_MODEL");
        guard.set("HOME", home.to_string_lossy().as_ref());
        let zai = client_for_profile("zai", settings_of(1024));
        let _unused = std::fs::remove_dir_all(&home);
        let model = match &zai {
            Ok(JudgeClient::Zai(client)) => Some(ApiClient::model(client)),
            _ => None,
        };
        assert_eq!(
            model.as_deref(),
            Some("glm-5.3-flash"),
            "a bare profile takes its model from the layered env, env file included"
        );
        Ok(())
    }

    #[test]
    fn the_profile_model_part_wins_over_the_env_model() -> Result<(), std::io::Error> {
        let home = temp_home("profile-wins")?;
        std::fs::write(
            home.join(".difftrace").join("env"),
            "ZAI_API_KEY=file-key\nZAI_MODEL=glm-5.3-flash\n",
        )?;
        let guard = EnvGuard::acquire(&["HOME", "ZAI_API_KEY", "ZHIPUAI_API_KEY", "ZAI_MODEL"]);
        guard.remove("ZAI_API_KEY");
        guard.remove("ZHIPUAI_API_KEY");
        guard.remove("ZAI_MODEL");
        guard.set("HOME", home.to_string_lossy().as_ref());
        let zai = client_for_profile("zai:glm-5.3", settings_of(1024));
        let _unused = std::fs::remove_dir_all(&home);
        let model = match &zai {
            Ok(JudgeClient::Zai(client)) => Some(ApiClient::model(client)),
            _ => None,
        };
        assert_eq!(
            model.as_deref(),
            Some("glm-5.3"),
            "the explicit profile model beats the env fallback"
        );
        Ok(())
    }

    #[test]
    fn a_local_profile_model_part_spares_the_env_requirement() -> Result<(), std::io::Error> {
        let home = temp_home("local-model")?;
        let guard =
            EnvGuard::acquire(&["HOME", "DIFFTRACE_LOCAL_BASE_URL", "DIFFTRACE_LOCAL_MODEL"]);
        guard.set("HOME", home.to_string_lossy().as_ref());
        guard.set("DIFFTRACE_LOCAL_BASE_URL", "http://localhost:11434");
        guard.remove("DIFFTRACE_LOCAL_MODEL");
        let outcome = client_for_profile("local:qwen", default_settings());
        let _unused = std::fs::remove_dir_all(&home);
        let model = match &outcome {
            Ok(JudgeClient::Local(client)) => Some(ApiClient::model(client)),
            _ => None,
        };
        assert_eq!(
            model.as_deref(),
            Some("qwen"),
            "the model part feeds the self-hosted client"
        );
        Ok(())
    }

    #[test]
    fn a_colon_with_no_model_after_it_rejects() {
        let outcome = client_for_profile("zai:", settings_of(1024));
        assert!(
            outcome.is_err_and(|err| err.to_string().contains("zai:")),
            "an empty model part is a config error, never a silent bare profile"
        );
    }
}
