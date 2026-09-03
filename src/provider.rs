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
    let provider = &cfg.provider;
    match provider.profile {
        ProviderProfile::Anthropic => {
            let key = env_key("ANTHROPIC_API_KEY")?;
            let mut builder = AnthropicClient::builder().with_api_key(key);
            if let Some(model) = &provider.model {
                builder = builder.with_model(model.clone());
            }
            if let Some(base_url) = &provider.base_url {
                builder = builder.with_base_url(base_url.clone());
            }
            builder
                .build()
                .map(DifftraceClient::Anthropic)
                .map_err(|source| DifftraceError::ClientBuild { source })
        }
        ProviderProfile::OpenAi => {
            let key = env_key("OPENAI_API_KEY")?;
            let mut builder = OpenAiClient::builder().with_api_key(key);
            if let Some(model) = &provider.model {
                builder = builder.with_model(model.clone());
            }
            if let Some(base_url) = &provider.base_url {
                builder = builder.with_base_url(base_url.clone());
            }
            builder
                .build()
                .map(DifftraceClient::OpenAi)
                .map_err(|source| DifftraceError::ClientBuild { source })
        }
        ProviderProfile::Ollama => {
            let model = provider
                .model
                .clone()
                .ok_or(DifftraceError::OllamaModelMissing)?;
            let key = std::env::var("OLLAMA_API_KEY")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| NO_AUTH_KEY.to_owned());
            let base_url = provider
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
    }
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
            },
            ..DifftraceConfig::default()
        }
    }

    #[test]
    fn anthropic_builds_with_an_env_key() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let client = build_client(&cfg(ProviderProfile::Anthropic)).unwrap();
        assert_eq!(client.model(), "test-model");
    }

    #[test]
    fn a_missing_anthropic_key_names_the_env_var() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.remove("ANTHROPIC_API_KEY");
        let err = build_client(&cfg(ProviderProfile::Anthropic)).unwrap_err();
        assert!(
            err.to_string().contains("ANTHROPIC_API_KEY"),
            "error must name the env var: {err}"
        );
    }

    #[test]
    fn openai_builds_with_an_env_key() {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.set("OPENAI_API_KEY", "env-key");
        let client = build_client(&cfg(ProviderProfile::OpenAi)).unwrap();
        assert_eq!(client.model(), "test-model");
    }

    #[test]
    fn a_missing_openai_key_names_the_env_var() {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.remove("OPENAI_API_KEY");
        let err = build_client(&cfg(ProviderProfile::OpenAi)).unwrap_err();
        assert!(
            err.to_string().contains("OPENAI_API_KEY"),
            "error must name the env var: {err}"
        );
    }

    #[test]
    fn a_custom_base_url_applies() {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.set("OPENAI_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::OpenAi);
        config.provider.base_url = Some("https://proxy.example/v1".to_owned());
        let client = build_client(&config).unwrap();
        assert_eq!(client.base_url(), "https://proxy.example/v1");
    }

    #[test]
    fn an_empty_api_key_counts_as_missing() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "");
        let err = build_client(&cfg(ProviderProfile::Anthropic)).unwrap_err();
        assert!(matches!(
            err,
            DifftraceError::MissingApiKey {
                env_var: "ANTHROPIC_API_KEY"
            }
        ));
    }

    #[test]
    fn an_anthropic_base_url_applies() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Anthropic);
        config.provider.base_url = Some("https://proxy.anthropic.example".to_owned());
        let client = build_client(&config).unwrap();
        assert_eq!(client.base_url(), "https://proxy.anthropic.example");
    }

    #[test]
    fn an_unset_model_keeps_the_provider_default() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Anthropic);
        config.provider.model = None;
        let client = build_client(&config).unwrap();
        assert!(!client.model().is_empty());
    }

    #[test]
    fn ollama_builds_without_a_key_at_the_local_endpoint() {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.remove("OLLAMA_API_KEY");
        let client = build_client(&cfg(ProviderProfile::Ollama)).unwrap();
        assert_eq!(client.base_url(), OLLAMA_BASE_URL);
        assert_eq!(client.model(), "test-model");
    }

    #[test]
    fn an_ollama_cloud_key_comes_from_the_env() {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.set("OLLAMA_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Ollama);
        config.provider.base_url = Some("https://cloud.example/v1".to_owned());
        let client = build_client(&config).unwrap();
        assert_eq!(client.base_url(), "https://cloud.example/v1");
    }

    #[test]
    fn ollama_requires_a_model() {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.remove("OLLAMA_API_KEY");
        let mut config = cfg(ProviderProfile::Ollama);
        config.provider.model = None;
        let err = build_client(&config).unwrap_err();
        assert!(matches!(err, DifftraceError::OllamaModelMissing));
    }
}
