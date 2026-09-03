//! Construction of the loopctl API client from difftrace configuration.
//!
//! Maps [`ProviderProfile`] onto loopctl's concrete provider clients,
//! wrapping the result in the [`DifftraceClient`] enum the review loop
//! monomorphizes over. API keys resolve exclusively from the environment;
//! the config file never carries credentials.

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

/// Sentinel API key used when a provider requires no authentication.
///
/// A local Ollama server accepts any credential, so when no key is
/// configured this dummy is sent rather than erroring — the request
/// succeeds and the user is not forced to invent a placeholder key.
const NO_AUTH_KEY: &str = "ollama";

/// Default base URL of the OpenAI-compatible endpoint a local Ollama
/// server serves.
///
/// Ollama speaks the OpenAI chat-completions protocol under `/v1`; the
/// suffix is part of the endpoint, not a loopctl convention.
const OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

/// The concrete provider client difftrace monomorphizes the review loop
/// over.
///
/// A runtime-selected enum over loopctl's two provider client families, so
/// the review loop's per-turn LLM call is statically dispatched rather
/// than going through `dyn ApiClient`. [`build_client`] picks the variant
/// from [`ProviderProfile`]; the `Ollama` profile rides the OpenAI
/// protocol variant via its compatible endpoint. Every
/// [`ApiClient`] method forwards to the inner client unchanged.
pub enum DifftraceClient {
    /// An Anthropic-protocol provider client.
    ///
    /// Selected for the [`ProviderProfile::Anthropic`] default profile.
    /// Wraps loopctl's `AnthropicClient`.
    Anthropic(AnthropicClient),

    /// An OpenAI-protocol provider client.
    ///
    /// Selected for [`ProviderProfile::OpenAi`] and
    /// [`ProviderProfile::Ollama`], the latter pointing at the local
    /// Ollama endpoint. Wraps loopctl's `OpenAiClient`.
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

/// Build a [`DifftraceClient`] for the profile named by the config.
///
/// # API-key resolution
///
/// Keys resolve exclusively from the environment: `ANTHROPIC_API_KEY` for
/// the Anthropic profile, `OPENAI_API_KEY` for the OpenAI profile, and
/// `OLLAMA_API_KEY` for an authenticated Ollama deployment. A local
/// Ollama with no key configured is given a dummy credential rather than
/// erroring. A missing required key returns
/// [`DifftraceError::MissingApiKey`] naming the expected variable.
///
/// # Model resolution
///
/// `provider.model` overrides the provider's own default when set. The
/// `Ollama` profile requires it — Ollama has no portable default model.
///
/// # Errors
///
/// Returns [`DifftraceError::MissingApiKey`] when a required key is
/// absent, [`DifftraceError::OllamaModelMissing`] for a model-less Ollama
/// profile, and [`DifftraceError::ClientBuild`] when the underlying HTTP
/// client cannot be constructed (typically an invalid `base_url`).
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

/// Read a required API key from an environment variable.
///
/// Empty strings count as absent so a placeholder export does not produce
/// an authenticated-looking request that fails server-side.
///
/// # Errors
///
/// Returns [`DifftraceError::MissingApiKey`] naming `var` when the
/// variable is unset or empty.
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
    fn test_anthropic_builds_with_env_key() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let client = build_client(&cfg(ProviderProfile::Anthropic)).unwrap();
        assert_eq!(client.model(), "test-model");
    }

    #[test]
    fn test_anthropic_missing_key_names_env_var() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.remove("ANTHROPIC_API_KEY");
        let err = build_client(&cfg(ProviderProfile::Anthropic)).unwrap_err();
        assert!(
            err.to_string().contains("ANTHROPIC_API_KEY"),
            "error must name the env var: {err}"
        );
    }

    #[test]
    fn test_openai_builds_with_env_key() {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.set("OPENAI_API_KEY", "env-key");
        let client = build_client(&cfg(ProviderProfile::OpenAi)).unwrap();
        assert_eq!(client.model(), "test-model");
    }

    #[test]
    fn test_openai_missing_key_names_env_var() {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.remove("OPENAI_API_KEY");
        let err = build_client(&cfg(ProviderProfile::OpenAi)).unwrap_err();
        assert!(
            err.to_string().contains("OPENAI_API_KEY"),
            "error must name the env var: {err}"
        );
    }

    #[test]
    fn test_custom_base_url_applies() {
        let env = loopctl::testing::EnvGuard::acquire(&["OPENAI_API_KEY"]);
        env.set("OPENAI_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::OpenAi);
        config.provider.base_url = Some("https://proxy.example/v1".to_owned());
        let client = build_client(&config).unwrap();
        assert_eq!(client.base_url(), "https://proxy.example/v1");
    }

    #[test]
    fn test_unset_model_keeps_provider_default() {
        let env = loopctl::testing::EnvGuard::acquire(&["ANTHROPIC_API_KEY"]);
        env.set("ANTHROPIC_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Anthropic);
        config.provider.model = None;
        let client = build_client(&config).unwrap();
        assert!(!client.model().is_empty());
    }

    #[test]
    fn test_ollama_builds_without_key_at_local_endpoint() {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.remove("OLLAMA_API_KEY");
        let client = build_client(&cfg(ProviderProfile::Ollama)).unwrap();
        assert_eq!(client.base_url(), OLLAMA_BASE_URL);
        assert_eq!(client.model(), "test-model");
    }

    #[test]
    fn test_ollama_cloud_key_from_env() {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.set("OLLAMA_API_KEY", "env-key");
        let mut config = cfg(ProviderProfile::Ollama);
        config.provider.base_url = Some("https://cloud.example/v1".to_owned());
        let client = build_client(&config).unwrap();
        assert_eq!(client.base_url(), "https://cloud.example/v1");
    }

    #[test]
    fn test_ollama_requires_model() {
        let env = loopctl::testing::EnvGuard::acquire(&["OLLAMA_API_KEY"]);
        env.remove("OLLAMA_API_KEY");
        let mut config = cfg(ProviderProfile::Ollama);
        config.provider.model = None;
        let err = build_client(&config).unwrap_err();
        assert!(matches!(err, DifftraceError::OllamaModelMissing));
    }
}
