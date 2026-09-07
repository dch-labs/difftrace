//! Binary entry point: wires config, provider, gateway, diff index, and
//! the review runner into the `difftrace review` command.

use std::process::ExitCode;

use clap::Parser as _;

use difftrace::cli::Cli;
use difftrace::cli::Command;
use difftrace::cli::ReplyArgs;
use difftrace::diff::DiffIndex;
use difftrace::error::DifftraceError;
use difftrace::review::ReplyMode;
use difftrace::review::ReplyTarget;
use difftrace::review::ReviewRunner;
use difftrace::review::runner::fetch_pinned_diff;
use difftrace::session::Session;
use difftrace::session::open_session;
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
            finish(runtime.block_on(run_chat(args, target, ReplyMode::Chat)))
        }
        Command::Plan(args) => {
            let target = match reply_target(&args) {
                Ok(target) => target,
                Err(err) => return finish(Err(err)),
            };
            finish(runtime.block_on(run_chat(args, target, ReplyMode::Plan)))
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
    let Session {
        mut config,
        client,
        gateway,
    } = open_session(&args.repo, args.config.as_deref())?;
    eprintln!("difftrace: fetching pull request #{}…", args.pr);
    let (overview, raw_diff) =
        fetch_pinned_diff(gateway.as_ref(), args.pr, args.sha.as_deref()).await?;
    let index = std::sync::Arc::new(DiffIndex::parse(&raw_diff)?);
    eprintln!(
        "difftrace: {} changed file(s), reviewing in batches of {}…",
        index.len(),
        config.review.batch_files
    );
    if let Some(path) = &args.memory {
        config.review.memory_content = difftrace::review::memory::load(path);
    }
    let head_sha = overview.head_sha.clone();
    let output_budget = difftrace::provider::enforced_output_budget(&config);
    let runner = ReviewRunner::new(
        client,
        std::sync::Arc::clone(&gateway) as std::sync::Arc<dyn difftrace::github::PrGateway>,
        index,
        overview,
        config.review,
        trajectory_dir(),
    )
    .with_output_budget(output_budget);
    let outcome = match runner.review_all(args.dry_run).await {
        Ok(outcome) => outcome,
        Err(err) => {
            record_errored_round(&args, &head_sha);
            return Err(err);
        }
    };
    record_round_learnings(&args, &outcome);
    report_outcome(&args, &outcome);
    Ok(ExitCode::SUCCESS)
}

fn record_errored_round(args: &difftrace::cli::ReviewArgs, head_sha: &str) {
    // The round failed before completing; the review itself may or may
    // not already stand on GitHub. Record the round without claiming a
    // posting that did not happen.
    if args.dry_run {
        return;
    }
    let Some(path) = &args.memory else {
        return;
    };
    let section = difftrace::review::memory::errored_section(head_sha);
    if let Err(err) = difftrace::review::memory::append(path, &section) {
        eprintln!("difftrace: cannot update the memory file: {err}");
    }
}

fn record_round_learnings(
    args: &difftrace::cli::ReviewArgs,
    outcome: &difftrace::review::ReviewOutcome,
) {
    if args.dry_run {
        return;
    }
    let Some(path) = &args.memory else {
        return;
    };
    let section = difftrace::review::memory::learning_section(
        &outcome.head_sha,
        &outcome.raised_titles(),
        &outcome.fixed_this_round,
        &outcome.unreviewed_batches,
    );
    if let Err(err) = difftrace::review::memory::append(path, &section) {
        eprintln!("difftrace: cannot update the memory file: {err}");
    }
}

fn report_outcome(args: &difftrace::cli::ReviewArgs, outcome: &difftrace::review::ReviewOutcome) {
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
}

async fn run_chat(
    args: ReplyArgs,
    target: ReplyTarget,
    mode: ReplyMode,
) -> Result<ExitCode, DifftraceError> {
    let Session {
        config,
        client,
        gateway,
    } = open_session(&args.repo, args.config.as_deref())?;
    let verb = match mode {
        ReplyMode::Chat => "answering a question",
        ReplyMode::Plan => "planning a fix",
    };
    eprintln!("difftrace: {verb} on pull request #{}…", args.pr);
    let (overview, raw_diff) =
        fetch_pinned_diff(gateway.as_ref(), args.pr, args.sha.as_deref()).await?;
    let index = std::sync::Arc::new(DiffIndex::parse(&raw_diff)?);
    let runner = ReviewRunner::new(
        client,
        std::sync::Arc::clone(&gateway) as std::sync::Arc<dyn difftrace::github::PrGateway>,
        index,
        overview,
        config.review,
        trajectory_dir(),
    );
    let outcome = match mode {
        ReplyMode::Chat => runner.reply(target).await?,
        ReplyMode::Plan => runner.plan(target).await?,
    };
    if outcome.refused {
        eprintln!(
            "difftrace: refused — posted the authorization note to the {}",
            outcome.target
        );
    } else {
        let noun = match mode {
            ReplyMode::Chat => "reply",
            ReplyMode::Plan => "plan",
        };
        println!("difftrace: {noun} posted to the {}", outcome.target);
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
