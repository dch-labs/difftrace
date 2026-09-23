//! The v2 command surface: the full flag grammar, layered
//! configuration resolution, and the exit-code contract; dispatch
//! validates inputs and reports what is not yet implemented.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::config::{
    self, DifftraceConfig, EnvSource, OraclePolicy, apply_env_overrides_with, load_layered_config,
};
use crate::contracts::Severity;
use crate::diff::Scope;
use crate::error::DtError;

pub const SUBCOMMANDS: [&str; 6] = ["review", "reject", "suppress", "state", "reply", "eval"];

#[derive(Debug, Parser)]
#[command(
    name = "difftrace",
    version,
    about = "A diff-review agent with a contractual model of truth"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand, PartialEq)]
pub enum Command {
    Review(ReviewArgs),
    Reject {
        id: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        theme: bool,
    },
    Suppress {
        #[command(subcommand)]
        action: SuppressAction,
    },
    State {
        #[command(subcommand)]
        action: StateAction,
    },
    Reply {
        target: String,
        #[arg(long, value_parser = parse_reply_mode, default_value = "answer")]
        mode: ReplyMode,
    },
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },
}

impl Command {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Review(_) => "review",
            Self::Reject { .. } => "reject",
            Self::Suppress { .. } => "suppress",
            Self::State { .. } => "state",
            Self::Reply { .. } => "reply",
            Self::Eval { .. } => "eval",
        }
    }
}

#[derive(Debug, Clone, Args, Default, PartialEq)]
pub struct ReviewArgs {
    #[arg(long)]
    pub base: Option<String>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub pr: Option<u64>,
    #[arg(long, value_parser = parse_scope)]
    pub scope: Option<Scope>,
    #[arg(long, value_delimiter = ',')]
    pub focus: Option<Vec<String>>,
    #[arg(long, value_parser = parse_severity)]
    pub severity: Option<Severity>,
    #[arg(long)]
    pub judges: Option<usize>,
    #[arg(long)]
    pub judge: Vec<String>,
    #[arg(long)]
    pub passes: Option<u64>,
    #[arg(long, value_parser = parse_oracle_policy)]
    pub oracle: Option<OraclePolicy>,
    #[arg(long)]
    pub budget_secs: Option<u64>,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub fix_prompts: bool,
}

impl ReviewArgs {
    pub fn validate(&self) -> Result<(), DtError> {
        if self.base.is_some() && self.repo.is_some() {
            return Err(DtError::Cli(
                "--base and --repo are mutually exclusive: local mode reviews origin/<base>..HEAD, hosted mode needs --repo with --pr".to_owned(),
            ));
        }
        if self.repo.is_some() && self.pr.is_none() {
            return Err(DtError::Cli(
                "--repo requires --pr with the pull request number".to_owned(),
            ));
        }
        if self.repo.is_none() && self.pr.is_some() {
            return Err(DtError::Cli(
                "--pr requires --repo: hosted mode is selected by --repo".to_owned(),
            ));
        }
        if self.repo.is_some() && self.scope == Some(Scope::Full) {
            return Err(DtError::Cli(
                "the full scope needs the tracked file list and is local-only".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub enum SuppressAction {
    List,
    Clear { id: Option<String> },
}

#[derive(Debug, Clone, Copy, Subcommand, PartialEq, Eq)]
pub enum StateAction {
    Show,
    Reset,
}

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub enum EvalAction {
    Run {
        #[arg(long)]
        set: String,
        #[arg(long)]
        runner: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyMode {
    Answer,
    Plan,
}

fn parse_reply_mode(raw: &str) -> Result<ReplyMode, String> {
    match raw.trim().to_lowercase().as_str() {
        "answer" => Ok(ReplyMode::Answer),
        "plan" => Ok(ReplyMode::Plan),
        other => Err(format!(
            "unknown reply mode {other:?}; expected answer or plan"
        )),
    }
}

fn parse_scope(raw: &str) -> Result<Scope, String> {
    raw.parse::<Scope>()
}

fn parse_severity(raw: &str) -> Result<Severity, String> {
    raw.parse::<Severity>()
}

fn parse_oracle_policy(raw: &str) -> Result<OraclePolicy, String> {
    raw.parse::<OraclePolicy>()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRun {
    pub config: DifftraceConfig,
    pub base: String,
    pub repo: Option<String>,
    pub pr: Option<u64>,
    pub scope: Scope,
    pub severity: Severity,
    pub oracle: OraclePolicy,
    pub focus: Vec<String>,
    pub passes: u64,
    pub budget_secs: u64,
    pub dry_run: bool,
    pub json: bool,
    pub fix_prompts: bool,
}

pub fn resolve_run(args: &ReviewArgs, env: &EnvSource) -> Result<ResolvedRun, DtError> {
    args.validate()?;
    let mut config = load_layered_config(args.config.as_deref(), None, None)?;
    apply_env_overrides_with(&mut config, env)?;
    if let Some(count) = args.judges {
        config.judges.count = count;
    }
    if !args.judge.is_empty() {
        let names: Vec<String> = args
            .judge
            .iter()
            .filter(|name| !name.is_empty())
            .cloned()
            .collect();
        if names.is_empty() {
            return Err(DtError::Config(
                "judges.profiles must name at least one profile".to_owned(),
            ));
        }
        config.judges.profiles = names;
    }
    if config.judges.count == 0 {
        return Err(DtError::Config(
            "judges.count must be at least one".to_owned(),
        ));
    }
    if config.judges.profiles.is_empty() {
        return Err(DtError::Config(
            "judges.profiles must name at least one profile".to_owned(),
        ));
    }
    let scope = match args.scope {
        Some(scope) => scope,
        None => config
            .review
            .scope
            .parse::<Scope>()
            .map_err(|source| DtError::Config(format!("review.scope: {source}")))?,
    };
    let severity = match args.severity {
        Some(severity) => severity,
        None => config
            .review
            .severity
            .parse::<Severity>()
            .map_err(|source| DtError::Config(format!("review.severity: {source}")))?,
    };
    let oracle = match args.oracle {
        Some(policy) => policy,
        None => config
            .oracle
            .policy
            .parse::<OraclePolicy>()
            .map_err(|source| DtError::Config(format!("oracle.policy: {source}")))?,
    };
    let passes = positive_knob(
        args.passes,
        env.lookup("DIFFTRACE_PASSES"),
        1,
        "DIFFTRACE_PASSES",
    )?;
    let budget_secs = positive_knob(
        args.budget_secs,
        env.lookup("DIFFTRACE_BUDGET_SECS"),
        1800,
        "DIFFTRACE_BUDGET_SECS",
    )?;
    let focus = match args.focus.clone() {
        Some(list) => list.into_iter().filter(|item| !item.is_empty()).collect(),
        None => env
            .lookup("DIFFTRACE_FOCUS")
            .map(|raw| config::split_list(&raw))
            .unwrap_or_default(),
    };
    Ok(ResolvedRun {
        config,
        base: args.base.clone().unwrap_or_else(|| "master".to_owned()),
        repo: args.repo.clone(),
        pr: args.pr,
        scope,
        severity,
        oracle,
        focus,
        passes,
        budget_secs,
        dry_run: args.dry_run,
        json: args.json,
        fix_prompts: args.fix_prompts,
    })
}

fn positive_knob(
    flag: Option<u64>,
    env_value: Option<String>,
    default: u64,
    name: &str,
) -> Result<u64, DtError> {
    let raw = match flag {
        Some(value) => value,
        None => match env_value {
            Some(text) => text.parse::<u64>().map_err(|source| {
                DtError::Config(format!("{name}: expected a number, {source}"))
            })?,
            None => default,
        },
    };
    if raw == 0 {
        return Err(DtError::Config(format!("{name} must be at least one")));
    }
    Ok(raw)
}

pub fn dispatch(command: &Command) -> Result<RunOutcome, DtError> {
    dispatch_with(command, &EnvSource::live())
}

pub fn dispatch_with(command: &Command, env: &EnvSource) -> Result<RunOutcome, DtError> {
    if let Command::Review(args) = command {
        resolve_run(args, env)?;
    }
    Err(not_yet(command.as_str()))
}

fn not_yet(subcommand: &str) -> DtError {
    DtError::Cli(format!(
        "{subcommand} is not yet implemented on the v2 skeleton"
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Clean,
    BlockingFindings,
    InfrastructureFailure,
    PartialRound,
}

impl RunOutcome {
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::BlockingFindings => 1,
            Self::InfrastructureFailure => 2,
            Self::PartialRound => 3,
        }
    }
}

impl From<DtError> for RunOutcome {
    fn from(err: DtError) -> Self {
        match err {
            DtError::PartialRound { .. } => Self::PartialRound,
            _ => Self::InfrastructureFailure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn parse(words: &[&str]) -> Result<Command, DtError> {
        Cli::try_parse_from(std::iter::once("difftrace").chain(words.iter().copied()))
            .map(|parsed| parsed.command)
            .map_err(|source| DtError::Cli(source.to_string()))
    }

    fn env_of(pairs: &[(&str, &str)]) -> EnvSource {
        let vars = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>();
        EnvSource::from_maps(vars, BTreeMap::new())
    }

    #[test]
    fn known_subcommands_parse_and_unknown_ones_are_rejected() {
        for word in SUBCOMMANDS {
            let parsed = parse(&[word, "list"])
                .or_else(|_| parse(&[word, "show"]))
                .or_else(|_| parse(&[word, "run", "--set", "golden", "--runner", "v1"]))
                .or_else(|_| parse(&[word, "target"]))
                .or_else(|_| parse(&[word, "an-id"]))
                .or_else(|_| parse(&[word]));
            assert_eq!(parsed.as_ref().map(Command::as_str).ok(), Some(word));
        }
        assert!(parse(&["teleport"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        assert_eq!(RunOutcome::Clean.exit_code(), 0);
        assert_eq!(RunOutcome::BlockingFindings.exit_code(), 1);
        assert_eq!(RunOutcome::InfrastructureFailure.exit_code(), 2);
        assert_eq!(RunOutcome::PartialRound.exit_code(), 3);
    }

    #[test]
    fn the_full_review_flag_surface_parses() -> Result<(), DtError> {
        let parsed = parse(&[
            "review",
            "--base",
            "main",
            "--scope",
            "deep",
            "--focus",
            "security,tests",
            "--severity",
            "warning",
            "--judges",
            "5",
            "--judge",
            "zai:glm-5.3",
            "--judge",
            "local",
            "--passes",
            "3",
            "--oracle",
            "check",
            "--budget-secs",
            "900",
            "--config",
            "/tmp/opencode/difftrace.toml",
            "--dry-run",
            "--json",
            "--fix-prompts",
        ])?;
        let Command::Review(args) = parsed else {
            return Err(DtError::Cli("expected the review subcommand".to_owned()));
        };
        assert_eq!(args.base.as_deref(), Some("main"));
        assert_eq!(args.scope, Some(Scope::Deep));
        assert_eq!(
            args.focus,
            Some(vec!["security".to_owned(), "tests".to_owned()])
        );
        assert_eq!(args.severity, Some(Severity::Warning));
        assert_eq!(args.judges, Some(5));
        assert_eq!(
            args.judge,
            vec!["zai:glm-5.3".to_owned(), "local".to_owned()]
        );
        assert_eq!(args.passes, Some(3));
        assert_eq!(args.oracle, Some(OraclePolicy::Check));
        assert_eq!(args.budget_secs, Some(900));
        assert_eq!(
            args.config,
            Some(PathBuf::from("/tmp/opencode/difftrace.toml"))
        );
        assert!(args.dry_run && args.json && args.fix_prompts);
        assert!(args.repo.is_none() && args.pr.is_none());
        let bare = parse(&["review"])?;
        let Command::Review(empty) = bare else {
            return Err(DtError::Cli("expected the review subcommand".to_owned()));
        };
        assert_eq!(empty, ReviewArgs::default());
        assert!(parse(&["review", "--scope", "sideways"]).is_err());
        assert!(parse(&["review", "--severity", "loud"]).is_err());
        assert!(parse(&["review", "--oracle", "maybe"]).is_err());
        Ok(())
    }

    #[test]
    fn base_and_repo_are_mutually_exclusive() {
        let both = ReviewArgs {
            base: Some("main".to_owned()),
            repo: Some("owner/name".to_owned()),
            pr: Some(7),
            ..ReviewArgs::default()
        };
        assert!(both.validate().is_err());
        let hosted_without_pr = ReviewArgs {
            repo: Some("owner/name".to_owned()),
            ..ReviewArgs::default()
        };
        assert!(hosted_without_pr.validate().is_err());
        let pr_without_repo = ReviewArgs {
            pr: Some(7),
            ..ReviewArgs::default()
        };
        assert!(pr_without_repo.validate().is_err());
        let full_hosted = ReviewArgs {
            repo: Some("owner/name".to_owned()),
            pr: Some(7),
            scope: Some(Scope::Full),
            ..ReviewArgs::default()
        };
        assert!(full_hosted.validate().is_err());
        let local = ReviewArgs::default();
        assert!(local.validate().is_ok());
        let hosted = ReviewArgs {
            repo: Some("owner/name".to_owned()),
            pr: Some(7),
            ..ReviewArgs::default()
        };
        assert!(hosted.validate().is_ok());
    }

    #[test]
    fn every_subcommand_surface_parses() -> Result<(), DtError> {
        let reject = parse(&["reject", "some-id", "--reason", "not a defect", "--theme"])?;
        assert_eq!(
            reject,
            Command::Reject {
                id: "some-id".to_owned(),
                reason: Some("not a defect".to_owned()),
                theme: true,
            }
        );
        assert_eq!(
            parse(&["suppress", "list"])?,
            Command::Suppress {
                action: SuppressAction::List,
            }
        );
        assert_eq!(
            parse(&["suppress", "clear", "an-id"])?,
            Command::Suppress {
                action: SuppressAction::Clear {
                    id: Some("an-id".to_owned()),
                },
            }
        );
        assert_eq!(
            parse(&["state", "show"])?,
            Command::State {
                action: StateAction::Show,
            }
        );
        assert_eq!(
            parse(&["state", "reset"])?,
            Command::State {
                action: StateAction::Reset,
            }
        );
        assert_eq!(
            parse(&["reply", "https://example.invalid/pr/1", "--mode", "plan"])?,
            Command::Reply {
                target: "https://example.invalid/pr/1".to_owned(),
                mode: ReplyMode::Plan,
            }
        );
        assert_eq!(
            parse(&["reply", "pr"])?,
            Command::Reply {
                target: "pr".to_owned(),
                mode: ReplyMode::Answer,
            }
        );
        assert!(parse(&["reply", "pr", "--mode", "chat"]).is_err());
        assert_eq!(
            parse(&[
                "eval",
                "run",
                "--set",
                "golden",
                "--runner",
                "coderabbit",
                "--json"
            ])?,
            Command::Eval {
                action: EvalAction::Run {
                    set: "golden".to_owned(),
                    runner: "coderabbit".to_owned(),
                    json: true,
                },
            }
        );
        Ok(())
    }

    #[test]
    fn dispatch_keeps_the_not_yet_contract() -> Result<(), DtError> {
        let dir = temp_dir("dispatch")?;
        let empty_config = dir.join("empty.toml");
        std::fs::write(&empty_config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let env = env_of(&[]);
        let review = parse(&[
            "review",
            "--config",
            empty_config.to_str().unwrap_or_default(),
        ])?;
        let err = dispatch_with(&review, &env)
            .err()
            .unwrap_or(DtError::Cli(String::new()));
        assert!(
            err.to_string().contains("not yet implemented"),
            "review dispatches to a not-yet error: {err}"
        );
        for word in ["reject", "suppress", "state", "reply", "eval"] {
            let command = parse(&[word, "list"])
                .or_else(|_| parse(&[word, "show"]))
                .or_else(|_| parse(&[word, "run", "--set", "golden", "--runner", "v1"]))
                .or_else(|_| parse(&[word, "target"]))
                .or_else(|_| parse(&[word, "an-id"]))
                .or_else(|_| parse(&[word]));
            let command = command.map_err(|source| DtError::Cli(source.to_string()))?;
            let err = dispatch_with(&command, &env)
                .err()
                .unwrap_or(DtError::Cli(String::new()));
            assert!(
                err.to_string().contains("not yet implemented"),
                "{word} dispatches to a not-yet error: {err}"
            );
        }
        std::fs::remove_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    #[test]
    fn non_utf8_arguments_still_error_cleanly() {
        let lossy = String::from_utf8_lossy(&[0x2d, 0x2d, 0x66, 0xff, 0xff, 0x6e]).into_owned();
        assert!(parse(&[lossy.as_str()]).is_err());
        assert!(Cli::try_parse_from(["difftrace", lossy.as_str()]).is_err());
    }

    fn write_config(dir: &Path, body: &str) -> Result<(), DtError> {
        std::fs::create_dir_all(dir.join(".difftrace"))
            .map_err(|source| DtError::Config(source.to_string()))?;
        std::fs::write(dir.join(".difftrace").join("config.toml"), body)
            .map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    fn temp_dir(name: &str) -> Result<std::path::PathBuf, DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-{name}"));
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(dir)
    }

    #[test]
    fn layering_flag_beats_env_beats_file() -> Result<(), DtError> {
        let dir = temp_dir("layering")?;
        write_config(&dir, "[review]\nscope = \"diff\"\n")?;
        let file_scope = ReviewArgs {
            config: Some(dir.join(".difftrace").join("config.toml")),
            ..ReviewArgs::default()
        };
        let file_only = resolve_run(&file_scope, &env_of(&[]))?;
        assert_eq!(file_only.scope, Scope::Diff);
        assert_eq!(file_only.passes, 1);
        assert_eq!(file_only.budget_secs, 1800);
        let env_layer = resolve_run(&file_scope, &env_of(&[("DIFFTRACE_SCOPE", "deep")]))?;
        assert_eq!(env_layer.scope, Scope::Deep);
        let flag_layer = resolve_run(
            &ReviewArgs {
                config: Some(dir.join(".difftrace").join("config.toml")),
                scope: Some(Scope::Full),
                passes: Some(3),
                budget_secs: Some(900),
                ..ReviewArgs::default()
            },
            &env_of(&[
                ("DIFFTRACE_SCOPE", "deep"),
                ("DIFFTRACE_PASSES", "2"),
                ("DIFFTRACE_BUDGET_SECS", "2700"),
            ]),
        )?;
        assert_eq!(flag_layer.scope, Scope::Full);
        assert_eq!(flag_layer.passes, 3);
        assert_eq!(flag_layer.budget_secs, 900);
        let env_knobs = resolve_run(
            &file_scope,
            &env_of(&[
                ("DIFFTRACE_PASSES", "2"),
                ("DIFFTRACE_BUDGET_SECS", "2700"),
                ("DIFFTRACE_FOCUS", "security, tests"),
            ]),
        )?;
        assert_eq!(env_knobs.passes, 2);
        assert_eq!(env_knobs.budget_secs, 2700);
        assert_eq!(
            env_knobs.focus,
            vec!["security".to_owned(), "tests".to_owned()]
        );
        let empty_env = resolve_run(&file_scope, &env_of(&[("DIFFTRACE_PASSES", "")]))?;
        assert_eq!(empty_env.passes, 1);
        let bad_env = resolve_run(&file_scope, &env_of(&[("DIFFTRACE_PASSES", "zero")]));
        assert!(bad_env.is_err_and(|err| err.to_string().contains("DIFFTRACE_PASSES")));
        std::fs::remove_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    #[test]
    fn an_empty_judge_name_rejects_at_the_boundary() -> Result<(), DtError> {
        let dir = temp_dir("judge-empty")?;
        let config = dir.join("empty.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let empty = resolve_run(
            &ReviewArgs {
                judge: vec![String::new()],
                config: Some(config.clone()),
                ..ReviewArgs::default()
            },
            &env_of(&[]),
        );
        assert!(
            empty.is_err_and(|err| err.to_string().contains("judges.profiles")),
            "an empty --judge value must reject like the env path, not resolve an empty profile name"
        );
        let mixed = resolve_run(
            &ReviewArgs {
                judge: vec![String::new(), "zai:glm-5.3".to_owned()],
                config: Some(config),
                ..ReviewArgs::default()
            },
            &env_of(&[]),
        )?;
        assert_eq!(
            mixed.config.judges.profiles,
            vec!["zai:glm-5.3".to_owned()],
            "an empty entry among real names drops rather than poisoning the list"
        );
        std::fs::remove_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }
}
