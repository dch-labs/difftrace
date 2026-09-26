//! One judge's full sweep: per-region loops, budgeted tools, and a
//! well-formed report or absence — never a partial report.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use loopctl::api::ApiClient;
use loopctl::config::SessionConfig;
use loopctl::engine::{BareLoop, Loop, RunConfig};
use serde::Deserialize;

use crate::aspects::AspectRegistry;
use crate::config::JudgesConfig;
use crate::contracts::{
    AspectId, CellVerdict, Cite, ClaimType, CoveragePlan, DiffSource, FindingDraft, FindingV2,
    JudgeReport, Region, RegionId, RegionKind, RegionReport, RoundState, Severity,
};
use crate::diff::DiffIndex;
use crate::error::{DtError, JudgeId};
use crate::gateway::PrGateway;

use super::flow;
use super::prompt;
use super::provider::{self, JudgeClient};
use super::tools;

pub struct JudgeSpec {
    pub id: JudgeId,
    pub profile: String,
    pub temperature: f32,
    pub seed: u64,
    pub max_tool_calls_per_region: usize,
}

pub async fn run_judge(
    spec: &JudgeSpec,
    cfg: &JudgesConfig,
    plan: &CoveragePlan,
    gateway: Arc<dyn PrGateway>,
    state: &RoundState,
    aspects: &AspectRegistry,
) -> Result<JudgeReport, DtError> {
    match provider::client_for_profile(&spec.profile, cfg.max_tokens)? {
        JudgeClient::Zai(client) | JudgeClient::Anthropic(client) => {
            run_judge_with(spec, cfg, Arc::new(client), plan, gateway, state, aspects).await
        }
        JudgeClient::Local(client) => {
            run_judge_with(spec, cfg, Arc::new(client), plan, gateway, state, aspects).await
        }
    }
}

pub async fn run_judge_with<C: ApiClient>(
    spec: &JudgeSpec,
    cfg: &JudgesConfig,
    client: Arc<C>,
    plan: &CoveragePlan,
    gateway: Arc<dyn PrGateway>,
    state: &RoundState,
    aspects: &AspectRegistry,
) -> Result<JudgeReport, DtError> {
    let DiffSource::Unified(diff_text) = gateway.diff();
    DiffIndex::parse(&diff_text).map_err(|cause| absent(spec, cause))?;
    let budget = Arc::new(AtomicUsize::new(0));
    let mut reports: Vec<RegionReport> = Vec::new();
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
        budget.store(0, Ordering::Relaxed);
        let report = run_region(
            &RegionContext {
                spec,
                cfg,
                gateway: &gateway,
                state,
                aspects,
                diff_text: &diff_text,
                budget: &budget,
            },
            &client,
            RegionId(idx),
            region,
            &asked,
        )
        .await?;
        reports.push(report);
    }
    Ok(JudgeReport {
        judge: spec.id.clone(),
        regions: reports,
    })
}

struct RegionContext<'a> {
    spec: &'a JudgeSpec,
    cfg: &'a JudgesConfig,
    gateway: &'a Arc<dyn PrGateway>,
    state: &'a RoundState,
    aspects: &'a AspectRegistry,
    diff_text: &'a str,
    budget: &'a Arc<AtomicUsize>,
}

async fn run_region<C: ApiClient>(
    context: &RegionContext<'_>,
    client: &Arc<C>,
    region_id: RegionId,
    region: &Region,
    asked: &[AspectId],
) -> Result<RegionReport, DtError> {
    let spec = context.spec;
    let cfg = context.cfg;
    let gateway = context.gateway;
    let snapshot = gateway
        .read_file_at_head(&region.file)
        .map_err(|err| absent(spec, err.to_string()))?;
    let flow_map = match region.kind {
        RegionKind::File | RegionKind::Repo => Some(
            flow::build(
                region,
                &snapshot.content,
                gateway.as_ref(),
                cfg.flow_map_max_callers,
            )
            .map_err(|err| absent(spec, err.to_string()))?,
        ),
        RegionKind::Added | RegionKind::Hunk => None,
    };
    let system = prompt::system(
        &spec.id,
        &prompt::open_issues_block(context.state, &region.file),
        &prompt::suppressions_block(context.state, &region.file),
        &context.aspects.definitions_block(asked),
    );
    let user = prompt::region_prompt(
        region,
        region_id.0,
        &crate::diff::region_diff(
            context.diff_text,
            &region.file,
            region.start,
            region.end,
            prompt::REGION_CONTEXT_LINES,
        ),
        &prompt::truncate_snapshot(&snapshot.content),
        flow_map.as_ref(),
        asked,
        prompt::REPORT_SCHEMA,
    );
    let registry = tools::region_registry(
        Arc::clone(gateway),
        Arc::clone(context.budget),
        spec.max_tool_calls_per_region,
        cfg.search_tool,
    );
    let session = SessionConfig::default().with_system_prompt(system);
    let run_config = RunConfig::default();
    let mut agent = BareLoop::new(Arc::clone(client), registry, session);
    let budget_secs = cfg.stream_timeout_secs;
    let wire = tokio::time::timeout(Duration::from_secs(budget_secs), async {
        let run = agent
            .run(&user, &run_config)
            .await
            .map_err(|err| err.to_string())?;
        if let Some(wire) = parse_report(&run.output.unwrap_or_default()) {
            return Ok(wire);
        }
        let corrective = agent
            .run(
                "Return only the JSON report for this region, exactly per the schema, with no other text.",
                &run_config,
            )
            .await
            .map_err(|err| err.to_string())?;
        parse_report(&corrective.output.unwrap_or_default())
            .ok_or_else(|| "the report never parsed as JSON per the schema".to_owned())
    })
    .await
    .map_err(|_| absent(spec, format!("exceeded the {budget_secs}s budget")))?
    .map_err(|cause| absent(spec, cause))?;
    map_wire(wire, region_id, asked).map_err(|cause| absent(spec, cause))
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

fn build_finding(region: RegionId, aspect: &str, wire: FindingWire) -> Result<FindingV2, String> {
    let claim = match wire.claim.as_str() {
        "in-diff" => ClaimType::InDiff,
        "needs-execution" => ClaimType::NeedsExecution,
        "external-assumption" => ClaimType::ExternalAssumption,
        other => return Err(format!("unknown claim {other:?}")),
    };
    let severity: Severity = wire.severity.parse()?;
    let mut evidence: Vec<Cite> = Vec::new();
    for cite in wire.evidence {
        let Some((file, line)) = cite.split_once(':') else {
            return Err(format!("evidence {cite:?} is not file:line"));
        };
        let line: u32 = line
            .trim()
            .parse()
            .map_err(|_| format!("evidence {cite:?} has no line number"))?;
        evidence.push(Cite {
            file: file.to_owned(),
            line,
        });
    }
    Ok(FindingV2::new(FindingDraft {
        region,
        aspect: AspectId(aspect.to_owned()),
        title: wire.title,
        mechanism: wire.mechanism,
        claim,
        evidence,
        severity,
        fix_hint: wire.fix_hint,
    }))
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
            temperature: 0.0,
            seed: 7,
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

    async fn drive(client: MockApiClient, plan: &CoveragePlan) -> Result<JudgeReport, DtError> {
        run_judge_with(
            &spec(),
            &JudgesConfig::default(),
            Arc::new(client),
            plan,
            Arc::new(FakeGateway),
            &state(),
            &AspectRegistry::new(),
        )
        .await
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
            ]),
            &plan(1, &["error-paths"]),
        )
        .await;
        assert!(
            matches!(&outcome, Err(DtError::JudgeAbsent { judge, cause }) if judge == "alpha"
                && cause.contains("never parsed")),
            "prose after the corrective turn is absence, not a partial report: {outcome:?}"
        );
        let wrong_aspects = drive(
            MockApiClient::new("mock").with_responses(vec![json_response(
                r#"{"region":0,"note":"n","cells":[{"aspect":"vibes","verdict":"clean"}]}"#,
            )]),
            &plan(1, &["error-paths"]),
        )
        .await;
        assert!(
            matches!(&wrong_aspects, Err(DtError::JudgeAbsent { cause, .. }) if cause.contains("error-paths")),
            "a report that skips the asked aspects is malformed: {wrong_aspects:?}"
        );
    }
}
