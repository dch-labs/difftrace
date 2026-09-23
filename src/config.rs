//! Configuration: layered loading (explicit path, repo file, global
//! file, defaults) into [`DifftraceConfig`], env overrides over the
//! flag-mirrored knobs, and the external tool tables. Every field
//! defaults, so an absent file is valid; secrets never live in this
//! file — their resolution stays with the environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
        config.validate_external_tools()?;
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
            vars: std::env::vars().collect(),
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
        map.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    map
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
        config.oracle.policy = policy;
    }
    Ok(())
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
        assert_eq!(
            config.aspects.default,
            DEFAULT_ASPECTS
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
        );
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
        assert_eq!(config.oracle.policy, "test");
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
}
