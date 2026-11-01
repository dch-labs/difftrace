//! The command surface: the full flag grammar, layered
//! configuration resolution, and the exit-code contract; dispatch
//! validates inputs and reports what is not yet implemented.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use crate::aspects::AspectRegistry;
use crate::config::{
    self, DifftraceConfig, EnvSource, OraclePolicy, apply_env_overrides_with, load_layered_config,
};
use crate::contracts::{AspectId, DiffSource, IssueStatus, RoundVerdict, Severity};
use crate::diff::{self, Scope};
use crate::error::DtError;
use crate::gateway::PrGateway;
use crate::gateway::local::LocalGateway;
use crate::judges;
use crate::judges::pass::JudgeSpec;
use crate::logging;
use crate::orchestrator::{self, RunBudget};
use crate::render::{self, RenderOptions};
use crate::reply;
use crate::state::{self, file};

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
    #[arg(long)]
    pub actor: Option<String>,
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
    pub region_retries: Option<u64>,
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
                "--base and --repo are mutually exclusive: local mode reviews merge-base(<base>, HEAD) using the LOCAL ref named by --base (an origin/<base> fallback is not implied), hosted mode needs --repo with --pr".to_owned(),
            ));
        }
        if self.repo.is_some() && self.pr.is_none() {
            return Err(DtError::Cli(
                "--repo requires --pr with the pull request number".to_owned(),
            ));
        }
        if self.repo.is_some() && self.actor.is_none() {
            return Err(DtError::Cli(
                "--repo requires --actor <login>: the comment author spending the model tokens must hold write access"
                    .to_owned(),
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
    pub actor: Option<String>,
    pub scope: Scope,
    pub severity: Severity,
    pub oracle: Option<OraclePolicy>,
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
    if let Some(name) = config
        .judges
        .profiles
        .iter()
        .find(|name| name.contains('#'))
    {
        return Err(DtError::Config(format!(
            "judges profile {name:?} must not contain '#': the panel mints replica ids as profile#2 and the marker must stay reserved"
        )));
    }
    if let Some(warning) = under_quota_panel_warning(&config.judges.profiles, config.judges.count) {
        tracing::warn!(target: "difftrace::cli", "{warning}");
    }
    if let Some(warning) =
        single_provider_panel_warning(&config.judges.profiles, config.judges.count)
    {
        tracing::warn!(target: "difftrace::cli", "{warning}");
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
        Some(policy) => Some(policy),
        None => match env.lookup("DIFFTRACE_ORACLE") {
            Some(raw) => Some(
                raw.parse::<OraclePolicy>()
                    .map_err(|source| DtError::Config(format!("DIFFTRACE_ORACLE: {source}")))?,
            ),
            None => match config.oracle.policy.as_deref() {
                Some(text) => Some(
                    text.parse::<OraclePolicy>()
                        .map_err(|source| DtError::Config(format!("oracle.policy: {source}")))?,
                ),
                None => None,
            },
        },
    };
    let passes = positive_knob(
        args.passes,
        env.lookup("DIFFTRACE_PASSES"),
        1,
        "--passes",
        "DIFFTRACE_PASSES",
    )?;
    let budget_secs = optional_knob(
        args.budget_secs,
        env.lookup("DIFFTRACE_BUDGET_SECS"),
        0,
        "DIFFTRACE_BUDGET_SECS",
    )?;
    let region_retries = optional_knob(
        args.region_retries,
        env.lookup("DIFFTRACE_REGION_RETRIES"),
        u64::from(config.judges.region_retries),
        "DIFFTRACE_REGION_RETRIES",
    )?;
    if region_retries > 10 {
        return Err(DtError::Cli(
            "--region-retries must be at most 10: unbounded retries belong under an explicit --budget-secs ceiling"
                .to_owned(),
        ));
    }
    config.judges.region_retries = u8::try_from(region_retries)
        .map_err(|_| DtError::Cli("--region-retries must be at most 10".to_owned()))?;
    let focus = match args.focus.clone() {
        Some(list) => {
            let kept: Vec<String> = list.into_iter().filter(|item| !item.is_empty()).collect();
            if kept.is_empty() {
                return Err(DtError::Cli(
                    "--focus must name at least one aspect id: an all-empty list would silently review everything"
                        .to_owned(),
                ));
            }
            kept
        }
        None => match env.lookup("DIFFTRACE_FOCUS") {
            Some(raw) => {
                let list = config::split_list(&raw);
                if list.is_empty() {
                    return Err(DtError::Config(
                        "DIFFTRACE_FOCUS: expected at least one aspect id".to_owned(),
                    ));
                }
                list
            }
            None => Vec::new(),
        },
    };
    Ok(ResolvedRun {
        config,
        base: args.base.clone().unwrap_or_else(|| "master".to_owned()),
        repo: args.repo.clone(),
        pr: args.pr,
        actor: args.actor.clone(),
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
    flag_name: &str,
    env_name: &str,
) -> Result<u64, DtError> {
    let (raw, origin) = match flag {
        Some(value) => (value, flag_name),
        None => match env_value {
            Some(text) => (
                text.parse::<u64>().map_err(|source| {
                    DtError::Config(format!("{env_name}: expected a number, {source}"))
                })?,
                env_name,
            ),
            None => (default, env_name),
        },
    };
    if raw == 0 {
        return Err(DtError::Config(format!("{origin} must be at least one")));
    }
    Ok(raw)
}

fn optional_knob(
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
    Ok(raw)
}

pub fn dispatch(command: &Command) -> Result<RunOutcome, DtError> {
    dispatch_with(command, &EnvSource::live())
}

#[must_use]
pub fn usage_error_line(err: &clap::Error) -> String {
    format!("difftrace: usage error: {err}")
}

pub fn dispatch_with(command: &Command, env: &EnvSource) -> Result<RunOutcome, DtError> {
    let start = std::env::current_dir()
        .map_err(|err| DtError::Git(format!("cannot read the working directory: {err}")))?;
    let home = dirs::home_dir().ok_or_else(|| DtError::Config("no home directory".to_owned()))?;
    let mut stdout = io::stdout().lock();
    dispatch_at(command, env, &start, &home, &mut stdout)
}

fn dispatch_at(
    command: &Command,
    env: &EnvSource,
    start: &Path,
    home: &Path,
    out: &mut dyn Write,
) -> Result<RunOutcome, DtError> {
    if let Command::Review(args) = command {
        let resolved = resolve_run(args, env)?;
        if resolved.repo.is_some() {
            return run_hosted_dispatch(&resolved, env, start, home, out);
        }
        if resolved.dry_run {
            let text = run_dry_run_at(start, &resolved, &home.join(".difftrace").join("state"))?;
            write_report(out, &text)?;
            return Ok(RunOutcome::Clean);
        }
        let gateway = LocalGateway::open_at(start, &resolved.base)?;
        return run_local_review_at(&resolved, gateway, home, out);
    }
    if let Command::Reject { id, reason, theme } = command {
        return run_reject_at(
            start,
            home,
            out,
            id,
            reason.as_deref().unwrap_or("owner rejected"),
            *theme,
        );
    }
    if let Command::Suppress { action } = command {
        return run_suppress_at(start, home, out, action);
    }
    if let Command::Reply { target, mode } = command {
        return run_reply_at(start, home, out, target, *mode == ReplyMode::Plan);
    }
    Err(not_yet(command.as_str()))
}

fn write_report(out: &mut dyn Write, text: &str) -> Result<(), DtError> {
    let outcome = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    match outcome {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(DtError::Output(err.to_string())),
    }
}

fn run_reject_at(
    start: &Path,
    home: &Path,
    out: &mut dyn Write,
    id: &str,
    reason: &str,
    theme: bool,
) -> Result<RunOutcome, DtError> {
    let gateway = LocalGateway::open_head(start)?;
    let checkout = gateway.checkout_label();
    let head = gateway.pinned_range().head_sha;
    let state_dir = home.join(".difftrace").join("state");
    let state_path = file::state_path(&state_dir, &checkout);
    let mut state = file::load(&state_path, &checkout)?;
    let (report_path, report) = newest_report(home, &checkout)?;
    let finding = report
        .confirmed
        .iter()
        .map(|entry| &entry.finding)
        .chain(report.solo.iter().map(|entry| &entry.finding))
        .chain(report.weak.iter().map(|lead| &lead.finding))
        .find(|finding| finding.id.0 == id);
    let Some(finding) = finding else {
        return Err(DtError::Config(format!(
            "no finding {id:?} in the newest report {} — reject takes a finding id from the latest review",
            report_path.display()
        )));
    };
    let Some(cite) = finding.evidence.first() else {
        return Err(DtError::Config(format!(
            "finding {id:?} carries no anchored cite to key the suppression"
        )));
    };
    let suppression_id = state::reject_finding(
        &mut state,
        &cite.file,
        cite.line,
        &finding.title,
        theme,
        reason,
        &head,
    );
    file::save(&state_path, &state)?;
    let rule = state
        .suppressions
        .iter()
        .find(|rule| rule.id == suppression_id)
        .ok_or_else(|| DtError::Config("the fresh suppression is missing".to_owned()))?;
    write_report(
        out,
        &format!(
            "rejected {id} · suppression {suppression_id} · theme \"{}\" · glob {}\n",
            rule.theme, rule.file_glob
        ),
    )?;
    Ok(RunOutcome::Clean)
}

fn report_order_key(path: &Path) -> (u64, u32) {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let stem = name.strip_suffix(".report.json").unwrap_or(name.as_str());
    let mut fields = stem.rsplit('-');
    let tail = fields.next().unwrap_or_default();
    let stamp = fields
        .next()
        .unwrap_or_default()
        .parse::<u64>()
        .unwrap_or(0);
    let collision = tail
        .rsplit_once('.')
        .and_then(|(_, suffix)| suffix.parse::<u32>().ok())
        .unwrap_or(0);
    (stamp, collision)
}

fn newest_report(home: &Path, checkout: &str) -> Result<(PathBuf, RoundVerdict), DtError> {
    let reviews = home.join(".difftrace").join("reviews");
    let prefix = format!("{}-", state::file::sanitize_label(checkout));
    let mut reports: Vec<PathBuf> = std::fs::read_dir(&reviews)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name().is_some_and(|name| {
                        let name = name.to_string_lossy();
                        name.starts_with(&prefix) && name.ends_with(".report.json")
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    reports.sort_by_key(|path| report_order_key(path));
    let Some(newest) = reports.pop() else {
        return Err(DtError::Config(format!(
            "no report for checkout {checkout:?} under {} — run a review first",
            reviews.display()
        )));
    };
    let text = std::fs::read_to_string(&newest)
        .map_err(|err| DtError::Config(format!("cannot read {}: {err}", newest.display())))?;
    let verdict: RoundVerdict = serde_json::from_str(&text).map_err(|err| {
        DtError::Config(format!(
            "the report {} does not parse: {err}",
            newest.display()
        ))
    })?;
    Ok((newest, verdict))
}

fn run_suppress_at(
    start: &Path,
    home: &Path,
    out: &mut dyn Write,
    action: &SuppressAction,
) -> Result<RunOutcome, DtError> {
    let gateway = LocalGateway::open_head(start)?;
    let checkout = gateway.checkout_label();
    let state_dir = home.join(".difftrace").join("state");
    let state_path = file::state_path(&state_dir, &checkout);
    let mut state = file::load(&state_path, &checkout)?;
    match action {
        SuppressAction::List => {
            if state.suppressions.is_empty() {
                write_report(out, "no suppressions\n")?;
            } else {
                let lines: Vec<String> = state
                    .suppressions
                    .iter()
                    .map(|rule| {
                        format!(
                            "{} · theme \"{}\" · {} · hits {} · {} (created {})\n",
                            rule.id,
                            rule.theme,
                            rule.file_glob,
                            rule.hits,
                            rule.reason,
                            rule.created
                        )
                    })
                    .collect();
                write_report(out, &lines.join(""))?;
            }
        }
        SuppressAction::Clear { id } => {
            let rejected_before = state
                .issues
                .iter()
                .filter(|issue| matches!(issue.status, IssueStatus::Rejected { .. }))
                .count();
            let removed = state::clear_suppression(&mut state, id.as_deref());
            if removed > 0 {
                file::save(&state_path, &state)?;
            }
            let reopened = rejected_before.saturating_sub(
                state
                    .issues
                    .iter()
                    .filter(|issue| matches!(issue.status, IssueStatus::Rejected { .. }))
                    .count(),
            );
            let receipt = match (removed, id) {
                (0, Some(wanted)) => format!("no suppression {wanted:?} to clear\n"),
                (0, None) => "no suppressions to clear\n".to_owned(),
                (count, _) => {
                    format!("cleared {count} suppression(s), reopened {reopened} issue(s)\n")
                }
            };
            write_report(out, &receipt)?;
        }
    }
    Ok(RunOutcome::Clean)
}

fn prepare_reply(
    start: &Path,
    home: &Path,
    question: &str,
    plan: bool,
) -> Result<(reply::ReplyJob, Option<String>), DtError> {
    let gateway = LocalGateway::open_head(start)?;
    let checkout = gateway.checkout_label();
    let state_path = file::state_path(&home.join(".difftrace").join("state"), &checkout);
    let state = file::load(&state_path, &checkout)?;
    let (_report_path, report) = newest_report(home, &checkout)?;
    let confirmation = reply::reject_intent(question).map(|id| {
        format!("to reject it, run: difftrace reject {id} — chat never writes state itself")
    });
    let job = reply::ReplyJob {
        question: question.to_owned(),
        plan,
        context: reply::context_block(&report, &state),
    };
    Ok((job, confirmation))
}

fn run_reply_at(
    start: &Path,
    home: &Path,
    out: &mut dyn Write,
    question: &str,
    plan: bool,
) -> Result<RunOutcome, DtError> {
    let (job, confirmation) = prepare_reply(start, home, question, plan)?;
    let mut config = config::load_layered_config(None, None, None)?;
    config::apply_env_overrides_with(&mut config, &EnvSource::live())?;
    let profile = config
        .judges
        .profiles
        .first()
        .cloned()
        .unwrap_or_else(|| "zai".to_owned());
    let client =
        judges::provider::client_for_profile(&profile, config.judges.model_settings(&profile))?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| DtError::Config(format!("cannot start the async runtime: {err}")))?;
    let answered = match client {
        judges::provider::JudgeClient::Zai(client)
        | judges::provider::JudgeClient::Anthropic(client) => {
            runtime.block_on(reply::run_reply_with(job, Arc::new(client)))?
        }
        judges::provider::JudgeClient::Local(client) => {
            runtime.block_on(reply::run_reply_with(job, Arc::new(client)))?
        }
    };
    write_report(
        out,
        &render::chat::render_answer(question, &answered.text, confirmation.as_deref()),
    )?;
    Ok(RunOutcome::Clean)
}

fn run_hosted_dispatch(
    resolved: &ResolvedRun,
    env: &EnvSource,
    start: &Path,
    home: &Path,
    out: &mut dyn Write,
) -> Result<RunOutcome, DtError> {
    let token = env
        .lookup("GITHUB_TOKEN")
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            DtError::Cli("hosted review needs GITHUB_TOKEN in the environment".to_owned())
        })?;
    let repo = resolved.repo.clone().unwrap_or_default();
    let (owner, name) = split_repo(&repo)?;
    let pr = resolved
        .pr
        .ok_or_else(|| DtError::Cli("--repo requires --pr".to_owned()))?;
    let actor = resolved
        .actor
        .clone()
        .ok_or_else(|| DtError::Cli("--repo requires --actor <login>".to_owned()))?;
    let gateway = crate::gateway::github::GitHubGateway::open_pr(start, &token, owner, name, pr)?;
    let range = gateway.pinned_range();
    let root = gateway.root().to_path_buf();
    let shared = Arc::new(gateway);
    run_hosted_review_at(
        HostedRun {
            resolved,
            gateway: shared,
            repo_root: root,
            range,
            home,
            actor: &actor,
        },
        out,
    )
}

fn split_repo(repo: &str) -> Result<(&str, &str), DtError> {
    match repo.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
            Ok((owner, name))
        }
        _ => Err(DtError::Cli(format!(
            "--repo must read owner/name, got {repo:?}"
        ))),
    }
}

fn posting_index(gateway: &dyn PrGateway) -> diff::DiffIndex {
    let DiffSource::Unified(text) = gateway.diff();
    diff::DiffIndex::parse(&text).unwrap_or_default()
}

fn regions_for(
    resolved: &ResolvedRun,
    gateway: &dyn PrGateway,
) -> Result<Vec<crate::contracts::Region>, DtError> {
    let max_lines = resolved.config.judges.region_max_lines;
    let DiffSource::Unified(text) = gateway.diff();
    let built = match resolved.scope {
        Scope::Full => {
            let index = diff::DiffIndex::parse(&text)
                .map_err(|err| DtError::Git(format!("the pinned diff does not parse: {err}")))?;
            diff::repo_regions_bounded(&gateway.list_files_at_head()?, &index)
        }
        Scope::Diff | Scope::Deep => diff::build_regions(&text, resolved.scope)
            .map_err(|err| DtError::Git(format!("the pinned diff does not parse: {err}")))?,
    };
    Ok(diff::fit_regions(
        &built,
        &text,
        max_lines,
        diff::REGION_MATERIAL_BUDGET_BYTES,
    ))
}

struct HostedRun<'a, G: PrGateway + 'static> {
    resolved: &'a ResolvedRun,
    gateway: Arc<G>,
    repo_root: PathBuf,
    range: crate::contracts::PinnedRange,
    home: &'a Path,
    actor: &'a str,
}

fn shared_gateway<G: PrGateway + 'static>(gateway: &Arc<G>) -> Arc<dyn PrGateway> {
    let cloned: Arc<G> = Arc::clone(gateway);
    cloned
}

fn run_hosted_review_at<G: crate::hosted::HostedOps + PrGateway + 'static>(
    run: HostedRun<'_, G>,
    out: &mut dyn Write,
) -> Result<RunOutcome, DtError> {
    let HostedRun {
        resolved,
        gateway,
        repo_root,
        range,
        home,
        actor,
    } = run;
    let hosted: &dyn crate::hosted::HostedOps = gateway.as_ref();
    let permission = hosted.permission(actor)?;
    if !crate::hosted::permission_allows(&permission) {
        return Err(DtError::Cli(format!(
            "actor {actor:?} holds {permission:?} access on the repository — a hosted review needs write"
        )));
    }
    let marker = hosted.marker_comment()?;
    let mut state = None;
    if let Some((_id, body)) = &marker {
        state = state::marker::extract(body)?;
    }
    let head_runs = hosted.fetch_check_runs(&range.head_sha)?;
    let base_runs = hosted.fetch_check_runs(&range.base_sha)?;
    let mut config = resolved.config.clone();
    let policy = OraclePolicy::resolve(
        resolved.oracle.map(|policy| policy.to_string()).as_deref(),
        &repo_root,
    )?;
    config.oracle.policy = Some(policy.to_string());
    let aspects = AspectRegistry::from_config(&config.aspects)?;
    let focus = resolved
        .focus
        .iter()
        .map(|id| AspectId(id.clone()))
        .collect::<Vec<_>>();
    let regions = regions_for(resolved, gateway.as_ref())?;
    let plan = judges::plan(range, &regions, &aspects, &focus, resolved.severity)?;
    let budget = RunBudget {
        total: Duration::from_secs(resolved.budget_secs),
        tools: Duration::from_secs(config.tools.budget_secs),
        oracle: Duration::from_secs(config.oracle.budget_secs),
    };
    let checkout = repo_root.file_name().map_or_else(
        || "_".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let inputs = orchestrator::RunInputs {
        gateway: shared_gateway(&gateway),
        repo_root,
        checkout,
        judges: judge_specs(resolved),
        plan,
        budget,
        state_dir: home.join(".difftrace").join("state"),
        cache_dir: home.join(".difftrace").join("cache"),
        state,
        tool_source: orchestrator::ToolSource::CheckRuns {
            head: head_runs,
            base: base_runs,
        },
        dry_run: resolved.dry_run,
        config,
    };
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| DtError::Config(format!("cannot start the async runtime: {err}")))?;
    let ((verdict, outcome), pending) = runtime.block_on(orchestrator::run_review(inputs))?;
    let options = RenderOptions {
        fix_prompts: resolved.fix_prompts,
    };
    if resolved.dry_run {
        let threads = hosted.own_threads()?;
        let posting = crate::hosted::plan_posting(
            &verdict,
            &threads,
            outcome == RunOutcome::PartialRound,
            &posting_index(gateway.as_ref()),
        );
        let next_state = pending.as_ref().unwrap_or(&verdict.state);
        let body = state::marker::embed(&render::comment::render(&verdict, &options), next_state)?;
        write_report(
            out,
            &format!(
                "{}\n--- the comment that would be posted ---\n{body}\n--- the posting plan ---\ninline {} · replies {} · resolutions {}\n",
                render::console::render(&verdict, &options),
                posting.review.comments.len(),
                posting.replies.len(),
                posting.resolutions.len()
            ),
        )?;
        return Ok(outcome);
    }
    let threads = hosted.own_threads()?;
    let posting = crate::hosted::plan_posting(
        &verdict,
        &threads,
        outcome == RunOutcome::PartialRound,
        &posting_index(gateway.as_ref()),
    );
    hosted.submit_review(&posting.review)?;
    for reply in &posting.replies {
        if let Err(err) = hosted.reply_to_thread(reply.comment_id, &reply.body) {
            tracing::warn!(
                target: "difftrace::hosted",
                cause = %err,
                "a re-raise reply failed; the thread stays open"
            );
        }
    }
    for resolution in &posting.resolutions {
        if let Err(err) = hosted.reply_to_thread(resolution.comment_id, &resolution.note) {
            tracing::warn!(
                target: "difftrace::hosted",
                cause = %err,
                "the resolution note failed; resolving anyway"
            );
        }
        if let Err(err) = hosted.resolve_thread(&resolution.thread_id) {
            tracing::warn!(
                target: "difftrace::hosted",
                cause = %err,
                "thread resolution failed; the next round retries it"
            );
        }
    }
    let next_state = pending.as_ref().unwrap_or(&verdict.state);
    let body = state::marker::embed(&render::comment::render(&verdict, &options), next_state)?;
    upsert_marker_comment(hosted, marker.as_ref(), &body)?;
    write_report(
        out,
        &format!(
            "review posted: {} inline · {} re-raise replies · {} resolutions · exit {}\n",
            posting.review.comments.len(),
            posting.replies.len(),
            posting.resolutions.len(),
            outcome.exit_code()
        ),
    )?;
    Ok(outcome)
}

fn upsert_marker_comment(
    hosted: &dyn crate::hosted::HostedOps,
    marker: Option<&(u64, String)>,
    body: &str,
) -> Result<(), DtError> {
    let mut last = None;
    for _attempt in 0..3 {
        let result = match marker {
            Some((id, _)) => hosted.update_issue_comment(*id, body),
            None => hosted.post_issue_comment(body),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::warn!(
                    target: "difftrace::hosted",
                    cause = %err,
                    "the verdict comment upsert failed; retrying"
                );
                last = Some(err);
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    Err(last
        .unwrap_or_else(|| DtError::Gateway("the verdict comment cannot be updated".to_owned())))
}

#[must_use]
fn under_quota_panel_warning(profiles: &[String], count: usize) -> Option<String> {
    let distinct = profiles.len().min(count);
    (distinct > 0 && distinct < crate::judges::JUDGE_QUORUM).then(|| {
        format!(
            "the panel carries {distinct} distinct {} — a finding needs {} distinct profiles to confirm; singletons will only ever be solo suggestions",
            if distinct == 1 { "profile" } else { "profiles" },
            crate::judges::JUDGE_QUORUM
        )
    })
}

#[must_use]
fn single_provider_panel_warning(profiles: &[String], count: usize) -> Option<String> {
    let providers: std::collections::BTreeSet<&str> = profiles
        .iter()
        .map(|profile| profile.split(':').next().unwrap_or(profile))
        .collect();
    (count >= 2 && providers.len() == 1).then(|| {
        format!(
            "every judge rides the one provider {:?} — one account's quota or outage stalls the whole panel",
            providers.first().copied().unwrap_or_default()
        )
    })
}

fn judge_specs(resolved: &ResolvedRun) -> Vec<JudgeSpec> {
    let judges = &resolved.config.judges;
    let mut specs = Vec::new();
    let mut seen: BTreeMap<&String, usize> = BTreeMap::new();
    for profile in judges.profiles.iter().cycle().take(judges.count) {
        for _ in 0..resolved.passes {
            let occurrence = seen.entry(profile).or_default();
            *occurrence = occurrence.saturating_add(1);
            let id = if *occurrence > 1 {
                format!("{profile}#{occurrence}")
            } else {
                profile.clone()
            };
            specs.push(JudgeSpec {
                id,
                profile: profile.clone(),
                max_tool_calls_per_region: judges.tool_calls_per_region,
            });
        }
    }
    specs
}

fn run_local_review_at(
    resolved: &ResolvedRun,
    gateway: LocalGateway,
    home: &Path,
    out: &mut dyn Write,
) -> Result<RunOutcome, DtError> {
    let repo_root = gateway.root().to_path_buf();
    let checkout = gateway.checkout_label();
    let mut config = resolved.config.clone();
    let policy = OraclePolicy::resolve(
        resolved.oracle.map(|policy| policy.to_string()).as_deref(),
        &repo_root,
    )?;
    config.oracle.policy = Some(policy.to_string());
    let aspects = AspectRegistry::from_config(&config.aspects)?;
    let focus = resolved
        .focus
        .iter()
        .map(|id| AspectId(id.clone()))
        .collect::<Vec<_>>();
    let regions = regions_for(resolved, &gateway)?;
    let plan = judges::plan(
        gateway.pinned_range(),
        &regions,
        &aspects,
        &focus,
        resolved.severity,
    )?;
    let budget = RunBudget {
        total: Duration::from_secs(resolved.budget_secs),
        tools: Duration::from_secs(config.tools.budget_secs),
        oracle: Duration::from_secs(config.oracle.budget_secs),
    };
    let inputs = orchestrator::RunInputs {
        gateway: Arc::new(gateway),
        repo_root,
        checkout: checkout.clone(),
        judges: judge_specs(resolved),
        plan,
        budget,
        state_dir: home.join(".difftrace").join("state"),
        cache_dir: home.join(".difftrace").join("cache"),
        state: None,
        tool_source: orchestrator::ToolSource::Local,
        dry_run: false,
        config,
    };
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| DtError::Config(format!("cannot start the async runtime: {err}")))?;
    let ((verdict, outcome), pending) = runtime.block_on(orchestrator::run_review(inputs))?;
    let text = if resolved.json {
        render::json::render(&verdict)
    } else {
        render::console::render(
            &verdict,
            &RenderOptions {
                fix_prompts: resolved.fix_prompts,
            },
        )
    };
    write_report(out, &text)?;
    if let Some(pending) = pending {
        file::save(
            &file::state_path(&home.join(".difftrace").join("state"), &checkout),
            &pending,
        )?;
    }
    let reviews = home.join(".difftrace").join("reviews");
    let transcript_body = format!("{text}{}", render::region_stats_section(&verdict));
    if let Err(err) = logging::write_transcript(
        &reviews,
        &checkout,
        &verdict.range.head_sha,
        &transcript_body,
    ) {
        tracing::warn!(target: "difftrace::cli", cause = %err, "transcript not written");
    }
    if let Err(err) = logging::write_report(
        &reviews,
        &checkout,
        &verdict.range.head_sha,
        &render::json::render(&verdict),
    ) {
        tracing::warn!(target: "difftrace::cli", cause = %err, "report not written");
    }
    Ok(outcome)
}

pub fn run_dry_run_at(
    start: &Path,
    resolved: &ResolvedRun,
    state_dir: &Path,
) -> Result<String, DtError> {
    let gateway = LocalGateway::open_at(start, &resolved.base)?;
    let checkout = gateway.checkout_label();
    let range = gateway.pinned_range();
    let regions = regions_for(resolved, &gateway)?;
    let registry = AspectRegistry::from_config(&resolved.config.aspects)?;
    let focus = resolved
        .focus
        .iter()
        .map(|id| AspectId(id.clone()))
        .collect::<Vec<_>>();
    let coverage = judges::plan(range, &regions, &registry, &focus, resolved.severity)?;
    let summary = judges::summarize(&coverage, resolved.config.judges.count);
    let mut text = judges::render_dry_run(&summary, &coverage);
    let path = state::file::state_path(state_dir, &checkout);
    let line = if path.exists() {
        let state = state::file::load(&path, &checkout)?;
        let standing = state
            .issues
            .iter()
            .filter(|issue| issue.status == IssueStatus::Open)
            .count();
        format!(
            "state: round {} → {} preview · standing {} · suppressions {}\n",
            state.round,
            state.round.saturating_add(1),
            standing,
            state.suppressions.len()
        )
    } else {
        "state: no previous round\n".to_owned()
    };
    text.push_str(&line);
    Ok(text)
}

fn not_yet(subcommand: &str) -> DtError {
    DtError::Cli(format!("{subcommand} is not yet implemented"))
}

pub use crate::contracts::RunOutcome;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        Cite, ClaimType, CoverageSummary, Finding, FindingId, Issue, IssueIdentity, PinnedRange,
        Reconciled, RegionId, RoundState,
    };
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
    fn a_one_provider_panel_warns_before_spend() {
        assert!(
            single_provider_panel_warning(
                &["zai:glm-5.3".to_owned(), "zai:glm-5.3-flash".to_owned()],
                3
            )
            .is_some_and(|warning| warning.contains("one provider")),
            "a flash + flash#2 + glm panel is one account quota and says so"
        );
        assert!(
            single_provider_panel_warning(&["zai:glm-5.3".to_owned(), "local".to_owned()], 2)
                .is_none(),
            "a mixed-provider panel stays quiet"
        );
        assert!(
            single_provider_panel_warning(&["zai:glm-5.3".to_owned()], 1).is_none(),
            "a single judge has no panel to diversify"
        );
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
            "--region-retries",
            "3",
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
        assert_eq!(args.region_retries, Some(3));
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
        let message = both
            .validate()
            .err()
            .map(|err| err.to_string())
            .unwrap_or_default();
        assert!(
            message.contains("merge-base(<base>, HEAD)") && !message.contains("origin/<base>.."),
            "the error states the real local-ref semantics and promises no origin fallback: {message}"
        );
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
            actor: Some("maintainer".to_owned()),
            ..ReviewArgs::default()
        };
        assert!(hosted.validate().is_ok());
        let hosted_without_actor = ReviewArgs {
            repo: Some("owner/name".to_owned()),
            pr: Some(7),
            ..ReviewArgs::default()
        };
        let message = hosted_without_actor
            .validate()
            .err()
            .map(|err| err.to_string())
            .unwrap_or_default();
        assert!(
            message.contains("--actor"),
            "a hosted review names the comment author to authorize: {message}"
        );
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
        for word in ["reject", "suppress", "state", "reply", "eval"] {
            let command = parse(&[word, "list"])
                .or_else(|_| parse(&[word, "show"]))
                .or_else(|_| parse(&[word, "run", "--set", "golden", "--runner", "v1"]))
                .or_else(|_| parse(&[word, "target"]))
                .or_else(|_| parse(&[word, "an-id"]))
                .or_else(|_| parse(&[word]));
            let command = command.map_err(|source| DtError::Cli(source.to_string()))?;
            if matches!(word, "state" | "eval") {
                let err = dispatch_with(&command, &env)
                    .err()
                    .unwrap_or(DtError::Cli(String::new()));
                assert!(
                    err.to_string().contains("not yet implemented"),
                    "{word} dispatches to a not-yet error: {err}"
                );
                continue;
            }
            let text = dispatch_with(&command, &env)
                .err()
                .map_or(String::new(), |err| err.to_string());
            assert!(
                !text.contains("not yet implemented"),
                "{word} owns its own dispatch errors now: {text}"
            );
        }
        std::fs::remove_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    struct FailingSink;

    impl std::io::Write for FailingSink {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("report sink failed (EIO)"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("report sink failed (EIO)"))
        }
    }

    #[test]
    fn a_failed_report_sink_leaves_the_state_untouched() -> Result<(), DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-sink-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\n", "base")?;
        git(&dir, &["checkout", "-b", "topic"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\nfn two() {}\n", "head")?;
        let home = dir.join("home");
        std::fs::create_dir_all(&home).map_err(|source| DtError::Config(source.to_string()))?;
        let config = dir.join("review.toml");
        std::fs::write(
            &config,
            "[tools]\nenabled = []\n[judges]\ncount = 1\nprofiles = [\"local\"]\n",
        )
        .map_err(|source| DtError::Config(source.to_string()))?;
        let command = Command::Review(ReviewArgs {
            base: Some("main".to_owned()),
            config: Some(config),
            ..ReviewArgs::default()
        });
        let outcome = dispatch_at(&command, &env_of(&[]), &dir, &home, &mut FailingSink);
        let cause = outcome.err().map(|err| err.to_string()).unwrap_or_default();
        assert!(
            cause.contains("report sink failed"),
            "a dead sink is an infrastructure failure, not a verdict: {cause}"
        );
        let state = home.join(".difftrace").join("state");
        let leftovers: Vec<std::ffi::OsString> =
            std::fs::read_dir(&state).map_or(Vec::new(), |entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name())
                    .collect()
            });
        assert!(
            leftovers.is_empty(),
            "exit 2 means state untouched: nothing may land under the state dir: {leftovers:?}"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_local_review_dispatch_runs_and_exits_partial_without_providers() -> Result<(), DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-review-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\n", "base")?;
        git(&dir, &["checkout", "-b", "topic"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\nfn two() {}\n", "head")?;
        let home = dir.join("home");
        std::fs::create_dir_all(&home).map_err(|source| DtError::Config(source.to_string()))?;
        let config = dir.join("review.toml");
        std::fs::write(
            &config,
            "[tools]\nenabled = []\n[judges]\ncount = 1\nprofiles = [\"local\"]\n",
        )
        .map_err(|source| DtError::Config(source.to_string()))?;
        let command = Command::Review(ReviewArgs {
            base: Some("main".to_owned()),
            config: Some(config),
            ..ReviewArgs::default()
        });
        let mut sink = Vec::new();
        let outcome = dispatch_at(&command, &env_of(&[]), &dir, &home, &mut sink)?;
        assert_eq!(
            outcome,
            RunOutcome::PartialRound,
            "a judge profile without credentials is absence, not failure"
        );
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("## Verdict"),
            "the console verdict reaches the writer: {text}"
        );
        let reviews = home.join(".difftrace").join("reviews");
        let transcripts = std::fs::read_dir(&reviews).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".log"))
                .count()
        });
        assert_eq!(transcripts, 1, "one transcript lands under the given home");
        let reports = std::fs::read_dir(&reviews).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".report.json")
                })
                .count()
        });
        assert_eq!(reports, 1, "one report lands under the given home");
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn non_utf8_arguments_still_error_cleanly() {
        let lossy = String::from_utf8_lossy(&[0x2d, 0x2d, 0x66, 0xff, 0xff, 0x6e]).into_owned();
        assert!(parse(&[lossy.as_str()]).is_err());
        assert!(Cli::try_parse_from(["difftrace", lossy.as_str()]).is_err());
    }

    fn reject_fixture(
        name: &str,
    ) -> Result<
        (
            std::path::PathBuf,
            std::path::PathBuf,
            String,
            String,
            RoundVerdict,
        ),
        DtError,
    > {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-{name}-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        commit(&dir, "src/a.rs", "fn one() {}\n", "base")?;
        let gateway = LocalGateway::open_head(&dir)?;
        let checkout = gateway.checkout_label();
        let head = gateway.pinned_range().head_sha;
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".difftrace").join("reviews"))
            .map_err(|source| DtError::Config(source.to_string()))?;
        let finding = Finding {
            id: FindingId("0/error-paths/lock".to_owned()),
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: "Lock is dropped too early".to_owned(),
            mechanism: "src/a.rs:3 releases before the read completes".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 3,
            }],
            severity: Severity::Warning,
            fix_hint: None,
            discarded_cites: 0,
        };
        let issues = vec![Issue {
            identity: IssueIdentity {
                file: "src/a.rs".to_owned(),
                line: 3,
                title_tokens: vec![
                    "lock".to_owned(),
                    "is".to_owned(),
                    "dropped".to_owned(),
                    "too".to_owned(),
                    "early".to_owned(),
                ],
            },
            first_seen: "base".to_owned(),
            last_seen: head.clone(),
            status: IssueStatus::Open,
            misses: 0,
            votes_seen: vec![(1, 2)],
        }];
        let state = RoundState {
            checkout: checkout.clone(),
            round: 1,
            issues,
            suppressions: Vec::new(),
        };
        let verdict = RoundVerdict {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: head.clone(),
            },
            coverage: CoverageSummary {
                cells_total: 1,
                cells_judged: 1,
                cells_clean: 0,
                cells_defect: 1,
                judged_by_judge: BTreeMap::from([("zai".to_owned(), 1), ("local".to_owned(), 1)]),
                regions: 1,
                per_file: BTreeMap::new(),
                cells_short: 0,
            },
            tool_findings: Vec::new(),
            confirmed: vec![Reconciled {
                finding,
                votes: vec!["zai".to_owned(), "local".to_owned()],
                families: vec!["zai".to_owned(), "local".to_owned()],
                severity: Severity::Warning,
            }],
            solo: Vec::new(),
            weak: Vec::new(),
            refuted: Vec::new(),
            suppressed: Vec::new(),
            dropped: Vec::new(),
            absent_judges: Vec::new(),
            incomplete: Vec::new(),
            tools_unavailable: Vec::new(),
            stats: Vec::new(),
            lead_verdicts: BTreeMap::new(),
            state,
        };
        let body = serde_json::to_string(&verdict)
            .map_err(|source| DtError::Config(source.to_string()))?;
        let mut older_verdict = verdict.clone();
        if let Some(entry) = older_verdict.confirmed.first_mut() {
            entry.finding.id = crate::contracts::FindingId("0/error-paths/old".to_owned());
        }
        let older_body = serde_json::to_string(&older_verdict)
            .map_err(|source| DtError::Config(source.to_string()))?;
        std::fs::write(
            home.join(".difftrace").join("reviews").join(format!(
                "{}-1000000000-stale.report.json",
                file::sanitize_label(&checkout)
            )),
            older_body,
        )
        .map_err(|source| DtError::Config(source.to_string()))?;
        std::fs::write(
            home.join(".difftrace").join("reviews").join(format!(
                "{}-2000000000-{}.report.json",
                file::sanitize_label(&checkout),
                head
            )),
            body,
        )
        .map_err(|source| DtError::Config(source.to_string()))?;
        file::save(
            &file::state_path(&home.join(".difftrace").join("state"), &checkout),
            &verdict.state,
        )?;
        Ok((dir, home, checkout, head, verdict))
    }

    #[test]
    fn a_reject_by_id_builds_its_suppression_from_the_newest_report() -> Result<(), DtError> {
        let (dir, home, checkout, head, verdict) = reject_fixture("reject")?;
        let state_path = file::state_path(&home.join(".difftrace").join("state"), &checkout);
        let mut sink = Vec::new();
        let outcome = dispatch_at(
            &Command::Reject {
                id: "0/error-paths/lock".to_owned(),
                reason: None,
                theme: false,
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        )?;
        assert_eq!(outcome, RunOutcome::Clean);
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("rejected 0/error-paths/lock") && text.contains("suppression s1"),
            "the receipt names the finding and its suppression: {text}"
        );
        assert!(
            text.contains("glob src/a.rs"),
            "a plain reject keys the finding's file: {text}"
        );
        let state = file::load(&state_path, &checkout)?;
        let rule = state
            .suppressions
            .first()
            .ok_or_else(|| DtError::Config("the suppression never landed".to_owned()))?;
        assert_eq!(rule.theme, "dropped early is lock too");
        assert_eq!(rule.created, head);
        assert_eq!(state.issues.len(), verdict.state.issues.len());
        assert!(
            matches!(
                state.issues.first().map(|issue| &issue.status),
                Some(IssueStatus::Rejected { .. })
            ),
            "the same-identity issue is rejected by name"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_stale_report_is_never_the_newest() -> Result<(), DtError> {
        let (dir, home, _checkout, _head, _verdict) = reject_fixture("reject-stale")?;
        let mut sink = Vec::new();
        let error_text = match dispatch_at(
            &Command::Reject {
                id: "0/error-paths/old".to_owned(),
                reason: None,
                theme: false,
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        ) {
            Err(err) => err.to_string(),
            Ok(_) => String::new(),
        };
        assert!(
            !error_text.is_empty()
                && error_text.contains("2000000000")
                && !error_text.contains("1000000000"),
            "an id that exists only in the older report errors against the newest report: {error_text}"
        );
        Ok(())
    }

    #[test]
    fn a_same_second_collision_still_picks_the_last_written() -> Result<(), DtError> {
        let (dir, home, checkout, head, mut collided) = reject_fixture("reject-collision")?;
        if let Some(entry) = collided.confirmed.first_mut() {
            entry.finding.id = crate::contracts::FindingId("0/error-paths/last".to_owned());
        }
        let body = serde_json::to_string(&collided)
            .map_err(|source| DtError::Config(source.to_string()))?;
        std::fs::write(
            home.join(".difftrace").join("reviews").join(format!(
                "{}-2000000000-{}.1.report.json",
                file::sanitize_label(&checkout),
                head
            )),
            body,
        )
        .map_err(|source| DtError::Config(source.to_string()))?;
        let mut sink = Vec::new();
        let outcome = dispatch_at(
            &Command::Reject {
                id: "0/error-paths/last".to_owned(),
                reason: None,
                theme: false,
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        )?;
        assert_eq!(
            outcome,
            RunOutcome::Clean,
            "the suffixed write is the newest"
        );
        let error_text = match dispatch_at(
            &Command::Reject {
                id: "0/error-paths/lock".to_owned(),
                reason: None,
                theme: false,
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        ) {
            Err(err) => err.to_string(),
            Ok(_) => String::new(),
        };
        assert!(
            error_text.contains(".1.report.json"),
            "an id left behind in the pre-collision report errors against the newest: {error_text}"
        );
        Ok(())
    }

    #[test]
    fn an_unknown_reject_id_errors_naming_the_report() -> Result<(), DtError> {
        let (dir, home, checkout, _head, _verdict) = reject_fixture("reject-miss")?;
        let state_path = file::state_path(&home.join(".difftrace").join("state"), &checkout);
        let mut sink = Vec::new();
        let error_text = match dispatch_at(
            &Command::Reject {
                id: "9/nowhere/ghost".to_owned(),
                reason: None,
                theme: false,
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        ) {
            Err(err) => err.to_string(),
            Ok(_) => String::new(),
        };
        assert!(
            !error_text.is_empty()
                && error_text.contains("9/nowhere/ghost")
                && error_text.contains(".report.json"),
            "the error names the id and the report it searched: {error_text}"
        );
        let state = file::load(&state_path, &checkout)?;
        assert!(
            state.suppressions.is_empty(),
            "a failed reject writes nothing"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn list_shows_hits_and_clear_removes() -> Result<(), DtError> {
        let (dir, home, checkout, head, _verdict) = reject_fixture("suppress")?;
        let state_path = file::state_path(&home.join(".difftrace").join("state"), &checkout);
        let mut state = file::load(&state_path, &checkout)?;
        let _unused = state::reject_finding(
            &mut state,
            "src/a.rs",
            3,
            "Lock is dropped too early",
            false,
            "owner rejected",
            &head,
        );
        if let Some(rule) = state.suppressions.first_mut() {
            rule.hits = 3;
        }
        let _unused = state::reject_finding(
            &mut state,
            "src/b.rs",
            5,
            "Buffer grows unbounded",
            false,
            "owner rejected",
            &head,
        );
        file::save(&state_path, &state)?;
        let mut sink = Vec::new();
        dispatch_at(
            &Command::Suppress {
                action: SuppressAction::List,
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        )?;
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("s1") && text.contains("hits 3") && text.contains("s2"),
            "list renders every suppression with its hit counter: {text}"
        );
        let mut sink = Vec::new();
        dispatch_at(
            &Command::Suppress {
                action: SuppressAction::Clear {
                    id: Some("s1".to_owned()),
                },
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        )?;
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("cleared 1 suppression(s), reopened 1 issue(s)"),
            "the clear receipt names what went and what reopened: {text}"
        );
        let state = file::load(&state_path, &checkout)?;
        assert_eq!(state.suppressions.len(), 1, "only the named id went");
        assert_eq!(
            state.suppressions.first().map(|rule| rule.id.as_str()),
            Some("s2")
        );
        assert!(
            matches!(
                state.issues.first().map(|issue| &issue.status),
                Some(IssueStatus::Open)
            ),
            "the issue s1 rejected is open again"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn clear_without_an_id_reopens_every_rejected_issue() -> Result<(), DtError> {
        let (dir, home, checkout, head, _verdict) = reject_fixture("suppress-all")?;
        let state_path = file::state_path(&home.join(".difftrace").join("state"), &checkout);
        let mut state = file::load(&state_path, &checkout)?;
        let _unused = state::reject_finding(
            &mut state,
            "src/a.rs",
            3,
            "Lock is dropped too early",
            false,
            "owner rejected",
            &head,
        );
        let _unused = state::reject_finding(
            &mut state,
            "src/b.rs",
            5,
            "Buffer grows unbounded",
            false,
            "owner rejected",
            &head,
        );
        file::save(&state_path, &state)?;
        let mut sink = Vec::new();
        dispatch_at(
            &Command::Suppress {
                action: SuppressAction::Clear { id: None },
            },
            &env_of(&[]),
            &dir,
            &home,
            &mut sink,
        )?;
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("cleared 2 suppression(s), reopened 1 issue(s)"),
            "a bare clear empties the set and reopens every orphan: {text}"
        );
        let state = file::load(&state_path, &checkout)?;
        assert!(state.suppressions.is_empty());
        assert!(
            state
                .issues
                .iter()
                .all(|issue| issue.status == IssueStatus::Open),
            "no dangling rejections survive"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_reply_preparation_builds_context_and_confirmation_without_touching_state()
    -> Result<(), DtError> {
        let (dir, home, checkout, _head, _verdict) = reject_fixture("reply")?;
        let state_path = file::state_path(&home.join(".difftrace").join("state"), &checkout);
        let before =
            std::fs::read(&state_path).map_err(|source| DtError::Config(source.to_string()))?;
        let (job, confirmation) = prepare_reply(
            &dir,
            &home,
            "can you reject 0/error-paths/lock, please?",
            false,
        )?;
        assert!(
            job.context.contains("[confirmed] 0/error-paths/lock"),
            "the materials are the newest report's findings: {}",
            job.context
        );
        assert_eq!(
            confirmation.as_deref(),
            Some(
                "to reject it, run: difftrace reject 0/error-paths/lock — chat never writes state itself"
            ),
            "a reject-shaped question confirms with the exact command"
        );
        let after =
            std::fs::read(&state_path).map_err(|source| DtError::Config(source.to_string()))?;
        assert_eq!(before, after, "preparing a reply never writes state");
        assert!(!job.plan, "answer mode is the default");
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
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
        assert_eq!(file_only.budget_secs, 0);
        let env_layer = resolve_run(&file_scope, &env_of(&[("DIFFTRACE_SCOPE", "deep")]))?;
        assert_eq!(env_layer.scope, Scope::Deep);
        let flag_layer = resolve_run(
            &ReviewArgs {
                config: Some(dir.join(".difftrace").join("config.toml")),
                scope: Some(Scope::Full),
                passes: Some(3),
                budget_secs: Some(900),
                region_retries: Some(3),
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
        assert_eq!(
            flag_layer.config.judges.region_retries, 3,
            "the flag wins over env and file"
        );
        let env_knobs = resolve_run(
            &file_scope,
            &env_of(&[
                ("DIFFTRACE_PASSES", "2"),
                ("DIFFTRACE_BUDGET_SECS", "2700"),
                ("DIFFTRACE_FOCUS", "security, tests"),
                ("DIFFTRACE_REGION_RETRIES", "2"),
            ]),
        )?;
        assert_eq!(env_knobs.passes, 2);
        assert_eq!(env_knobs.budget_secs, 2700);
        assert_eq!(
            env_knobs.config.judges.region_retries, 2,
            "the env layer wins over the file default"
        );
        assert_eq!(
            file_only.config.judges.region_retries, 3,
            "an absent knob falls back to the pinned default"
        );
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
    fn a_zero_from_the_flag_blames_the_flag_not_the_env() -> Result<(), DtError> {
        let dir = temp_dir("passes-zero")?;
        let config = dir.join("zero.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let args = ReviewArgs {
            passes: Some(0),
            config: Some(config.clone()),
            ..ReviewArgs::default()
        };
        let from_flag = resolve_run(&args, &env_of(&[]));
        assert!(
            from_flag.is_err_and(|err| err.to_string().contains("--passes")
                && !err.to_string().contains("DIFFTRACE_PASSES")),
            "the knob that supplied the zero is the knob the error names"
        );
        let from_env = resolve_run(
            &ReviewArgs {
                config: Some(config),
                ..ReviewArgs::default()
            },
            &env_of(&[("DIFFTRACE_PASSES", "0")]),
        );
        assert!(
            from_env.is_err_and(|err| err.to_string().contains("DIFFTRACE_PASSES")),
            "an env-supplied zero still names the env knob"
        );
        std::fs::remove_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    #[test]
    fn a_parse_failure_renders_as_a_labeled_usage_error() {
        let line = Cli::try_parse_from(["difftrace", "review", "--nonsense"])
            .err()
            .map(|err| usage_error_line(&err))
            .unwrap_or_default();
        assert!(
            line.starts_with("difftrace: usage error:"),
            "the label separates a typo from a broken environment: {line}"
        );
        assert!(
            line.contains("--nonsense"),
            "the clap diagnostic rides along under the label: {line}"
        );
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

    #[test]
    fn a_hosted_review_without_a_token_fails_before_any_work() -> Result<(), DtError> {
        let dir = temp_dir("hosted-no-token")?;
        let config = dir.join("empty.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let command = Command::Review(ReviewArgs {
            repo: Some("owner/name".to_owned()),
            pr: Some(7),
            actor: Some("maintainer".to_owned()),
            config: Some(config),
            dry_run: true,
            ..ReviewArgs::default()
        });
        let mut sink = Vec::new();
        let err = dispatch_at(&command, &env_of(&[]), &dir, &dir, &mut sink)
            .err()
            .unwrap_or(DtError::Cli(String::new()));
        assert!(
            err.to_string().contains("GITHUB_TOKEN"),
            "the token gate fires before any gateway construction or review work: {err}"
        );
        assert!(
            sink.is_empty(),
            "no local plan may print for hosted arguments"
        );
        std::fs::remove_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    #[test]
    fn an_all_empty_focus_list_rejects_instead_of_widening() -> Result<(), DtError> {
        let dir = temp_dir("focus-empty")?;
        let config = dir.join("empty.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let args = |focus: Option<Vec<String>>| ReviewArgs {
            focus,
            config: Some(config.clone()),
            ..ReviewArgs::default()
        };
        for focus in [vec![String::new()], vec![String::new(), String::new()]] {
            let err = resolve_run(&args(Some(focus)), &env_of(&[]))
                .err()
                .unwrap_or(DtError::Cli(String::new()));
            assert!(
                err.to_string().contains("--focus"),
                "an all-empty --focus must reject, not review everything: {err}"
            );
        }
        let env_err = resolve_run(&args(None), &env_of(&[("DIFFTRACE_FOCUS", ",")]))
            .err()
            .unwrap_or(DtError::Cli(String::new()));
        assert!(
            env_err.to_string().contains("DIFFTRACE_FOCUS"),
            "an all-empty env list must reject the same way: {env_err}"
        );
        let mixed = resolve_run(
            &args(Some(vec![String::new(), "security".to_owned()])),
            &env_of(&[]),
        )?;
        assert_eq!(
            mixed.focus,
            vec!["security".to_owned()],
            "an empty entry among real ids drops rather than widening"
        );
        std::fs::remove_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        Ok(())
    }

    fn git(dir: &Path, args: &[&str]) -> Result<(), DtError> {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .map_err(|source| DtError::Config(source.to_string()))?;
        if !output.status.success() {
            return Err(DtError::Config(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(())
    }

    fn commit(dir: &Path, file: &str, body: &str, message: &str) -> Result<(), DtError> {
        let path = dir.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| DtError::Config(source.to_string()))?;
        }
        std::fs::write(&path, body).map_err(|source| DtError::Config(source.to_string()))?;
        git(dir, &["add", "-A"])?;
        git(
            dir,
            &[
                "-c",
                "user.email=difftrace@example.invalid",
                "-c",
                "user.name=difftrace",
                "commit",
                "-m",
                message,
            ],
        )
    }

    #[test]
    fn dry_run_prints_the_plan_and_estimate() -> Result<(), DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-dryrun-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\n", "base")?;
        git(&dir, &["checkout", "-b", "topic"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\nfn two() {}\n", "head")?;
        let config = dir.join("empty.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let resolved = resolve_run(
            &ReviewArgs {
                base: Some("main".to_owned()),
                config: Some(config),
                dry_run: true,
                ..ReviewArgs::default()
            },
            &env_of(&[]),
        )?;
        let text = run_dry_run_at(&dir, &resolved, &dir.join("state"))?;
        assert!(text.contains("plan:"), "the summary line leads: {text}");
        assert!(
            text.contains("judge calls") && text.contains("tokens"),
            "the estimate prints: {text}"
        );
        assert!(
            text.contains("src/lib.rs") && text.contains("range:"),
            "the region outline and pinned range print: {text}"
        );
        assert!(
            text.contains("state: no previous round"),
            "a checkout without prior state says so: {text}"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn the_dry_run_text_carries_the_state_delta() -> Result<(), DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-delta-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\n", "base")?;
        git(&dir, &["checkout", "-b", "topic"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\nfn two() {}\n", "head")?;
        let config = dir.join("empty.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let resolved = resolve_run(
            &ReviewArgs {
                base: Some("main".to_owned()),
                config: Some(config),
                dry_run: true,
                ..ReviewArgs::default()
            },
            &env_of(&[]),
        )?;
        let checkout = dir
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let state_dir = dir.join("state");
        let state = crate::contracts::RoundState {
            checkout: checkout.clone(),
            round: 2,
            issues: vec![crate::contracts::Issue {
                identity: crate::contracts::IssueIdentity {
                    file: "src/lib.rs".to_owned(),
                    line: 1,
                    title_tokens: vec!["off".to_owned(), "by".to_owned(), "one".to_owned()],
                },
                first_seen: "head".to_owned(),
                last_seen: "head".to_owned(),
                status: crate::contracts::IssueStatus::Open,
                misses: 1,
                votes_seen: vec![(1, 2)],
            }],
            suppressions: vec![crate::contracts::Suppression {
                id: "sup-1".to_owned(),
                theme: "Buffer grows unbounded".to_owned(),
                file_glob: "src/*.rs".to_owned(),
                reason: "owner rejected the theme".to_owned(),
                created: "head".to_owned(),
                hits: 3,
            }],
        };
        state::file::save(&state::file::state_path(&state_dir, &checkout), &state)?;
        let text = run_dry_run_at(&dir, &resolved, &state_dir)?;
        assert!(
            text.contains("state: round 2 → 3 preview · standing 1 · suppressions 1"),
            "the delta previews the next round from the persisted state: {text}"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_dry_run_dispatch_writes_the_report_and_exits_clean() -> Result<(), DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-at-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\n", "base")?;
        git(&dir, &["checkout", "-b", "topic"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\nfn two() {}\n", "head")?;
        let config = dir.join("empty.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let command = Command::Review(ReviewArgs {
            base: Some("main".to_owned()),
            config: Some(config),
            dry_run: true,
            ..ReviewArgs::default()
        });
        let mut sink = Vec::new();
        let outcome = dispatch_at(&command, &env_of(&[]), &dir, &dir, &mut sink)?;
        assert_eq!(
            outcome,
            RunOutcome::Clean,
            "the dry-run review exits clean through dispatch"
        );
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("plan:"),
            "the report reaches the injected writer: {text}"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_full_scope_dry_run_covers_every_tracked_file() -> Result<(), DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-full-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\n", "base")?;
        commit(&dir, "docs/guide.md", "# Guide\n", "docs")?;
        git(&dir, &["checkout", "-b", "topic"])?;
        commit(&dir, "src/lib.rs", "fn one() {}\nfn two() {}\n", "head")?;
        let config = dir.join("empty.toml");
        std::fs::write(&config, "").map_err(|source| DtError::Config(source.to_string()))?;
        let resolved = resolve_run(
            &ReviewArgs {
                base: Some("main".to_owned()),
                scope: Some(Scope::Full),
                config: Some(config),
                dry_run: true,
                ..ReviewArgs::default()
            },
            &env_of(&[]),
        )?;
        let text = run_dry_run_at(&dir, &resolved, &dir.join("state"))?;
        assert!(
            text.contains("src/lib.rs")
                && text.contains("docs/guide.md")
                && text.contains("[repo]"),
            "every tracked file becomes a repo region: {text}"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    struct ClosedPipe;

    impl io::Write for ClosedPipe {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    struct FullDisk;

    impl io::Write for FullDisk {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("no space left on device"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("no space left on device"))
        }
    }

    #[test]
    fn a_closed_stdout_pipe_exits_clean_instead_of_panicking() {
        assert!(
            write_report(&mut ClosedPipe, "plan: 1 regions").is_ok(),
            "a closed pipe is the consumer's choice, not a failure"
        );
    }

    #[test]
    fn a_failed_stdout_write_fails_as_infrastructure() -> Result<(), DtError> {
        let Err(error) = write_report(&mut FullDisk, "plan: 1 regions") else {
            return Err(DtError::Cli("a dead stdout must fail the run".to_owned()));
        };
        assert!(
            matches!(error, DtError::Output(_)),
            "report writes carry their own variant: {error}"
        );
        assert_eq!(
            RunOutcome::from(error),
            RunOutcome::InfrastructureFailure,
            "a real write failure maps to exit code 2"
        );
        Ok(())
    }

    fn resolve_judged(toml: &str, passes: Option<u64>) -> Result<ResolvedRun, DtError> {
        let dir = std::env::temp_dir().join(format!("difftrace-cli-panel-{}", uuid_like_marker()));
        let _unused = std::fs::create_dir_all(&dir);
        let config = dir.join("panel.toml");
        std::fs::write(&config, toml).map_err(|source| DtError::Config(source.to_string()))?;
        resolve_run(
            &ReviewArgs {
                config: Some(config),
                passes,
                ..ReviewArgs::default()
            },
            &env_of(&[]),
        )
    }

    fn uuid_like_marker() -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn a_replicated_profile_gets_its_own_panel_id() -> Result<(), Box<dyn std::error::Error>> {
        let resolved = resolve_judged("[judges]\ncount = 2\nprofiles = [\"zai\"]\n", None)?;
        let ids: Vec<String> = judge_specs(&resolved).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["zai".to_owned(), "zai#2".to_owned()]);
        Ok(())
    }

    #[test]
    fn two_passes_mint_distinct_ids_for_the_same_profile() -> Result<(), Box<dyn std::error::Error>>
    {
        let resolved = resolve_judged("[judges]\ncount = 1\nprofiles = [\"zai\"]\n", Some(2))?;
        let specs = judge_specs(&resolved);
        let ids: Vec<String> = specs.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["zai".to_owned(), "zai#2".to_owned()]);
        assert!(
            specs.iter().all(|s| s.profile == "zai"),
            "both entries share the profile — the id distinguishes them"
        );
        Ok(())
    }

    #[test]
    fn a_single_judge_single_pass_keeps_the_plain_id() -> Result<(), Box<dyn std::error::Error>> {
        let resolved = resolve_judged("[judges]\ncount = 1\nprofiles = [\"zai\"]\n", None)?;
        let ids: Vec<String> = judge_specs(&resolved).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["zai".to_owned()]);
        Ok(())
    }

    #[test]
    fn a_profile_name_carrying_the_replica_marker_is_rejected() {
        let outcome = resolve_run(
            &ReviewArgs {
                judge: vec!["zai#2".to_owned()],
                ..ReviewArgs::default()
            },
            &env_of(&[]),
        );
        assert!(
            outcome
                .as_ref()
                .is_err_and(|err| err.to_string().contains('#')),
            "the panel marker stays reserved: {outcome:?}"
        );
    }

    #[test]
    fn an_under_quota_panel_warns_and_a_quorate_one_does_not() {
        assert!(
            under_quota_panel_warning(&["zai".to_owned()], 1).is_some(),
            "one profile under a two-profile quorum must not pass silently"
        );
        assert!(
            under_quota_panel_warning(&["zai".to_owned()], 3)
                .is_some_and(|warning| warning.contains("1 distinct profile")),
            "the warning counts distinct profiles, not panel slots"
        );
        assert!(
            under_quota_panel_warning(&["zai".to_owned(), "local".to_owned()], 2).is_none(),
            "a quorate panel stays silent"
        );
    }

    struct FakeHosted {
        reads: LocalGateway,
        permission: String,
        marker: std::sync::Mutex<Option<(u64, String)>>,
        threads: std::sync::Mutex<Vec<crate::contracts::Thread>>,
        log: std::sync::Mutex<Vec<String>>,
        next_id: std::sync::Mutex<u64>,
    }

    impl FakeHosted {
        fn new(reads: LocalGateway, permission: &str) -> Self {
            Self {
                reads,
                permission: permission.to_owned(),
                marker: std::sync::Mutex::new(None),
                threads: std::sync::Mutex::new(Vec::new()),
                log: std::sync::Mutex::new(Vec::new()),
                next_id: std::sync::Mutex::new(100),
            }
        }

        fn record(&self, line: &str) {
            self.log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(line.to_owned());
        }

        fn logged(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl PrGateway for FakeHosted {
        fn pinned_range(&self) -> PinnedRange {
            self.reads.pinned_range()
        }
        fn diff(&self) -> DiffSource {
            self.reads.diff()
        }
        fn read_file_at_head(&self, path: &str) -> Result<crate::contracts::FileSnapshot, DtError> {
            self.reads.read_file_at_head(path)
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
            self.reads.list_files_at_head()
        }
        fn search_at_head(
            &self,
            pattern: &str,
            cap: usize,
        ) -> Result<Vec<crate::contracts::SearchHit>, DtError> {
            self.reads.search_at_head(pattern, cap)
        }
    }

    impl crate::hosted::HostedOps for FakeHosted {
        fn permission(&self, _actor: &str) -> Result<String, DtError> {
            Ok(self.permission.clone())
        }
        fn pr_base_head(&self) -> Result<(String, String), DtError> {
            let range = self.reads.pinned_range();
            Ok((range.base_sha, range.head_sha))
        }
        fn marker_comment(&self) -> Result<Option<(u64, String)>, DtError> {
            Ok(self
                .marker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone())
        }
        fn update_issue_comment(&self, comment_id: u64, body: &str) -> Result<(), DtError> {
            self.record(&format!("update:{comment_id}"));
            *self
                .marker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((comment_id, body.to_owned()));
            Ok(())
        }
        fn post_issue_comment(&self, body: &str) -> Result<(), DtError> {
            let mut next = self
                .next_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *next = next.saturating_add(1);
            let id = *next;
            self.record(&format!("post:{id}"));
            *self
                .marker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((id, body.to_owned()));
            Ok(())
        }
        fn own_threads(&self) -> Result<Vec<crate::contracts::Thread>, DtError> {
            Ok(self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone())
        }
        fn submit_review(
            &self,
            submission: &crate::contracts::ReviewSubmission,
        ) -> Result<(), DtError> {
            self.record(&format!("review:{}", submission.event.as_str()));
            let mut threads = self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for comment in &submission.comments {
                let mut next = self
                    .next_id
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *next = next.saturating_add(1);
                threads.push(crate::contracts::Thread {
                    id: format!("T_{}", *next),
                    path: comment.path.clone(),
                    line: comment.line,
                    original_line: None,
                    comment_id: *next,
                });
            }
            Ok(())
        }
        fn reply_to_thread(&self, comment_id: u64, body: &str) -> Result<(), DtError> {
            self.record(&format!("reply:{comment_id}:{}", body.len()));
            Ok(())
        }
        fn resolve_thread(&self, thread_id: &str) -> Result<(), DtError> {
            self.record(&format!("resolve:{thread_id}"));
            self.threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|thread| thread.id != thread_id);
            Ok(())
        }
        fn fetch_check_runs(&self, _sha: &str) -> Result<Vec<crate::contracts::CheckRun>, DtError> {
            Ok(Vec::new())
        }
    }

    fn git_scratch(name: &str) -> Result<std::path::PathBuf, DtError> {
        let dir = temp_dir(name)?;
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|err| DtError::Config(err.to_string()))?;
        let initialized = std::process::Command::new("git")
            .current_dir(&dir)
            .args(["init", "-q", "-b", "main"])
            .output()
            .map_err(|err| DtError::Git(err.to_string()))?;
        if !initialized.status.success() {
            return Err(DtError::Git(
                String::from_utf8_lossy(&initialized.stderr)
                    .trim()
                    .to_owned(),
            ));
        }
        for (file, body) in [
            ("src/base.rs", "fn base() {}\n"),
            ("src/head.rs", "fn head() {}\n"),
        ] {
            let path = dir.join(file);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| DtError::Git(err.to_string()))?;
            }
            std::fs::write(&path, body).map_err(|err| DtError::Git(err.to_string()))?;
            let staged = std::process::Command::new("git")
                .current_dir(&dir)
                .args(["add", file])
                .output()
                .map_err(|err| DtError::Git(err.to_string()))?;
            if !staged.status.success() {
                return Err(DtError::Git(
                    String::from_utf8_lossy(&staged.stderr).trim().to_owned(),
                ));
            }
            let committed = std::process::Command::new("git")
                .current_dir(&dir)
                .args([
                    "-c",
                    "user.email=difftrace@example.invalid",
                    "-c",
                    "user.name=difftrace",
                    "commit",
                    "-qm",
                    file,
                ])
                .output()
                .map_err(|err| DtError::Git(err.to_string()))?;
            if !committed.status.success() {
                return Err(DtError::Git(
                    String::from_utf8_lossy(&committed.stderr).trim().to_owned(),
                ));
            }
        }
        Ok(dir)
    }

    fn seeded_threads() -> Vec<crate::contracts::Thread> {
        vec![
            crate::contracts::Thread {
                id: "T_1".to_owned(),
                path: "src/a.rs".to_owned(),
                line: 10,
                original_line: None,
                comment_id: 11,
            },
            crate::contracts::Thread {
                id: "T_2".to_owned(),
                path: "src/b.rs".to_owned(),
                line: 7,
                original_line: None,
                comment_id: 12,
            },
            crate::contracts::Thread {
                id: "T_3".to_owned(),
                path: "src/c.rs".to_owned(),
                line: 3,
                original_line: None,
                comment_id: 13,
            },
        ]
    }

    fn seeded_state() -> RoundState {
        let issue = |file: &str, line: u32, title: &str, status: IssueStatus| Issue {
            identity: IssueIdentity {
                file: file.to_owned(),
                line,
                title_tokens: crate::identity::title_tokens(title).into_iter().collect(),
            },
            first_seen: "s0".to_owned(),
            last_seen: "s1".to_owned(),
            status,
            misses: 2,
            votes_seen: Vec::new(),
        };
        RoundState {
            checkout: "work".to_owned(),
            round: 1,
            issues: vec![
                issue(
                    "src/a.rs",
                    10,
                    "retired theme",
                    IssueStatus::Retired {
                        round: 3,
                        sha: "s".to_owned(),
                    },
                ),
                issue(
                    "src/b.rs",
                    7,
                    "rejected theme",
                    IssueStatus::Rejected {
                        id: "s1".to_owned(),
                    },
                ),
                issue("src/c.rs", 3, "standing theme", IssueStatus::Open),
            ],
            suppressions: Vec::new(),
        }
    }

    fn hosted_resolved(dry_run: bool) -> ResolvedRun {
        let mut config = DifftraceConfig::default();
        config.judges.profiles = vec!["local".to_owned()];
        config.judges.count = 1;
        config.judges.retry_backoff_secs = 0;
        config.judges.region_retries = 0;
        config.tools.budget_secs = 1;
        ResolvedRun {
            config,
            base: "master".to_owned(),
            repo: None,
            pr: None,
            actor: Some("maintainer".to_owned()),
            scope: Scope::Diff,
            severity: Severity::Nitpick,
            oracle: None,
            focus: Vec::new(),
            passes: 1,
            budget_secs: 10,
            dry_run,
            json: false,
            fix_prompts: false,
        }
    }

    fn hosted_drive(
        name: &str,
        permission: &str,
        dry_run: bool,
        with_seeds: bool,
    ) -> Result<(std::sync::Arc<FakeHosted>, Vec<u8>), DtError> {
        let dir = git_scratch(name)?;
        let reads = LocalGateway::open_at(&dir, "main")?;
        let fake = FakeHosted::new(reads, permission);
        if with_seeds {
            if let Ok(body) = state::marker::embed("a previous verdict", &seeded_state()) {
                *fake
                    .marker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((1, body));
            }
            *fake
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = seeded_threads();
        }
        let shared = std::sync::Arc::new(fake);
        let resolved = hosted_resolved(dry_run);
        let range = shared.pinned_range();
        let home = temp_dir(&format!("{name}-home"))?;
        let mut sink = Vec::new();
        let run = run_hosted_review_at(
            HostedRun {
                resolved: &resolved,
                gateway: Arc::clone(&shared),
                repo_root: dir,
                range,
                home: &home,
                actor: "maintainer",
            },
            &mut sink,
        );
        run?;
        Ok((shared, sink))
    }

    #[test]
    fn a_hosted_round_posts_resolves_and_persists_state_through_the_marker() -> Result<(), DtError>
    {
        let (shared, sink) = hosted_drive("hosted-e2e", "write", false, true)?;
        let logged = shared.logged();
        assert!(
            logged.iter().any(|line| line == "review:COMMENT"),
            "a partial round posts a neutral review, never approval: {logged:?}"
        );
        assert!(
            logged.contains(&"resolve:T_1".to_owned())
                && logged.contains(&"resolve:T_2".to_owned()),
            "retired and rejected threads resolve with their note replies: {logged:?}"
        );
        assert!(
            !logged.contains(&"resolve:T_3".to_owned()),
            "the open issue's thread stays open: {logged:?}"
        );
        assert!(
            logged.iter().any(|line| line.starts_with("reply:11"))
                && logged.iter().any(|line| line.starts_with("reply:12")),
            "the resolution notes ride replies into their threads: {logged:?}"
        );
        assert!(
            logged.contains(&"update:1".to_owned())
                && !logged.iter().any(|line| line.starts_with("post:")),
            "a pre-existing marker is edited in place, never duplicated: {logged:?}"
        );
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("review posted: 0 inline · 0 re-raise replies · 2 resolutions"),
            "the receipt names what landed: {text}"
        );
        let marker = shared
            .marker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let (_id, body) = marker.ok_or(DtError::Config("marker missing".to_owned()))?;
        let carried = state::marker::extract(&body)?.ok_or(DtError::StateCorrupt(
            "the posted body carries no state".to_owned(),
        ))?;
        assert_eq!(
            carried.issues.len(),
            3,
            "the seeded issues ride the posted state"
        );
        Ok(())
    }

    #[test]
    fn an_unauthorized_hosted_actor_never_reaches_the_pipeline() -> Result<(), DtError> {
        let refused = hosted_drive("hosted-refused", "read", false, true);
        let err = refused
            .err()
            .ok_or(DtError::Config("the read actor must be refused".to_owned()))?;
        assert!(
            err.to_string().contains("needs write"),
            "the refusal names the access gap: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_hosted_dry_run_reads_state_but_writes_nothing() -> Result<(), DtError> {
        let (shared, sink) = hosted_drive("hosted-dry", "write", true, true)?;
        let text = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            text.contains("the comment that would be posted") && text.contains("resolutions 2"),
            "the dry run previews the comment and the plan: {text}"
        );
        assert!(
            shared.logged().is_empty(),
            "a dry run performs no hosted writes"
        );
        assert!(
            shared
                .marker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some(),
            "the pre-existing marker survives untouched"
        );
        Ok(())
    }
}
