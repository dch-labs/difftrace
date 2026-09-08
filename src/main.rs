//! Binary entry point: wires config, provider, gateway, diff index, and
//! the review runner into the `difftrace review` command.

use std::process::ExitCode;

use clap::Parser as _;

use difftrace::cli::Cli;
use difftrace::cli::Command;
use difftrace::cli::ReplyArgs;
use difftrace::diff::DiffIndex;
use difftrace::error::DifftraceError;
use difftrace::github::PrGateway as _;
use difftrace::review::ReplyMode;
use difftrace::review::ReplyTarget;
use difftrace::review::ReviewRunner;
use difftrace::review::runner::fetch_pinned_diff;
use difftrace::session::Session;
use difftrace::session::open_session;
use tracing::Instrument as _;

fn init_logging() {
    let result = tracing_subscriber::fmt()
        .with_env_filter(difftrace::review::logging::build_filter())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .try_init();
    if let Err(err) = result {
        eprintln!("difftrace: cannot install the log subscriber: {err}");
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let local_review = matches!(&cli.command, Command::Review(args) if args.base.is_some());
    if !local_review {
        init_logging();
    }
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

fn apply_review_overrides(
    args: &difftrace::cli::ReviewArgs,
    settings: &mut difftrace::config::ReviewSettings,
) {
    if let Some(scope) = args.scope {
        settings.scope = scope;
    }
    if let Some(focus) = &args.focus {
        settings.focus.clone_from(focus);
    }
    if let Some(severity) = args.severity {
        settings.severity = severity;
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
    if let Some(base) = args.base.as_deref() {
        return run_local(&args, base).await;
    }
    let repo = args.repo.as_deref().ok_or_else(|| {
        DifftraceError::Cli("--repo is required unless --base selects the local mode".to_owned())
    })?;
    let pr = args.pr.ok_or_else(|| {
        DifftraceError::Cli("--pr is required unless --base selects the local mode".to_owned())
    })?;
    if args.scope == Some(difftrace::config::ReviewMode::Full) {
        return Err(DifftraceError::Cli(
            "--scope full needs a local checkout: run it with --base".to_owned(),
        ));
    }
    let Session {
        mut config,
        client,
        gateway,
    } = open_session(repo, args.config.as_deref())?;
    apply_review_overrides(&args, &mut config.review);
    if config.review.scope == difftrace::config::ReviewMode::Full {
        return Err(DifftraceError::Cli(
            "--scope full needs a local checkout: run it with --base".to_owned(),
        ));
    }
    eprintln!(
        "difftrace {}: fetching pull request #{pr}…",
        env!("CARGO_PKG_VERSION")
    );
    let (overview, raw_diff) = fetch_pinned_diff(gateway.as_ref(), pr, args.sha.as_deref())
        .instrument(tracing::debug_span!("fetch_pr", pr))
        .await?;
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
    let outcome = match runner
        .review_all(args.dry_run)
        .instrument(tracing::debug_span!("review_round", dry_run = args.dry_run))
        .await
    {
        Ok(outcome) => outcome,
        Err(err) => {
            record_errored_round(&args, &head_sha);
            return Err(err);
        }
    };
    record_round_learnings(&args, &outcome);
    report_outcome(args.dry_run, &format!("#{pr}"), &outcome);
    Ok(ExitCode::SUCCESS)
}

async fn run_local(
    args: &difftrace::cli::ReviewArgs,
    base: &str,
) -> Result<ExitCode, DifftraceError> {
    if args.sha.is_some() {
        return Err(DifftraceError::Cli(
            "--sha does not apply with --base: the local head is whatever is checked out"
                .to_owned(),
        ));
    }
    if args.pr.is_some() {
        return Err(DifftraceError::Cli(
            "--pr does not apply with --base: the local review has no pull request".to_owned(),
        ));
    }
    let difftrace::session::LocalSession { mut config, client } =
        difftrace::session::open_local_session(args.config.as_deref())?;
    let gateway = difftrace::local::LocalGateway::new(std::path::Path::new("."), base)?;
    let overview = gateway.pr_overview(0).await?;
    let raw_diff = gateway.pr_diff(0).await?;
    let index = std::sync::Arc::new(DiffIndex::parse(&raw_diff)?);
    let log = local_run_log(&overview);
    init_local_logging(log.as_deref());
    tracing::debug!(
        target: "difftrace::session",
        profile = ?config.provider.profile,
        model = ?config.provider.model,
        max_tokens = ?config.provider.max_tokens,
        log_level = ?config.log_level,
        "local session resolved"
    );
    tracing::info!(
        target: "difftrace",
        "difftrace {} — local review of {base}..{} at {}",
        env!("CARGO_PKG_VERSION"),
        overview.head_branch,
        overview.head_sha
    );
    apply_review_overrides(args, &mut config.review);
    config.review.hunt_always = true;
    if let Some(path) = &args.memory {
        config.review.memory_content = difftrace::review::memory::load(path);
    }
    tracing::info!(
        target: "difftrace",
        files = index.len(),
        batch_files = config.review.batch_files,
        mode = ?config.review.scope,
        "reviewing"
    );
    let output_budget = difftrace::provider::enforced_output_budget(&config);
    let runner = ReviewRunner::new(
        client,
        std::sync::Arc::new(gateway) as std::sync::Arc<dyn difftrace::github::PrGateway>,
        index,
        overview,
        config.review,
        trajectory_dir(),
    )
    .with_output_budget(output_budget);
    let outcome = match runner
        .review_all(true)
        .instrument(tracing::debug_span!("local_round"))
        .await
    {
        Ok(outcome) => outcome,
        Err(err) => {
            append_run_log(
                log.as_deref(),
                &format!(
                    "\n--- run failed ---\n{}\n",
                    difftrace::error::error_chain(&err)
                ),
            );
            return Err(err);
        }
    };
    report_outcome(true, "local", &outcome);
    append_run_log(
        log.as_deref(),
        &format!(
            "\n--- round body ---\n{}\n\n--- difftrace comment ---\n{}\n\ndifftrace: \
             dry run — {} inline finding(s) would be posted, {} dropped\n",
            outcome.round_body.trim_end(),
            outcome.standing_body.trim_end(),
            outcome.comments.len(),
            outcome.dropped.len()
        ),
    );
    Ok(ExitCode::SUCCESS)
}

fn checkout_label() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|dir| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "checkout".to_owned())
}

fn local_run_log(overview: &difftrace::github::PrOverview) -> Option<std::path::PathBuf> {
    let Some(home) = dirs::home_dir() else {
        eprintln!("difftrace: cannot determine the home directory; no run log");
        return None;
    };
    let dir = home.join(".difftrace").join("reviews");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "difftrace: cannot create {}: {err}; no run log",
            dir.display()
        );
        return None;
    }
    let path = difftrace::review::logging::local_review_log(
        &dir,
        &checkout_label(),
        chrono::Local::now(),
        &overview.head_sha,
    );
    let header = format!(
        "# difftrace {} local review — {}..{} @ {}\n\n",
        env!("CARGO_PKG_VERSION"),
        overview.base_branch,
        overview.head_branch,
        overview.head_sha
    );
    match std::fs::write(&path, header) {
        Ok(()) => Some(path),
        Err(err) => {
            eprintln!(
                "difftrace: cannot write {}: {err}; no run log",
                path.display()
            );
            None
        }
    }
}

fn init_local_logging(log: Option<&std::path::Path>) {
    let file = log.and_then(|path| {
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|err| {
                eprintln!(
                    "difftrace: cannot reopen {}: {err}; logging to stderr only",
                    path.display()
                );
            })
            .ok()
    });
    match file {
        Some(file) => {
            if !difftrace::review::logging::install_local(file) {
                init_logging();
            }
        }
        None => init_logging(),
    }
}

fn append_run_log(log: Option<&std::path::Path>, body: &str) {
    let Some(path) = log else { return };
    let appended = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, body.as_bytes()));
    if let Err(err) = appended {
        eprintln!("difftrace: cannot update {}: {err}", path.display());
    }
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

fn report_outcome(dry: bool, target: &str, outcome: &difftrace::review::ReviewOutcome) {
    if dry {
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
            "difftrace: review posted to {target} — {} inline finding(s), {} dropped",
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
    eprintln!(
        "difftrace {}: {verb} on pull request #{}…",
        env!("CARGO_PKG_VERSION"),
        args.pr
    );
    let (overview, raw_diff) = fetch_pinned_diff(gateway.as_ref(), args.pr, args.sha.as_deref())
        .instrument(tracing::debug_span!("fetch_pr", pr = args.pr))
        .await?;
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
