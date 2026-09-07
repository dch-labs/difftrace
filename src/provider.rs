//! Builds the loopctl API client from configuration: the
//! [`DifftraceClient`] enum (Anthropic / OpenAI protocol; Ollama rides
//! the OpenAI protocol at a local endpoint). Keys come from the
//! environment only.

use std::future::Future;
use std::pin::Pin;

use futures::Stream;
use loopctl::api::ApiClient;
use loopctl::api::NonStreamingResponse;
use loopctl::api::StreamRequest;
use loopctl::api::error::ApiError;
use loopctl::message::Message;
use loopctl::provider::AnthropicClient;
use loopctl::provider::OpenAiClient;
use loopctl::stream::StreamEvent;

use crate::config::DifftraceConfig;
use crate::config::ProviderProfile;
use crate::error::DifftraceError;

const NO_AUTH_KEY: &str = "ollama";

const OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

const DEFAULT_OUTPUT_BUDGET: u32 = 8192;

#[must_use]
pub fn enforced_output_budget(cfg: &DifftraceConfig) -> Option<u32> {
    match cfg.provider.profile {
        ProviderProfile::Anthropic | ProviderProfile::Zai => Some(budget(cfg)),
        ProviderProfile::OpenAi | ProviderProfile::Ollama => None,
    }
}

fn budget(cfg: &DifftraceConfig) -> u32 {
    cfg.provider.max_tokens.unwrap_or(DEFAULT_OUTPUT_BUDGET)
}

pub enum DifftraceClient {
    Anthropic(AnthropicClient),
    OpenAi(OpenAiClient),
}

impl std::fmt::Debug for DifftraceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic(_) => f.pad("DifftraceClient::Anthropic(..)"),
            Self::OpenAi(_) => f.pad("DifftraceClient::OpenAi(..)"),
        }
    }
}

impl ApiClient for DifftraceClient {
    fn model(&self) -> String {
        match self {
            Self::Anthropic(c) => c.model(),
            Self::OpenAi(c) => c.model(),
        }
    }

    fn set_model(&self, model: &str) -> bool {
        match self {
            Self::Anthropic(c) => c.set_model(model),
            Self::OpenAi(c) => c.set_model(model),
        }
    }

    fn base_url(&self) -> String {
        match self {
            Self::Anthropic(c) => c.base_url(),
            Self::OpenAi(c) => c.base_url(),
        }
    }

    fn stream_messages(
        &self,
        request: &StreamRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ApiError>> + Send + 'static>> {
        match self {
            Self::Anthropic(c) => c.stream_messages(request),
            Self::OpenAi(c) => c.stream_messages(request),
        }
    }

    fn create_message(
        &self,
        request: &StreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NonStreamingResponse, ApiError>> + Send + '_>> {
        match self {
            Self::Anthropic(c) => c.create_message(request),
            Self::OpenAi(c) => c.create_message(request),
        }
    }

    fn stream_messages_with_options(
        &self,
        request: &StreamRequest,
        options: loopctl::structured::RequestOptions,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ApiError>> + Send + 'static>> {
        match self {
            Self::Anthropic(c) => c.stream_messages_with_options(request, options),
            Self::OpenAi(c) => c.stream_messages_with_options(request, options),
        }
    }

    fn create_message_with_options(
        &self,
        request: &StreamRequest,
        options: loopctl::structured::RequestOptions,
    ) -> Pin<Box<dyn Future<Output = Result<NonStreamingResponse, ApiError>> + Send + '_>> {
        match self {
            Self::Anthropic(c) => c.create_message_with_options(request, options),
            Self::OpenAi(c) => c.create_message_with_options(request, options),
        }
    }

    fn extract_structured(&self, message: &Message) -> serde_json::Value {
        match self {
            Self::Anthropic(c) => c.extract_structured(message),
            Self::OpenAi(c) => c.extract_structured(message),
        }
    }
}

pub fn build_client(cfg: &DifftraceConfig) -> Result<DifftraceClient, DifftraceError> {
    let client = match cfg.provider.profile {
        ProviderProfile::Anthropic => anthropic_client(cfg),
        ProviderProfile::OpenAi => openai_client(cfg),
        ProviderProfile::Zai => zai_client(cfg),
        ProviderProfile::Ollama => ollama_client(cfg),
    }?;
    tracing::info!(
        target: "difftrace::provider",
        profile = ?cfg.provider.profile,
        model = %client.model(),
        "provider client built"
    );
    Ok(client)
}

fn anthropic_client(cfg: &DifftraceConfig) -> Result<DifftraceClient, DifftraceError> {
    let key = env_key("ANTHROPIC_API_KEY")?;
    let mut builder = AnthropicClient::builder()
        .with_api_key(key)
        .with_max_tokens(budget(cfg));
    if let Some(model) = &cfg.provider.model {
        builder = builder.with_model(model.clone());
    }
    if let Some(base_url) = &cfg.provider.base_url {
        builder = builder.with_base_url(base_url.clone());
    }
    builder
        .build()
        .map(DifftraceClient::Anthropic)
        .map_err(|source| DifftraceError::ClientBuild { source })
}

fn openai_client(cfg: &DifftraceConfig) -> Result<DifftraceClient, DifftraceError> {
    let key = env_key("OPENAI_API_KEY")?;
    let mut builder = OpenAiClient::builder().with_api_key(key);
    if let Some(model) = &cfg.provider.model {
        builder = builder.with_model(model.clone());
    }
    if let Some(base_url) = &cfg.provider.base_url {
        builder = builder.with_base_url(base_url.clone());
    }
    builder
        .build()
        .map(DifftraceClient::OpenAi)
        .map_err(|source| DifftraceError::ClientBuild { source })
}

fn zai_client(cfg: &DifftraceConfig) -> Result<DifftraceClient, DifftraceError> {
    let key = env_key_with_alias("ZAI_API_KEY", "ZHIPUAI_API_KEY")?;
    let mut builder = loopctl::provider::zai_builder()
        .with_api_key(key)
        .with_max_tokens(budget(cfg));
    if let Some(model) = &cfg.provider.model {
        builder = builder.with_model(model.clone());
    }
    if let Some(base_url) = &cfg.provider.base_url {
        builder = builder.with_base_url(base_url.clone());
    }
    builder
        .build()
        .map(DifftraceClient::Anthropic)
        .map_err(|source| DifftraceError::ClientBuild { source })
}

fn ollama_client(cfg: &DifftraceConfig) -> Result<DifftraceClient, DifftraceError> {
    let model = cfg
        .provider
        .model
        .clone()
        .ok_or(DifftraceError::OllamaModelMissing)?;
    let key = std::env::var("OLLAMA_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| NO_AUTH_KEY.to_owned());
    let base_url = cfg
        .provider
        .base_url
        .clone()
        .unwrap_or_else(|| OLLAMA_BASE_URL.to_owned());
    OpenAiClient::builder()
        .with_api_key(key)
        .with_base_url(base_url)
        .with_model(model)
        .build()
        .map(DifftraceClient::OpenAi)
        .map_err(|source| DifftraceError::ClientBuild { source })
}

fn env_key_with_alias(
    primary: &'static str,
    alias: &'static str,
) -> Result<String, DifftraceError> {
    let non_empty = |var: &'static str| std::env::var(var).ok().filter(|v| !v.is_empty());
    non_empty(primary)
        .or_else(|| non_empty(alias))
        .ok_or(DifftraceError::MissingApiKey { env_var: primary })
}

fn env_key(var: &'static str) -> Result<String, DifftraceError> {
    std::env::var(var)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(DifftraceError::MissingApiKey { env_var: var })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(profile: ProviderProfile) -> DifftraceConfig {
        DifftraceConfig {
            provider: crate::config::ProviderConfig {
                profile,
                model: Some("test-model".to_owned()),
                base_url: None,
                max_tokens: None,
            },
            ..DifftraceConfig::default()
        }
    }

    #[test]
    fn the_output_budget_is_enforced_where_the_provider_takes_one() {
        assert_eq!(
            enforced_output_budget(&cfg(ProviderProfile::Anthropic)),
            Some(8192)
        );
        assert_eq!(
            enforced_output_budget(&cfg(ProviderProfile::Zai)),
            Some(8192)
        );
        assert_eq!(enforced_output_budget(&cfg(ProviderProfile::OpenAi)), None);
        let mut configured = cfg(ProviderProfile::Zai);
        configured.provider.max_tokens = Some(32_768);
        assert_eq!(enforced_output_budget(&configured), Some(32_768));
    }

    #[test]
    fn anthropic_builds_with_an_env_key() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let client = build_client(&cfg(ProviderProfile::Anthropic))?;
        assert_eq!(client.model(), "test-model");
        Ok(())
    }

    #[test]
    fn the_built_client_logs_its_profile_and_model() -> Result<(), Box<dyn std::error::Error>> {
        let (logs, _guard) = crate::review::logging::test_support::install();
        let env = loopctl::testing::EnvGuard::acquire(&["ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        env.set("ZAI_API_KEY", "env-key");
        env.remove("ZHIPUAI_API_KEY");
        let client = build_client(&cfg(ProviderProfile::Zai))?;
        assert_eq!(client.model(), "test-model");
        let text = logs.text();
        assert!(
            text.contains("provider client built"),
            "the startup line must reach an installed subscriber: {text}"
        );
        assert!(
            text.contains("test-model"),
            "the resolved model is named: {text}"
        );
        assert!(text.contains("Zai"), "the profile is named: {text}");
        Ok(())
    }

    #[test]
    fn a_missing_anthropic_key_names_the_env_var() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.remove("ANTHROPIC_API_KEY");
        let err = build_client(&cfg(ProviderProfile::Anthropic))
            .err()
            .ok_or("expected an error")?;
        assert!(
            err.to_string().contains("ANTHROPIC_API_KEY"),
            "error must name the env var: {err}"
        );
        Ok(())
    }

    #[test]
    fn openai_builds_with_an_env_key() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.set("OPENAI_API_KEY", "env-key");
        let client = build_client(&cfg(ProviderProfile::OpenAi))?;
        assert_eq!(client.model(), "test-model");
        Ok(())
    }

    #[test]
    fn a_missing_openai_key_names_the_env_var() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.remove("OPENAI_API_KEY");
        let err = build_client(&cfg(ProviderProfile::OpenAi))
            .err()
            .ok_or("expected an error")?;
        assert!(
            err.to_string().contains("OPENAI_API_KEY"),
            "error must name the env var: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_custom_base_url_applies() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.set("OPENAI_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::OpenAi);
        config.provider.base_url = Some("https://proxy.example/v1".to_owned());
        let client = build_client(&config)?;
        assert_eq!(client.base_url(), "https://proxy.example/v1");
        Ok(())
    }

    #[test]
    fn an_empty_api_key_counts_as_missing() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "");
        let err = build_client(&cfg(ProviderProfile::Anthropic))
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(
            err,
            DifftraceError::MissingApiKey {
                env_var: "ANTHROPIC_API_KEY"
            }
        ));
        Ok(())
    }

    #[test]
    fn an_anthropic_base_url_applies() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Anthropic);
        config.provider.base_url = Some("https://proxy.anthropic.example".to_owned());
        let client = build_client(&config)?;
        assert_eq!(client.base_url(), "https://proxy.anthropic.example");
        Ok(())
    }

    #[test]
    fn an_unset_model_keeps_the_provider_default() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Anthropic);
        config.provider.model = None;
        let client = build_client(&config)?;
        assert!(!client.model().is_empty());
        Ok(())
    }

    #[test]
    fn ollama_builds_without_a_key_at_the_local_endpoint() -> Result<(), Box<dyn std::error::Error>>
    {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.remove("OLLAMA_API_KEY");
        let client = build_client(&cfg(ProviderProfile::Ollama))?;
        assert_eq!(client.base_url(), OLLAMA_BASE_URL);
        assert_eq!(client.model(), "test-model");
        Ok(())
    }

    #[test]
    fn an_ollama_cloud_key_comes_from_the_env() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.set("OLLAMA_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Ollama);
        config.provider.base_url = Some("https://cloud.example/v1".to_owned());
        let client = build_client(&config)?;
        assert_eq!(client.base_url(), "https://cloud.example/v1");
        Ok(())
    }

    #[test]
    fn zai_builds_with_env_key_at_its_endpoint() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        env.set("ZAI_API_KEY", "env-key");
        env.remove("ZHIPUAI_API_KEY");
        let client = build_client(&cfg(ProviderProfile::Zai))?;
        assert_eq!(client.base_url(), "https://api.z.ai/api/anthropic");
        assert_eq!(client.model(), "test-model");
        let mut default_model = cfg(ProviderProfile::Zai);
        default_model.provider.model = None;
        let client = build_client(&default_model)?;
        assert_eq!(client.model(), "glm-4.7");
        Ok(())
    }

    #[test]
    fn a_zhipuai_alias_key_satisfies_the_zai_profile() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        env.remove("ZAI_API_KEY");
        env.set("ZHIPUAI_API_KEY", "alias-key");
        let client = build_client(&cfg(ProviderProfile::Zai))?;
        assert_eq!(client.base_url(), "https://api.z.ai/api/anthropic");
        Ok(())
    }

    #[test]
    fn an_empty_primary_key_falls_through_to_the_alias() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        env.set("ZAI_API_KEY", "");
        env.set("ZHIPUAI_API_KEY", "alias-key");
        let client = build_client(&cfg(ProviderProfile::Zai))?;
        assert_eq!(client.base_url(), "https://api.z.ai/api/anthropic");
        Ok(())
    }

    #[test]
    fn an_empty_alias_key_still_falls_back_to_missing() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        env.set("ZAI_API_KEY", "");
        env.set("ZHIPUAI_API_KEY", "");
        let err = build_client(&cfg(ProviderProfile::Zai))
            .err()
            .ok_or("expected an error")?;
        assert!(
            err.to_string().contains("ZAI_API_KEY"),
            "error names the primary: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_missing_zai_key_names_the_primary_variable() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ZAI_API_KEY", "ZHIPUAI_API_KEY"]);
        env.remove("ZAI_API_KEY");
        env.remove("ZHIPUAI_API_KEY");
        let err = build_client(&cfg(ProviderProfile::Zai))
            .err()
            .ok_or("expected an error")?;
        assert!(
            err.to_string().contains("ZAI_API_KEY"),
            "error must name the env var: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_zai_model_override_replaces_the_default() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["ZAI_API_KEY"]);
        env.set("ZAI_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Zai);
        config.provider.model = Some("glm-5.0".to_owned());
        let client = build_client(&config)?;
        assert_eq!(client.model(), "glm-5.0");
        assert_eq!(client.base_url(), "https://api.z.ai/api/anthropic");
        Ok(())
    }

    #[test]
    fn ollama_requires_a_model() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.remove("OLLAMA_API_KEY");
        let mut config = cfg(ProviderProfile::Ollama);
        config.provider.model = None;
        let err = build_client(&config).err().ok_or("expected an error")?;
        assert!(matches!(err, DifftraceError::OllamaModelMissing));
        Ok(())
    }
}
