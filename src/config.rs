//! Configuration: `~/.difftrace/config.toml` into
//! [`DifftraceConfig`]. Every field defaults, so an absent file is
//! valid; secrets never live in this file — their resolution is the
//! environment's job and lands with the gateway.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::DtError;

pub const DEFAULT_JUDGE_PANEL: [&str; 3] = ["zai:glm-5.3", "zai:glm-5.3-flash", "local"];
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
    pub hosted: HostedConfig,
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
        toml::from_str(contents).map_err(|source| DtError::Config(source.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JudgesConfig {
    pub count: usize,
    pub profiles: Vec<String>,
    pub temperature: f64,
    pub max_tokens: u32,
    pub stream_timeout_secs: u64,
    pub search_tool: bool,
    pub tool_calls_per_region: usize,
    pub flow_map_max_callers: usize,
}

impl Default for JudgesConfig {
    fn default() -> Self {
        Self {
            count: 3,
            profiles: DEFAULT_JUDGE_PANEL
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            temperature: 0.0,
            max_tokens: 32_768,
            stream_timeout_secs: 300,
            search_tool: true,
            tool_calls_per_region: 10,
            flow_map_max_callers: 20,
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
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: vec!["fmt".to_owned(), "clippy".to_owned()],
            budget_secs: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OracleConfig {
    pub policy: String,
    pub budget_secs: u64,
    pub per_round_max: usize,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            policy: "off".to_owned(),
            budget_secs: 180,
            per_round_max: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RatelimitConfig {
    pub per_provider_lanes: usize,
    pub backoff_base_secs: u64,
    pub jitter: f64,
}

impl Default for RatelimitConfig {
    fn default() -> Self {
        Self {
            per_provider_lanes: 4,
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
    pub retire_after_misses: u32,
    pub line_drift_tolerance: u64,
    pub token_overlap_pct: u64,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            scope: "diff".to_owned(),
            severity: "nitpick".to_owned(),
            retire_after_misses: 2,
            line_drift_tolerance: 5,
            token_overlap_pct: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HostedConfig {
    pub state_marker: String,
}

impl Default for HostedConfig {
    fn default() -> Self {
        Self {
            state_marker: "<!-- difftrace:state ".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_the_design_tables() {
        let config = DifftraceConfig::from_toml_str("").unwrap_or_default();
        assert_eq!(
            config.judges.profiles,
            DEFAULT_JUDGE_PANEL
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
            "the answered judge panel is the default"
        );
        assert_eq!(config.judges.count, 3);
        assert!((config.judges.temperature - 0.0).abs() < 1e-9);
        assert_eq!(config.judges.max_tokens, 32_768);
        assert_eq!(config.oracle.policy, "off");
        assert_eq!(config.oracle.per_round_max, 5);
        assert_eq!(config.tools.enabled, vec!["fmt", "clippy"]);
        assert_eq!(config.ratelimit.per_provider_lanes, 4);
        assert!((config.ratelimit.jitter - 0.3).abs() < 1e-9);
        assert_eq!(config.review.scope, "diff");
        assert_eq!(config.review.severity, "nitpick");
        assert_eq!(config.review.retire_after_misses, 2);
        assert_eq!(
            config.aspects.default,
            DEFAULT_ASPECTS
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(config.hosted.state_marker, "<!-- difftrace:state ");
    }

    #[test]
    fn partial_sections_keep_their_defaults() {
        let config =
            DifftraceConfig::from_toml_str("[oracle]\npolicy = \"check\"\n").unwrap_or_default();
        assert_eq!(config.oracle.policy, "check");
        assert_eq!(config.oracle.budget_secs, 180);
        assert_eq!(config.judges.count, 3);
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
}
