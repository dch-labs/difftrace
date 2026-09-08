//! Configuration loading: `~/.difftrace/config.toml` into
//! [`DifftraceConfig`]. Every field defaults, so an absent file is valid;
//! secrets never live in the config file — they resolve from the
//! environment, with `~/.difftrace/env` as the lowest-priority source.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::DifftraceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProfile {
    Anthropic,
    OpenAi,
    #[default]
    Zai,
    Ollama,
    Local,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub profile: ProviderProfile,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    pub api_base_url: Option<String>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMode {
    #[default]
    Diff,
    Deep,
    Full,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum IssueKind {
    Security,
    Correctness,
    Concurrency,
    Performance,
    Tests,
    Docs,
}

impl IssueKind {
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Security => "security — injection, auth, secrets, unsafe input handling",
            Self::Correctness => "correctness — logic defects, wrong results, broken contracts",
            Self::Concurrency => "concurrency — races, deadlocks, lifetime and handoff gaps",
            Self::Performance => "performance — unbounded growth, wasted work, hot paths",
            Self::Tests => "tests — missing or unsound coverage, tests that cannot fail",
            Self::Docs => "docs — comments and docs that misstate the code",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ReviewSettings {
    pub max_findings_per_file: usize,
    pub batch_files: usize,
    pub max_turns: usize,
    pub reply_max_turns: usize,
    pub verify_findings: bool,
    pub miss_hunt: bool,
    pub hunt_always: bool,
    pub memory_content: Option<String>,
    pub stream_timeout_secs: u64,
    pub max_parallel_batches: usize,
    pub batch_attempts: usize,
    pub retry_backoff_base_secs: u64,
    pub scope: ReviewMode,
    pub focus: Vec<IssueKind>,
    pub severity: crate::findings::Severity,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            max_findings_per_file: 5,
            batch_files: 4,
            max_turns: 16,
            reply_max_turns: 8,
            verify_findings: true,
            miss_hunt: true,
            hunt_always: false,
            memory_content: None,
            stream_timeout_secs: 900,
            max_parallel_batches: 1,
            batch_attempts: 10,
            retry_backoff_base_secs: 8,
            scope: ReviewMode::Diff,
            focus: Vec::new(),
            severity: crate::findings::Severity::Nitpick,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    #[default]
    Info,
    Error,
    Warn,
    Debug,
    Trace,
}

impl LogLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

pub fn parse_log_level(raw: &str) -> Result<LogLevel, String> {
    match raw {
        "error" => Ok(LogLevel::Error),
        "warn" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        other => Err(format!(
            "unknown log level {other:?}; expected error, warn, info, debug, or trace"
        )),
    }
}

pub fn parse_scope(raw: &str) -> Result<ReviewMode, String> {
    match raw {
        "diff" => Ok(ReviewMode::Diff),
        "deep" => Ok(ReviewMode::Deep),
        "full" => Ok(ReviewMode::Full),
        other => Err(format!(
            "unknown review scope {other:?}; expected diff, deep, or full"
        )),
    }
}

pub fn parse_issue_kind(raw: &str) -> Result<IssueKind, String> {
    match raw {
        "security" => Ok(IssueKind::Security),
        "correctness" => Ok(IssueKind::Correctness),
        "concurrency" => Ok(IssueKind::Concurrency),
        "performance" => Ok(IssueKind::Performance),
        "tests" => Ok(IssueKind::Tests),
        "docs" => Ok(IssueKind::Docs),
        other => Err(format!(
            "unknown issue kind {other:?}; expected security, correctness, concurrency, \
             performance, tests, or docs"
        )),
    }
}

pub fn parse_severity(raw: &str) -> Result<crate::findings::Severity, String> {
    match raw {
        "critical" => Ok(crate::findings::Severity::Critical),
        "warning" => Ok(crate::findings::Severity::Warning),
        "suggestion" => Ok(crate::findings::Severity::Suggestion),
        "nitpick" => Ok(crate::findings::Severity::Nitpick),
        other => Err(format!(
            "unknown severity {other:?}; expected critical, warning, suggestion, or nitpick"
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DifftraceConfig {
    pub provider: ProviderConfig,
    pub github: GitHubConfig,
    pub review: ReviewSettings,
    pub log_level: LogLevel,
}

impl DifftraceConfig {
    pub fn apply_env_overrides(&mut self) -> Result<(), crate::error::DifftraceError> {
        self.apply_env_overrides_with(&load_env_file())
    }

    pub fn apply_env_overrides_with(
        &mut self,
        env_file: &BTreeMap<String, String>,
    ) -> Result<(), crate::error::DifftraceError> {
        if let Some(profile) = layered_lookup("DIFFTRACE_PROFILE", env_file) {
            self.provider.profile =
                parse_profile(&profile).map_err(crate::error::DifftraceError::Cli)?;
        }
        if let Some(level) = layered_lookup("DIFFTRACE_LOG", env_file) {
            self.log_level = parse_log_level(&level).map_err(crate::error::DifftraceError::Cli)?;
        }
        if let Some(model) = layered_lookup("DIFFTRACE_MODEL", env_file) {
            self.provider.model = Some(model);
        }
        if let Some(url) = layered_lookup("DIFFTRACE_BASE_URL", env_file) {
            self.provider.base_url = Some(url);
        }
        if let Some(tokens) = layered_lookup("DIFFTRACE_MAX_TOKENS", env_file) {
            let parsed = tokens.trim().parse::<u32>().map_err(|_| {
                DifftraceError::Cli(format!("invalid DIFFTRACE_MAX_TOKENS: {tokens}"))
            })?;
            if parsed == 0 {
                return Err(DifftraceError::Cli(
                    "DIFFTRACE_MAX_TOKENS must be at least 1".to_owned(),
                ));
            }
            self.provider.max_tokens = Some(parsed);
        }
        if let Some(secs) = layered_lookup("DIFFTRACE_STREAM_TIMEOUT_SECS", env_file) {
            let parsed = secs.trim().parse::<u64>().map_err(|_| {
                DifftraceError::Cli(format!("invalid DIFFTRACE_STREAM_TIMEOUT_SECS: {secs}"))
            })?;
            if parsed == 0 {
                return Err(DifftraceError::Cli(
                    "DIFFTRACE_STREAM_TIMEOUT_SECS must be at least 1".to_owned(),
                ));
            }
            self.review.stream_timeout_secs = parsed;
        }
        if let Some(lanes) = layered_lookup("DIFFTRACE_MAX_PARALLEL_BATCHES", env_file) {
            let parsed = lanes.trim().parse::<usize>().map_err(|_| {
                DifftraceError::Cli(format!("invalid DIFFTRACE_MAX_PARALLEL_BATCHES: {lanes}"))
            })?;
            if parsed == 0 {
                return Err(DifftraceError::Cli(
                    "DIFFTRACE_MAX_PARALLEL_BATCHES must be at least 1".to_owned(),
                ));
            }
            self.review.max_parallel_batches = parsed;
        }
        if let Some(size) = layered_lookup("DIFFTRACE_BATCH_FILES", env_file) {
            let parsed = size.trim().parse::<usize>().map_err(|_| {
                DifftraceError::Cli(format!("invalid DIFFTRACE_BATCH_FILES: {size}"))
            })?;
            if parsed == 0 {
                return Err(DifftraceError::Cli(
                    "DIFFTRACE_BATCH_FILES must be at least 1".to_owned(),
                ));
            }
            self.review.batch_files = parsed;
        }
        if let Some(scope) = layered_lookup("DIFFTRACE_SCOPE", env_file) {
            self.review.scope = parse_scope(&scope).map_err(DifftraceError::Cli)?;
        }
        if let Some(focus) = layered_lookup("DIFFTRACE_FOCUS", env_file) {
            let mut kinds = Vec::new();
            for item in focus.split(',') {
                kinds.push(parse_issue_kind(item.trim()).map_err(DifftraceError::Cli)?);
            }
            self.review.focus = kinds;
        }
        if let Some(severity) = layered_lookup("DIFFTRACE_SEVERITY", env_file) {
            self.review.severity = parse_severity(&severity).map_err(DifftraceError::Cli)?;
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

#[must_use]
pub fn resolve_log_level() -> String {
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    if !rust_log.is_empty() {
        return String::new();
    }
    let mut config = DifftraceConfig::load().unwrap_or_default();
    if config.apply_env_overrides().is_err() {
        return LogLevel::Info.as_str().to_owned();
    }
    config.log_level.as_str().to_owned()
}

#[must_use]
pub fn load_env_file() -> BTreeMap<String, String> {
    // Tests never read the real home env file — that would make them
    // machine-dependent; file-backed behavior is pinned through the
    // explicit `_with` variants.
    if cfg!(test) {
        return BTreeMap::new();
    }
    let Some(home) = dirs::home_dir() else {
        return BTreeMap::new();
    };
    match std::fs::read_to_string(home.join(".difftrace").join("env")) {
        Ok(text) => parse_env_file(&text),
        Err(_) => BTreeMap::new(),
    }
}

fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        map.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    map
}

pub(crate) fn layered_lookup(name: &str, env_file: &BTreeMap<String, String>) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            env_file
                .get(name)
                .cloned()
                .filter(|value| !value.is_empty())
        })
}

pub fn parse_profile(raw: &str) -> Result<ProviderProfile, String> {
    match raw {
        "anthropic" => Ok(ProviderProfile::Anthropic),
        "openai" => Ok(ProviderProfile::OpenAi),
        "zai" => Ok(ProviderProfile::Zai),
        "ollama" => Ok(ProviderProfile::Ollama),
        "local" => Ok(ProviderProfile::Local),
        other => Err(format!(
            "unknown provider profile {other:?}; expected anthropic, openai, zai, ollama, or local"
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
        assert_eq!(cfg.provider.profile, ProviderProfile::Zai);
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
        assert_eq!(parse_profile("local")?, ProviderProfile::Local);
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
    fn the_max_tokens_env_override_parses_into_the_provider_config()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_MAX_TOKENS"]);
        env.set("DIFFTRACE_MAX_TOKENS", "32768");
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides()?;
        assert_eq!(config.provider.max_tokens, Some(32_768));
        env.remove("DIFFTRACE_MAX_TOKENS");
        Ok(())
    }

    #[test]
    fn an_invalid_max_tokens_override_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_MAX_TOKENS"]);
        env.set("DIFFTRACE_MAX_TOKENS", "lots");
        let mut config = DifftraceConfig::default();
        let Err(err) = config.apply_env_overrides() else {
            return Err("expected an invalid token count to fail".into());
        };
        assert!(err.to_string().contains("DIFFTRACE_MAX_TOKENS"));
        env.set("DIFFTRACE_MAX_TOKENS", "0");
        let mut config = DifftraceConfig::default();
        let Err(err) = config.apply_env_overrides() else {
            return Err("expected a zero budget to be rejected".into());
        };
        assert!(
            err.to_string().contains("at least 1"),
            "names the constraint: {err}"
        );
        env.remove("DIFFTRACE_MAX_TOKENS");
        Ok(())
    }

    #[test]
    fn the_parallel_batches_env_override_reaches_the_review_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = DifftraceConfig::default();
        assert_eq!(
            config.review.max_parallel_batches, 1,
            "the default keeps batches sequential"
        );
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_MAX_PARALLEL_BATCHES"]);
        env.set("DIFFTRACE_MAX_PARALLEL_BATCHES", "3");
        config.apply_env_overrides()?;
        assert_eq!(config.review.max_parallel_batches, 3);
        env.set("DIFFTRACE_MAX_PARALLEL_BATCHES", "0");
        let mut config = DifftraceConfig::default();
        let Err(err) = config.apply_env_overrides() else {
            return Err("expected a zero lane count to be rejected".into());
        };
        assert!(
            err.to_string().contains("at least 1"),
            "names the constraint: {err}"
        );
        env.remove("DIFFTRACE_MAX_PARALLEL_BATCHES");
        Ok(())
    }

    #[test]
    fn the_batch_files_env_override_reaches_the_review_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_BATCH_FILES"]);
        env.set("DIFFTRACE_BATCH_FILES", "1");
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides()?;
        assert_eq!(config.review.batch_files, 1);
        env.set("DIFFTRACE_BATCH_FILES", "0");
        let mut config = DifftraceConfig::default();
        let Err(err) = config.apply_env_overrides() else {
            return Err("expected a zero batch size to be rejected".into());
        };
        assert!(
            err.to_string().contains("at least 1"),
            "names the constraint: {err}"
        );
        env.remove("DIFFTRACE_BATCH_FILES");
        Ok(())
    }

    #[test]
    fn the_stream_timeout_env_override_reaches_the_review_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_STREAM_TIMEOUT_SECS"]);
        env.set("DIFFTRACE_STREAM_TIMEOUT_SECS", "1200");
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides()?;
        assert_eq!(config.review.stream_timeout_secs, 1_200);
        env.remove("DIFFTRACE_STREAM_TIMEOUT_SECS");
        Ok(())
    }

    #[test]
    fn an_invalid_stream_timeout_override_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_STREAM_TIMEOUT_SECS"]);
        env.set("DIFFTRACE_STREAM_TIMEOUT_SECS", "0");
        let mut config = DifftraceConfig::default();
        let Err(err) = config.apply_env_overrides() else {
            return Err("expected a zero stream timeout to fail".into());
        };
        assert!(
            err.to_string().contains("DIFFTRACE_STREAM_TIMEOUT_SECS"),
            "names the variable: {err}"
        );
        env.remove("DIFFTRACE_STREAM_TIMEOUT_SECS");
        Ok(())
    }

    #[test]
    fn an_unset_or_empty_env_override_keeps_the_config() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_PROFILE"]);
        env.remove("DIFFTRACE_PROFILE");
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides()?;
        assert_eq!(config.provider.profile, ProviderProfile::Zai);
        env.set("DIFFTRACE_PROFILE", "");
        config.apply_env_overrides()?;
        assert_eq!(config.provider.profile, ProviderProfile::Zai);
        Ok(())
    }

    #[test]
    fn the_log_level_override_parses_and_rejects() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(parse_log_level("debug")?, LogLevel::Debug);
        assert_eq!(parse_log_level("trace")?, LogLevel::Trace);
        let err = parse_log_level("loud")
            .err()
            .ok_or("expected a rejection")?;
        assert!(
            err.contains("debug") && err.contains("loud"),
            "the rejection names the value and the valid levels: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_log_level_override_applies_from_the_environment_or_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_LOG"]);
        env.remove("DIFFTRACE_LOG");
        let mut file = std::collections::BTreeMap::new();
        file.insert("DIFFTRACE_LOG".to_owned(), "debug".to_owned());
        let mut config = DifftraceConfig::default();
        assert_eq!(config.log_level, LogLevel::Info);
        config.apply_env_overrides_with(&file)?;
        assert_eq!(config.log_level, LogLevel::Debug);
        env.set("DIFFTRACE_LOG", "trace");
        config.apply_env_overrides()?;
        assert_eq!(
            config.log_level,
            LogLevel::Trace,
            "the environment wins over the env file"
        );
        env.set("DIFFTRACE_LOG", "verbose");
        assert!(
            config.apply_env_overrides().is_err(),
            "an unknown level fails loudly instead of being ignored"
        );
        Ok(())
    }

    #[test]
    fn the_env_file_parses_keys_and_skips_noise() {
        let map = parse_env_file(
            "# comment\n\nZAI_API_KEY=sk-1\n  GITHUB_TOKEN  =  ghp_2  \nbroken-line\nURL=https://x.example/?a=1&b=2\nEMPTY=\n",
        );
        assert_eq!(map.get("ZAI_API_KEY").map(String::as_str), Some("sk-1"));
        assert_eq!(map.get("GITHUB_TOKEN").map(String::as_str), Some("ghp_2"));
        assert_eq!(
            map.get("URL").map(String::as_str),
            Some("https://x.example/?a=1&b=2"),
            "values keep their inner equals signs"
        );
        assert!(map.contains_key("EMPTY"));
        assert!(
            !map.contains_key("broken-line"),
            "malformed lines are skipped"
        );
    }

    #[test]
    fn the_environment_wins_over_the_env_file() {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_PROBE"]);
        let mut file = std::collections::BTreeMap::new();
        file.insert("DIFFTRACE_PROBE".to_owned(), "from-file".to_owned());
        env.set("DIFFTRACE_PROBE", "from-env");
        assert_eq!(
            layered_lookup("DIFFTRACE_PROBE", &file).as_deref(),
            Some("from-env")
        );
        env.remove("DIFFTRACE_PROBE");
        assert_eq!(
            layered_lookup("DIFFTRACE_PROBE", &file).as_deref(),
            Some("from-file")
        );
        env.set("DIFFTRACE_PROBE", "");
        assert_eq!(
            layered_lookup("DIFFTRACE_PROBE", &file).as_deref(),
            Some("from-file"),
            "an empty environment value counts as unset"
        );
        file.insert("DIFFTRACE_PROBE".to_owned(), String::new());
        assert_eq!(
            layered_lookup("DIFFTRACE_PROBE", &file),
            None,
            "an empty file value counts as unset"
        );
    }

    #[test]
    fn a_profile_override_can_come_from_the_env_file() -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_PROFILE"]);
        env.remove("DIFFTRACE_PROFILE");
        let mut file = std::collections::BTreeMap::new();
        file.insert("DIFFTRACE_PROFILE".to_owned(), "openai".to_owned());
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides_with(&file)?;
        assert_eq!(config.provider.profile, ProviderProfile::OpenAi);
        Ok(())
    }

    #[test]
    fn the_base_url_env_override_replaces_the_configured_endpoint()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&["DIFFTRACE_BASE_URL"]);
        env.set("DIFFTRACE_BASE_URL", "http://localhost:8080/v1");
        let mut config = DifftraceConfig::default();
        config.provider.base_url = Some("http://localhost:11434/v1".to_owned());
        config.apply_env_overrides()?;
        assert_eq!(
            config.provider.base_url.as_deref(),
            Some("http://localhost:8080/v1"),
            "the environment points the profile at a different endpoint"
        );
        env.remove("DIFFTRACE_BASE_URL");
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
    fn the_scope_focus_and_severity_overrides_reach_the_review_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let env = loopctl::testing::EnvGuard::acquire(&[
            "DIFFTRACE_SCOPE",
            "DIFFTRACE_FOCUS",
            "DIFFTRACE_SEVERITY",
        ]);
        env.set("DIFFTRACE_SCOPE", "deep");
        env.set("DIFFTRACE_FOCUS", "security, tests");
        env.set("DIFFTRACE_SEVERITY", "warning");
        let mut config = DifftraceConfig::default();
        config.apply_env_overrides()?;
        assert_eq!(config.review.scope, ReviewMode::Deep);
        assert_eq!(
            config.review.focus,
            vec![IssueKind::Security, IssueKind::Tests]
        );
        assert_eq!(config.review.severity, crate::findings::Severity::Warning);
        env.remove("DIFFTRACE_SCOPE");
        env.remove("DIFFTRACE_FOCUS");
        env.remove("DIFFTRACE_SEVERITY");
        Ok(())
    }

    #[test]
    fn unknown_scope_or_focus_strings_name_the_valid_ones() -> Result<(), Box<dyn std::error::Error>>
    {
        let Err(scope_err) = parse_scope("wide") else {
            return Err("an unknown scope must fail".into());
        };
        assert!(scope_err.contains("deep"), "names the options: {scope_err}");
        let Err(kind_err) = parse_issue_kind("vibes") else {
            return Err("an unknown kind must fail".into());
        };
        assert!(kind_err.contains("security"), "names the kinds: {kind_err}");
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
