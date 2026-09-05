//! Binary entry point: wires config, provider, gateway, diff index, and
//! the review runner into the `difftrace review` command.

use std::process::ExitCode;

use clap::Parser as _;

use difftrace::cli::Cli;
use difftrace::cli::Command;
use difftrace::cli::ReplyArgs;
use difftrace::cli::parse_repo;
use difftrace::config::DifftraceConfig;
use difftrace::diff::DiffIndex;
use difftrace::error::DifftraceError;
use difftrace::github::GitHubClient;
use difftrace::github::PrGateway;
use difftrace::provider::build_client;
use difftrace::review::ReplyTarget;
use difftrace::review::ReviewRunner;
use tracing_subscriber::EnvFilter;

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    if let Err(err) = result {
        eprintln!("difftrace: cannot install the log subscriber: {err}");
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("difftrace: cannot start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    match cli.command {
        Command::Review(args) => finish(runtime.block_on(run(args))),
        Command::Reply(args) => {
            let target = match reply_target(&args) {
                Ok(target) => target,
                Err(err) => return finish(Err(err)),
            };
            finish(runtime.block_on(run_reply(args, target)))
        }
    }
}

fn finish(result: Result<ExitCode, DifftraceError>) -> ExitCode {
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("difftrace: {}", difftrace::error::error_chain(&err));
            ExitCode::FAILURE
        }
    }
}

fn reply_target(args: &ReplyArgs) -> Result<ReplyTarget, DifftraceError> {
    if let Some(id) = args.review_comment {
        return Ok(ReplyTarget::ReviewComment { id });
    }
    args.issue_comment
        .map(|id| ReplyTarget::IssueComment { id })
        .ok_or_else(|| DifftraceError::Reply {
            message: "exactly one of --issue-comment or --review-comment is required".to_owned(),
        })
}

async fn run(args: difftrace::cli::ReviewArgs) -> Result<ExitCode, DifftraceError> {
    let repo = parse_repo(&args.repo).map_err(DifftraceError::Cli)?;
    let mut config = match &args.config {
        Some(path) if !path.is_file() => {
            return Err(DifftraceError::Cli(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        Some(path) => DifftraceConfig::load_from(path)?,
        None => DifftraceConfig::load()?,
    };
    config.apply_env_overrides()?;
    let client = std::sync::Arc::new(build_client(&config)?);
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(DifftraceError::MissingApiKey {
            env_var: "GITHUB_TOKEN",
        })?;
    let gateway = std::sync::Arc::new(GitHubClient::new(
        token,
        repo.clone(),
        config.github.api_base_url.as_deref(),
    )?);
    eprintln!("difftrace: fetching pull request #{}…", args.pr);
    let overview = gateway.pr_overview(args.pr).await?;
    let raw_diff = gateway.pr_diff(args.pr).await?;
    let index = std::sync::Arc::new(DiffIndex::parse(&raw_diff)?);
    eprintln!(
        "difftrace: {} changed file(s), reviewing in batches of {}…",
        index.len(),
        config.review.batch_files
    );
    let trajectory_dir = trajectory_dir();
    let runner = ReviewRunner::new(
        client,
        std::sync::Arc::clone(&gateway) as std::sync::Arc<dyn PrGateway>,
        index,
        overview,
        config.review,
        trajectory_dir,
    );
    let outcome = runner.review_all(args.dry_run).await?;
    if args.dry_run {
        println!("{}", outcome.round_body.trim_end());
        println!();
        println!("--- difftrace comment ---");
        println!("{}", outcome.standing_body.trim_end());
        eprintln!(
            "difftrace: dry run — {} inline finding(s) would be posted, {} dropped",
            outcome.comments.len(),
            outcome.dropped.len()
        );
    } else {
        println!(
            "difftrace: review posted to #{} — {} inline finding(s), {} dropped",
            args.pr,
            outcome.comments.len(),
            outcome.dropped.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn run_reply(args: ReplyArgs, target: ReplyTarget) -> Result<ExitCode, DifftraceError> {
    let repo = parse_repo(&args.repo).map_err(DifftraceError::Cli)?;
    let mut config = match &args.config {
        Some(path) if !path.is_file() => {
            return Err(DifftraceError::Cli(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        Some(path) => DifftraceConfig::load_from(path)?,
        None => DifftraceConfig::load()?,
    };
    config.apply_env_overrides()?;
    let client = std::sync::Arc::new(build_client(&config)?);
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(DifftraceError::MissingApiKey {
            env_var: "GITHUB_TOKEN",
        })?;
    let gateway = std::sync::Arc::new(GitHubClient::new(
        token,
        repo,
        config.github.api_base_url.as_deref(),
    )?);
    eprintln!(
        "difftrace: answering a question on pull request #{}…",
        args.pr
    );
    let overview = gateway.pr_overview(args.pr).await?;
    let raw_diff = gateway.pr_diff(args.pr).await?;
    let index = std::sync::Arc::new(DiffIndex::parse(&raw_diff)?);
    let trajectory_dir = trajectory_dir();
    let runner = ReviewRunner::new(
        client,
        std::sync::Arc::clone(&gateway) as std::sync::Arc<dyn PrGateway>,
        index,
        overview,
        config.review,
        trajectory_dir,
    );
    let outcome = runner.reply(target).await?;
    if outcome.refused {
        eprintln!(
            "difftrace: refused — posted the authorization note to the {}",
            outcome.target
        );
    } else {
        println!("difftrace: reply posted to the {}", outcome.target);
    }
    Ok(ExitCode::SUCCESS)
}

fn trajectory_dir() -> Option<std::path::PathBuf> {
    let Some(home) = dirs::home_dir() else {
        eprintln!("difftrace: cannot determine the home directory; trajectory capture is disabled");
        return None;
    };
    let dir = std::path::Path::new(&home)
        .join(".difftrace")
        .join("trajectories");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(err) => {
            eprintln!(
                "difftrace: cannot create {}: {err}; trajectory capture is disabled",
                dir.display()
            );
            None
        }
    }
}
