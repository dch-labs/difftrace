//! Configuration: layered loading (explicit path, repo file, global
//! file, defaults) into [`DifftraceConfig`], env overrides over the
//! flag-mirrored knobs, and the external tool tables. Every field
//! defaults, so an absent file is valid; secrets never live in this
//! file — their resolution stays with the environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::DtError;

pub const DEFAULT_JUDGE_PANEL: [&str; 3] = ["zai:glm-5.3", "zai:glm-5.3-flash", "local"];
pub const DEFAULT_MAX_TOKENS: u32 = 32_768;
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
pub const DEFAULT_ASPECTS: [&str; 6] = [
    "error-paths",
    "lifecycle",
    "concurrency",
    "security",
    "api-contract",
    "tests",
];

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DifftraceConfig {
    pub judges: JudgesConfig,
    pub aspects: AspectsConfig,
    pub tools: ToolsConfig,
    pub oracle: OracleConfig,
    pub ratelimit: RatelimitConfig,
    pub review: ReviewConfig,
}

impl DifftraceConfig {
    pub fn load() -> Result<Self, DtError> {
        let home =
            dirs::home_dir().ok_or_else(|| DtError::Config("no home directory".to_owned()))?;
        Self::load_from(&home.join(".difftrace").join("config.toml"))
    }

    pub fn load_from(path: &Path) -> Result<Self, DtError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::from_toml_str(&contents),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(DtError::Config(format!(
                "cannot read {}: {err}",
                path.display()
            ))),
        }
    }

    pub fn from_toml_str(contents: &str) -> Result<Self, DtError> {
        let config: Self =
            toml::from_str(contents).map_err(|source| DtError::Config(source.to_string()))?;
        config.validate_judges()?;
        config.validate_external_tools()?;
        config.validate_ratelimit()?;
        Ok(config)
    }
}

#[must_use]
pub fn find_repo_config(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join(".difftrace").join("config.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

pub fn load_layered_config(
    explicit: Option<&Path>,
    start_dir: Option<&Path>,
    global: Option<&Path>,
) -> Result<DifftraceConfig, DtError> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(DtError::Config(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        return DifftraceConfig::load_from(path);
    }
    let repo_start = match start_dir {
        Some(dir) => Some(dir.to_owned()),
        None => std::env::current_dir()
            .map_err(|source| DtError::Config(source.to_string()))
            .ok(),
    };
    if let Some(path) = repo_start.as_deref().and_then(find_repo_config) {
        return DifftraceConfig::load_from(&path);
    }
    match global {
        Some(path) => DifftraceConfig::load_from(path),
        None => DifftraceConfig::load(),
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JudgesConfig {
    pub count: usize,
    pub profiles: Vec<String>,
    pub max_tokens: u32,
    pub context_window: u64,
    pub stream_timeout_secs: u64,
    pub region_concurrency: usize,
    pub retry_backoff_secs: u64,
    pub region_retries: u8,
    pub search_tool: bool,
    pub tool_calls_per_region: usize,
    pub flow_map_max_callers: usize,
    pub region_max_lines: u32,
    pub escalate_profile: Option<String>,
    pub model: Vec<ModelConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    pub profile: String,
    pub effort: Option<String>,
    pub replay_reasoning: Option<bool>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u64>,
}

impl JudgesConfig {
    #[must_use]
    pub fn model_settings(&self, profile: &str) -> ModelSettings {
        self.model
            .iter()
            .find(|entry| entry.profile == profile)
            .map_or(
                ModelSettings {
                    effort: None,
                    replay_reasoning: false,
                    max_tokens: self.max_tokens,
                    context_window: self.context_window,
                },
                |entry| ModelSettings {
                    effort: entry.effort.as_deref().and_then(parse_effort),
                    replay_reasoning: entry.replay_reasoning.unwrap_or(false),
                    max_tokens: entry.max_tokens.unwrap_or(self.max_tokens),
                    context_window: entry.context_window.unwrap_or(self.context_window),
                },
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSettings {
    pub effort: Option<EffortRung>,
    pub replay_reasoning: bool,
    pub max_tokens: u32,
    pub context_window: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortRung {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

fn parse_effort(name: &str) -> Option<EffortRung> {
    match name.trim().to_lowercase().as_str() {
        "none" => Some(EffortRung::None),
        "minimal" => Some(EffortRung::Minimal),
        "low" => Some(EffortRung::Low),
        "medium" => Some(EffortRung::Medium),
        "high" => Some(EffortRung::High),
        "xhigh" => Some(EffortRung::Xhigh),
        "max" => Some(EffortRung::Max),
        _ => None,
    }
}

impl Default for JudgesConfig {
    fn default() -> Self {
        Self {
            count: 3,
            profiles: DEFAULT_JUDGE_PANEL
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            max_tokens: DEFAULT_MAX_TOKENS,
            context_window: DEFAULT_CONTEXT_WINDOW,
            stream_timeout_secs: 300,
            region_concurrency: 8,
            retry_backoff_secs: 8,
            region_retries: 3,
            search_tool: true,
            tool_calls_per_region: 10,
            flow_map_max_callers: 20,
            model: Vec::new(),
            region_max_lines: 3_000,
            escalate_profile: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AspectsConfig {
    pub default: Vec<String>,
    pub paths: BTreeMap<String, AspectOverride>,
}

impl Default for AspectsConfig {
    fn default() -> Self {
        Self {
            default: DEFAULT_ASPECTS.iter().map(|s| (*s).to_owned()).collect(),
            paths: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AspectOverride {
    pub extra: Vec<String>,
    pub replace: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ToolsConfig {
    pub enabled: Vec<String>,
    pub budget_secs: u64,
    pub external: Vec<ExternalTool>,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: vec!["fmt".to_owned(), "clippy".to_owned()],
            budget_secs: 120,
            external: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTool {
    pub name: String,
    pub command: String,
    pub format: String,
    pub run_on: String,
    #[serde(default = "default_external_budget_secs")]
    pub budget_secs: u64,
}

fn default_external_budget_secs() -> u64 {
    60
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OraclePolicy {
    Off,
    Check,
    Test,
}

impl OraclePolicy {
    pub fn resolve(raw: Option<&str>, repo_root: &std::path::Path) -> Result<Self, DtError> {
        let Some(text) = raw else {
            let workspace = repo_root.join("Cargo.toml").exists();
            if workspace {
                tracing::info!(
                    target: "difftrace::config",
                    "oracle policy auto-escalates to check for this cargo workspace; set oracle.policy = \"off\" to disable"
                );
                return Ok(Self::Check);
            }
            return Ok(Self::Off);
        };
        text.parse::<Self>()
            .map_err(|cause| DtError::Config(format!("oracle.policy: {cause}")))
    }
}

impl std::fmt::Display for OraclePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Off => "off",
            Self::Check => "check",
            Self::Test => "test",
        };
        f.write_str(text)
    }
}

impl std::str::FromStr for OraclePolicy {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "check" => Ok(Self::Check),
            "test" => Ok(Self::Test),
            other => Err(format!(
                "unknown oracle policy {other:?}; expected off, check, or test"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OracleConfig {
    pub policy: Option<String>,
    pub budget_secs: u64,
    pub per_round_max: usize,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            policy: None,
            budget_secs: 180,
            per_round_max: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RatelimitConfig {
    pub per_provider_lanes: usize,
    pub global_lanes: usize,
    pub backoff_base_secs: u64,
    pub jitter: f64,
}

impl Default for RatelimitConfig {
    fn default() -> Self {
        Self {
            per_provider_lanes: 4,
            global_lanes: 8,
            backoff_base_secs: 5,
            jitter: 0.3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReviewConfig {
    pub scope: String,
    pub severity: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            scope: "diff".to_owned(),
            severity: "nitpick".to_owned(),
        }
    }
}

impl DifftraceConfig {
    fn validate_external_tools(&self) -> Result<(), DtError> {
        for tool in &self.tools.external {
            if tool.name.trim().is_empty() || tool.command.trim().is_empty() {
                return Err(DtError::Config(format!(
                    "external tool {:?} needs a non-empty name and command",
                    tool.name
                )));
            }
            if tool.format != "diag" && tool.format != "cargo-json" {
                return Err(DtError::Config(format!(
                    "external tool {:?} has unknown format {:?}; expected diag or cargo-json",
                    tool.name, tool.format
                )));
            }
            if tool.run_on.trim().is_empty() {
                return Err(DtError::Config(format!(
                    "external tool {:?} needs a run_on glob",
                    tool.name
                )));
            }
            if tool.budget_secs == 0 {
                return Err(DtError::Config(format!(
                    "external tool {:?} needs a non-zero budget_secs",
                    tool.name
                )));
            }
        }
        if self.tools.budget_secs == 0 {
            return Err(DtError::Config(
                "tools.budget_secs must be positive: a zero budget marks every tool unavailable"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_judges(&self) -> Result<(), DtError> {
        if self.judges.count == 0 {
            return Err(DtError::Config(
                "judges.count must be positive: zero judges review nothing".to_owned(),
            ));
        }
        if self.judges.region_concurrency == 0 {
            return Err(DtError::Config(
                "judges.region_concurrency must be positive: zero lanes review nothing".to_owned(),
            ));
        }
        if self.judges.region_max_lines < 40 {
            return Err(DtError::Config(
                "judges.region_max_lines must be at least 40 lines: finer splits only multiply judge calls"
                    .to_owned(),
            ));
        }
        if self
            .judges
            .escalate_profile
            .as_ref()
            .is_some_and(|profile| profile.trim().is_empty())
        {
            return Err(DtError::Config(
                "judges.escalate_profile must name a provider[:model] profile, not an empty string"
                    .to_owned(),
            ));
        }
        if self.judges.context_window == 0 {
            return Err(DtError::Config(
                "judges.context_window must be positive: a zero window silently disables the compaction guard"
                    .to_owned(),
            ));
        }
        if self.judges.region_retries > 10 {
            return Err(DtError::Config(
                "judges.region_retries must be at most 10: unbounded retries belong under an explicit --budget-secs ceiling"
                    .to_owned(),
            ));
        }
        for entry in &self.judges.model {
            if entry.profile.trim().is_empty() {
                return Err(DtError::Config(
                    "judges.model profile must name the provider[:model] it configures, not an empty string"
                        .to_owned(),
                ));
            }
            if let Some(effort) = &entry.effort
                && parse_effort(effort).is_none()
            {
                return Err(DtError::Config(format!(
                    "judges.model effort {effort:?} is not a rung of the ladder; \
                     expected one of none, minimal, low, medium, high, xhigh, max"
                )));
            }
            if entry.context_window == Some(0) {
                return Err(DtError::Config(format!(
                    "judges.model context_window for {:?} must be positive: zero silently disables the compaction guard",
                    entry.profile
                )));
            }
        }
        Ok(())
    }

    fn validate_ratelimit(&self) -> Result<(), DtError> {
        if self.ratelimit.per_provider_lanes == 0 {
            return Err(DtError::Config(
                "ratelimit.per_provider_lanes must be positive: zero lanes admit no request"
                    .to_owned(),
            ));
        }
        if self.ratelimit.global_lanes == 0 {
            return Err(DtError::Config(
                "ratelimit.global_lanes must be positive: a zero global pool hangs every acquire"
                    .to_owned(),
            ));
        }
        if self.ratelimit.backoff_base_secs == 0 {
            return Err(DtError::Config(
                "ratelimit.backoff_base_secs must be positive: a zero backoff recreates the lockstep storm"
                    .to_owned(),
            ));
        }
        if !self.ratelimit.jitter.is_finite() || self.ratelimit.jitter < 0.0 {
            return Err(DtError::Config(
                "ratelimit.jitter must be a finite number of at least zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct EnvSource {
    vars: BTreeMap<String, String>,
    file: BTreeMap<String, String>,
}

impl EnvSource {
    #[must_use]
    pub fn from_maps(vars: BTreeMap<String, String>, file: BTreeMap<String, String>) -> Self {
        Self { vars, file }
    }

    #[must_use]
    pub fn live() -> Self {
        Self {
            vars: std::env::vars_os()
                .filter_map(|(name, value)| {
                    Some((name.into_string().ok()?, value.into_string().ok()?))
                })
                .collect(),
            file: load_env_file(),
        }
    }

    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<String> {
        let non_empty = |value: &str| !value.is_empty();
        self.vars
            .get(name)
            .filter(|value| non_empty(value))
            .cloned()
            .or_else(|| {
                self.file
                    .get(name)
                    .filter(|value| non_empty(value))
                    .cloned()
            })
    }
}

#[must_use]
pub fn load_env_file() -> BTreeMap<String, String> {
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
        map.insert(
            key.trim().to_owned(),
            strip_one_quote_layer(value.trim()).to_owned(),
        );
    }
    map
}

fn strip_one_quote_layer(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|mid| mid.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

pub(crate) fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn apply_env_overrides_with(
    config: &mut DifftraceConfig,
    env: &EnvSource,
) -> Result<(), DtError> {
    if let Some(scope) = env.lookup("DIFFTRACE_SCOPE") {
        scope
            .parse::<crate::diff::Scope>()
            .map_err(|source| DtError::Config(format!("DIFFTRACE_SCOPE: {source}")))?;
        config.review.scope = scope;
    }
    if let Some(severity) = env.lookup("DIFFTRACE_SEVERITY") {
        severity
            .parse::<crate::contracts::Severity>()
            .map_err(|source| DtError::Config(format!("DIFFTRACE_SEVERITY: {source}")))?;
        config.review.severity = severity;
    }
    if let Some(count) = env.lookup("DIFFTRACE_JUDGES") {
        let parsed = count.parse::<usize>().map_err(|source| {
            DtError::Config(format!("DIFFTRACE_JUDGES: expected a number, {source}"))
        })?;
        if parsed == 0 {
            return Err(DtError::Config(
                "DIFFTRACE_JUDGES: expected at least one judge".to_owned(),
            ));
        }
        config.judges.count = parsed;
    }
    if let Some(profiles) = env.lookup("DIFFTRACE_JUDGE") {
        let list = split_list(&profiles);
        if list.is_empty() {
            return Err(DtError::Config(
                "DIFFTRACE_JUDGE: expected at least one profile".to_owned(),
            ));
        }
        config.judges.profiles = list;
    }
    if let Some(policy) = env.lookup("DIFFTRACE_ORACLE") {
        policy
            .parse::<OraclePolicy>()
            .map_err(|source| DtError::Config(format!("DIFFTRACE_ORACLE: {source}")))?;
        config.oracle.policy = Some(policy);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_settings_default_to_the_global_ceilings() -> Result<(), Box<dyn std::error::Error>> {
        let config = DifftraceConfig::from_toml_str("")?;
        let settings = config.judges.model_settings("zai:glm-5.3");
        assert_eq!(settings.max_tokens, 32_768);
        assert_eq!(settings.context_window, DEFAULT_CONTEXT_WINDOW);
        assert_eq!(settings.effort, None);
        assert!(!settings.replay_reasoning);
        Ok(())
    }

    #[test]
    fn the_context_window_resolves_per_profile_and_rejects_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = DifftraceConfig::from_toml_str(
            "[judges]\ncontext_window = 128000\n\n[[judges.model]]\nprofile = \"zai:glm-5.3\"\ncontext_window = 64000\n",
        )?;
        let plain = config.judges.model_settings("zai:glm-5.3");
        assert_eq!(
            plain.context_window, 64_000,
            "the per-profile entry wins over the global window"
        );
        let other = config.judges.model_settings("local");
        assert_eq!(
            other.context_window, 128_000,
            "profiles without an entry inherit the global window"
        );
        let zero_global = DifftraceConfig::from_toml_str("[judges]\ncontext_window = 0\n");
        assert!(
            zero_global.is_err_and(|err| err.to_string().contains("context_window")),
            "a zero window is loud: it silently disables the compaction guard"
        );
        let zero_entry = DifftraceConfig::from_toml_str(
            "[[judges.model]]\nprofile = \"zai\"\ncontext_window = 0\n",
        );
        assert!(
            zero_entry.is_err_and(|err| err.to_string().contains("zai")),
            "a zero per-profile window names its profile"
        );
        Ok(())
    }

    #[test]
    fn a_model_entry_overrides_only_its_named_profile() -> Result<(), Box<dyn std::error::Error>> {
        let config = DifftraceConfig::from_toml_str(
            r#"
[judges]
max_tokens = 16384

[[judges.model]]
profile = "zai:glm-5.3"
effort = "high"
replay_reasoning = true
max_tokens = 65536
"#,
        )?;
        let listed = config.judges.model_settings("zai:glm-5.3");
        assert_eq!(listed.effort, Some(EffortRung::High));
        assert!(listed.replay_reasoning);
        assert_eq!(listed.max_tokens, 65_536);
        let other = config.judges.model_settings("zai:glm-5.3-flash");
        assert_eq!(other.max_tokens, 16_384);
        assert_eq!(other.effort, None);
        Ok(())
    }

    #[test]
    fn an_unknown_effort_rung_rejects_at_load() {
        let config = DifftraceConfig::from_toml_str(
            r#"
[[judges.model]]
profile = "zai"
effort = "ludicrous"
"#,
        );
        assert!(config.is_err());
    }

    #[test]
    fn an_empty_model_profile_rejects_at_load() {
        let config = DifftraceConfig::from_toml_str(
            r#"
[[judges.model]]
profile = ""
effort = "low"
"#,
        );
        assert!(config.is_err());
    }

    #[test]
    fn every_effort_rung_parses_from_its_snake_case_name() {
        for (name, rung) in [
            ("none", EffortRung::None),
            ("minimal", EffortRung::Minimal),
            ("low", EffortRung::Low),
            ("medium", EffortRung::Medium),
            ("high", EffortRung::High),
            ("xhigh", EffortRung::Xhigh),
            ("max", EffortRung::Max),
        ] {
            assert_eq!(parse_effort(name), Some(rung));
        }
        assert_eq!(parse_effort("HIGH"), Some(EffortRung::High));
        assert_eq!(parse_effort(" turbo"), None);
    }

    #[test]
    fn config_defaults_match_the_design_tables() -> Result<(), Box<dyn std::error::Error>> {
        let config = DifftraceConfig::from_toml_str("")?;
        assert_eq!(
            config.judges.profiles,
            DEFAULT_JUDGE_PANEL
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
            "the answered judge panel is the default"
        );
        assert_eq!(config.judges.count, 3);
        assert_eq!(config.judges.max_tokens, 32_768);
        assert_eq!(config.oracle.policy, None, "an absent policy means auto");
        assert_eq!(config.oracle.per_round_max, 5);
        assert_eq!(config.tools.enabled, vec!["fmt", "clippy"]);
        assert_eq!(config.ratelimit.per_provider_lanes, 4);
        assert!((config.ratelimit.jitter - 0.3).abs() < 1e-9);
        assert_eq!(config.review.scope, "diff");
        assert_eq!(config.review.severity, "nitpick");
        assert_eq!(
            config.aspects.default,
            DEFAULT_ASPECTS
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn a_zero_tool_budget_rejects() {
        let outcome = DifftraceConfig::from_toml_str("[tools]\nbudget_secs = 0\n");
        assert!(
            outcome.is_err_and(|err| err.to_string().contains("budget_secs")),
            "a zero whole-layer budget silently degrades every tool to unavailable"
        );
        assert!(DifftraceConfig::from_toml_str("[tools]\nbudget_secs = 30\n").is_ok());
    }

    #[test]
    fn degenerate_judges_knobs_reject() {
        for (body, knob) in [
            ("[judges]\ncount = 0\n", "count"),
            ("[judges]\nregion_concurrency = 0\n", "region_concurrency"),
            ("[judges]\nregion_retries = 11\n", "region_retries"),
        ] {
            let outcome = DifftraceConfig::from_toml_str(body);
            assert!(
                outcome
                    .as_ref()
                    .is_err_and(|err| err.to_string().contains(knob)),
                "{body} rejects naming the knob"
            );
        }
    }

    #[test]
    fn degenerate_ratelimit_knobs_reject() {
        for (body, knob) in [
            (
                "[ratelimit]\nper_provider_lanes = 0\n",
                "per_provider_lanes",
            ),
            ("[ratelimit]\nglobal_lanes = 0\n", "global_lanes"),
            ("[ratelimit]\nbackoff_base_secs = 0\n", "backoff_base_secs"),
            ("[ratelimit]\njitter = -0.1\n", "jitter"),
            ("[ratelimit]\njitter = inf\n", "jitter"),
            ("[ratelimit]\njitter = nan\n", "jitter"),
        ] {
            let outcome = DifftraceConfig::from_toml_str(body);
            assert!(
                outcome.is_err_and(|err| err.to_string().contains(knob)),
                "{knob} degenerates the rate limiter and must reject"
            );
        }
        assert!(DifftraceConfig::from_toml_str("[ratelimit]\njitter = 0.0\n").is_ok());
    }

    #[test]
    fn lifecycle_knobs_reject_as_unknown_fields_until_wired() {
        for knob in [
            "retire_after_misses",
            "line_drift_tolerance",
            "token_overlap_pct",
        ] {
            let config = DifftraceConfig::from_toml_str(&format!("[review]\n{knob} = 5\n"));
            assert!(
                config.is_err_and(|err| err.to_string().contains(knob)),
                "{knob} has no shipped behavior and must not parse"
            );
        }
        let hosted = DifftraceConfig::from_toml_str("[hosted]\nstate_marker = \"x\"\n");
        assert!(
            hosted.is_err_and(|err| err.to_string().contains("hosted")),
            "the hosted table returns with the hosted gateway"
        );
    }

    #[test]
    fn partial_sections_keep_their_defaults() {
        let config =
            DifftraceConfig::from_toml_str("[oracle]\npolicy = \"check\"\n").unwrap_or_default();
        assert_eq!(config.oracle.policy.as_deref(), Some("check"));
        assert_eq!(config.oracle.budget_secs, 180);
        assert_eq!(config.judges.count, 3);
    }

    #[test]
    fn the_oracle_policy_resolves_against_the_repo() -> Result<(), DtError> {
        let dir = std::env::temp_dir().join(format!("dt-oracle-auto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|err| DtError::Config(err.to_string()))?;
        assert_eq!(
            OraclePolicy::resolve(None, &dir)?,
            OraclePolicy::Off,
            "no workspace and no policy stays off"
        );
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n")
            .map_err(|err| DtError::Config(err.to_string()))?;
        assert_eq!(
            OraclePolicy::resolve(None, &dir)?,
            OraclePolicy::Check,
            "a cargo workspace auto-escalates an absent policy to check"
        );
        assert_eq!(
            OraclePolicy::resolve(Some("off"), &dir)?,
            OraclePolicy::Off,
            "an explicit off wins over the workspace default"
        );
        assert_eq!(
            OraclePolicy::resolve(Some("test"), &dir)?,
            OraclePolicy::Test
        );
        assert!(OraclePolicy::resolve(Some("vibes"), &dir).is_err());
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn the_split_and_escalation_knobs_default_and_validate() {
        let defaults = JudgesConfig::default();
        assert_eq!(defaults.region_max_lines, 3_000);
        assert_eq!(defaults.escalate_profile, None);
        assert!(
            DifftraceConfig::from_toml_str("[judges]\nregion_max_lines = 10\n")
                .is_err_and(|err| err.to_string().contains("region_max_lines")),
            "splits below the snapshot budget only multiply judge calls"
        );
        assert!(
            DifftraceConfig::from_toml_str("[judges]\nescalate_profile = \"\"\"\n")
                .is_err_and(|err| err.to_string().contains("escalate_profile")),
            "an empty escalation profile is a config error, not a silent no-op"
        );
        let shaped = DifftraceConfig::from_toml_str(
            "[judges]\nregion_max_lines = 120\nescalate_profile = \"zai:glm-5.3\"\n",
        )
        .unwrap_or_default();
        assert_eq!(shaped.judges.region_max_lines, 120);
        assert_eq!(
            shaped.judges.escalate_profile.as_deref(),
            Some("zai:glm-5.3")
        );
    }

    #[test]
    fn the_shipped_example_config_matches_the_compiled_defaults()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("example.config.toml");
        let example = DifftraceConfig::load_from(&path)?;
        assert_eq!(
            example,
            DifftraceConfig::default(),
            "the example sells itself as the shipped defaults; every knob must match them"
        );
        Ok(())
    }

    #[test]
    fn the_shipped_example_config_loads_as_is() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("example.config.toml");
        let outcome = DifftraceConfig::load_from(&path);
        assert!(
            outcome.is_ok(),
            "the shipped example must parse: {:?}",
            outcome.err()
        );
        let config = outcome.unwrap_or_default();
        assert_eq!(config.judges.count, 3);
        assert_eq!(config.judges.region_retries, 3);
        assert_eq!(config.judges.region_max_lines, 3_000);
        assert_eq!(
            config.oracle.policy, None,
            "the example leaves the oracle on auto"
        );
        assert_eq!(config.ratelimit.per_provider_lanes, 4);
        assert!(
            config.aspects.paths.is_empty(),
            "the example stays the pure defaults; the override demo lives in its comments"
        );
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        assert!(DifftraceConfig::from_toml_str("[judges]\ncounts = 3\n").is_err());
    }

    #[test]
    fn aspect_path_overrides_parse() {
        let config = DifftraceConfig::from_toml_str(
            "[aspects.paths.\"**/tests/**\"]\nextra = [\"fixtures\"]\n",
        )
        .unwrap_or_default();
        let override_entry = config
            .aspects
            .paths
            .get("**/tests/**")
            .cloned()
            .unwrap_or_default();
        assert_eq!(override_entry.extra, vec!["fixtures"]);
        assert!(override_entry.replace.is_none());
    }

    #[test]
    fn external_tool_entries_parse_with_their_budgets() -> Result<(), DtError> {
        let config = DifftraceConfig::from_toml_str(
            "[[tools.external]]\nname = \"eslint\"\ncommand = \"npx eslint --format unix \\\"{files}\\\"\"\nformat = \"diag\"\nrun_on = \"**/*.{js,ts,tsx}\"\nbudget_secs = 45\n\n[[tools.external]]\nname = \"cargo-check\"\ncommand = \"cargo check --message-format=json\"\nformat = \"cargo-json\"\nrun_on = \"**/*.rs\"\n",
        )?;
        assert_eq!(config.tools.external.len(), 2);
        let (eslint, cargo_check) = match config.tools.external.as_slice() {
            [first, second] => (first.clone(), second.clone()),
            _ => {
                return Err(DtError::Config(
                    "expected the two external tool entries".to_owned(),
                ));
            }
        };
        assert_eq!(eslint.name, "eslint");
        assert_eq!(eslint.budget_secs, 45);
        assert_eq!(cargo_check.budget_secs, 60, "omitted budgets default to 60");
        assert!(DifftraceConfig::from_toml_str(
            "[[tools.external]]\nname = \"x\"\ncommand = \"x\"\nformat = \"yaml\"\nrun_on = \"**\"\n"
        )
        .is_err_and(|err| err.to_string().contains("format")));
        assert!(DifftraceConfig::from_toml_str(
            "[[tools.external]]\nname = \"x\"\ncommand = \"x\"\nformat = \"diag\"\nrun_on = \"**\"\nbudget_secs = 0\n"
        )
        .is_err_and(|err| err.to_string().contains("budget")));
        assert!(DifftraceConfig::from_toml_str(
            "[[tools.external]]\nname = \"\"\ncommand = \"x\"\nformat = \"diag\"\nrun_on = \"**\"\n"
        )
        .is_err());
        Ok(())
    }

    fn scratch_dir(name: &str) -> Result<PathBuf, DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-config-{name}"));
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(dir)
    }

    fn write_file(path: &Path, body: &str) -> Result<(), DtError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| DtError::Config(source.to_string()))?;
        }
        std::fs::write(path, body).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    #[test]
    fn the_repo_config_replaces_the_global_file() -> Result<(), DtError> {
        let repo = scratch_dir("repo")?;
        let nested = repo.join("crates").join("inner");
        let global_dir = scratch_dir("global")?;
        let global_path = global_dir.join("config.toml");
        write_file(
            &global_path,
            "[review]\nscope = \"full\"\n[tools]\nenabled = []\n",
        )?;
        write_file(
            &repo.join(".difftrace").join("config.toml"),
            "[review]\nscope = \"deep\"\n",
        )?;
        assert_eq!(
            find_repo_config(&nested),
            Some(repo.join(".difftrace").join("config.toml")),
            "discovery walks up to the repo root"
        );
        let layered = load_layered_config(None, Some(&nested), Some(&global_path))?;
        assert_eq!(
            layered.review.scope, "deep",
            "the repo file wins when present"
        );
        assert!(
            !layered.tools.enabled.is_empty(),
            "whole-file precedence: global tables do not leak through"
        );
        std::fs::remove_file(repo.join(".difftrace").join("config.toml"))
            .map_err(|source| DtError::Config(source.to_string()))?;
        let global_layer = load_layered_config(None, Some(&nested), Some(&global_path))?;
        assert_eq!(global_layer.review.scope, "full");
        assert!(global_layer.tools.enabled.is_empty());
        write_file(&repo.join("explicit.toml"), "[review]\nscope = \"diff\"\n")?;
        let explicit = load_layered_config(
            Some(&repo.join("explicit.toml")),
            Some(&nested),
            Some(&global_path),
        )?;
        assert_eq!(explicit.review.scope, "diff");
        assert!(
            load_layered_config(Some(&repo.join("missing.toml")), None, None).is_err(),
            "an explicit config path is required to exist"
        );
        std::fs::remove_dir_all(&repo).map_err(|source| DtError::Config(source.to_string()))?;
        std::fs::remove_dir_all(&global_dir)
            .map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    #[test]
    fn env_overrides_parse_or_error_naming_the_variable() -> Result<(), DtError> {
        let mut config = DifftraceConfig::from_toml_str("[review]\nscope = \"diff\"\n")?;
        apply_env_overrides_with(
            &mut config,
            &EnvSource::from_maps(
                [("DIFFTRACE_SCOPE".to_owned(), "deep".to_owned())]
                    .into_iter()
                    .collect(),
                BTreeMap::new(),
            ),
        )?;
        assert_eq!(config.review.scope, "deep");
        let bad_scope = apply_env_overrides_with(
            &mut config,
            &EnvSource::from_maps(
                [("DIFFTRACE_SCOPE".to_owned(), "sideways".to_owned())]
                    .into_iter()
                    .collect(),
                BTreeMap::new(),
            ),
        );
        assert!(
            bad_scope
                .as_ref()
                .is_err_and(|err| err.to_string().contains("DIFFTRACE_SCOPE")),
            "errors name the variable: {bad_scope:?}"
        );
        let bad_judges = apply_env_overrides_with(
            &mut config,
            &EnvSource::from_maps(
                [("DIFFTRACE_JUDGES".to_owned(), "0".to_owned())]
                    .into_iter()
                    .collect(),
                BTreeMap::new(),
            ),
        );
        assert!(
            bad_judges
                .as_ref()
                .is_err_and(|err| err.to_string().contains("DIFFTRACE_JUDGES")),
            "zero judges reject: {bad_judges:?}"
        );
        apply_env_overrides_with(
            &mut config,
            &EnvSource::from_maps(
                [
                    (
                        "DIFFTRACE_JUDGE".to_owned(),
                        "zai:glm-5.3, local".to_owned(),
                    ),
                    ("DIFFTRACE_ORACLE".to_owned(), "test".to_owned()),
                ]
                .into_iter()
                .collect(),
                BTreeMap::new(),
            ),
        )?;
        assert_eq!(
            config.judges.profiles,
            vec!["zai:glm-5.3".to_owned(), "local".to_owned()]
        );
        assert_eq!(config.oracle.policy.as_deref(), Some("test"));
        let empty_env = apply_env_overrides_with(
            &mut config,
            &EnvSource::from_maps(
                [("DIFFTRACE_SCOPE".to_owned(), String::new())]
                    .into_iter()
                    .collect(),
                BTreeMap::new(),
            ),
        );
        assert!(empty_env.is_ok());
        assert_eq!(
            config.review.scope, "deep",
            "an empty env value counts as unset"
        );
        Ok(())
    }

    #[test]
    fn quoted_env_file_values_lose_one_matching_layer() {
        let file =
            parse_env_file("A=\"double\"\nB='single'\nC=\"unmatched\nD=\"'nested'\"\nE=\"\"\n");
        assert_eq!(
            file.get("A").map(String::as_str),
            Some("double"),
            "one matching double-quote layer strips"
        );
        assert_eq!(file.get("B").map(String::as_str), Some("single"));
        assert_eq!(
            file.get("C").map(String::as_str),
            Some("\"unmatched"),
            "an unmatched quote stays literal"
        );
        assert_eq!(
            file.get("D").map(String::as_str),
            Some("'nested'"),
            "exactly one layer strips, never two"
        );
        assert_eq!(file.get("E").map(String::as_str), Some(""));
    }

    #[test]
    fn the_env_file_is_the_lowest_env_layer() {
        let file = parse_env_file(
            "# comment\n\nDIFFTRACE_SCOPE=deep\n  DIFFTRACE_JUDGE = zai:glm-5.3 \nmalformed-line\n",
        );
        assert_eq!(
            file.get("DIFFTRACE_SCOPE").map(String::as_str),
            Some("deep")
        );
        assert_eq!(
            file.get("DIFFTRACE_JUDGE").map(String::as_str),
            Some("zai:glm-5.3")
        );
        assert!(!file.contains_key("malformed-line"));
        let env = EnvSource::from_maps(
            [("DIFFTRACE_SCOPE".to_owned(), String::new())]
                .into_iter()
                .collect(),
            file,
        );
        assert_eq!(
            env.lookup("DIFFTRACE_SCOPE").as_deref(),
            Some("deep"),
            "an empty process value falls through to the file"
        );
        assert_eq!(
            env.lookup("DIFFTRACE_JUDGE").as_deref(),
            Some("zai:glm-5.3")
        );
        assert_eq!(env.lookup("DIFFTRACE_PASSES"), None);
        let both_empty = EnvSource::from_maps(
            [("DIFFTRACE_JUDGE".to_owned(), String::new())]
                .into_iter()
                .collect(),
            [("DIFFTRACE_JUDGE".to_owned(), String::new())]
                .into_iter()
                .collect(),
        );
        assert_eq!(both_empty.lookup("DIFFTRACE_JUDGE"), None);
    }

    #[test]
    #[cfg(unix)]
    fn a_non_utf8_environment_variable_skips_instead_of_panicking() -> Result<(), DtError> {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        if std::env::var_os("DIFFTRACE_NON_UTF8_PROBE").is_some() {
            let env = EnvSource::live();
            assert_eq!(
                env.lookup("DIFFTRACE_HOSTILE_VAR"),
                None,
                "the byte-valued variable never reads as a knob"
            );
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|source| DtError::Config(source.to_string()))?;
        let child = std::process::Command::new(exe)
            .args([
                "--exact",
                "config::tests::a_non_utf8_environment_variable_skips_instead_of_panicking",
            ])
            .env("DIFFTRACE_NON_UTF8_PROBE", "1")
            .env(
                "DIFFTRACE_HOSTILE_VAR",
                OsStr::from_bytes(b"\xff\xfe not unicode"),
            )
            .output()
            .map_err(|source| DtError::Config(source.to_string()))?;
        assert!(
            child.status.success(),
            "a non-UTF-8 variable must not panic the process: {}",
            String::from_utf8_lossy(&child.stderr)
        );
        Ok(())
    }
}
