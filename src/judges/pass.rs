//! One judge's full sweep: concurrent regions through the lane pool,
//! budgeted tools, bounded per-region retries with backoff, and every
//! failure recorded — a judge never leaves a silent hole.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use loopctl::api::ApiClient;
use loopctl::config::SessionConfig;
use loopctl::engine::{BareLoop, Loop, RunConfig};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::aspects::AspectRegistry;
use crate::config::JudgesConfig;
use crate::contracts::{
    AspectId, CellVerdict, Cite, ClaimType, CoveragePlan, DiffSource, Finding, FindingDraft,
    JudgeReport, Region, RegionId, RegionKind, RegionReport, RegionStats, RoundState, Severity,
};
use crate::diff::DiffIndex;
use crate::error::{DtError, JudgeId};
use crate::gateway::PrGateway;
use crate::ratelimit::{self, CooldownGate, Lanes};
use crate::render;

use super::flow;
use super::prompt;
use super::provider::{self, JudgeClient};
use super::tools;

#[derive(Clone)]
pub struct JudgeSpec {
    pub id: JudgeId,
    pub profile: String,
    pub max_tool_calls_per_region: usize,
}

pub struct SweepJob {
    pub spec: JudgeSpec,
    pub cfg: JudgesConfig,
    pub plan: CoveragePlan,
    pub gateway: Arc<dyn PrGateway>,
    pub state: RoundState,
    pub aspects: Arc<AspectRegistry>,
    pub deadline: std::time::Instant,
    pub lanes: Arc<Semaphore>,
    pub cooldown: Arc<CooldownGate>,
    pub provider_lanes: Arc<Lanes>,
    pub untracked: Arc<Vec<String>>,
    pub served: Arc<std::sync::Mutex<crate::contracts::ServedReads>>,
}

pub async fn run_judge(job: SweepJob) -> Result<JudgeReport, DtError> {
    match provider::client_for_profile(
        &job.spec.profile,
        job.cfg.model_settings(&job.spec.profile),
    )? {
        JudgeClient::Zai(client) | JudgeClient::Anthropic(client) => {
            run_judge_with(job, Arc::new(client)).await
        }
        JudgeClient::Local(client) => run_judge_with(job, Arc::new(client)).await,
    }
}

pub async fn run_judge_with<C: ApiClient + 'static>(
    job: SweepJob,
    client: Arc<C>,
) -> Result<JudgeReport, DtError> {
    let SweepJob {
        spec,
        cfg,
        plan,
        gateway,
        state,
        aspects,
        deadline,
        lanes,
        cooldown,
        provider_lanes,
        untracked,
        served,
    } = job;
    let DiffSource::Unified(diff_text) = gateway.diff();
    DiffIndex::parse(&diff_text).map_err(|cause| absent(&spec, cause))?;
    let diff_text: Arc<str> = Arc::from(diff_text);
    let mut set: JoinSet<(usize, Result<RegionReport, String>, RegionStats)> = JoinSet::new();
    let gate_wait = Arc::new(AtomicU64::new(0));
    let mut total = 0usize;
    for (idx, region) in plan.regions.iter().enumerate() {
        let asked = plan
            .cells
            .iter()
            .filter(|(region_id, _)| region_id.0 == idx)
            .map(|(_, aspect)| aspect.clone())
            .collect::<Vec<_>>();
        if asked.is_empty() {
            continue;
        }
        total = total.saturating_add(1);
        set.spawn(judge_region(
            RegionJob {
                spec: spec.clone(),
                cfg: cfg.clone(),
                gateway: Arc::clone(&gateway),
                state: state.clone(),
                aspects: Arc::clone(&aspects),
                diff_text: Arc::clone(&diff_text),
                deadline,
                lanes: Arc::clone(&lanes),
                cooldown: Arc::clone(&cooldown),
                gate_wait: Arc::clone(&gate_wait),
                provider_lanes: Arc::clone(&provider_lanes),
                untracked: Arc::clone(&untracked),
                served: Arc::clone(&served),
            },
            Arc::clone(&client),
            RegionId(idx),
            region.clone(),
            asked,
            0,
        ));
    }
    tracing::info!(
        target: "difftrace::judges",
        judge = %spec.id,
        regions = total,
        lanes = cfg.region_concurrency,
        "sweep started"
    );
    let sweep_started = std::time::Instant::now();
    let mut settled: Vec<(usize, Result<RegionReport, String>, RegionStats)> = Vec::new();
    let mut done = 0usize;
    while let Some(joined) = set.join_next().await {
        let (idx, outcome, stats) = joined.map_err(|err| absent(&spec, err.to_string()))?;
        done = done.saturating_add(1);
        log_region_settled(&spec, idx, done, total, &outcome, &stats, sweep_started);
        settled.push((idx, outcome, stats));
    }
    settled.sort_by_key(|(idx, _, _)| *idx);
    let mut reports: Vec<RegionReport> = Vec::new();
    let mut failed: Vec<RegionId> = Vec::new();
    let mut stats_out: Vec<RegionStats> = Vec::new();
    let mut last_cause = String::new();
    for (idx, outcome, stats) in settled {
        stats_out.push(stats);
        match outcome {
            Ok(report) => reports.push(report),
            Err(cause) => {
                last_cause = cause;
                failed.push(RegionId(idx));
            }
        }
    }
    log_sweep_finished(
        &spec,
        failed.len(),
        &stats_out,
        sweep_started,
        gate_wait.load(Ordering::Relaxed),
    );
    if reports.is_empty() && !failed.is_empty() {
        let last = if last_cause.is_empty() {
            "the wall budget elapsed".to_owned()
        } else {
            last_cause
        };
        let cause = format!(
            "{} of {} regions failed after {} {}; last: {last}",
            failed.len(),
            failed.len().saturating_add(reports.len()),
            cfg.region_retries,
            if cfg.region_retries == 1 {
                "retry"
            } else {
                "retries"
            }
        );
        return Err(absent(&spec, cause));
    }
    Ok(JudgeReport {
        judge: spec.id.clone(),
        regions: reports,
        failed,
        stats: stats_out,
    })
}

#[derive(Clone)]
struct RegionJob {
    spec: JudgeSpec,
    cfg: JudgesConfig,
    gateway: Arc<dyn PrGateway>,
    state: RoundState,
    aspects: Arc<AspectRegistry>,
    diff_text: Arc<str>,
    deadline: std::time::Instant,
    lanes: Arc<Semaphore>,
    cooldown: Arc<CooldownGate>,
    gate_wait: Arc<AtomicU64>,
    provider_lanes: Arc<Lanes>,
    untracked: Arc<Vec<String>>,
    served: Arc<std::sync::Mutex<crate::contracts::ServedReads>>,
}

#[derive(Default)]
struct AttemptCost {
    turns: usize,
    tool_calls: usize,
    input_tokens: u64,
    output_tokens: u64,
    run_secs: Vec<u64>,
}

fn add_cost(total: &mut AttemptCost, part: &AttemptCost) {
    total.turns = total.turns.saturating_add(part.turns);
    total.tool_calls = total.tool_calls.saturating_add(part.tool_calls);
    total.input_tokens = total.input_tokens.saturating_add(part.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(part.output_tokens);
    total.run_secs.extend(part.run_secs.iter().copied());
}

async fn wait_behind_cooldown(job: &RegionJob, provider: &str) -> Option<String> {
    let cooldown_left = job.cooldown.remaining(provider);
    if cooldown_left.is_zero() {
        return None;
    }
    let wall_left = job
        .deadline
        .saturating_duration_since(std::time::Instant::now());
    job.gate_wait.fetch_add(
        u32::try_from(cooldown_left.min(wall_left).as_millis()).map_or(u64::MAX, u64::from),
        Ordering::Relaxed,
    );
    if tokio::time::timeout(wall_left, job.cooldown.wait(provider))
        .await
        .is_err()
    {
        return Some("the wall budget elapsed behind the provider cooldown".to_owned());
    }
    None
}

type RegionOutcome = (usize, Result<RegionReport, String>, RegionStats);

fn requeue_stalled_region<C: ApiClient + 'static>(
    job: RegionJob,
    client: Arc<C>,
    region_id: RegionId,
    region: Region,
    asked: Vec<AspectId>,
    parent_attempts: u8,
    parent_error: String,
    parent_cost: AttemptCost,
) -> Pin<Box<dyn Future<Output = RegionOutcome> + Send>> {
    Box::pin(async move {
        requeue_stalled_region_inner(
            job,
            client,
            region_id,
            region,
            asked,
            parent_attempts,
            parent_error,
            parent_cost,
        )
        .await
    })
}

async fn requeue_stalled_region_inner<C: ApiClient + 'static>(
    job: RegionJob,
    client: Arc<C>,
    region_id: RegionId,
    region: Region,
    asked: Vec<AspectId>,
    parent_attempts: u8,
    parent_error: String,
    parent_cost: AttemptCost,
) -> (usize, Result<RegionReport, String>, RegionStats) {
    let idx = region_id.0;
    let started = std::time::Instant::now();
    let judge_id = job.spec.id.clone();
    let span = region.end.saturating_sub(region.start);
    let mid = region.start.saturating_add(span / 2).max(region.start);
    let lower = Region {
        end: mid,
        ..region.clone()
    };
    let upper = Region {
        start: mid.saturating_add(1).min(region.end),
        ..region.clone()
    };
    tracing::warn!(
        target: "difftrace::judges",
        judge = %job.spec.id,
        region = idx,
        spans = %format!("{}-{} + {}-{}", lower.start, lower.end, upper.start, upper.end),
        cause = %parent_error,
        "the full-span conversation stalled its compactor; re-queueing two half-span attempts once"
    );
    let mut cost = parent_cost;
    let (_, lower_outcome, lower_stats) = judge_region(
        job.clone(),
        Arc::clone(&client),
        region_id,
        lower,
        asked.clone(),
        1,
    )
    .await;
    let (_, upper_outcome, upper_stats) =
        judge_region(job, client, region_id, upper, asked, 1).await;
    for stats in [&lower_stats, &upper_stats] {
        cost.turns = cost.turns.saturating_add(stats.turns);
        cost.tool_calls = cost.tool_calls.saturating_add(stats.tool_calls);
        cost.input_tokens = cost.input_tokens.saturating_add(stats.input_tokens);
        cost.output_tokens = cost.output_tokens.saturating_add(stats.output_tokens);
        cost.run_secs.extend(stats.run_secs.iter().copied());
    }
    let attempts = parent_attempts
        .saturating_add(lower_stats.attempts)
        .saturating_add(upper_stats.attempts);
    let judged = lower_stats.judged && upper_stats.judged;
    let outcome = merge_half_reports(lower_outcome, upper_outcome);
    let cause = outcome.as_ref().err().cloned();
    (
        idx,
        outcome,
        RegionStats {
            judge: judge_id,
            region: region_id,
            file: region.file.clone(),
            attempts,
            judged,
            secs: started.elapsed().as_secs(),
            turns: cost.turns,
            tool_calls: cost.tool_calls,
            input_tokens: cost.input_tokens,
            output_tokens: cost.output_tokens,
            run_secs: cost.run_secs.clone(),
            cause,
        },
    )
}

fn merge_half_reports(
    lower: Result<RegionReport, String>,
    upper: Result<RegionReport, String>,
) -> Result<RegionReport, String> {
    let lower = lower.map_err(|cause| format!("a half-span re-queue still failed: {cause}"))?;
    let upper = upper.map_err(|cause| format!("a half-span re-queue still failed: {cause}"))?;
    let mut cells: Vec<(AspectId, CellVerdict)> = Vec::new();
    for (aspect, lower_verdict) in lower.cells {
        let upper_verdict = upper
            .cells
            .iter()
            .find(|(asked, _)| asked == &aspect)
            .map(|(_, verdict)| verdict.clone());
        let merged = match upper_verdict {
            None => lower_verdict,
            Some(upper_verdict) => stronger_verdict(lower_verdict, upper_verdict),
        };
        cells.push((aspect, merged));
    }
    for (aspect, verdict) in upper.cells {
        if !cells.iter().any(|(asked, _)| asked == &aspect) {
            cells.push((aspect, verdict));
        }
    }
    Ok(RegionReport {
        region: lower.region,
        note: format!("{} · {}", lower.note, upper.note),
        cells,
    })
}

fn stronger_verdict(left: CellVerdict, right: CellVerdict) -> CellVerdict {
    if matches!(left, CellVerdict::Defect(_)) {
        return left;
    }
    if matches!(right, CellVerdict::Defect(_)) {
        return right;
    }
    if matches!(left, CellVerdict::OutOfScope) || matches!(right, CellVerdict::OutOfScope) {
        return CellVerdict::OutOfScope;
    }
    CellVerdict::Clean
}

async fn judge_region<C: ApiClient + 'static>(
    job: RegionJob,
    client: Arc<C>,
    region_id: RegionId,
    region: Region,
    asked: Vec<AspectId>,
    depth: u8,
) -> (usize, Result<RegionReport, String>, RegionStats) {
    let idx = region_id.0;
    let mut cost = AttemptCost::default();
    let queued_stats = |attempts: u8, cause: Option<String>| RegionStats {
        judge: job.spec.id.clone(),
        region: region_id,
        file: region.file.clone(),
        attempts,
        judged: false,
        secs: 0,
        turns: cost.turns,
        tool_calls: cost.tool_calls,
        input_tokens: cost.input_tokens,
        output_tokens: cost.output_tokens,
        run_secs: cost.run_secs.clone(),
        cause,
    };
    if std::time::Instant::now() >= job.deadline {
        return (
            idx,
            Err("the wall budget elapsed before the region could run".to_owned()),
            queued_stats(
                0,
                Some("the wall budget elapsed before the region could run".to_owned()),
            ),
        );
    }
    let permit = match Arc::clone(&job.lanes).acquire_owned().await {
        Ok(permit) => permit,
        Err(err) => {
            return (
                idx,
                Err(err.to_string()),
                queued_stats(0, Some(err.to_string())),
            );
        }
    };
    let started = std::time::Instant::now();
    let stats_for =
        |attempts: u8, judged: bool, cost: &AttemptCost, cause: Option<String>| RegionStats {
            judge: job.spec.id.clone(),
            region: region_id,
            file: region.file.clone(),
            attempts,
            judged,
            secs: started.elapsed().as_secs(),
            turns: cost.turns,
            tool_calls: cost.tool_calls,
            input_tokens: cost.input_tokens,
            output_tokens: cost.output_tokens,
            run_secs: cost.run_secs.clone(),
            cause,
        };
    let mut attempts: u8 = 0;
    loop {
        attempts = attempts.saturating_add(1);
        let provider = provider_of(&job.spec.profile);
        if let Some(cause) = wait_behind_cooldown(&job, &provider).await {
            return (
                idx,
                Err(cause.clone()),
                stats_for(attempts.saturating_sub(1), false, &cost, Some(cause)),
            );
        }
        match attempt_region(&job, &client, region_id, &region, &asked).await {
            Ok((report, spent)) => {
                add_cost(&mut cost, &spent);
                if attempts > 1 {
                    tracing::warn!(
                        target: "difftrace::judges",
                        judge = %job.spec.id,
                        region = idx,
                        retry = attempts.saturating_sub(1),
                        "region recovered on a retry"
                    );
                }
                return (idx, Ok(report), stats_for(attempts, true, &cost, None));
            }
            Err((error, spent)) => {
                add_cost(&mut cost, &spent);
                let kind = classify(&error);
                if kind == FailKind::RateLimited {
                    job.cooldown.trip(&provider, retry_after_hint(&error));
                }
                let spent_retries = attempts.saturating_sub(1);
                if kind == FailKind::Context
                    && depth == 0
                    && region.end != u32::MAX
                    && region.end.saturating_sub(region.start) >= 2
                {
                    drop(permit);
                    return requeue_stalled_region(
                        job, client, region_id, region, asked, attempts, error, cost,
                    )
                    .await;
                }
                let terminal = match kind {
                    FailKind::Budget => Some(
                        "region too large for its stream budget; a retry cannot fit the same wall, so none is spent",
                    ),
                    FailKind::Context => Some(
                        "the judge conversation cannot be compacted further; an identical retry stalls identically, so none is spent",
                    ),
                    _ if spent_retries >= job.cfg.region_retries => {
                        Some("region failed after all its retries; the sweep continues without it")
                    }
                    _ => None,
                };
                if let Some(message) = terminal {
                    tracing::warn!(
                        target: "difftrace::judges",
                        judge = %job.spec.id,
                        region = idx,
                        retries = spent_retries,
                        cause = %error,
                        "{message}"
                    );
                    let cause = error.clone();
                    return (
                        idx,
                        Err(error),
                        stats_for(attempts, false, &cost, Some(cause)),
                    );
                }
                if let Some(cause) = retry_pause(&job, &provider, attempts, &error).await {
                    tracing::warn!(
                        target: "difftrace::judges",
                        judge = %job.spec.id,
                        region = idx,
                        cause = %cause,
                        "region failed and the wall budget rules out a retry"
                    );
                    return (
                        idx,
                        Err(cause.clone()),
                        stats_for(attempts, false, &cost, Some(cause)),
                    );
                }
            }
        }
    }
}
async fn retry_pause(job: &RegionJob, provider: &str, attempts: u8, error: &str) -> Option<String> {
    let wall_left = job
        .deadline
        .saturating_duration_since(std::time::Instant::now());
    let cooldown_left = job.cooldown.remaining(provider);
    if wall_left.is_zero() {
        return Some(if cooldown_left.is_zero() {
            error.to_owned()
        } else {
            format!(
                "{error} (provider {provider} is cooling down for {}s and the wall budget rules out waiting)",
                cooldown_left.as_secs()
            )
        });
    }
    let backoff = if job.cfg.retry_backoff_secs > 0 {
        let mut rng = ratelimit::SplitMix64::from_entropy();
        ratelimit::backoff_wait(
            u32::from(attempts),
            Duration::from_secs(job.cfg.retry_backoff_secs),
            &mut rng,
        )
    } else {
        Duration::ZERO
    };
    let wait = cooldown_left.max(backoff).min(wall_left);
    if !wait.is_zero() {
        job.gate_wait.fetch_add(
            u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        tokio::time::sleep(wait).await;
    }
    None
}

fn log_region_settled(
    spec: &JudgeSpec,
    idx: usize,
    done: usize,
    total: usize,
    outcome: &Result<RegionReport, String>,
    stats: &RegionStats,
    sweep_started: std::time::Instant,
) {
    match outcome {
        Ok(_) => tracing::info!(
            target: "difftrace::judges",
            judge = %spec.id,
            region = idx,
            done = done,
            total = total,
            attempts = stats.attempts,
            dur = %render::fmt_duration(stats.secs),
            runs = %fmt_run_secs(&stats.run_secs),
            turns = stats.turns,
            tool_calls = stats.tool_calls,
            tokens_in = stats.input_tokens,
            tokens_out = stats.output_tokens,
            elapsed = %render::fmt_duration(sweep_started.elapsed().as_secs()),
            "region judged"
        ),
        Err(cause) => tracing::warn!(
            target: "difftrace::judges",
            judge = %spec.id,
            region = idx,
            done = done,
            total = total,
            attempts = stats.attempts,
            dur = %render::fmt_duration(stats.secs),
            runs = %fmt_run_secs(&stats.run_secs),
            turns = stats.turns,
            tool_calls = stats.tool_calls,
            tokens_in = stats.input_tokens,
            tokens_out = stats.output_tokens,
            cause = %cause,
            "region failed"
        ),
    }
}

fn fmt_run_secs(secs: &[u64]) -> String {
    secs.iter()
        .map(|s| render::fmt_duration(*s))
        .collect::<Vec<_>>()
        .join(", ")
}

fn log_sweep_finished(
    spec: &JudgeSpec,
    failed: usize,
    stats: &[RegionStats],
    sweep_started: std::time::Instant,
    gate_wait_ms: u64,
) {
    let judged_count = stats.iter().filter(|entry| entry.judged).count();
    let turns: usize = stats.iter().map(|entry| entry.turns).sum();
    let tool_calls: usize = stats.iter().map(|entry| entry.tool_calls).sum();
    let tokens_in: u64 = stats.iter().map(|entry| entry.input_tokens).sum();
    let tokens_out: u64 = stats.iter().map(|entry| entry.output_tokens).sum();
    tracing::info!(
        target: "difftrace::judges",
        judge = %spec.id,
        judged = judged_count,
        failed,
        mins = sweep_started.elapsed().as_secs() / 60,
        dur = %render::fmt_duration(sweep_started.elapsed().as_secs()),
        turns,
        tool_calls,
        tokens_in,
        tokens_out,
        gate_wait_ms,
        "sweep finished"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailKind {
    Budget,
    Context,
    RateLimited,
    Other,
}

// Digs the provider's `retry after Some(12s)` directive (plain form
// accepted) out of the error text; no hint means the gate's own backoff.
fn retry_after_hint(error: &str) -> Option<Duration> {
    let lowered = error.to_lowercase();
    let tail = lowered.split("retry after").nth(1)?;
    let tail = tail.trim_start();
    let tail = tail.strip_prefix("some").unwrap_or(tail).trim_start();
    let tail = tail.strip_prefix('(').unwrap_or(tail);
    let mut rest = tail;
    let mut nanos: u128 = 0;
    let mut any = false;
    loop {
        let number_end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        if number_end == 0 {
            break;
        }
        let number = rest.get(..number_end)?;
        rest = rest.get(number_end..)?;
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != 'µ')
            .unwrap_or(rest.len());
        let unit = rest.get(..unit_end)?;
        let unit_nanos: u128 = match unit {
            "h" => 3_600_000_000_000,
            "m" => 60_000_000_000,
            "s" => 1_000_000_000,
            "ms" => 1_000_000,
            "us" | "µs" => 1_000,
            "ns" => 1,
            _ => break,
        };
        nanos = nanos.saturating_add(decimal_nanos(number, unit_nanos)?);
        any = true;
        rest = rest.get(unit_end..)?;
        let trimmed = rest.trim_start();
        if trimmed.len() == rest.len() {
            break;
        }
        rest = trimmed;
    }
    (any && nanos > 0).then(|| Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX)))
}

// `number` × `unit_nanos` in integer math: "1.5" with a seconds unit is
// 1_500_000_000ns; an absurd integer part carries no hint at all.
fn decimal_nanos(number: &str, unit_nanos: u128) -> Option<u128> {
    let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
    let whole: u128 = whole.parse::<u128>().ok()?;
    let mut value = whole.saturating_mul(unit_nanos);
    if !frac.is_empty() {
        let digits: u128 = frac.parse::<u128>().ok()?;
        if let Some(scale) = 10_u128.checked_pow(u32::try_from(frac.len()).ok()?)
            && let Some(part) = digits
                .checked_mul(unit_nanos)
                .and_then(|product| product.checked_div(scale))
        {
            value = value.saturating_add(part);
        }
    }
    Some(value)
}

fn classify(error: &str) -> FailKind {
    let lowered = error.to_lowercase();
    if lowered.contains("made no progress") && lowered.contains("cannot shrink") {
        FailKind::Context
    } else if lowered.contains("exceeded the") && lowered.contains("s budget") {
        FailKind::Budget
    } else if lowered.contains("rate limit") || lowered.contains("http 429") {
        FailKind::RateLimited
    } else {
        FailKind::Other
    }
}

fn provider_of(profile: &str) -> String {
    profile
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase()
}

#[must_use]
pub fn attempt_budget(per_call: Duration, turns_allowed: usize) -> Duration {
    let calls = turns_allowed.saturating_add(2);
    let calls = u32::try_from(calls).unwrap_or(u32::MAX);
    per_call.saturating_mul(calls)
}

fn session_for(system: String, window: u64) -> SessionConfig {
    SessionConfig::default()
        .with_system_prompt(system)
        .with_context_window(window)
}

async fn attempt_region<C: ApiClient>(
    job: &RegionJob,
    client: &Arc<C>,
    region_id: RegionId,
    region: &Region,
    asked: &[AspectId],
) -> Result<(RegionReport, AttemptCost), (String, AttemptCost)> {
    let spec = &job.spec;
    let cfg = &job.cfg;
    let gateway = &job.gateway;
    let state = &job.state;
    let aspects = &job.aspects;
    let diff_text: &str = &job.diff_text;
    let deadline = job.deadline;
    let snapshot = gateway
        .read_file_at_head(&region.file)
        .map_err(|err| (err.to_string(), AttemptCost::default()))?;
    let flow_map = match region.kind {
        RegionKind::File | RegionKind::Repo => Some(
            flow::build(
                region,
                &snapshot.content,
                gateway.as_ref(),
                cfg.flow_map_max_callers,
            )
            .map_err(|err| (err.to_string(), AttemptCost::default()))?,
        ),
        RegionKind::Added | RegionKind::Hunk => None,
    };
    let system = prompt::system(
        &spec.id,
        &prompt::open_issues_block(state, &region.file),
        &prompt::suppressions_block(state, &region.file),
        &aspects.definitions_block(asked),
    );
    let user = prompt::region_prompt(
        region,
        region_id.0,
        &crate::diff::region_diff(
            diff_text,
            &region.file,
            region.start,
            region.end,
            prompt::REGION_CONTEXT_LINES,
        ),
        &prompt::truncate_snapshot(&snapshot.content),
        flow_map.as_ref(),
        asked,
        prompt::REPORT_SCHEMA,
        &prompt::untracked_block(&job.untracked),
    );
    let budget = Arc::new(AtomicUsize::new(0));
    let registry = tools::region_registry(
        Arc::clone(gateway),
        budget,
        spec.max_tool_calls_per_region,
        cfg.search_tool,
        &job.served,
    );
    let session = session_for(system, job.cfg.model_settings(&spec.profile).context_window);
    let turns_allowed = spec.max_tool_calls_per_region.saturating_add(2);
    let mut run_config = RunConfig::default();
    run_config.max_turns = turns_allowed;
    let managers = loopctl::managers::LoopManagers::new()
        .with_stream_handler(loopctl::stream::handler::StreamHandler::new());
    let mut agent = BareLoop::new_with_managers(Arc::clone(client), registry, session, managers);
    let provider = provider_of(&spec.profile);
    let _provider_lane = job
        .provider_lanes
        .acquire(&provider)
        .await
        .map_err(|err| (err.to_string(), AttemptCost::default()))?;
    let per_call = Duration::from_secs(cfg.stream_timeout_secs);
    let attempt_wall = attempt_budget(per_call, turns_allowed);
    let timeout = attempt_wall.min(deadline.saturating_duration_since(std::time::Instant::now()));
    let budget_secs = timeout.as_secs();
    let (wire, attempt_cost) = match tokio::time::timeout(
        timeout,
        drive_region_runs(&mut agent, &user, &run_config, spec, region_id),
    )
    .await
    {
        Ok(Ok(pair)) => pair,
        Ok(Err((cause, spent))) => return Err((cause, spent)),
        Err(_) => {
            return Err((
                format!(
                    "judge {} did not answer: exceeded the {budget_secs}s budget",
                    spec.id
                ),
                AttemptCost::default(),
            ));
        }
    };
    let report = match map_wire(wire, region_id, asked) {
        Ok(report) => report,
        Err(cause) => {
            return Err((
                format!("judge {} did not answer: {cause}", spec.id),
                attempt_cost,
            ));
        }
    };
    Ok((report, attempt_cost))
}

async fn drive_region_runs<C: ApiClient>(
    agent: &mut BareLoop<C>,
    user: &str,
    run_config: &RunConfig,
    spec: &JudgeSpec,
    region_id: RegionId,
) -> Result<(RegionWire, AttemptCost), (String, AttemptCost)> {
    let mut cost = AttemptCost::default();
    let run_started = std::time::Instant::now();
    let run = match agent.run(user, run_config).await {
        Ok(run) => run,
        Err(err) => {
            cost.run_secs.push(run_started.elapsed().as_secs());
            tracing::warn!(
                target: "difftrace::judges",
                judge = %spec.id,
                region = region_id.0,
                run = "primary",
                cause = %err,
                "run failed; its wall seconds still count"
            );
            return Err((format!("judge {} did not answer: {err}", spec.id), cost));
        }
    };
    let run_secs = run_started.elapsed().as_secs();
    tracing::info!(
        target: "difftrace::judges",
        judge = %spec.id,
        region = region_id.0,
        run = "primary",
        dur = %render::fmt_duration(run_secs),
        turns = run.turn_count(),
        "run finished"
    );
    cost.run_secs.push(run_secs);
    cost.turns = cost.turns.saturating_add(run.turn_count());
    cost.tool_calls = cost.tool_calls.saturating_add(run.tool_call_count());
    cost.input_tokens = cost.input_tokens.saturating_add(run.input_tokens());
    cost.output_tokens = cost.output_tokens.saturating_add(run.output_tokens());
    if let Some(wire) = parse_report(&run.output.unwrap_or_default()) {
        return Ok((wire, cost));
    }
    let corrective_started = std::time::Instant::now();
    let corrective = match agent
        .run(
            "Return only the JSON report for this region, exactly per the schema, with no other text.",
            run_config,
        )
        .await
    {
        Ok(corrective) => corrective,
        Err(err) => {
            cost.run_secs.push(corrective_started.elapsed().as_secs());
            tracing::warn!(
                target: "difftrace::judges",
                judge = %spec.id,
                region = region_id.0,
                run = "corrective",
                cause = %err,
                "run failed; its wall seconds still count"
            );
            return Err((format!("judge {} did not answer: {err}", spec.id), cost));
        }
    };
    let corrective_secs = corrective_started.elapsed().as_secs();
    tracing::info!(
        target: "difftrace::judges",
        judge = %spec.id,
        region = region_id.0,
        run = "corrective",
        dur = %render::fmt_duration(corrective_secs),
        turns = corrective.turn_count(),
        "run finished"
    );
    cost.run_secs.push(corrective_secs);
    cost.turns = cost.turns.saturating_add(corrective.turn_count());
    cost.tool_calls = cost.tool_calls.saturating_add(corrective.tool_call_count());
    cost.input_tokens = cost.input_tokens.saturating_add(corrective.input_tokens());
    cost.output_tokens = cost
        .output_tokens
        .saturating_add(corrective.output_tokens());
    let Some(wire) = parse_report(&corrective.output.unwrap_or_default()) else {
        return Err((
            format!(
                "judge {} did not answer: the report never parsed as JSON per the schema",
                spec.id
            ),
            cost,
        ));
    };
    Ok((wire, cost))
}

fn absent(spec: &JudgeSpec, cause: String) -> DtError {
    DtError::JudgeAbsent {
        judge: spec.id.clone(),
        cause,
    }
}

fn parse_report(text: &str) -> Option<RegionWire> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

fn map_wire(
    wire: RegionWire,
    region: RegionId,
    asked: &[AspectId],
) -> Result<RegionReport, String> {
    if wire.region != region.0 {
        return Err(format!(
            "the report names region {} but this is region {}",
            wire.region, region.0
        ));
    }
    let mut verdicts: BTreeMap<String, CellVerdict> = BTreeMap::new();
    for cell in wire.cells {
        let verdict = match cell.verdict.as_str() {
            "clean" => CellVerdict::Clean,
            "out-of-scope" => CellVerdict::OutOfScope,
            "defect" => {
                let finding = cell.finding.ok_or("a defect verdict carries no finding")?;
                CellVerdict::Defect(build_finding(region, &cell.aspect, finding)?)
            }
            other => return Err(format!("unknown verdict {other:?}")),
        };
        if verdicts.insert(cell.aspect.clone(), verdict).is_some() {
            return Err(format!("aspect {:?} is judged twice", cell.aspect));
        }
    }
    let mut cells: Vec<(AspectId, CellVerdict)> = Vec::new();
    for aspect in asked {
        let verdict = verdicts
            .remove(aspect.0.as_str())
            .ok_or_else(|| format!("aspect {:?} has no verdict", aspect.0))?;
        cells.push((aspect.clone(), verdict));
    }
    if !verdicts.is_empty() {
        return Err(format!(
            "the report judges aspects that were not asked: {verdicts:?}"
        ));
    }
    Ok(RegionReport {
        region,
        note: wire.note,
        cells,
    })
}

fn build_finding(region: RegionId, aspect: &str, wire: FindingWire) -> Result<Finding, String> {
    let claim = match wire.claim.as_str() {
        "in-diff" => ClaimType::InDiff,
        "needs-execution" => ClaimType::NeedsExecution,
        "external-assumption" => ClaimType::ExternalAssumption,
        other => return Err(format!("unknown claim {other:?}")),
    };
    let severity: Severity = wire.severity.parse()?;
    let mut discarded_cites: usize = 0;
    let mut evidence: Vec<Cite> = Vec::new();
    for cite in &wire.evidence {
        let Some((file, line)) = cite.rsplit_once(':') else {
            if cite.chars().any(|c: char| c.is_ascii_digit()) {
                discarded_cites = discarded_cites.saturating_add(1);
            }
            continue;
        };
        match line.trim().parse::<u32>() {
            Ok(line) if !file.trim().is_empty() => evidence.push(Cite {
                file: file.trim().to_owned(),
                line,
            }),
            _ => {
                if !file.trim().is_empty() {
                    discarded_cites = discarded_cites.saturating_add(1);
                }
            }
        }
    }
    let mut finding = Finding::new(FindingDraft {
        region,
        aspect: AspectId(aspect.to_owned()),
        title: wire.title,
        mechanism: wire.mechanism,
        claim,
        evidence,
        severity,
        fix_hint: wire.fix_hint,
    });
    finding.discarded_cites = discarded_cites;
    Ok(finding)
}

#[derive(Debug, Deserialize)]
struct RegionWire {
    region: usize,
    note: String,
    cells: Vec<CellWire>,
}

#[derive(Debug, Deserialize)]
struct CellWire {
    aspect: String,
    verdict: String,
    finding: Option<FindingWire>,
}

#[derive(Debug, Deserialize)]
struct FindingWire {
    title: String,
    mechanism: String,
    claim: String,
    severity: String,
    evidence: Vec<String>,
    fix_hint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_served() -> Arc<std::sync::Mutex<crate::contracts::ServedReads>> {
        Arc::new(std::sync::Mutex::new(crate::contracts::ServedReads::new()))
    }
    use crate::contracts::{DiffSource, FileSnapshot, PinnedRange, RegionKind, SearchHit};
    use loopctl::testing::{MockApiClient, MockResponse};

    struct FakeGateway;

    impl PrGateway for FakeGateway {
        fn pinned_range(&self) -> PinnedRange {
            PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            }
        }
        fn diff(&self) -> DiffSource {
            DiffSource::Unified(String::new())
        }
        fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, DtError> {
            Ok(FileSnapshot {
                path: path.to_owned(),
                content: "fn a() {}\n".to_owned(),
            })
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
            Ok(Vec::new())
        }
        fn search_at_head(&self, _pattern: &str, _cap: usize) -> Result<Vec<SearchHit>, DtError> {
            Ok(Vec::new())
        }
    }

    fn spec() -> JudgeSpec {
        JudgeSpec {
            id: "alpha".to_owned(),
            profile: "mock".to_owned(),
            max_tool_calls_per_region: 10,
        }
    }

    fn plan(regions: usize, aspects: &[&str]) -> CoveragePlan {
        CoveragePlan {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            regions: (0..regions)
                .map(|_| Region {
                    file: "src/a.rs".to_owned(),
                    start: 1,
                    end: 2,
                    kind: RegionKind::Added,
                })
                .collect(),
            cells: aspects
                .iter()
                .flat_map(|aspect| {
                    (0..regions).map(|idx| (RegionId(idx), AspectId((*aspect).to_owned())))
                })
                .collect(),
            narrowed: false,
        }
    }

    fn state() -> RoundState {
        RoundState {
            checkout: "difftrace".to_owned(),
            round: 1,
            issues: Vec::new(),
            suppressions: Vec::new(),
        }
    }

    fn json_response(text: &str) -> MockResponse {
        MockResponse {
            text: text.to_owned(),
            tool_call: None,
            stop_reason: "end_turn".to_owned(),
        }
    }

    fn good_report() -> String {
        r#"{"region":0,"note":"traced both aspects through the flow","cells":[
            {"aspect":"error-paths","verdict":"defect","finding":{
                "title":"The unwrap panics on empty input",
                "mechanism":"an empty file reaches the unwrap and the process dies",
                "claim":"in-diff","severity":"warning","evidence":["src/a.rs:2"]}},
            {"aspect":"security","verdict":"clean"}]}"#
            .to_owned()
    }

    fn clean_report_for(region: usize) -> String {
        format!(
            r#"{{"region":{region},"note":"traced the aspect","cells":[{{"aspect":"error-paths","verdict":"clean"}}]}}"#
        )
    }

    async fn drive(client: MockApiClient, plan: &CoveragePlan) -> Result<JudgeReport, DtError> {
        drive_until(client, plan, far_deadline()).await
    }

    fn far_deadline() -> std::time::Instant {
        let now = std::time::Instant::now();
        now.checked_add(Duration::from_secs(3600)).unwrap_or(now)
    }

    async fn drive_until(
        client: MockApiClient,
        plan: &CoveragePlan,
        deadline: std::time::Instant,
    ) -> Result<JudgeReport, DtError> {
        drive_cfg(
            client,
            plan,
            deadline,
            JudgesConfig {
                retry_backoff_secs: 0,
                ..JudgesConfig::default()
            },
        )
        .await
    }

    async fn drive_cfg(
        client: MockApiClient,
        plan: &CoveragePlan,
        deadline: std::time::Instant,
        cfg: JudgesConfig,
    ) -> Result<JudgeReport, DtError> {
        run_judge_with(
            SweepJob {
                spec: spec(),
                cfg,
                plan: plan.clone(),
                gateway: Arc::new(FakeGateway),
                state: state(),
                aspects: Arc::new(AspectRegistry::new()),
                deadline,
                lanes: Arc::new(Semaphore::new(1)),
                cooldown: Arc::new(ratelimit::CooldownGate::new(0.0, Duration::ZERO)),
                provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
                untracked: Arc::new(Vec::new()),
                served: test_served(),
            },
            Arc::new(client),
        )
        .await
    }

    #[test]
    fn the_run_list_renders_in_the_human_duration_format() {
        // The summary line speaks the same register as dur: 1m16s+15s,
        // never the debug slice [76, 15].
        assert_eq!(fmt_run_secs(&[76, 15]), "1m16s, 15s");
        assert_eq!(fmt_run_secs(&[119]), "1m59s");
        assert_eq!(fmt_run_secs(&[3661, 60]), "1h01m, 1m");
        assert_eq!(fmt_run_secs(&[]), "", "an empty list renders empty");
    }

    #[test]
    fn the_stats_carry_one_duration_per_engine_run() {
        // One entry per engine run, in order — first answer and each
        // corrective turn — feeding the run_secs log and stats.
        let mut cost = AttemptCost::default();
        cost.run_secs.push(4);
        let mut second = AttemptCost::default();
        second.run_secs.push(7);
        second.run_secs.push(2);
        add_cost(&mut cost, &second);
        assert_eq!(cost.run_secs, vec![4, 7, 2], "one entry per run, in order");
    }

    #[tokio::test]
    async fn a_mock_judge_returns_a_well_formed_report() -> Result<(), DtError> {
        let report = drive(
            MockApiClient::new("mock").with_responses(vec![json_response(&good_report())]),
            &plan(1, &["error-paths", "security"]),
        )
        .await?;
        assert_eq!(report.judge, "alpha");
        assert_eq!(report.regions.len(), 1);
        assert!(report.regions.first().is_some_and(|region| {
            region.note == "traced both aspects through the flow"
                && region.cells.len() == 2
                && matches!(
                    region.cells.first(),
                    Some((aspect, CellVerdict::Defect(finding))) if aspect.0 == "error-paths"
                        && finding.severity == Severity::Warning
                        && finding.claim == ClaimType::InDiff
                        && finding.id.0.starts_with("0/error-paths/")
                )
                && matches!(
                    region.cells.get(1),
                    Some((aspect, CellVerdict::Clean)) if aspect.0 == "security"
                )
        }));
        Ok(())
    }

    #[tokio::test]
    async fn a_provider_failure_makes_the_judge_absent_without_a_partial_report() {
        let outcome = drive(
            MockApiClient::new("mock").with_responses(vec![json_response(&good_report())]),
            &plan(2, &["error-paths"]),
        )
        .await;
        assert!(
            matches!(&outcome, Err(DtError::JudgeAbsent { judge, .. }) if judge == "alpha"),
            "the second region's failed call voids the first region's report: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_region_with_no_asked_aspects_is_skipped_without_a_client_call() -> Result<(), DtError>
    {
        let mut coverage = plan(2, &["error-paths"]);
        coverage.cells = vec![(RegionId(0), AspectId("error-paths".to_owned()))];
        let report = drive(
            MockApiClient::new("mock").with_responses(vec![json_response(
                r#"{"region":0,"note":"traced the asked aspect","cells":[{"aspect":"error-paths","verdict":"clean"}]}"#,
            )]),
            &coverage,
        )
        .await?;
        assert_eq!(
            report.regions.len(),
            1,
            "the zero-cell region judges nothing and voids nothing"
        );
        assert!(
            report
                .regions
                .first()
                .is_some_and(|region| region.region.0 == 0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_malformed_report_makes_the_judge_absent() {
        let outcome = drive(
            MockApiClient::new("mock").with_responses(vec![
                json_response("I prefer to answer in prose."),
                json_response("still prose, no JSON"),
                json_response("prose again, no schema"),
                json_response("still no JSON"),
                json_response("prose on the third attempt"),
                json_response("no JSON here either"),
                json_response("fourth attempt, still prose"),
                json_response("never a report"),
            ]),
            &plan(1, &["error-paths"]),
        )
        .await;
        assert!(
            matches!(&outcome, Err(DtError::JudgeAbsent { judge, cause }) if judge == "alpha"
                && cause.contains("failed after 3 retries")
                && cause.contains("never parsed")),
            "prose after the corrective turn on every attempt is absence, never a partial report: {outcome:?}"
        );
        let wrong_aspects = drive(
            MockApiClient::new("mock").with_responses(vec![
                json_response(
                    r#"{"region":0,"note":"n","cells":[{"aspect":"vibes","verdict":"clean"}]}"#,
                );
                8
            ]),
            &plan(1, &["error-paths"]),
        )
        .await;
        assert!(
            matches!(&wrong_aspects, Err(DtError::JudgeAbsent { cause, .. }) if cause.contains("error-paths")),
            "a report that skips the asked aspects is malformed on every attempt: {wrong_aspects:?}"
        );
    }

    #[tokio::test]
    async fn a_prose_evidence_entry_discards_instead_of_voiding_the_judge() -> Result<(), DtError> {
        let prose = r#"{"region":0,"note":"traced","cells":[
            {"aspect":"error-paths","verdict":"defect","finding":{
                "title":"The dependency is not rev-pinned",
                "mechanism":"an empty file reaches the unwrap and the process dies at src/a.rs:2",
                "claim":"in-diff","severity":"warning",
                "evidence":["Cargo.toml line 14 (head): loopctl has no rev","src/a.rs:2"]}}]}"#;
        let report = drive(
            MockApiClient::new("mock").with_responses(vec![json_response(prose)]),
            &plan(1, &["error-paths"]),
        )
        .await?;
        let finding = report
            .regions
            .first()
            .and_then(|region| region.cells.first())
            .and_then(|(_, verdict)| match verdict {
                CellVerdict::Defect(finding) => Some(finding),
                CellVerdict::Clean | CellVerdict::OutOfScope => None,
            })
            .ok_or_else(|| DtError::JudgeAbsent {
                judge: "alpha".to_owned(),
                cause: "the defect must map".to_owned(),
            })?;
        assert_eq!(
            finding.evidence,
            vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 2
            }],
            "the prose citation is discarded and the file:line citation survives: {:?}",
            finding.evidence
        );
        let all_prose = r#"{"region":0,"note":"traced","cells":[
            {"aspect":"error-paths","verdict":"defect","finding":{
                "title":"The dependency is not rev-pinned",
                "mechanism":"an empty file reaches the unwrap and the process dies at src/a.rs:2",
                "claim":"in-diff","severity":"warning",
                "evidence":["Cargo.toml line 14 (head): loopctl has no rev"]}}]}"#;
        let report = drive(
            MockApiClient::new("mock").with_responses(vec![json_response(all_prose)]),
            &plan(1, &["error-paths"]),
        )
        .await?;
        let empty = report
            .regions
            .first()
            .and_then(|region| region.cells.first())
            .and_then(|(_, verdict)| match verdict {
                CellVerdict::Defect(finding) => Some(finding),
                CellVerdict::Clean | CellVerdict::OutOfScope => None,
            })
            .ok_or_else(|| DtError::JudgeAbsent {
                judge: "alpha".to_owned(),
                cause: "the defect must map".to_owned(),
            })?;
        assert!(
            empty.evidence.is_empty(),
            "a finding left with no evidence maps and drops at the gate, never voids the judge"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_region_retries_once_and_keeps_the_rest_of_the_sweep() -> Result<(), DtError> {
        let report = drive(
            MockApiClient::new("mock").with_responses(vec![
                json_response(&clean_report_for(0)),
                json_response("prose, not a report"),
                json_response("still prose"),
                json_response(&clean_report_for(1)),
            ]),
            &plan(2, &["error-paths"]),
        )
        .await?;
        assert_eq!(report.regions.len(), 2, "the retried region recovers");
        assert!(
            report.failed.is_empty(),
            "a recovered region leaves no failure behind: {:?}",
            report.failed
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_region_exhausting_its_retries_is_recorded_and_the_sweep_continues()
    -> Result<(), DtError> {
        let report = drive(
            MockApiClient::new("mock").with_responses(vec![
                json_response(&clean_report_for(0)),
                json_response("prose, not a report"),
                json_response("still prose"),
                json_response("prose again"),
                json_response("and again, no JSON"),
                json_response("third try, still prose"),
                json_response("nope"),
                json_response("never a report"),
                json_response("exhausted"),
            ]),
            &plan(2, &["error-paths"]),
        )
        .await?;
        assert_eq!(
            report.regions.len(),
            1,
            "the good region survives the other's failure"
        );
        assert_eq!(
            report.failed,
            vec![RegionId(1)],
            "the region that exhausts its retries is recorded, not fatal"
        );
        let by_region = |id: usize| {
            report
                .stats
                .iter()
                .find(|entry| entry.region.0 == id)
                .cloned()
        };
        let ok = by_region(0);
        assert!(
            matches!(&ok, Some(entry) if entry.judged && entry.attempts == 1 && entry.turns >= 1),
            "the settled region carries its stats: {ok:?}"
        );
        let bad = by_region(1);
        assert!(
            matches!(&bad, Some(entry) if !entry.judged && entry.attempts == 4
                && entry.cause.as_deref().is_some_and(|cause| cause.contains("never parsed"))),
            "the failed region carries attempts, outcome, and cause: {bad:?}"
        );
        assert_eq!(report.stats.len(), 2, "every settled region is statted");
        Ok(())
    }

    #[test]
    fn failure_kinds_classify_their_causes() {
        assert_eq!(
            classify("judge a did not answer: exceeded the 300s budget"),
            FailKind::Budget
        );
        assert_eq!(
            classify(
                "judge a did not answer: Compaction made no progress at 148899 tokens: the configured compactor cannot shrink this conversation further"
            ),
            FailKind::Context,
            "a compaction stall is a context condition, not a transient failure"
        );
        assert_eq!(
            classify("API error: stream failed: rate limit, 0 events processed"),
            FailKind::RateLimited
        );
        assert_eq!(
            classify("API error: HTTP 429: too many requests"),
            FailKind::RateLimited
        );
        assert_eq!(
            classify("the report never parsed as JSON per the schema"),
            FailKind::Other
        );
        assert_eq!(provider_of("zai:glm-5.3-flash"), "zai");
        assert_eq!(provider_of("local"), "local");
    }

    #[test]
    fn the_attempt_budget_scales_with_the_turns_allowed() {
        let hour = Duration::from_hours(1);
        let minute = Duration::from_secs(60);
        assert_eq!(
            attempt_budget(minute.saturating_mul(5), 12),
            hour + minute.saturating_mul(10),
            "per-call budget times calls (turns plus initial and corrective slack)"
        );
        assert_eq!(
            attempt_budget(minute.saturating_mul(15), 0),
            minute.saturating_mul(30),
            "even a toolless region keeps room for the corrective re-ask"
        );
    }

    #[test]
    fn the_session_carries_the_configured_context_window() {
        let session = session_for("judge system".to_owned(), 128_000);
        assert_eq!(
            session.context_window, 128_000,
            "the configured window anchors the compaction thresholds, not the 200k default"
        );
        assert_eq!(
            session.system_prompt.as_deref(),
            Some("judge system"),
            "the system prompt rides the same session"
        );
    }

    struct FlakyStreamClient {
        inner: MockApiClient,
        errors: std::sync::Mutex<std::collections::VecDeque<String>>,
    }

    impl FlakyStreamClient {
        fn new(inner: MockApiClient, errors: Vec<String>) -> Self {
            Self {
                inner,
                errors: std::sync::Mutex::new(errors.into()),
            }
        }

        fn pop(&self) -> Option<String> {
            self.errors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
        }
    }

    impl loopctl::api::ApiClient for FlakyStreamClient {
        fn model(&self) -> String {
            self.inner.model()
        }

        fn create_message(
            &self,
            request: &loopctl::api::StreamRequest,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            loopctl::api::NonStreamingResponse,
                            loopctl::api::error::ApiError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            if let Some(err) = self.pop() {
                let error = loopctl::api::error::ApiError::Api(err, None);
                return Box::pin(async move { Err(error) });
            }
            self.inner.create_message(request)
        }

        fn stream_messages(
            &self,
            request: &loopctl::api::StreamRequest,
        ) -> Pin<
            Box<
                dyn futures::Stream<
                        Item = Result<loopctl::stream::StreamEvent, loopctl::api::error::ApiError>,
                    > + Send
                    + 'static,
            >,
        > {
            if let Some(err) = self.pop() {
                let error = loopctl::api::error::ApiError::Api(err, None);
                return Box::pin(futures::stream::once(async move { Err(error) }));
            }
            self.inner.stream_messages(request)
        }

        fn stream_messages_with_options(
            &self,
            request: &loopctl::api::StreamRequest,
            options: loopctl::structured::RequestOptions,
        ) -> Pin<
            Box<
                dyn futures::Stream<
                        Item = Result<loopctl::stream::StreamEvent, loopctl::api::error::ApiError>,
                    > + Send
                    + 'static,
            >,
        > {
            if let Some(err) = self.pop() {
                let error = loopctl::api::error::ApiError::Api(err, None);
                return Box::pin(futures::stream::once(async move { Err(error) }));
            }
            self.inner.stream_messages_with_options(request, options)
        }
    }

    struct PendClient;

    impl loopctl::api::ApiClient for PendClient {
        fn model(&self) -> String {
            "pend".to_owned()
        }

        fn create_message(
            &self,
            _request: &loopctl::api::StreamRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            loopctl::api::NonStreamingResponse,
                            loopctl::api::error::ApiError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(std::future::pending())
        }

        fn stream_messages(
            &self,
            _request: &loopctl::api::StreamRequest,
        ) -> std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = Result<loopctl::stream::StreamEvent, loopctl::api::error::ApiError>,
                    > + Send
                    + 'static,
            >,
        > {
            Box::pin(futures::stream::pending())
        }
    }

    #[tokio::test]
    async fn a_stream_budget_failure_spends_no_retry() {
        let plan_coverage = plan(1, &["error-paths"]);
        let region = plan_coverage.regions.first().cloned().unwrap_or(Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 2,
            kind: RegionKind::Added,
        });
        let job = RegionJob {
            spec: spec(),
            cfg: JudgesConfig {
                stream_timeout_secs: 0,
                retry_backoff_secs: 0,
                region_retries: 3,
                ..JudgesConfig::default()
            },
            gateway: Arc::new(FakeGateway),
            state: state(),
            aspects: Arc::new(AspectRegistry::new()),
            diff_text: Arc::from(""),
            deadline: far_deadline(),
            lanes: Arc::new(Semaphore::new(1)),
            cooldown: Arc::new(ratelimit::CooldownGate::new(0.0, Duration::ZERO)),
            gate_wait: Arc::new(AtomicU64::new(0)),
            provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
            untracked: Arc::new(Vec::new()),
            served: test_served(),
        };
        let (idx, outcome, stats) = judge_region(
            job,
            Arc::new(PendClient),
            RegionId(0),
            region,
            vec![AspectId("error-paths".to_owned())],
            0,
        )
        .await;
        assert_eq!(idx, 0);
        assert!(
            outcome
                .as_ref()
                .is_err_and(|cause| cause.contains("exceeded the")),
            "the pending stream dies on the budget: {outcome:?}"
        );
        assert!(
            !stats.judged
                && stats.attempts == 1
                && stats
                    .cause
                    .as_deref()
                    .is_some_and(|cause| cause.contains("exceeded the")),
            "a region that cannot fit the budget spends exactly one attempt even with retries configured: {stats:?}"
        );
    }

    #[tokio::test]
    async fn a_compaction_stall_requeues_two_half_span_attempts_once() {
        let region = Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 10,
            kind: RegionKind::Added,
        };
        let job = RegionJob {
            spec: spec(),
            cfg: JudgesConfig {
                retry_backoff_secs: 0,
                region_retries: 0,
                ..JudgesConfig::default()
            },
            gateway: Arc::new(FakeGateway),
            state: state(),
            aspects: Arc::new(AspectRegistry::new()),
            diff_text: Arc::from(""),
            deadline: far_deadline(),
            lanes: Arc::new(Semaphore::new(1)),
            cooldown: Arc::new(ratelimit::CooldownGate::new(0.0, Duration::ZERO)),
            gate_wait: Arc::new(AtomicU64::new(0)),
            provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
            untracked: Arc::new(Vec::new()),
            served: test_served(),
        };
        let stall = "Compaction made no progress at 148899 tokens: the configured compactor cannot shrink this conversation further".to_owned();
        let (idx, outcome, stats) = judge_region(
            job,
            Arc::new(FlakyStreamClient::new(
                MockApiClient::new("mock").with_responses(vec![
                    json_response(&clean_report_for(0)),
                    json_response(&clean_report_for(0)),
                ]),
                vec![stall; 8],
            )),
            RegionId(0),
            region,
            vec![AspectId("error-paths".to_owned())],
            0,
        )
        .await;
        assert_eq!(idx, 0);
        assert!(
            outcome.is_ok(),
            "the half-span attempts recover the stalled region: {outcome:?}"
        );
        assert_eq!(
            stats.attempts, 3,
            "one full-span stall plus two half-span attempts, all recorded: {stats:?}"
        );
        assert!(stats.judged);
    }

    #[tokio::test]
    async fn a_stall_that_survives_the_half_span_requeue_fails_loudly() {
        let region = Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 10,
            kind: RegionKind::Added,
        };
        let job = RegionJob {
            spec: spec(),
            cfg: JudgesConfig {
                retry_backoff_secs: 0,
                region_retries: 0,
                ..JudgesConfig::default()
            },
            gateway: Arc::new(FakeGateway),
            state: state(),
            aspects: Arc::new(AspectRegistry::new()),
            diff_text: Arc::from(""),
            deadline: far_deadline(),
            lanes: Arc::new(Semaphore::new(1)),
            cooldown: Arc::new(ratelimit::CooldownGate::new(0.0, Duration::ZERO)),
            gate_wait: Arc::new(AtomicU64::new(0)),
            provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
            untracked: Arc::new(Vec::new()),
            served: test_served(),
        };
        let stall = "Compaction made no progress at 148899 tokens: the configured compactor cannot shrink this conversation further".to_owned();
        let (idx, outcome, stats) = judge_region(
            job,
            Arc::new(FlakyStreamClient::new(
                MockApiClient::new("mock"),
                vec![stall; 24],
            )),
            RegionId(0),
            region,
            vec![AspectId("error-paths".to_owned())],
            0,
        )
        .await;
        assert_eq!(idx, 0);
        assert!(
            outcome
                .as_ref()
                .is_err_and(|cause| cause.contains("half-span re-queue still failed")),
            "the exhausted requeue names its shape: {outcome:?}"
        );
        assert_eq!(
            stats.attempts, 3,
            "the full span plus both halves each spent exactly one attempt: {stats:?}"
        );
        assert!(!stats.judged);
    }

    #[tokio::test]
    async fn a_transient_stream_error_is_absorbed_by_the_engine_ladder() {
        let plan_coverage = plan(1, &["error-paths"]);
        let region = plan_coverage.regions.first().cloned().unwrap_or(Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 2,
            kind: RegionKind::Added,
        });
        let job = RegionJob {
            spec: spec(),
            cfg: JudgesConfig {
                retry_backoff_secs: 0,
                region_retries: 0,
                ..JudgesConfig::default()
            },
            gateway: Arc::new(FakeGateway),
            state: state(),
            aspects: Arc::new(AspectRegistry::new()),
            diff_text: Arc::from(""),
            deadline: far_deadline(),
            lanes: Arc::new(Semaphore::new(1)),
            cooldown: Arc::new(ratelimit::CooldownGate::new(0.0, Duration::ZERO)),
            gate_wait: Arc::new(AtomicU64::new(0)),
            provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
            untracked: Arc::new(Vec::new()),
            served: test_served(),
        };
        let (idx, outcome, stats) = judge_region(
            job,
            Arc::new(FlakyStreamClient::new(
                MockApiClient::new("mock")
                    .with_responses(vec![json_response(&clean_report_for(0))]),
                vec!["API error: stream failed: rate limit, 0 events processed".to_owned()],
            )),
            RegionId(0),
            region,
            vec![AspectId("error-paths".to_owned())],
            0,
        )
        .await;
        assert_eq!(idx, 0);
        assert!(
            outcome.is_ok(),
            "one transient error never reaches the region ladder: {outcome:?}"
        );
        assert_eq!(
            stats.attempts, 1,
            "the engine absorbed the blip, so the region spent exactly one attempt: {stats:?}"
        );
    }

    #[tokio::test]
    async fn a_compaction_stall_spends_no_retry() {
        let plan_coverage = plan(1, &["error-paths"]);
        let region = plan_coverage.regions.first().cloned().unwrap_or(Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 2,
            kind: RegionKind::Added,
        });
        let job = RegionJob {
            spec: spec(),
            cfg: JudgesConfig {
                retry_backoff_secs: 0,
                region_retries: 3,
                ..JudgesConfig::default()
            },
            gateway: Arc::new(FakeGateway),
            state: state(),
            aspects: Arc::new(AspectRegistry::new()),
            diff_text: Arc::from(""),
            deadline: far_deadline(),
            lanes: Arc::new(Semaphore::new(1)),
            cooldown: Arc::new(ratelimit::CooldownGate::new(0.0, Duration::ZERO)),
            gate_wait: Arc::new(AtomicU64::new(0)),
            provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
            untracked: Arc::new(Vec::new()),
            served: test_served(),
        };
        let (idx, outcome, stats) = judge_region(
            job,
            Arc::new(FlakyStreamClient::new(
                MockApiClient::new("mock"),
                vec![
                    "Compaction made no progress at 148899 tokens: the configured compactor cannot shrink this conversation further".to_owned();
                    8
                ],
            )),
            RegionId(0),
            region,
            vec![AspectId("error-paths".to_owned())],
            0,
        )
        .await;
        assert_eq!(idx, 0);
        assert!(
            outcome
                .as_ref()
                .is_err_and(|cause| cause.contains("made no progress")),
            "the stall itself is the recorded cause once the engine ladder is exhausted: {outcome:?}"
        );
        assert!(
            !stats.judged && stats.attempts == 1,
            "a deterministic stall spends exactly one attempt even with three retries configured: {stats:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_attempt_still_records_its_run_seconds() {
        let plan_coverage = plan(1, &["error-paths"]);
        let region = plan_coverage.regions.first().cloned().unwrap_or(Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 2,
            kind: RegionKind::Added,
        });
        let job = RegionJob {
            spec: spec(),
            cfg: JudgesConfig {
                retry_backoff_secs: 0,
                region_retries: 0,
                ..JudgesConfig::default()
            },
            gateway: Arc::new(FakeGateway),
            state: state(),
            aspects: Arc::new(AspectRegistry::new()),
            diff_text: Arc::from(""),
            deadline: far_deadline(),
            lanes: Arc::new(Semaphore::new(1)),
            cooldown: Arc::new(ratelimit::CooldownGate::new(0.0, Duration::ZERO)),
            gate_wait: Arc::new(AtomicU64::new(0)),
            provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
            untracked: Arc::new(Vec::new()),
            served: test_served(),
        };
        let (idx, outcome, stats) = judge_region(
            job,
            Arc::new(FlakyStreamClient::new(
                MockApiClient::new("mock"),
                vec!["API error: stream failed: rate limit".to_owned(); 8],
            )),
            RegionId(0),
            region,
            vec![AspectId("error-paths".to_owned())],
            0,
        )
        .await;
        assert_eq!(idx, 0);
        assert!(
            outcome
                .as_ref()
                .is_err_and(|cause| cause.contains("did not answer")),
            "the failure is the recorded cause: {outcome:?}"
        );
        assert_eq!(stats.attempts, 1, "zero retries spend one attempt");
        assert_eq!(
            stats.run_secs.len(),
            1,
            "the failed run's wall seconds still count in the stats: {stats:?}"
        );
    }

    #[tokio::test]
    async fn a_rate_limited_region_trips_the_provider_cooldown_and_retries() -> Result<(), DtError>
    {
        let gate = Arc::new(ratelimit::CooldownGate::new(0.0, Duration::from_secs(0)));
        let lanes_pool = Arc::new(ratelimit::Lanes::new(64, 8));
        let client = MockApiClient::new("mock").with_responses(vec![
            json_response(&clean_report_for(0)),
            json_response("prose, not a report"),
            json_response("still prose"),
            json_response("API error: stream failed: rate limit, 0 events processed"),
            json_response(&clean_report_for(1)),
        ]);
        let report = run_judge_with(
            SweepJob {
                spec: spec(),
                cfg: JudgesConfig {
                    retry_backoff_secs: 0,
                    ..JudgesConfig::default()
                },
                plan: plan(2, &["error-paths"]),
                gateway: Arc::new(FakeGateway),
                state: state(),
                aspects: Arc::new(AspectRegistry::new()),
                deadline: far_deadline(),
                lanes: Arc::new(Semaphore::new(1)),
                cooldown: Arc::clone(&gate),
                provider_lanes: lanes_pool,
                untracked: Arc::new(Vec::new()),
                served: test_served(),
            },
            Arc::new(client),
        )
        .await?;
        assert!(
            report.failed.is_empty(),
            "the region settles once the cooldown clears: {:?}",
            report.failed
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_region_uses_its_configured_retry_count() -> Result<(), DtError> {
        let report = drive_cfg(
            MockApiClient::new("mock").with_responses(vec![
                json_response(&clean_report_for(0)),
                json_response("prose, not a report"),
                json_response("still prose"),
                json_response("prose again"),
                json_response("and again, no JSON"),
                json_response("third try, still prose"),
                json_response("nope"),
            ]),
            &plan(2, &["error-paths"]),
            far_deadline(),
            JudgesConfig {
                retry_backoff_secs: 0,
                region_retries: 2,
                ..JudgesConfig::default()
            },
        )
        .await?;
        assert_eq!(report.failed, vec![RegionId(1)], "three attempts exhaust");
        assert_eq!(
            report
                .stats
                .iter()
                .find(|entry| entry.region.0 == 1)
                .map(|entry| entry.attempts),
            Some(3),
            "attempts record the configured retries: {:?}",
            report.stats
        );
        Ok(())
    }

    #[tokio::test]
    async fn zero_retries_fail_a_region_fast() -> Result<(), DtError> {
        let report = drive_cfg(
            MockApiClient::new("mock").with_responses(vec![
                json_response(&clean_report_for(0)),
                json_response("prose, not a report"),
                json_response("still prose"),
            ]),
            &plan(2, &["error-paths"]),
            far_deadline(),
            JudgesConfig {
                retry_backoff_secs: 0,
                region_retries: 0,
                ..JudgesConfig::default()
            },
        )
        .await?;
        assert_eq!(report.failed, vec![RegionId(1)], "no second attempt");
        assert_eq!(
            report
                .stats
                .iter()
                .find(|entry| entry.region.0 == 1)
                .map(|entry| entry.attempts),
            Some(1),
            "the failed region records exactly one attempt: {:?}",
            report.stats
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_judge_whose_every_region_fails_is_absent() {
        let outcome = drive(
            MockApiClient::new("mock").with_responses(vec![
                json_response("prose, not a report"),
                json_response("still prose"),
            ]),
            &plan(1, &["error-paths"]),
        )
        .await;
        assert!(
            matches!(&outcome, Err(DtError::JudgeAbsent { judge, .. }) if judge == "alpha"),
            "a judge that answers nothing stays absent: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_sweep_past_its_wall_budget_fails_every_region_without_a_call() {
        let outcome = drive_until(
            MockApiClient::new("mock").with_responses(vec![]),
            &plan(2, &["error-paths"]),
            std::time::Instant::now(),
        )
        .await;
        assert!(
            matches!(&outcome, Err(DtError::JudgeAbsent { cause, .. }) if cause.contains("wall budget")),
            "the sweep stops at the wall budget and never spends a call past it: {outcome:?}"
        );
    }

    #[test]
    fn a_schema_violating_adjacent_preexisting_claim_rejects() {
        let wire = FindingWire {
            title: "The guard is dropped too early".to_owned(),
            mechanism: "src/a.rs:2 drops the guard".to_owned(),
            claim: "adjacent-preexisting".to_owned(),
            severity: "warning".to_owned(),
            evidence: vec!["src/a.rs:2".to_owned()],
            fix_hint: None,
        };
        let outcome = build_finding(RegionId(0), "error-paths", wire);
        let cause = outcome.err().unwrap_or_default();
        assert!(
            cause.contains("adjacent-preexisting"),
            "the parser offers only the schema's claim set; reconcile owns the relabel: {cause}"
        );
    }

    #[test]
    fn a_colon_less_line_attempt_counts_as_discarded() -> Result<(), String> {
        let wire = FindingWire {
            title: "The guard is dropped too early".to_owned(),
            mechanism: "the lock falls out of scope before the write".to_owned(),
            claim: "in-diff".to_owned(),
            severity: "warning".to_owned(),
            evidence: vec!["line 42".to_owned()],
            fix_hint: None,
        };
        let finding = build_finding(RegionId(0), "error-paths", wire)?;
        assert!(
            finding.evidence.is_empty(),
            "a line-only string never becomes a cite"
        );
        assert_eq!(
            finding.discarded_cites, 1,
            "a digit-bearing colon-less string is a failed cite attempt, so the drop lands in the unparseable bucket"
        );
        Ok(())
    }

    #[test]
    fn a_windows_cite_parses_at_the_last_colon_and_unparseable_candidates_are_counted()
    -> Result<(), Box<dyn std::error::Error>> {
        let wire = FindingWire {
            title: "The guard is dropped too early".to_owned(),
            mechanism: "the lock falls out of scope before the write".to_owned(),
            claim: "in-diff".to_owned(),
            severity: "warning".to_owned(),
            evidence: vec![
                "C:\\repo\\src\\guard.rs:12".to_owned(),
                "Cargo.toml line 14 (head): loopctl has no rev".to_owned(),
                "plain prose without a colon".to_owned(),
            ],
            fix_hint: None,
        };
        let finding = build_finding(RegionId(0), "error-paths", wire)?;
        assert_eq!(
            finding.evidence.len(),
            1,
            "the drive-letter path is a cite, split at the LAST colon"
        );
        assert_eq!(finding.evidence.first().map(|cite| cite.line), Some(12));
        assert_eq!(
            finding.discarded_cites, 1,
            "the colon-bearing candidate that will not parse is counted, bare prose is not"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_cooling_host_delays_a_regions_first_attempt() -> Result<(), DtError> {
        let gate = Arc::new(ratelimit::CooldownGate::new(0.0, Duration::from_secs(30)));
        gate.trip(&provider_of("mock"), Some(Duration::from_secs(1)));
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&clean_report_for(0))]);
        let started = std::time::Instant::now();
        let report = run_judge_with(
            SweepJob {
                spec: spec(),
                cfg: JudgesConfig {
                    retry_backoff_secs: 0,
                    ..JudgesConfig::default()
                },
                plan: plan(1, &["error-paths"]),
                gateway: Arc::new(FakeGateway),
                state: state(),
                aspects: Arc::new(AspectRegistry::new()),
                deadline: far_deadline(),
                lanes: Arc::new(Semaphore::new(1)),
                cooldown: Arc::clone(&gate),
                provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
                untracked: Arc::new(Vec::new()),
                served: test_served(),
            },
            Arc::new(client),
        )
        .await?;
        let waited = started.elapsed();
        assert!(report.failed.is_empty());
        assert!(
            waited >= Duration::from_millis(900),
            "the first attempt waited out the host cooldown, not just a retry: {waited:?}"
        );
        Ok(())
    }

    #[test]
    fn the_retry_after_hint_reads_the_providers_directive() {
        assert_eq!(
            retry_after_hint("Rate limit exceeded (retry after Some(12s)): HTTP 429: slow"),
            Some(Duration::from_secs(12)),
            "loopctl's exact rendering carries the directive"
        );
        assert_eq!(
            retry_after_hint("rate limit exceeded (retry after 60s)"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            retry_after_hint(
                "API error: stream failed: rate limit (retry after 60s), 3 events processed"
            ),
            Some(Duration::from_secs(60)),
            "the stream layer's composite render carries the directive too"
        );
        assert_eq!(
            retry_after_hint("retry after 1m 30s more"),
            Some(Duration::from_secs(90)),
            "Duration's multi-token debug form parses whole"
        );
        assert_eq!(
            retry_after_hint("retry after 500ms"),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            retry_after_hint("retry after 1.5s"),
            Some(Duration::from_millis(1500)),
            "Duration's fractional debug form parses in integer math"
        );
        assert_eq!(
            retry_after_hint("Rate limit exceeded (retry after None): slow"),
            None,
            "an absent directive falls back to the gate's own backoff"
        );
        assert_eq!(
            retry_after_hint("please retry after a moment"),
            None,
            "prose without a duration carries no hint"
        );
        assert_eq!(retry_after_hint("stream failed mid-body"), None);
    }

    #[tokio::test]
    async fn a_rate_limited_region_waits_out_the_gate_even_with_backoff_disabled()
    -> Result<(), DtError> {
        let gate = Arc::new(ratelimit::CooldownGate::new(
            0.0,
            Duration::from_millis(150),
        ));
        // A deterministic floor: the header-style interval skips jitter
        // (scale = 1.0 at jitter 0); the rate-limit trip may only extend it.
        let client = MockApiClient::new("mock")
            .with_responses(vec![json_response(&clean_report_for(0))])
            .with_errors(vec![Some(
                "API error: stream failed: rate limit, 0 events processed".to_owned(),
            )]);
        gate.trip(&provider_of("mock"), Some(Duration::from_secs(1)));
        let started = std::time::Instant::now();
        let report = run_judge_with(
            SweepJob {
                spec: spec(),
                cfg: JudgesConfig {
                    retry_backoff_secs: 0,
                    region_retries: 2,
                    ..JudgesConfig::default()
                },
                plan: plan(1, &["error-paths"]),
                gateway: Arc::new(FakeGateway),
                state: state(),
                aspects: Arc::new(AspectRegistry::new()),
                deadline: far_deadline(),
                lanes: Arc::new(Semaphore::new(1)),
                cooldown: Arc::clone(&gate),
                provider_lanes: Arc::new(ratelimit::Lanes::new(64, 8)),
                untracked: Arc::new(Vec::new()),
                served: test_served(),
            },
            Arc::new(client),
        )
        .await?;
        let waited = started.elapsed();
        assert!(
            report.failed.is_empty(),
            "the region settles after the gate: {:?}",
            report.failed
        );
        assert!(
            waited >= Duration::from_millis(950),
            "the gate is honored even with retry_backoff_secs = 0 — the sweep slept only {waited:?}"
        );
        Ok(())
    }
}
