//! One review round: stage-0 tools, the concurrent judge panel, oracle
//! verification of needs-execution claims, and reconciliation into the
//! verdict — with the state persisting only after the report sinks.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use loopctl::api::ApiClient;
use tokio::sync::Semaphore;

use crate::ratelimit::{CooldownGate, Lanes};
use tokio::task::JoinSet;

use crate::aspects::AspectRegistry;
use crate::config::{DifftraceConfig, OraclePolicy};
use crate::contracts::{
    CellVerdict, CheckRun, CiteAuthority, ClaimType, CoveragePlan, DiffSource, Finding,
    JudgeReport, RegionKind, RoundState, RoundVerdict, RunOutcome, ToolEvidence, ToolFinding,
};
use crate::diff::DiffIndex;
use crate::error::{DtError, JudgeId};
use crate::gateway::PrGateway;
use crate::judges::JUDGE_QUORUM;
use crate::judges::pass::{self, JudgeSpec};
use crate::oracle;
use crate::reconcile::{judge_profile, reconcile};
use crate::state::{self, file};
use crate::tools::crates::WorkspaceMap;
use crate::tools::differential;
use crate::tools::rust::{Executor, ProcessExecutor, builtin_adapters};
use crate::tools::{ToolAdapter, external_adapters, normalize_output};
use crate::tools::{cicoverage, docrefs, testcontext};

#[derive(Debug, Clone, Copy)]
pub struct RunBudget {
    pub total: Duration,
    pub tools: Duration,
    pub oracle: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    Local,
    CheckRuns {
        head: Vec<CheckRun>,
        base: Vec<CheckRun>,
    },
}

pub struct RunInputs {
    pub gateway: Arc<dyn PrGateway>,
    pub repo_root: PathBuf,
    pub checkout: String,
    pub judges: Vec<JudgeSpec>,
    pub plan: CoveragePlan,
    pub budget: RunBudget,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state: Option<RoundState>,
    pub tool_source: ToolSource,
    pub dry_run: bool,
    pub config: DifftraceConfig,
}

struct Stage0 {
    registry: Arc<AspectRegistry>,
    tools: ToolEvidence,
    loaded: RoundState,
}

// The tighter of the tools budget and the run wall; neither set means
// no ceiling.
#[must_use]
fn tools_deadline(now: Instant, tools: Duration, wall: Option<Instant>) -> Option<Instant> {
    let own = (tools.as_secs() > 0)
        .then(|| now.checked_add(tools))
        .flatten();
    match (own, wall) {
        (Some(own), Some(wall)) => Some(own.min(wall)),
        (Some(own), None) => Some(own),
        (None, wall) => wall,
    }
}

fn wall_budget(total: Duration) -> Duration {
    if total.is_zero() {
        Duration::from_hours(365 * 24)
    } else {
        total
    }
}

pub async fn run_review(
    inputs: RunInputs,
) -> Result<((RoundVerdict, RunOutcome), Option<RoundState>), DtError> {
    let executor = ProcessExecutor;
    let started = Instant::now();
    let deadline = started
        .checked_add(wall_budget(inputs.budget.total))
        .unwrap_or(started);
    let stage = prepare(&inputs, &executor, deadline)?;
    let lanes = Arc::new(Semaphore::new(inputs.config.judges.region_concurrency));
    let cooldown = Arc::new(CooldownGate::new(
        inputs.config.ratelimit.jitter,
        Duration::from_secs(inputs.config.ratelimit.backoff_base_secs),
    ));
    let provider_lanes = Arc::new(Lanes::new(
        inputs.config.ratelimit.per_provider_lanes,
        inputs.config.ratelimit.global_lanes,
    ));
    let untracked = Arc::new(untracked_or_default(&inputs.gateway));
    let served = served_reads();
    let mut panel: JoinSet<Result<JudgeReport, DtError>> = JoinSet::new();
    for spec in inputs.judges.clone() {
        if Instant::now() >= deadline {
            tracing::warn!(
                target: "difftrace::orchestrator",
                judge = %spec.id,
                "budget exhausted before the judge could run"
            );
            continue;
        }
        panel.spawn(pass::run_judge(pass::SweepJob {
            spec,
            cfg: inputs.config.judges.clone(),
            plan: inputs.plan.clone(),
            gateway: Arc::clone(&inputs.gateway),
            state: stage.loaded.clone(),
            aspects: Arc::clone(&stage.registry),
            deadline,
            lanes: Arc::clone(&lanes),
            cooldown: Arc::clone(&cooldown),
            provider_lanes: Arc::clone(&provider_lanes),
            untracked: Arc::clone(&untracked),
            served: Arc::clone(&served),
        }));
    }
    let mut reports = collect_panel(panel).await;
    if let Some(job) = escalation_job(
        &reports,
        &inputs,
        &stage,
        deadline,
        Arc::clone(&cooldown),
        Arc::clone(&provider_lanes),
        Arc::clone(&untracked),
        Arc::clone(&served),
    ) {
        match pass::run_judge(job).await {
            Ok(report) => reports.push(report),
            Err(err) => tracing::warn!(
                target: "difftrace::orchestrator",
                cause = %err,
                "escalation judge absent; the panel verdict stands on its own"
            ),
        }
    }
    conclude(
        inputs,
        stage,
        &reports,
        &executor,
        deadline,
        &served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

pub async fn run_review_with<C: ApiClient + 'static>(
    inputs: RunInputs,
    client: Arc<C>,
    executor: &dyn Executor,
) -> Result<((RoundVerdict, RunOutcome), Option<RoundState>), DtError> {
    let started = Instant::now();
    let deadline = started
        .checked_add(wall_budget(inputs.budget.total))
        .unwrap_or(started);
    let stage = prepare(&inputs, executor, deadline)?;
    let lanes = Arc::new(Semaphore::new(inputs.config.judges.region_concurrency));
    let cooldown = Arc::new(CooldownGate::new(
        inputs.config.ratelimit.jitter,
        Duration::from_secs(inputs.config.ratelimit.backoff_base_secs),
    ));
    let provider_lanes = Arc::new(Lanes::new(
        inputs.config.ratelimit.per_provider_lanes,
        inputs.config.ratelimit.global_lanes,
    ));
    let untracked = Arc::new(untracked_or_default(&inputs.gateway));
    let served = served_reads();
    let mut panel: JoinSet<Result<JudgeReport, DtError>> = JoinSet::new();
    for spec in inputs.judges.clone() {
        if Instant::now() >= deadline {
            tracing::warn!(
                target: "difftrace::orchestrator",
                judge = %spec.id,
                "budget exhausted before the judge could run"
            );
            continue;
        }
        panel.spawn(pass::run_judge_with(
            pass::SweepJob {
                spec,
                cfg: inputs.config.judges.clone(),
                plan: inputs.plan.clone(),
                gateway: Arc::clone(&inputs.gateway),
                state: stage.loaded.clone(),
                aspects: Arc::clone(&stage.registry),
                deadline,
                lanes: Arc::clone(&lanes),
                cooldown: Arc::clone(&cooldown),
                provider_lanes: Arc::clone(&provider_lanes),
                untracked: Arc::clone(&untracked),
                served: Arc::clone(&served),
            },
            Arc::clone(&client),
        ));
    }
    let mut reports = collect_panel(panel).await;
    if let Some(job) = escalation_job(
        &reports,
        &inputs,
        &stage,
        deadline,
        Arc::clone(&cooldown),
        Arc::clone(&provider_lanes),
        Arc::clone(&untracked),
        Arc::clone(&served),
    ) {
        match pass::run_judge_with(job, Arc::clone(&client)).await {
            Ok(report) => reports.push(report),
            Err(err) => tracing::warn!(
                target: "difftrace::orchestrator",
                cause = %err,
                "escalation judge absent; the panel verdict stands on its own"
            ),
        }
    }
    conclude(
        inputs,
        stage,
        &reports,
        executor,
        deadline,
        &served
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

fn served_reads() -> Arc<std::sync::Mutex<crate::contracts::ServedReads>> {
    Arc::new(std::sync::Mutex::new(crate::contracts::ServedReads::new()))
}

fn untracked_or_default(gateway: &Arc<dyn PrGateway>) -> Vec<String> {
    match gateway.untracked_files() {
        Ok(files) => files,
        Err(err) => {
            tracing::warn!(
                target: "difftrace::orchestrator",
                cause = %err,
                "the untracked-file listing failed; judges lose the not-tracked materials and doc-refs stays silent"
            );
            Vec::new()
        }
    }
}

fn escalation_job(
    reports: &[JudgeReport],
    inputs: &RunInputs,
    stage: &Stage0,
    deadline: Instant,
    cooldown: Arc<CooldownGate>,
    provider_lanes: Arc<Lanes>,
    untracked: Arc<Vec<String>>,
    served: Arc<std::sync::Mutex<crate::contracts::ServedReads>>,
) -> Option<pass::SweepJob> {
    let profile = inputs.config.judges.escalate_profile.clone()?;
    if Instant::now() >= deadline {
        tracing::warn!(
            target: "difftrace::orchestrator",
            "the wall budget rules out an escalation pass"
        );
        return None;
    }
    let sub_plan = sub_quorum_defect_plan(reports, &inputs.plan)?;
    let regions = sub_plan.regions;
    let cells: Vec<_> = sub_plan
        .cells
        .into_iter()
        .filter(|(region_id, _)| {
            regions
                .get(region_id.0)
                .is_some_and(|region| region.kind != RegionKind::Repo)
        })
        .collect();
    if cells.is_empty() {
        return None;
    }
    let sub_plan = CoveragePlan {
        range: sub_plan.range,
        regions,
        cells,
        narrowed: sub_plan.narrowed,
    };
    let id = format!("{profile}#esc");
    let spec = pass::JudgeSpec {
        id,
        profile,
        max_tool_calls_per_region: inputs.config.judges.tool_calls_per_region,
    };
    let regions = sub_plan
        .cells
        .iter()
        .map(|(region, _)| region.0)
        .collect::<std::collections::BTreeSet<usize>>()
        .len();
    tracing::info!(
        target: "difftrace::orchestrator",
        judge = %spec.id,
        regions,
        "escalation sweep started for sub-quorum defects"
    );
    Some(pass::SweepJob {
        spec,
        cfg: inputs.config.judges.clone(),
        plan: sub_plan,
        gateway: Arc::clone(&inputs.gateway),
        state: stage.loaded.clone(),
        aspects: Arc::clone(&stage.registry),
        deadline,
        lanes: Arc::new(Semaphore::new(inputs.config.judges.region_concurrency)),
        cooldown,
        provider_lanes,
        untracked,
        served,
    })
}

fn sub_quorum_defect_plan(reports: &[JudgeReport], full: &CoveragePlan) -> Option<CoveragePlan> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut defect_cells: BTreeMap<
        usize,
        (BTreeSet<crate::contracts::AspectId>, BTreeSet<JudgeId>),
    > = BTreeMap::new();
    for report in reports {
        for region in &report.regions {
            for (aspect, verdict) in &region.cells {
                if let CellVerdict::Defect(_) = verdict {
                    let entry = defect_cells.entry(region.region.0).or_default();
                    entry.0.insert(aspect.clone());
                    entry.1.insert(report.judge.clone());
                }
            }
        }
    }
    let mut cells = Vec::new();

    for (region_idx, (aspects, judges)) in defect_cells {
        let profiles = judges
            .iter()
            .map(|id| judge_profile(id))
            .collect::<BTreeSet<String>>();
        if !judges.is_empty() && profiles.len() < JUDGE_QUORUM {
            for aspect in aspects {
                cells.push((crate::contracts::RegionId(region_idx), aspect));
            }
        }
    }
    if cells.is_empty() {
        return None;
    }
    Some(CoveragePlan {
        range: full.range.clone(),
        regions: full.regions.clone(),
        cells,
        narrowed: false,
    })
}

async fn collect_panel(mut panel: JoinSet<Result<JudgeReport, DtError>>) -> Vec<JudgeReport> {
    let mut reports: Vec<JudgeReport> = Vec::new();
    while let Some(joined) = panel.join_next().await {
        match joined {
            Ok(Ok(report)) => reports.push(report),
            Ok(Err(err)) => tracing::warn!(
                target: "difftrace::orchestrator",
                cause = %err,
                "judge absent"
            ),
            Err(err) => tracing::warn!(
                target: "difftrace::orchestrator",
                cause = %err,
                "judge task failed"
            ),
        }
    }
    reports
}

fn prepare(
    inputs: &RunInputs,
    executor: &dyn Executor,
    deadline: Instant,
) -> Result<Stage0, DtError> {
    let registry = Arc::new(AspectRegistry::from_config(&inputs.config.aspects)?);
    let mut tools = run_s0(inputs, executor, deadline)?;
    tools.findings.extend(docrefs::check(
        &inputs.plan.regions,
        inputs.gateway.as_ref(),
    ));
    tools.findings.extend(cicoverage::check(
        &inputs.plan.regions,
        inputs.gateway.as_ref(),
    ));
    tools.findings = testcontext::demote_test_module_hits(tools.findings, |file| {
        inputs
            .gateway
            .read_file_at_head(file)
            .ok()
            .map(|snapshot| snapshot.content)
    });
    let loaded = if let Some(state) = &inputs.state {
        state.clone()
    } else {
        let path = file::state_path(&inputs.state_dir, &inputs.checkout);
        file::load(&path, &inputs.checkout)?
    };
    Ok(Stage0 {
        registry,
        tools,
        loaded,
    })
}

fn run_s0(
    inputs: &RunInputs,
    executor: &dyn Executor,
    deadline: Instant,
) -> Result<ToolEvidence, DtError> {
    if let ToolSource::CheckRuns { head, base } = &inputs.tool_source {
        return Ok(check_run_evidence(head, base));
    }
    let mut adapters: Vec<Box<dyn ToolAdapter>> = Vec::new();
    let mut unavailable: Vec<String> = Vec::new();
    match WorkspaceMap::load(&inputs.repo_root) {
        Ok(map) => adapters.extend(
            builtin_adapters(
                &map,
                &inputs.config.tools.enabled,
                inputs.budget.tools.as_secs(),
            )?
            .into_iter()
            .map(|adapter| Box::new(adapter) as Box<dyn ToolAdapter>),
        ),
        Err(err) => {
            tracing::warn!(
                target: "difftrace::orchestrator",
                cause = %err,
                "no workspace map: built-in tools skipped"
            );
            unavailable.push("built-in tools skipped: the workspace map is unreadable".to_owned());
        }
    }
    adapters.extend(
        external_adapters(&inputs.config.tools)?
            .into_iter()
            .map(|adapter| Box::new(adapter) as Box<dyn ToolAdapter>),
    );
    adapters.retain(|adapter| adapter.should_run(&inputs.plan.regions));
    if adapters.is_empty() {
        return Ok(ToolEvidence {
            findings: Vec::new(),
            unavailable,
        });
    }
    let mut targets: Vec<PathBuf> = Vec::new();
    for region in &inputs.plan.regions {
        let path = PathBuf::from(&region.file);
        if !targets.contains(&path) {
            targets.push(path);
        }
    }
    let refs: Vec<&dyn ToolAdapter> = adapters.iter().map(|boxed| &**boxed).collect();
    let tools_deadline = tools_deadline(
        Instant::now(),
        inputs.budget.tools,
        (inputs.budget.total > Duration::ZERO).then_some(deadline),
    );
    let outcome = differential::run_all_bounded(
        &refs,
        &targets,
        &inputs.plan.range,
        &inputs.repo_root,
        &inputs.cache_dir,
        executor,
        tools_deadline,
    )?;
    for reason in &outcome.unavailable {
        tracing::warn!(target: "difftrace::orchestrator", "{reason}");
    }
    unavailable.extend(outcome.unavailable);
    Ok(ToolEvidence {
        findings: outcome.new.into_iter().chain(outcome.context).collect(),
        unavailable,
    })
}

pub(crate) fn check_run_evidence(head_runs: &[CheckRun], base_runs: &[CheckRun]) -> ToolEvidence {
    let head_findings: Vec<ToolFinding> = head_runs
        .iter()
        .flat_map(|run| run.findings.iter().cloned())
        .collect();
    let base_findings: Vec<ToolFinding> = base_runs
        .iter()
        .flat_map(|run| run.findings.iter().cloned())
        .collect();
    let (new, context) = differential::attribute(&head_findings, &base_findings);
    let unavailable: Vec<String> = head_runs
        .iter()
        .filter(|run| run.conclusion == "failure" && run.findings.is_empty())
        .map(|run| format!("check run {} failed without annotations", run.name))
        .collect();
    ToolEvidence {
        findings: normalize_output(new.into_iter().chain(context).collect()),
        unavailable,
    }
}

fn execute_cheap_claims(
    verdict: &mut RoundVerdict,
    repo_root: &std::path::Path,
    budget: Duration,
    deadline: Instant,
    max_claims: usize,
    executor: &dyn Executor,
) {
    let exec_claims: Vec<&Finding> = verdict
        .weak
        .iter()
        .map(|lead| &lead.finding)
        .chain(verdict.dropped.iter().map(|dropped| &dropped.finding))
        .collect();
    let exec_verdicts = crate::execclaims::execute_claims(
        &exec_claims,
        repo_root,
        budget.min(deadline.saturating_duration_since(Instant::now())),
        max_claims,
        executor,
    );
    for (id, exec) in exec_verdicts {
        verdict.lead_verdicts.entry(id).or_insert(exec);
    }
}

fn conclude(
    inputs: RunInputs,
    stage: Stage0,
    reports: &[JudgeReport],
    executor: &dyn Executor,
    deadline: Instant,
    served: &crate::contracts::ServedReads,
) -> Result<((RoundVerdict, RunOutcome), Option<RoundState>), DtError> {
    let RunInputs {
        gateway,
        repo_root,
        checkout: _checkout,
        judges,
        plan,
        budget,
        state_dir: _state_dir,
        cache_dir,
        state: _state,
        tool_source: _tool_source,
        dry_run,
        config,
    } = inputs;
    let Stage0 {
        registry,
        tools,
        loaded,
    } = stage;
    let mut panel: Vec<JudgeId> = Vec::new();
    for spec in &judges {
        if !panel.contains(&spec.id) {
            panel.push(spec.id.clone());
        }
    }
    let claims: Vec<&Finding> = reports
        .iter()
        .flat_map(|report| report.regions.iter())
        .flat_map(|region| region.cells.iter())
        .filter_map(|(_, verdict)| match verdict {
            CellVerdict::Defect(finding) => Some(finding),
            CellVerdict::Clean | CellVerdict::OutOfScope => None,
        })
        .filter(|finding| finding.claim == ClaimType::NeedsExecution)
        .collect();
    let policy = OraclePolicy::resolve(config.oracle.policy.as_deref(), &repo_root)?;
    let oracle_started = Instant::now();
    let oracle_results = oracle::verify(
        &claims,
        policy,
        &repo_root,
        &plan.range.head_sha,
        budget
            .oracle
            .min(deadline.saturating_duration_since(Instant::now())),
        config.oracle.per_round_max,
        executor,
        &cache_dir,
    );
    let DiffSource::Unified(diff_text) = gateway.diff();
    let index = DiffIndex::parse(&diff_text)
        .map_err(|cause| DtError::Git(format!("the pinned diff does not parse: {cause}")))?;
    let mut snapshot_windows: std::collections::BTreeMap<
        String,
        crate::judges::prompt::SnapshotWindow,
    > = std::collections::BTreeMap::new();
    for region in &plan.regions {
        if region.kind != RegionKind::File && region.kind != RegionKind::Repo {
            continue;
        }
        if snapshot_windows.contains_key(&region.file) {
            continue;
        }
        match gateway.read_file_at_head(&region.file) {
            Ok(snapshot) => {
                snapshot_windows.insert(
                    region.file.clone(),
                    crate::judges::prompt::snapshot_window(&snapshot.content),
                );
            }
            Err(err) => tracing::warn!(
                target: "difftrace::orchestrator",
                file = %region.file,
                cause = %err,
                "the head snapshot is unreadable; cites into this file keep only the diff's own lines as authority"
            ),
        }
    }
    let authority = CiteAuthority {
        index: &index,
        snapshots: &snapshot_windows,
        served,
    };
    let mut verdict = reconcile(
        reports,
        &plan,
        &loaded,
        &oracle_results,
        &registry,
        &authority,
        &panel,
        &tools,
    );
    execute_cheap_claims(
        &mut verdict,
        &repo_root,
        budget.oracle,
        deadline,
        config.oracle.per_round_max,
        executor,
    );
    let lead_claims: Vec<&Finding> = verdict
        .weak
        .iter()
        .map(|lead| &lead.finding)
        .chain(verdict.dropped.iter().map(|dropped| &dropped.finding))
        .filter(|finding| finding.claim == ClaimType::NeedsExecution)
        .collect();
    if !lead_claims.is_empty() && policy != OraclePolicy::Off {
        let spent = oracle_started.elapsed();
        let lead_results = oracle::verify(
            &lead_claims,
            policy,
            &repo_root,
            &plan.range.head_sha,
            budget
                .oracle
                .saturating_sub(spent)
                .min(deadline.saturating_duration_since(Instant::now())),
            config.oracle.per_round_max,
            executor,
            &cache_dir,
        );
        for (id, outcome) in lead_results.verdicts {
            verdict.lead_verdicts.insert(id.0, outcome);
        }
    }
    let outcome = verdict_outcome(&verdict);
    if dry_run {
        return Ok(((verdict, outcome), None));
    }
    let full = verdict.coverage.cells_judged == verdict.coverage.cells_total
        && verdict.absent_judges.is_empty()
        && verdict.incomplete.is_empty();
    let pending = if full {
        verdict.state.clone()
    } else {
        let mut partial = loaded;
        state::apply_partial_round(&mut partial, &verdict);
        verdict.state = partial.clone();
        partial
    };
    Ok(((verdict, outcome), Some(pending)))
}

fn verdict_outcome(verdict: &RoundVerdict) -> RunOutcome {
    let full = verdict.coverage.cells_judged == verdict.coverage.cells_total
        && verdict.absent_judges.is_empty()
        && verdict.incomplete.is_empty();
    if !full {
        RunOutcome::PartialRound
    } else if verdict.blocking_count() > 0 {
        RunOutcome::BlockingFindings
    } else {
        RunOutcome::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        AspectId, Cite, CoverageSummary, DiffSource, FileSnapshot, PinnedRange, Reconciled, Region,
        RegionId, RegionKind, SearchHit, Severity, Suppression,
    };
    use crate::state::apply_round;
    use crate::tools::CommandSpec;
    use crate::tools::rust::{ExecOutput, ToolRunError};
    use loopctl::testing::{MockApiClient, MockResponse};
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::path::Path;
    use std::pin::Pin;

    struct FakeGateway;

    impl PrGateway for FakeGateway {
        fn pinned_range(&self) -> PinnedRange {
            PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            }
        }
        fn diff(&self) -> DiffSource {
            DiffSource::Unified(
                [
                    "diff --git a/src/a.rs b/src/a.rs",
                    "new file mode 100644",
                    "--- /dev/null",
                    "+++ b/src/a.rs",
                    "@@ -0,0 +1,3 @@",
                    "+fn a() {}",
                    "+fn b() {}",
                    "+fn c() {}",
                ]
                .join("\n"),
            )
        }
        fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, DtError> {
            Ok(FileSnapshot {
                path: path.to_owned(),
                content: (1..=12)
                    .map(|n| format!("fn f{n}() {{}}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
            Ok(vec!["src/a.rs".to_owned()])
        }
        fn search_at_head(&self, _pattern: &str, _cap: usize) -> Result<Vec<SearchHit>, DtError> {
            Ok(Vec::new())
        }
    }

    fn annotation(tool: &str, file: &str, line: u32, message: &str) -> ToolFinding {
        ToolFinding {
            tool: tool.to_owned(),
            file: file.to_owned(),
            line,
            severity: crate::contracts::Severity::Warning,
            message: message.to_owned(),
        }
    }

    #[test]
    fn check_run_evidence_carries_both_sides_and_names_annotation_free_failures() {
        let head_runs = vec![
            CheckRun {
                name: "clippy".to_owned(),
                conclusion: "failure".to_owned(),
                summary: String::new(),
                findings: vec![
                    annotation("clippy", "src/a.rs", 10, "branch-new failure"),
                    annotation("clippy", "src/a.rs", 42, "pre-existing debt"),
                ],
            },
            CheckRun {
                name: "audit".to_owned(),
                conclusion: "failure".to_owned(),
                summary: String::new(),
                findings: Vec::new(),
            },
        ];
        let base_runs = vec![CheckRun {
            name: "clippy".to_owned(),
            conclusion: "success".to_owned(),
            summary: String::new(),
            findings: vec![annotation("clippy", "src/a.rs", 40, "pre-existing debt")],
        }];
        let evidence = check_run_evidence(&head_runs, &base_runs);
        assert_eq!(
            evidence.findings.len(),
            2,
            "both the new failure and the attributed context ride the evidence"
        );
        assert!(
            evidence
                .findings
                .iter()
                .any(|finding| finding.message == "branch-new failure"),
            "the head-only finding is present: {:?}",
            evidence.findings
        );
        assert_eq!(
            evidence.unavailable,
            vec!["check run audit failed without annotations".to_owned()],
            "an annotation-free failure is disclosed instead of vanishing"
        );
        let empty = check_run_evidence(&Vec::new(), &Vec::new());
        assert!(
            empty.findings.is_empty() && empty.unavailable.is_empty(),
            "empty check-run sets read as clean evidence"
        );
    }

    struct FakeExecutor {
        code: i32,
    }

    impl Executor for FakeExecutor {
        fn run(&self, _spec: &CommandSpec) -> Result<ExecOutput, ToolRunError> {
            Ok(ExecOutput {
                text: String::new(),
                code: Some(self.code),
            })
        }
    }

    fn spec(id: &str) -> JudgeSpec {
        JudgeSpec {
            id: id.to_owned(),
            profile: "mock".to_owned(),
            max_tool_calls_per_region: 10,
        }
    }

    fn plan(regions: usize) -> CoveragePlan {
        CoveragePlan {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            regions: (0..regions)
                .map(|_| Region {
                    file: "src/a.rs".to_owned(),
                    start: 1,
                    end: 3,
                    kind: RegionKind::Added,
                })
                .collect(),
            cells: (0..regions)
                .map(|idx| (RegionId(idx), AspectId("error-paths".to_owned())))
                .collect(),
            narrowed: false,
        }
    }

    fn plan_kind(kind: RegionKind, regions: usize) -> CoveragePlan {
        CoveragePlan {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            regions: (0..regions)
                .map(|_| Region {
                    file: "src/a.rs".to_owned(),
                    start: 1,
                    end: 3,
                    kind,
                })
                .collect(),
            cells: (0..regions)
                .map(|idx| (RegionId(idx), AspectId("error-paths".to_owned())))
                .collect(),
            narrowed: false,
        }
    }

    fn config() -> DifftraceConfig {
        let mut config = DifftraceConfig::default();
        config.tools.enabled = Vec::new();
        config.judges.region_concurrency = 1;
        config.judges.retry_backoff_secs = 0;
        config
    }

    fn inputs(judges: Vec<JudgeSpec>, config: DifftraceConfig) -> RunInputs {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "difftrace-orchestrator-{}-{unique}",
            std::process::id()
        ));
        let _unused = std::fs::remove_dir_all(&base);
        let state_dir = base.join("state");
        let cache_dir = base.join("cache");
        let _unused = std::fs::create_dir_all(&state_dir);
        let _unused = std::fs::create_dir_all(&cache_dir);
        RunInputs {
            gateway: Arc::new(FakeGateway),
            repo_root: base,
            checkout: "work".to_owned(),
            judges,
            plan: plan(1),
            budget: RunBudget {
                total: Duration::from_secs(600),
                tools: Duration::from_secs(10),
                oracle: Duration::from_secs(10),
            },
            state_dir,
            cache_dir,
            state: None,
            tool_source: ToolSource::Local,
            dry_run: false,
            config,
        }
    }

    fn git_fixture(name: &str) -> Result<(PathBuf, String), DtError> {
        let dir = std::env::temp_dir().join(format!(
            "difftrace-orchestrator-git-{name}-{}",
            std::process::id()
        ));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).map_err(|err| DtError::Git(err.to_string()))?;
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        for args in [vec!["init", "-q", "-b", "main"], vec!["add", "-A"]] {
            let output = std::process::Command::new("git")
                .current_dir(&dir)
                .args(&args)
                .output()
                .map_err(|err| DtError::Git(err.to_string()))?;
            if !output.status.success() {
                return Err(DtError::Git(format!(
                    "git {:?}: {}",
                    args,
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        let output = std::process::Command::new("git")
            .current_dir(&dir)
            .args([
                "-c",
                "user.email=difftrace@example.invalid",
                "-c",
                "user.name=difftrace",
                "commit",
                "-q",
                "-m",
                "base",
            ])
            .output()
            .map_err(|err| DtError::Git(err.to_string()))?;
        if !output.status.success() {
            return Err(DtError::Git(format!(
                "git commit: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let rev = std::process::Command::new("git")
            .current_dir(&dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|err| DtError::Git(err.to_string()))?;
        let sha = String::from_utf8(rev.stdout)
            .map_err(|err| DtError::Git(err.to_string()))?
            .trim()
            .to_owned();
        Ok((dir, sha))
    }

    fn json_response(text: &str) -> MockResponse {
        MockResponse {
            text: text.to_owned(),
            tool_call: None,
            stop_reason: "end_turn".to_owned(),
        }
    }

    fn clean_report() -> String {
        r#"{"region":0,"note":"traced the aspect","cells":[{"aspect":"error-paths","verdict":"clean"}]}"#
            .to_owned()
    }

    fn needs_execution_report() -> String {
        r#"{"region":0,"note":"traced the aspect","cells":[{"aspect":"error-paths","verdict":"defect","finding":{
            "title":"The crate does not compile",
            "mechanism":"src/a.rs:2 breaks the build for every caller",
            "claim":"needs-execution","severity":"warning","evidence":["src/a.rs:2"]}}]}"#
            .to_owned()
    }

    fn suppressed_report() -> String {
        r#"{"region":0,"note":"traced the aspect","cells":[{"aspect":"error-paths","verdict":"defect","finding":{
            "title":"Buffer grows unbounded",
            "mechanism":"src/a.rs:2 grows the buffer without bound",
            "claim":"in-diff","severity":"warning","evidence":["src/a.rs:2"]}}]}"#
            .to_owned()
    }

    fn clean_report_for(region: usize) -> String {
        format!(
            r#"{{"region":{region},"note":"traced the aspect","cells":[{{"aspect":"error-paths","verdict":"clean"}}]}}"#
        )
    }

    fn defect_report_for(region: usize) -> String {
        format!(
            r#"{{"region":{region},"note":"traced the aspect","cells":[{{"aspect":"error-paths","verdict":"defect","finding":{{
                "title":"The unwrap panics on empty input",
                "mechanism":"an empty file reaches the unwrap and the process dies at src/a.rs:2",
                "claim":"in-diff","severity":"warning","evidence":["src/a.rs:2"]}}}}]}}"#
        )
    }

    fn persist(state_dir: &Path, checkout: &str, pending: Option<RoundState>) {
        if let Some(pending) = pending {
            file::save(&file::state_path(state_dir, checkout), &pending).unwrap_or(());
        }
    }

    fn saved_state(state_dir: &Path, checkout: &str) -> RoundState {
        file::load(&file::state_path(state_dir, checkout), checkout).unwrap_or(RoundState {
            checkout: checkout.to_owned(),
            round: 0,
            issues: Vec::new(),
            suppressions: Vec::new(),
        })
    }

    fn reconciled(title: &str) -> Reconciled {
        Reconciled {
            finding: crate::contracts::Finding::new(crate::contracts::FindingDraft {
                region: RegionId(0),
                aspect: AspectId("error-paths".to_owned()),
                title: title.to_owned(),
                mechanism: "the bound includes the sentinel".to_owned(),
                claim: crate::contracts::ClaimType::InDiff,
                evidence: vec![Cite {
                    file: "src/a.rs".to_owned(),
                    line: 2,
                }],
                severity: Severity::Warning,
                fix_hint: None,
            }),
            votes: vec!["a".to_owned(), "b".to_owned()],
            families: Vec::new(),
            severity: Severity::Warning,
        }
    }

    fn round_verdict(confirmed: Vec<Reconciled>, judged: usize) -> RoundVerdict {
        RoundVerdict {
            weak: Vec::new(),
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            coverage: CoverageSummary {
                cells_total: 1,
                cells_judged: judged,
                cells_clean: 0,
                cells_defect: judged,
                judged_by_judge: BTreeMap::new(),
                regions: 1,
                per_file: BTreeMap::new(),
                cells_short: 0,
            },
            tool_findings: Vec::new(),
            confirmed,
            solo: Vec::new(),
            refuted: Vec::new(),
            suppressed: Vec::new(),
            dropped: Vec::new(),
            absent_judges: Vec::new(),
            incomplete: Vec::new(),
            tools_unavailable: Vec::new(),
            stats: Vec::new(),
            lead_verdicts: std::collections::BTreeMap::new(),
            state: RoundState {
                checkout: "work".to_owned(),
                round: 0,
                issues: Vec::new(),
                suppressions: Vec::new(),
            },
        }
    }

    fn seeded_round_one_issue(inputs: &RunInputs) -> Result<(), DtError> {
        let mut state = RoundState {
            checkout: inputs.checkout.clone(),
            round: 0,
            issues: Vec::new(),
            suppressions: Vec::new(),
        };
        apply_round(
            &mut state,
            &round_verdict(vec![reconciled("Off by one")], 1),
            true,
            false,
        );
        file::save(
            &file::state_path(&inputs.state_dir, &inputs.checkout),
            &state,
        )
    }

    fn seeded_suppression(inputs: &RunInputs) -> Result<(), DtError> {
        let state = RoundState {
            checkout: inputs.checkout.clone(),
            round: 4,
            issues: Vec::new(),
            suppressions: vec![Suppression {
                id: "sup-1".to_owned(),
                theme: "Buffer grows unbounded".to_owned(),
                file_glob: "src/a.rs".to_owned(),
                reason: "owner rejected the theme".to_owned(),
                created: "head".to_owned(),
                hits: 0,
            }],
        };
        file::save(
            &file::state_path(&inputs.state_dir, &inputs.checkout),
            &state,
        )
    }

    #[tokio::test]
    async fn a_full_round_returns_pending_state_and_persists_nothing_by_itself()
    -> Result<(), DtError> {
        let inputs = inputs(vec![spec("alpha")], config());
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&clean_report())]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::Clean);
        assert_eq!(
            verdict.state.round, 1,
            "the verdict carries the advanced state for the caller"
        );
        assert!(
            pending.as_ref().is_some_and(|state| state.round == 1),
            "a real round always returns advanced pending state: {pending:?}"
        );
        assert!(
            !file::state_path(&state_dir, &checkout).exists(),
            "the round persists nothing itself — the report sink gates the save"
        );
        persist(&state_dir, &checkout, pending);
        assert_eq!(
            saved_state(&state_dir, &checkout).round,
            1,
            "persisting the pending state after the sink is the contract"
        );
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[tokio::test]
    async fn e2e_clean_verdict_exits_zero_and_writes_state() -> Result<(), DtError> {
        let inputs = inputs(vec![spec("alpha")], config());
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&clean_report())]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::Clean);
        assert!(verdict.confirmed.is_empty() && verdict.solo.is_empty());
        persist(&state_dir, &checkout, pending);
        let state = saved_state(&state_dir, &checkout);
        assert_eq!(state.round, 1, "a full clean round advances the state");
        assert!(state.issues.is_empty());
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[tokio::test]
    async fn e2e_blocking_exits_one() -> Result<(), DtError> {
        let mut cfg = config();
        cfg.oracle.policy = Some("test".to_owned());
        let inputs = inputs(vec![spec("alpha")], cfg);
        std::fs::write(
            inputs.repo_root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )
        .map_err(|err| DtError::Config(err.to_string()))?;
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let client = MockApiClient::new("mock")
            .with_responses(vec![json_response(&needs_execution_report())]);
        let executor = FakeExecutor { code: 101 };
        let ((verdict, outcome), pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::BlockingFindings);
        assert!(
            verdict
                .confirmed
                .first()
                .is_some_and(|entry| entry.severity == Severity::Warning),
            "a failing oracle check confirms the single judge's claim"
        );
        persist(&state_dir, &checkout, pending);
        let state = saved_state(&state_dir, &checkout);
        assert!(
            state
                .issues
                .first()
                .is_some_and(|issue| issue.status == crate::contracts::IssueStatus::Open),
            "the blocking finding stands as an open issue"
        );
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[tokio::test]
    async fn a_partial_round_exits_three_and_does_not_retire() -> Result<(), DtError> {
        let inputs = inputs(vec![spec("alpha")], config());
        seeded_round_one_issue(&inputs)?;
        let before = saved_state(&inputs.state_dir, &inputs.checkout);
        assert_eq!(before.round, 1);
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let client = MockApiClient::new("mock");
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::PartialRound);
        assert_eq!(verdict.absent_judges.len(), 1, "the judge never answered");
        persist(&state_dir, &checkout, pending);
        let after = saved_state(&state_dir, &checkout);
        assert_eq!(after.round, 1, "a partial round does not advance the clock");
        let misses_before = before.issues.first().map_or(0, |issue| issue.misses);
        assert!(
            after
                .issues
                .first()
                .is_some_and(|issue| issue.misses == misses_before
                    && issue.status == crate::contracts::IssueStatus::Open),
            "misses stay frozen and nothing retires on an absent judge"
        );
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[tokio::test]
    async fn a_partial_round_still_counts_suppression_hits() -> Result<(), DtError> {
        let mut inputs = inputs(vec![spec("a"), spec("b")], config());
        inputs.plan = plan(2);
        seeded_suppression(&inputs)?;
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let client = MockApiClient::new("mock").with_responses(vec![
            json_response(&suppressed_report()),
            json_response(&clean_report_for(1)),
            json_response("I prefer to answer in prose."),
        ]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::PartialRound, "judge b never answered");
        assert_eq!(verdict.suppressed.len(), 1);
        persist(&state_dir, &checkout, pending);
        let after = saved_state(&state_dir, &checkout);
        assert_eq!(
            after.round, 4,
            "the partial round keeps the old round number"
        );
        assert!(
            after
                .suppressions
                .first()
                .is_some_and(|rule| rule.hits == 1),
            "a suppression that fired in a partial round still counts its hit"
        );
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[tokio::test]
    async fn two_runs_on_identical_inputs_produce_equivalent_verdicts() -> Result<(), DtError> {
        let executor = FakeExecutor { code: 0 };
        let first = inputs(vec![spec("alpha")], config());
        let cleanup_one = first.state_dir.parent().map(Path::to_path_buf);
        let (first_state, first_checkout) = (first.state_dir.clone(), first.checkout.clone());
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&clean_report())]);
        let ((one, outcome_one), pending_one) =
            run_review_with(first, Arc::new(client), &executor).await?;
        persist(&first_state, &first_checkout, pending_one);
        let second = inputs(vec![spec("alpha")], config());
        let cleanup_two = second.state_dir.parent().map(Path::to_path_buf);
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&clean_report())]);
        let ((two, outcome_two), _pending_two) =
            run_review_with(second, Arc::new(client), &executor).await?;
        assert_eq!(outcome_one, RunOutcome::Clean);
        assert_eq!(outcome_two, RunOutcome::Clean);
        assert!(
            crate::contracts::equivalent(&one, &two),
            "identical inputs must yield equivalent verdicts"
        );
        if let Some(dir) = cleanup_one {
            let _unused = std::fs::remove_dir_all(&dir);
        }
        if let Some(dir) = cleanup_two {
            let _unused = std::fs::remove_dir_all(&dir);
        }
        Ok(())
    }

    #[tokio::test]
    async fn an_escalation_judge_confirms_a_solo_defect() -> Result<(), DtError> {
        let mut shaped = config();
        shaped.judges.escalate_profile = Some("escalate:flagship".to_owned());
        shaped.oracle.policy = Some("off".to_owned());
        let run = inputs(vec![spec("alpha")], shaped);
        let (state_dir, checkout) = (run.state_dir.clone(), run.checkout.clone());
        let client = MockApiClient::new("mock").with_responses(vec![
            json_response(&defect_report_for(0)),
            json_response(&defect_report_for(0)),
        ]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), _pending) =
            run_review_with(run, Arc::new(client), &executor).await?;
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        assert_eq!(
            outcome,
            RunOutcome::BlockingFindings,
            "the escalated second vote turns the solo defect into a confirmed one"
        );
        assert_eq!(
            verdict.confirmed.len(),
            1,
            "two distinct judges voted the same defect"
        );
        assert!(
            verdict
                .stats
                .iter()
                .any(|entry| entry.judge == "escalate:flagship#esc"),
            "the escalation sweep rides the stats: {:?}",
            verdict.stats
        );
        drop(checkout);
        Ok(())
    }

    #[tokio::test]
    async fn a_dropped_needs_execution_lead_is_tried_by_the_oracle() -> Result<(), DtError> {
        let mut shaped = config();
        shaped.oracle.policy = Some("check".to_owned());
        let run = inputs(vec![spec("alpha")], shaped);
        let (state_dir, _checkout) = (run.state_dir.clone(), run.checkout.clone());
        let prose_lead = r#"{"region":0,"note":"n","cells":[{"aspect":"error-paths","verdict":"defect","finding":{
            "title":"The build breaks when the lockfile is absent",
            "mechanism":"an empty file reaches the unwrap and the process dies at src/a.rs:2",
            "claim":"needs-execution","severity":"warning",
            "evidence":["Cargo.toml line 14 (head): loopctl has no rev"]}}]}"#;
        let client = MockApiClient::new("mock").with_responses(vec![json_response(prose_lead)]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, _outcome), _pending) =
            run_review_with(run, Arc::new(client), &executor).await?;
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        assert_eq!(
            verdict.weak.len(),
            1,
            "the prose evidence weakens into a lead at the gate"
        );
        assert!(
            verdict.dropped.is_empty(),
            "an evidence miss never silently drops"
        );
        assert_eq!(
            verdict.lead_verdicts.len(),
            1,
            "the dropped needs-execution lead is still tried once: {:?}",
            verdict.lead_verdicts
        );
        Ok(())
    }

    #[tokio::test]
    async fn exec_claim_verdicts_survive_beside_lead_oracle_results() -> Result<(), DtError> {
        let mut shaped = config();
        shaped.oracle.policy = Some("check".to_owned());
        let mut run = inputs(vec![spec("alpha")], shaped);
        run.plan = plan(2);
        let (state_dir, _checkout) = (run.state_dir.clone(), run.checkout.clone());
        let cheap_claim = r#"{"region":0,"note":"traced","cells":[{"aspect":"error-paths","verdict":"defect","finding":{
            "title":"AGENTS.md is untracked and unignored by the packaging gate",
            "mechanism":"AGENTS.md is untracked and unignored so the packaging gate never sees it",
            "claim":"in-diff","severity":"warning",
            "evidence":["Cargo.toml line 14 (head): loopctl has no rev"]}}]}"#;
        let execution_claim = r#"{"region":1,"note":"traced","cells":[{"aspect":"error-paths","verdict":"defect","finding":{
            "title":"The crate does not compile with the new bound",
            "mechanism":"src/a.rs:2 breaks the build for every caller",
            "claim":"needs-execution","severity":"warning",
            "evidence":["Cargo.toml line 14 (head): loopctl has no rev"]}}]}"#;
        let client = MockApiClient::new("mock").with_responses(vec![
            json_response(cheap_claim),
            json_response(execution_claim),
        ]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, _outcome), _pending) =
            run_review_with(run, Arc::new(client), &executor).await?;
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        assert_eq!(
            verdict.weak.len(),
            2,
            "both prose-evidence findings weaken into leads: {:?}",
            verdict.weak.len()
        );
        assert_eq!(
            verdict.lead_verdicts.len(),
            2,
            "the executed cheap claim and the oracle-tried lead each keep their verdict: {:?}",
            verdict.lead_verdicts
        );
        assert!(
            verdict.lead_verdicts.values().any(|entry| matches!(
                entry,
                crate::contracts::OracleVerdict::Refuted { command } if command.contains("check-ignore")
            )),
            "the check-ignore execution still refutes the unignored claim: {:?}",
            verdict.lead_verdicts
        );
        assert!(
            verdict
                .lead_verdicts
                .values()
                .any(|entry| matches!(entry, crate::contracts::OracleVerdict::Inconclusive)),
            "the needs-execution lead is still tried by the lead oracle: {:?}",
            verdict.lead_verdicts
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_dry_run_returns_a_verdict_without_touching_disk() -> Result<(), DtError> {
        let mut inputs = inputs(vec![spec("alpha")], config());
        inputs.dry_run = true;
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&clean_report())]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), _pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::Clean);
        assert_eq!(
            verdict.coverage.cells_judged, verdict.coverage.cells_total,
            "the round itself is complete"
        );
        assert!(
            !file::state_path(&state_dir, &checkout).exists(),
            "a dry run leaves the state directory untouched"
        );
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[tokio::test]
    async fn a_budget_exhausted_before_a_judge_marks_it_absent() -> Result<(), DtError> {
        let mut inputs = inputs(vec![spec("alpha")], config());
        inputs.budget.total = Duration::from_nanos(1);
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&clean_report())]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::PartialRound);
        assert_eq!(verdict.absent_judges.len(), 1);
        assert_eq!(
            verdict.coverage.cells_judged, 0,
            "the canned answer is never spent when the budget is already gone"
        );
        persist(&state_dir, &checkout, pending);
        let state = saved_state(&state_dir, &checkout);
        assert_eq!(state.round, 0, "a partial round writes no round advance");
        assert_eq!(
            verdict.state.round, 0,
            "a partial round's verdict carries the persisted state, never an advanced round"
        );
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[tokio::test]
    async fn an_escalation_sweeping_an_absent_panel_profile_cannot_mask_it() -> Result<(), DtError>
    {
        let mut shaped = config();
        shaped.judges.escalate_profile = Some("mock".to_owned());
        shaped.judges.region_retries = 1;
        shaped.oracle.policy = Some("off".to_owned());
        let absent = pass::JudgeSpec {
            id: "mock".to_owned(),
            profile: "mock".to_owned(),
            max_tool_calls_per_region: 10,
        };
        let witness = pass::JudgeSpec {
            id: "local".to_owned(),
            profile: "mock".to_owned(),
            max_tool_calls_per_region: 10,
        };
        let run = inputs(vec![absent, witness], shaped);
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response("prose, never a report")]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), _pending) =
            run_review_with(run, Arc::new(client), &executor).await?;
        assert!(
            verdict.absent_judges.contains(&"mock".to_owned()),
            "the escalation's shared profile must not answer for the absent panel judge: {:?}",
            verdict.absent_judges
        );
        assert!(
            verdict
                .stats
                .iter()
                .all(|entry| entry.judge != "mock#esc" || !entry.judged),
            "the escalation rode the same broken profile and answered for nobody: {:?}",
            verdict.stats
        );
        assert_eq!(
            outcome,
            RunOutcome::PartialRound,
            "a masked absence would have turned exit 3 into a full round"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_replicated_panel_still_escalates_its_solo_defects() -> Result<(), DtError> {
        let mut shaped = config();
        shaped.judges.escalate_profile = Some("escalate:flagship".to_owned());
        shaped.oracle.policy = Some("off".to_owned());
        let replicas = vec![
            pass::JudgeSpec {
                id: "zai".to_owned(),
                profile: "mock".to_owned(),
                max_tool_calls_per_region: 10,
            },
            pass::JudgeSpec {
                id: "zai#2".to_owned(),
                profile: "mock".to_owned(),
                max_tool_calls_per_region: 10,
            },
        ];
        let run = inputs(replicas, shaped);
        let (state_dir, _checkout) = (run.state_dir.clone(), run.checkout.clone());
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&defect_report_for(0))]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), _pending) =
            run_review_with(run, Arc::new(client), &executor).await?;
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        assert_eq!(
            outcome,
            RunOutcome::BlockingFindings,
            "one model in two slots is still solo; the independent profile confirms it"
        );
        assert_eq!(
            verdict.confirmed.len(),
            1,
            "the escalation ran where two replica ids had faked a quorum: {:?}",
            verdict.solo
        );
        Ok(())
    }

    #[test]
    fn the_tools_wall_is_the_tighter_of_its_budget_and_the_runs() {
        let now = Instant::now();
        let wall = now + Duration::from_secs(1);
        assert_eq!(
            tools_deadline(now, Duration::from_secs(600), None),
            now.checked_add(Duration::from_secs(600)),
            "an unset run wall leaves the tools budget alone"
        );
        assert_eq!(
            tools_deadline(now, Duration::from_secs(600), Some(wall)),
            Some(wall),
            "a one-second run wall wins over a ten-minute tools budget"
        );
        assert_eq!(
            tools_deadline(now, Duration::ZERO, Some(wall)),
            Some(wall),
            "an unset tools budget is still bounded by the run wall"
        );
        assert_eq!(
            tools_deadline(now, Duration::ZERO, None),
            None,
            "no ceilings anywhere means no layer deadline"
        );
    }

    #[tokio::test]
    async fn an_explicit_run_wall_bounds_the_tool_layer_from_the_first_second()
    -> Result<(), DtError> {
        let repo = git_fixture("wall-before-tools")?;
        let base = repo.1;
        let mut shaped = config();
        shaped.tools.external = vec![crate::config::ExternalTool {
            name: "sleepy".to_owned(),
            command: "sleep 5".to_owned(),
            format: "diag".to_owned(),
            run_on: "**".to_owned(),
            budget_secs: 60,
        }];
        let mut run = inputs(vec![spec("alpha")], shaped);
        run.repo_root = repo.0;
        run.plan.range.base_sha = base.clone();
        run.plan.range.head_sha = base;
        run.budget = RunBudget {
            total: Duration::from_secs(2),
            tools: Duration::ZERO,
            oracle: Duration::from_secs(10),
        };
        let started = Instant::now();
        let ((verdict, outcome), _pending) = run_review_with(
            run,
            Arc::new(MockApiClient::new("mock")),
            &crate::tools::rust::ProcessExecutor,
        )
        .await?;
        assert!(
            started.elapsed() >= Duration::from_millis(900)
                && started.elapsed() < Duration::from_secs(4),
            "the head side really ran inside the wall and died on it: {:?}",
            started.elapsed()
        );
        assert_eq!(outcome, RunOutcome::PartialRound);
        assert!(
            verdict
                .tools_unavailable
                .iter()
                .any(|reason| reason.contains("shared tool-layer wall budget")),
            "the wall, not the tool's own budget, ended the layer: {:?}",
            verdict.tools_unavailable
        );
        Ok(())
    }

    struct LaneCounter {
        in_flight: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }

    impl LaneCounter {
        fn new() -> Self {
            Self {
                in_flight: std::sync::atomic::AtomicUsize::new(0),
                peak: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn enter(&self) {
            let now = self
                .in_flight
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .saturating_add(1);
            self.peak
                .fetch_max(now, std::sync::atomic::Ordering::SeqCst);
        }

        fn exit(&self) {
            self.in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct HeldStream {
        inner: Pin<
            Box<
                dyn futures::Stream<
                        Item = Result<loopctl::stream::StreamEvent, loopctl::api::error::ApiError>,
                    > + Send,
            >,
        >,
        counter: Arc<LaneCounter>,
    }

    impl futures::Stream for HeldStream {
        type Item = Result<loopctl::stream::StreamEvent, loopctl::api::error::ApiError>;

        fn poll_next(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            self.get_mut().inner.as_mut().poll_next(cx)
        }
    }

    impl Drop for HeldStream {
        fn drop(&mut self) {
            self.counter.exit();
        }
    }

    struct CountingClient {
        inner: Arc<MockApiClient>,
        counter: Arc<LaneCounter>,
        hold: Duration,
    }

    impl loopctl::api::ApiClient for CountingClient {
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
            let inner = Arc::clone(&self.inner);
            let counter = Arc::clone(&self.counter);
            let hold = self.hold;
            let request = request.clone();
            Box::pin(async move {
                counter.enter();
                tokio::time::sleep(hold).await;
                let outcome = inner.create_message(&request).await;
                counter.exit();
                outcome
            })
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
            let counter = Arc::clone(&self.counter);
            counter.enter();
            Box::pin(HeldStream {
                inner: Box::pin(self.inner.stream_messages(request)),
                counter,
            })
        }
    }

    #[tokio::test]
    async fn judges_on_one_provider_serialize_through_the_lane() -> Result<(), DtError> {
        let mut shaped = config();
        shaped.judges.region_concurrency = 2;
        shaped.ratelimit.per_provider_lanes = 1;
        let inputs = inputs(vec![spec("alpha"), spec("beta")], shaped);
        let (state_dir, checkout) = (inputs.state_dir.clone(), inputs.checkout.clone());
        let counter = Arc::new(LaneCounter::new());
        let client = CountingClient {
            inner: Arc::new(MockApiClient::new("mock").with_responses(vec![
                json_response(
                    &clean_report()
                );
                4
            ])),
            counter: Arc::clone(&counter),
            hold: Duration::from_millis(120),
        };
        let executor = FakeExecutor { code: 0 };
        let ((verdict, outcome), pending) =
            run_review_with(inputs, Arc::new(client), &executor).await?;
        assert_eq!(outcome, RunOutcome::Clean);
        assert!(
            verdict.absent_judges.is_empty(),
            "both judges delivered their sweeps: {:?}",
            verdict.absent_judges
        );
        assert_eq!(
            verdict.coverage.judged_by_judge.len(),
            2,
            "each judge judged the plan"
        );
        let peak = counter.peak.load(std::sync::atomic::Ordering::SeqCst);
        assert!(peak >= 1, "the panel made real model calls");
        assert!(
            peak <= 1,
            "one provider lane holds one judge's run at a time even with two regions in flight — peak {peak}"
        );
        persist(&state_dir, &checkout, pending);
        let _unused = std::fs::remove_dir_all(state_dir.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    #[test]
    fn the_exit_code_maps_full_blocking_and_partial() {
        let full_clean = round_verdict(Vec::new(), 1);
        let blocking = round_verdict(vec![reconciled("Off by one")], 1);
        let mut partial = round_verdict(Vec::new(), 1);
        partial.coverage.cells_judged = 0;
        assert_eq!(verdict_outcome(&full_clean), RunOutcome::Clean);
        assert_eq!(verdict_outcome(&blocking), RunOutcome::BlockingFindings);
        assert_eq!(verdict_outcome(&partial), RunOutcome::PartialRound);
        let mut incomplete = round_verdict(Vec::new(), 1);
        incomplete.incomplete = vec!["zai:glm-5.3".to_owned()];
        assert_eq!(
            verdict_outcome(&incomplete),
            RunOutcome::PartialRound,
            "a judge with failed regions makes the round partial even at full coverage"
        );
    }
    #[tokio::test]
    async fn a_file_region_cites_the_head_file_not_just_the_hunk() -> Result<(), DtError> {
        let mut run = inputs(vec![spec("alpha")], config());
        run.plan = plan_kind(RegionKind::File, 1);
        let outside = r#"{"region":0,"note":"traced the aspect","cells":[{"aspect":"error-paths","verdict":"defect","finding":{
            "title":"The sibling branch panics",
            "mechanism":"src/a.rs:2 opens the path that src/a.rs:8 unwraps",
            "claim":"in-diff","severity":"warning","evidence":["src/a.rs:8"]}}]}"#;
        let client = MockApiClient::new("mock").with_responses(vec![json_response(outside)]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, _outcome), _pending) =
            run_review_with(run, Arc::new(client), &executor).await?;
        assert!(
            verdict.dropped.is_empty(),
            "a File region was shown its whole head file, so line 8 is inside the materials: {:?}",
            verdict.dropped
        );
        assert_eq!(
            verdict.solo.len(),
            1,
            "admission is not confirmation — a lone judge still stays solo"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_escalation_never_expands_into_a_repo_region() -> Result<(), DtError> {
        let mut shaped = config();
        shaped.judges.escalate_profile = Some("escalate:flagship".to_owned());
        shaped.oracle.policy = Some("off".to_owned());
        let mut run = inputs(vec![spec("alpha")], shaped);
        run.plan = plan_kind(RegionKind::Repo, 1);
        let client =
            MockApiClient::new("mock").with_responses(vec![json_response(&defect_report_for(0))]);
        let executor = FakeExecutor { code: 0 };
        let ((verdict, _outcome), _pending) =
            run_review_with(run, Arc::new(client), &executor).await?;
        assert!(
            verdict.stats.iter().all(|entry| entry.judge == "alpha"),
            "the Repo region stays out of the escalation sub-plan: {:?}",
            verdict.stats
        );
        Ok(())
    }
}
