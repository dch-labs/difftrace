//! The pure fold from judge reports to a round verdict.

use std::collections::{BTreeMap, BTreeSet};

use crate::aspects::AspectRegistry;
use crate::contracts::{
    CellVerdict, CiteAuthority, ClaimType, CoveragePlan, CoverageSummary, DropReason, Dropped,
    FileCoverage, Finding, FindingDraft, FindingFailure, JudgeReport, OracleResults, OracleVerdict,
    Reconciled, RegionStats, RoundState, RoundVerdict, Severity, ToolEvidence, WeakLead,
    repaired_title, validate_finding,
};
use crate::error::JudgeId;
use crate::identity::{same_spot, titles_overlap_enough};
use crate::judges::JUDGE_QUORUM;
use crate::state::{apply_round, matches_suppression};

fn repair_cites(finding: &Finding, authority: &CiteAuthority<'_>) -> Option<Finding> {
    let needs_repair = finding.evidence.is_empty()
        || finding
            .evidence
            .iter()
            .any(|cite| !authority.index.resolve(cite));
    if !needs_repair {
        return None;
    }
    let mined: Vec<crate::contracts::Cite> = crate::contracts::mine_cites(&finding.mechanism)
        .into_iter()
        .filter(|cite| authority.index.resolve(cite))
        .collect();
    if mined.is_empty() {
        return None;
    }
    Some(Finding {
        evidence: mined,
        ..finding.clone()
    })
}

fn repair_title(finding: &Finding) -> Option<Finding> {
    let title = repaired_title(finding)?;
    Some(Finding::new(FindingDraft {
        region: finding.region,
        aspect: finding.aspect.clone(),
        title,
        mechanism: finding.mechanism.clone(),
        claim: finding.claim,
        evidence: finding.evidence.clone(),
        severity: finding.severity,
        fix_hint: finding.fix_hint.clone(),
    }))
}

struct Candidate {
    judge: JudgeId,
    profile: JudgeId,
    finding: Finding,
}

#[must_use]
pub(crate) fn judge_profile(id: &str) -> String {
    id.split('#').next().unwrap_or(id).to_owned()
}

#[must_use]
pub(crate) fn judge_family(profile: &str) -> String {
    let model = profile.split(':').nth(1).unwrap_or(profile);
    let stripped = model.split('#').next().unwrap_or(model);
    let Some(base) = stripped.strip_prefix("glm-") else {
        return stripped.to_owned();
    };
    let mut segments: Vec<&str> = base.split('-').collect();
    if segments.len() >= 2
        && segments
            .last()
            .is_some_and(|segment| !segment.chars().all(|ch| ch.is_ascii_digit()))
    {
        segments.pop();
    }
    let joined = segments.join("-");
    if joined.is_empty() {
        model.to_owned()
    } else {
        format!("glm-{joined}")
    }
}

fn collapse_repeated_mechanisms(weak: Vec<WeakLead>) -> Vec<WeakLead> {
    let mut kept: Vec<WeakLead> = Vec::new();
    for lead in weak {
        let mut merged_into = None;
        for target in &mut kept {
            let same_claim = target.finding.claim == lead.finding.claim;
            let mechanism_overlap =
                titles_overlap_enough(&target.finding.mechanism, &lead.finding.mechanism);
            let title_overlap = titles_overlap_enough(&target.finding.title, &lead.finding.title);
            let same_aspect = target.finding.aspect == lead.finding.aspect;
            if same_claim && same_aspect && (mechanism_overlap || title_overlap) {
                let site = if let Some(cite) = lead.finding.evidence.first() {
                    format!("{}:{}", cite.file, cite.line)
                } else {
                    lead.finding.region.0.to_string()
                };
                if !target.also_at.contains(&site) {
                    target.also_at.push(site);
                }
                merged_into = Some(());
                break;
            }
        }
        if merged_into.is_none() {
            kept.push(lead);
        }
    }
    kept
}

#[must_use]
fn distinct_families(group: &[&Candidate]) -> Vec<String> {
    let mut families: Vec<String> = Vec::new();
    for candidate in group {
        let family = judge_family(&candidate.profile);
        if !families.contains(&family) {
            families.push(family);
        }
    }
    families.sort();
    families
}

#[must_use]
fn distinct_profiles(group: &[&Candidate]) -> Vec<JudgeId> {
    let mut profiles: Vec<JudgeId> = Vec::new();
    for candidate in group {
        if !profiles.contains(&candidate.profile) {
            profiles.push(candidate.profile.clone());
        }
    }
    profiles
}

#[must_use]
pub fn reconcile(
    reports: &[JudgeReport],
    plan: &CoveragePlan,
    state: &RoundState,
    oracle: &OracleResults,
    aspects: &AspectRegistry,
    authority: &CiteAuthority<'_>,
    panel: &[JudgeId],
    tools: &ToolEvidence,
) -> RoundVerdict {
    let mut ordered: Vec<&JudgeReport> = reports.iter().collect();
    ordered.sort_by(|a, b| a.judge.cmp(&b.judge));
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut dropped: Vec<Dropped> = Vec::new();
    let mut weak: Vec<WeakLead> = Vec::new();
    for report in &ordered {
        for region_report in &report.regions {
            let Some(region) = plan.regions.get(region_report.region.0) else {
                continue;
            };
            for (_, verdict) in &region_report.cells {
                if let CellVerdict::Defect(finding) = verdict {
                    sort_finding(
                        finding,
                        region,
                        authority,
                        aspects,
                        &report.judge,
                        &mut candidates,
                        &mut weak,
                        &mut dropped,
                    );
                }
            }
        }
    }
    let weak_instances: BTreeSet<(String, String)> = weak
        .iter()
        .map(|entry| (entry.judge.clone(), entry.finding.id.0.clone()))
        .collect();
    let dropped_instances: BTreeSet<(String, String)> = dropped
        .iter()
        .map(|entry| (entry.judge.clone(), entry.finding.id.0.clone()))
        .collect();
    weak = collapse_repeated_mechanisms(weak);
    let mut confirmed: Vec<Reconciled> = Vec::new();
    let mut solo: Vec<Reconciled> = Vec::new();
    let mut refuted: Vec<Reconciled> = Vec::new();
    let mut suppressed: Vec<Reconciled> = Vec::new();
    for group in group_candidates(&candidates) {
        let Some(first) = group.first() else {
            continue;
        };
        let votes = distinct_judges(&group);
        let families = distinct_families(&group);
        let entry = Reconciled {
            families,
            votes,
            severity: group_severity(&group, aspects),
            finding: first.finding.clone(),
        };
        let fired = state.suppressions.iter().any(|rule| {
            group
                .iter()
                .any(|candidate| matches_suppression(rule, &candidate.finding))
        });
        let oracle_confirmed = group.iter().any(|candidate| {
            matches!(
                oracle.verdicts.get(&candidate.finding.id),
                Some(OracleVerdict::Confirmed)
            )
        });
        let oracle_refuted = group.iter().any(|candidate| {
            matches!(
                oracle.verdicts.get(&candidate.finding.id),
                Some(OracleVerdict::Refuted { .. })
            )
        });
        if fired {
            suppressed.push(entry);
        } else if distinct_profiles(&group).len() >= JUDGE_QUORUM || oracle_confirmed {
            confirmed.push(entry);
        } else if oracle_refuted {
            refuted.push(entry);
        } else {
            solo.push(entry);
        }
    }
    let present: BTreeSet<&str> = reports.iter().map(|report| report.judge.as_str()).collect();
    let absent_judges: Vec<JudgeId> = panel
        .iter()
        .filter(|id| !present.contains(id.as_str()))
        .cloned()
        .collect();
    let incomplete: Vec<JudgeId> = reports
        .iter()
        .filter(|report| !report.failed.is_empty())
        .map(|report| report.judge.as_str())
        .collect::<std::collections::BTreeSet<&str>>()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let coverage = coverage_summary(reports, plan, panel, &dropped_instances, &weak_instances);
    let full = coverage.cells_judged == coverage.cells_total
        && absent_judges.is_empty()
        && incomplete.is_empty();
    let mut region_stats: Vec<RegionStats> = ordered
        .into_iter()
        .flat_map(|report| report.stats.iter().cloned())
        .collect();
    region_stats
        .sort_by(|a, b| (a.region.0, a.judge.as_str()).cmp(&(b.region.0, b.judge.as_str())));
    let mut verdict = RoundVerdict {
        range: plan.range.clone(),
        coverage,
        tool_findings: tools.findings.clone(),
        confirmed,
        solo,
        weak,
        refuted,
        suppressed,
        dropped,
        absent_judges,
        incomplete,
        tools_unavailable: tools.unavailable.clone(),
        stats: region_stats,
        lead_verdicts: std::collections::BTreeMap::new(),
        state: state.clone(),
    };
    let mut updated = state.clone();
    apply_round(&mut updated, &verdict, full, plan.narrowed);
    verdict.state = updated;
    verdict
}

fn sort_finding(
    finding: &Finding,
    region: &crate::contracts::Region,
    authority: &CiteAuthority<'_>,
    aspects: &AspectRegistry,
    judge: &str,
    candidates: &mut Vec<Candidate>,
    weak: &mut Vec<WeakLead>,
    dropped: &mut Vec<Dropped>,
) {
    match validate_finding(finding, region, authority, aspects) {
        Ok(()) => {
            let mut finding = finding.clone();
            if finding.claim == ClaimType::InDiff
                && authority.index.cites_no_added_line(&finding.evidence)
            {
                finding.claim = ClaimType::AdjacentPreexisting;
            }
            candidates.push(Candidate {
                judge: judge.to_owned(),
                profile: judge_profile(judge),
                finding,
            });
        }
        Err(FindingFailure::Weaken(reason)) => {
            let finding = repair_cites(finding, authority).unwrap_or_else(|| finding.clone());
            weak.push(WeakLead {
                judge: judge.to_owned(),
                finding,
                reason,
                repaired: None,
                also_at: Vec::new(),
            });
        }
        Err(FindingFailure::Drop(reason)) => {
            let repaired = if reason == DropReason::MalformedTitle {
                repair_title(finding)
            } else {
                None
            };
            match repaired {
                Some(repaired) => {
                    let reason = match validate_finding(&repaired, region, authority, aspects) {
                        Ok(()) => DropReason::NeedsRepair,
                        Err(FindingFailure::Weaken(remaining)) => remaining,
                        Err(FindingFailure::Drop(_)) => reason,
                    };
                    weak.push(WeakLead {
                        judge: judge.to_owned(),
                        finding: repaired,
                        reason,
                        repaired: Some(finding.title.clone()),
                        also_at: Vec::new(),
                    });
                }
                None => dropped.push(Dropped {
                    judge: judge.to_owned(),
                    finding: finding.clone(),
                    reason,
                }),
            }
        }
    }
}

fn group_candidates(candidates: &[Candidate]) -> Vec<Vec<&Candidate>> {
    let mut groups: Vec<Vec<&Candidate>> = Vec::new();
    for candidate in candidates {
        let target = groups.iter_mut().find(|group| {
            group
                .iter()
                .any(|member| same_identity(&member.finding, &candidate.finding))
        });
        match target {
            Some(group) => group.push(candidate),
            None => groups.push(vec![candidate]),
        }
    }
    for group in &mut groups {
        group.sort_by(|left, right| left.finding.id.cmp(&right.finding.id));
    }
    groups
}

fn same_identity(a: &Finding, b: &Finding) -> bool {
    match (a.evidence.first(), b.evidence.first()) {
        (Some(left), Some(right)) => {
            left.file == right.file
                && same_spot(
                    u64::from(left.line),
                    &a.title,
                    u64::from(right.line),
                    &b.title,
                )
        }
        (None, None) => a.region == b.region && titles_overlap_enough(&a.title, &b.title),
        _ => false,
    }
}

fn distinct_judges(group: &[&Candidate]) -> Vec<JudgeId> {
    let mut votes: Vec<JudgeId> = Vec::new();
    for candidate in group {
        if !votes.contains(&candidate.judge) {
            votes.push(candidate.judge.clone());
        }
    }
    votes
}

fn group_severity(group: &[&Candidate], aspects: &AspectRegistry) -> Severity {
    let Some(first) = group.first() else {
        return Severity::Nitpick;
    };
    let mut levels: Vec<Severity> = Vec::new();
    let mut seen_profiles: Vec<JudgeId> = Vec::new();
    for entry in group {
        if !seen_profiles.contains(&entry.profile) {
            seen_profiles.push(entry.profile.clone());
            levels.push(entry.finding.severity);
        }
    }
    levels.sort_unstable();
    let median = levels
        .get(levels.len().saturating_sub(1) / 2)
        .copied()
        .unwrap_or(Severity::Nitpick);
    let capped = median.min(aspects.ceiling(&first.finding.aspect));
    if first.finding.claim == ClaimType::ExternalAssumption {
        capped.min(Severity::Suggestion)
    } else {
        capped
    }
}

fn coverage_summary(
    reports: &[JudgeReport],
    plan: &CoveragePlan,
    panel: &[JudgeId],
    dropped: &BTreeSet<(String, String)>,
    weak: &BTreeSet<(String, String)>,
) -> CoverageSummary {
    let threshold = panel.len().saturating_sub(1).max(1);
    let present = reports.len();
    let mut cells_judged: usize = 0;
    let mut cells_defect: usize = 0;
    let mut cells_short: usize = 0;
    let mut per_file: BTreeMap<String, FileCoverage> = BTreeMap::new();
    for (region, aspect) in &plan.cells {
        let mut judges_on_cell: BTreeSet<&str> = BTreeSet::new();
        let mut defect_survived = false;
        for report in reports {
            let verdict = report.regions.iter().find_map(|region_report| {
                if region_report.region != *region {
                    return None;
                }
                region_report
                    .cells
                    .iter()
                    .find(|(asked, _)| asked == aspect)
                    .map(|(_, verdict)| verdict)
            });
            let Some(verdict) = verdict else {
                continue;
            };
            judges_on_cell.insert(report.judge.as_str());
            if let CellVerdict::Defect(finding) = verdict
                && !dropped
                    .iter()
                    .any(|(judge, id)| judge == &report.judge && id == &finding.id.0)
                && !weak
                    .iter()
                    .any(|(judge, id)| judge == &report.judge && id == &finding.id.0)
            {
                defect_survived = true;
            }
        }
        let file = plan.regions.get(region.0).map(|entry| entry.file.clone());
        if let Some(file) = file {
            let entry = per_file.entry(file).or_default();
            entry.cells = entry.cells.saturating_add(1);
        }
        if judges_on_cell.len() < present {
            cells_short = cells_short.saturating_add(1);
            if let Some(file) = plan.regions.get(region.0).map(|entry| entry.file.clone()) {
                let entry = per_file.entry(file).or_default();
                entry.short = entry.short.saturating_add(1);
            }
        }
        if judges_on_cell.len() >= threshold {
            cells_judged = cells_judged.saturating_add(1);
            if defect_survived {
                cells_defect = cells_defect.saturating_add(1);
            }
            if let Some(file) = plan.regions.get(region.0).map(|entry| entry.file.clone()) {
                let entry = per_file.entry(file).or_default();
                entry.judged = entry.judged.saturating_add(1);
                if defect_survived {
                    entry.defect = entry.defect.saturating_add(1);
                } else {
                    entry.clean = entry.clean.saturating_add(1);
                }
            }
        }
    }
    let mut judged_by_judge: BTreeMap<JudgeId, usize> = BTreeMap::new();
    for report in reports {
        let count = plan
            .cells
            .iter()
            .filter(|(region, aspect)| {
                report.regions.iter().any(|region_report| {
                    region_report.region == *region
                        && region_report.cells.iter().any(|(asked, _)| asked == aspect)
                })
            })
            .count();
        judged_by_judge.insert(report.judge.clone(), count);
    }
    CoverageSummary {
        cells_total: plan.cells.len(),
        cells_judged,
        cells_clean: cells_judged.saturating_sub(cells_defect),
        cells_defect,
        judged_by_judge,
        regions: plan.regions.len(),
        per_file,
        cells_short,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static SERVED: std::sync::LazyLock<crate::contracts::ServedReads> =
        std::sync::LazyLock::new(crate::contracts::ServedReads::new);
    use crate::contracts::ToolFinding;
    use crate::contracts::{
        AspectId, Cite, DropReason, FindingDraft, Issue, IssueIdentity, IssueStatus, PinnedRange,
        Region, RegionId, RegionKind, RegionReport, Suppression, equivalent,
    };
    use crate::diff::DiffIndex;
    use crate::identity::title_tokens;
    use std::collections::BTreeMap;

    const DIFF: &str = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,6 +1,8 @@\n base\n ctx one\n+added one\n+added two\n tail one\n ctx two\n ctx three\n ctx four\n";

    fn aspected(
        title: &str,
        line: u32,
        severity: Severity,
        aspect: &str,
        claim: ClaimType,
    ) -> FindingDraft {
        FindingDraft {
            region: RegionId(0),
            aspect: AspectId(aspect.to_owned()),
            title: title.to_owned(),
            mechanism: "the walk reaches src/a.rs:3 and the guard fails".to_owned(),
            claim,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line,
            }],
            severity,
            fix_hint: None,
        }
    }

    fn draft(title: &str, line: u32, severity: Severity) -> FindingDraft {
        aspected(title, line, severity, "error-paths", ClaimType::InDiff)
    }

    fn defect(finding: &Finding) -> (AspectId, CellVerdict) {
        (
            AspectId("error-paths".to_owned()),
            CellVerdict::Defect(finding.clone()),
        )
    }

    fn report(judge: &str, cells: Vec<(AspectId, CellVerdict)>) -> JudgeReport {
        JudgeReport {
            judge: judge.to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![RegionReport {
                region: RegionId(0),
                note: "traced".to_owned(),
                cells,
            }],
        }
    }

    fn plan() -> CoveragePlan {
        CoveragePlan {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            regions: vec![Region {
                file: "src/a.rs".to_owned(),
                start: 3,
                end: 4,
                kind: RegionKind::Added,
            }],
            cells: vec![(RegionId(0), AspectId("error-paths".to_owned()))],
            narrowed: false,
        }
    }

    fn state() -> RoundState {
        RoundState {
            checkout: "trunk".to_owned(),
            round: 1,
            issues: Vec::new(),
            suppressions: Vec::new(),
        }
    }

    fn drive(
        reports: &[JudgeReport],
        panel: &[&str],
        state: &RoundState,
        oracle: &OracleResults,
        tools: &[ToolFinding],
    ) -> Result<RoundVerdict, Box<dyn std::error::Error>> {
        drive_with_authority(reports, panel, state, oracle, tools, &BTreeMap::new())
    }

    fn drive_with_authority(
        reports: &[JudgeReport],
        panel: &[&str],
        state: &RoundState,
        oracle: &OracleResults,
        tools: &[ToolFinding],
        snapshots: &BTreeMap<String, crate::judges::prompt::SnapshotWindow>,
    ) -> Result<RoundVerdict, Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let authority = CiteAuthority {
            index: &index,
            snapshots,
            served: &SERVED,
        };
        let panel_ids: Vec<JudgeId> = panel.iter().map(|id| (*id).to_owned()).collect();
        Ok(reconcile(
            reports,
            &plan(),
            state,
            oracle,
            &AspectRegistry::new(),
            &authority,
            &panel_ids,
            &crate::contracts::ToolEvidence {
                findings: tools.to_vec(),
                unavailable: Vec::new(),
            },
        ))
    }

    #[test]
    fn a_broken_cite_is_repaired_from_the_mechanism_not_emptied()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let authority = CiteAuthority {
            index: &index,
            snapshots: &BTreeMap::new(),
            served: &SERVED,
        };
        let shapes: [(&str, Vec<Cite>); 3] = [
            (
                "record_pinned_telemetry (src/a.rs:6) ships without a pin",
                Vec::new(),
            ),
            (
                "the store-refusal branch emits src/a.rs:7 on error",
                vec![Cite {
                    file: "tests/missing.rs".to_owned(),
                    line: 175,
                }],
            ),
            (
                "the walk reaches src/a.rs:6 and the guard fails",
                vec![Cite {
                    file: "src/a.rs".to_owned(),
                    line: 99,
                }],
            ),
        ];
        for (mechanism, evidence) in shapes {
            let finding = Finding::new(FindingDraft {
                region: RegionId(0),
                aspect: AspectId("tests".to_owned()),
                title: "New metric behavior ships with no test coverage".to_owned(),
                mechanism: mechanism.to_owned(),
                claim: ClaimType::InDiff,
                evidence,
                severity: Severity::Suggestion,
                fix_hint: None,
            });
            let reports = [JudgeReport {
                judge: "zai:glm-5.3".to_owned(),
                failed: Vec::new(),
                stats: Vec::new(),
                regions: vec![RegionReport {
                    region: RegionId(0),
                    note: "traced".to_owned(),
                    cells: vec![(AspectId("tests".to_owned()), CellVerdict::Defect(finding))],
                }],
            }];
            let verdict = reconcile(
                &reports,
                &plan(),
                &state(),
                &OracleResults::default(),
                &AspectRegistry::new(),
                &authority,
                &["zai:glm-5.3".to_owned()],
                &crate::contracts::ToolEvidence::default(),
            );
            let lead = verdict
                .weak
                .first()
                .ok_or_else(|| format!("no lead for {mechanism}"))?;
            let cite = lead
                .finding
                .evidence
                .first()
                .ok_or_else(|| format!("no repaired cite for {mechanism}"))?;
            assert_eq!(
                cite.file, "src/a.rs",
                "the lead carries a cite mined from its mechanism: {mechanism}"
            );
            assert!(
                cite.line >= 3 && cite.line <= 8,
                "the mined line falls inside the pinned diff: {mechanism} -> {}",
                cite.line
            );
            assert!(
                authority.index.resolve(cite),
                "the repaired cite resolves in the pinned diff"
            );
        }
        Ok(())
    }

    #[test]
    fn a_malformed_title_is_repaired_from_the_mechanism_not_dropped()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let authority = CiteAuthority {
            index: &index,
            snapshots: &BTreeMap::new(),
            served: &SERVED,
        };
        let long_title = "package-check discards cargo package's own failure and prints an unconstrained success line even when the listing step failed which hides packaging breakage from the merge gate".to_owned();
        let mechanism = "Makefile:220 runs `listing=$$(cargo package --list --allow-dirty --no-verify 2>/dev/null)` and never checks the exit".to_owned();
        let finding = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: long_title.clone(),
            mechanism: mechanism.clone(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 3,
            }],
            severity: Severity::Suggestion,
            fix_hint: None,
        });
        let reports = [JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![RegionReport {
                region: RegionId(0),
                note: "traced".to_owned(),
                cells: vec![(
                    AspectId("error-paths".to_owned()),
                    CellVerdict::Defect(finding),
                )],
            }],
        }];
        let verdict = reconcile(
            &reports,
            &plan(),
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        assert!(
            verdict.dropped.is_empty(),
            "the cosmetic title never destroys the finding: {:?}",
            verdict.dropped
        );
        let lead = verdict.weak.first().ok_or("the repaired lead must exist")?;
        assert_eq!(lead.reason, DropReason::MechanismCitesNoRegionLine);
        assert_eq!(lead.repaired.as_deref(), Some(long_title.as_str()));
        assert!(
            lead.finding.title.chars().count() <= 80,
            "the repaired title fits the schema: {:?}",
            lead.finding.title
        );
        assert_eq!(lead.finding.severity, Severity::Suggestion);
        assert_eq!(lead.finding.mechanism, mechanism);
        assert_eq!(lead.finding.evidence.first().ok_or("cite")?.line, 3);
        assert_eq!(lead.judge, "zai:glm-5.3");
        Ok(())
    }

    #[test]
    fn a_mechanism_restated_per_region_collapses_into_one_lead_with_sites()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let authority = CiteAuthority {
            index: &index,
            snapshots: &BTreeMap::new(),
            served: &SERVED,
        };
        let sites = [(3u32, RegionId(0)), (5, RegionId(0)), (7, RegionId(0))];
        let mut regions = Vec::new();
        for (line, region) in sites {
            regions.push(RegionReport {
                region,
                note: "traced".to_owned(),
                cells: vec![(
                    AspectId("tests".to_owned()),
                    CellVerdict::Defect(Finding::new(FindingDraft {
                        region,
                        aspect: AspectId("tests".to_owned()),
                        title: "New metric emissions have no coverage".to_owned(),
                        mechanism: format!(
                            "the new emission leg at src/a.rs:{line} has a presence gate and no behavioral pin"
                        ),
                        claim: ClaimType::InDiff,
                        evidence: vec![Cite {
                            file: "src/a.rs".to_owned(),
                            line,
                        }],
                        severity: Severity::Suggestion,
                        fix_hint: None,
                    })),
                )],
            });
        }
        let reports = [JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions,
        }];
        let verdict = reconcile(
            &reports,
            &plan(),
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        assert!(
            verdict.weak.len() == 1,
            "the restated mechanism collapses across regions: {:?}",
            verdict.weak.len()
        );
        let lead = verdict.weak.first().ok_or("lead")?;
        assert_eq!(
            lead.also_at,
            vec!["src/a.rs:7".to_owned()],
            "the affected sites ride the single claim"
        );
        Ok(())
    }

    #[test]
    fn restatements_sharing_a_title_collapse_without_mechanism_overlap()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let authority = CiteAuthority {
            index: &index,
            snapshots: &BTreeMap::new(),
            served: &SERVED,
        };
        let entry = |line: u32, mechanism: &str| {
            Finding::new(FindingDraft {
                region: RegionId(0),
                aspect: AspectId("tests".to_owned()),
                title: "New metric emissions have no behavioral pin".to_owned(),
                mechanism: mechanism.to_owned(),
                claim: ClaimType::InDiff,
                evidence: vec![Cite {
                    file: "src/a.rs".to_owned(),
                    line,
                }],
                severity: Severity::Suggestion,
                fix_hint: None,
            })
        };
        let recorder = "the recorder for the metric at src/a.rs:7 fires without any test asserting the stored value";
        let gate = "a presence gate covers this emission but never pins what lands";
        let reports = [JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![RegionReport {
                region: RegionId(0),
                note: "traced".to_owned(),
                cells: vec![
                    (
                        AspectId("tests".to_owned()),
                        CellVerdict::Defect(entry(3, recorder)),
                    ),
                    (
                        AspectId("tests".to_owned()),
                        CellVerdict::Defect(entry(5, gate)),
                    ),
                ],
            }],
        }];
        let verdict = reconcile(
            &reports,
            &plan(),
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        assert_eq!(
            verdict.weak.len(),
            1,
            "same-title restatements collapse even when the mechanisms word differently: {:?}",
            verdict
                .weak
                .iter()
                .map(|lead| lead.finding.title.clone())
                .collect::<Vec<_>>()
        );
        let distinct = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("tests".to_owned()),
            title: "The converter books the wrong action".to_owned(),
            mechanism: "a wholly different claim about a wholly different seam at src/a.rs:7"
                .to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 4,
            }],
            severity: Severity::Suggestion,
            fix_hint: None,
        });
        let reports = [JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![RegionReport {
                region: RegionId(0),
                note: "traced".to_owned(),
                cells: vec![
                    (
                        AspectId("tests".to_owned()),
                        CellVerdict::Defect(entry(3, recorder)),
                    ),
                    (AspectId("tests".to_owned()), CellVerdict::Defect(distinct)),
                ],
            }],
        }];
        let verdict = reconcile(
            &reports,
            &plan(),
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        assert_eq!(
            verdict.weak.len(),
            2,
            "distinct claims stay separate — the key never merges unrelated findings: {:?}",
            verdict
                .weak
                .iter()
                .map(|lead| lead.finding.title.clone())
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn a_two_vote_same_family_confirmation_records_one_family()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(judge_family("zai:glm-5.3"), "glm-5.3");
        assert_eq!(judge_family("zai:glm-5.3-flash"), "glm-5.3");
        assert_eq!(judge_family("zai:glm-5.3#2"), "glm-5.3");
        assert_eq!(judge_family("local"), "local");
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let reports = [
            report("zai:glm-5.3", vec![defect(&finding)]),
            report("zai:glm-5.3-flash", vec![defect(&finding)]),
        ];
        let verdict = drive(
            &reports,
            &["zai:glm-5.3", "zai:glm-5.3-flash"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        let confirmed = verdict.confirmed.first().ok_or("confirmed")?;
        assert_eq!(
            confirmed.votes.len(),
            2,
            "two distinct judges still meet the quorum"
        );
        assert_eq!(
            confirmed.families,
            vec!["glm-5.3".to_owned()],
            "same-family agreement is visible as one family, not two independent confirmations"
        );
        Ok(())
    }

    #[test]
    fn a_pre_existing_snippet_cited_on_an_unchanged_line_is_adjacent()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let authority = CiteAuthority {
            index: &index,
            snapshots: &BTreeMap::new(),
            served: &SERVED,
        };
        let mut wide = plan();
        if let Some(first) = wide.regions.first_mut() {
            first.end = 6;
        }
        let finding = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("canon".to_owned()),
            title: "Doc example pins loopctl 0.3 while the manifest bumps".to_owned(),
            mechanism: "src/a.rs:5 pins `loopctl =0.3` and the bump never touches it".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 5,
            }],
            severity: Severity::Suggestion,
            fix_hint: None,
        });
        let reports = [JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![RegionReport {
                region: RegionId(0),
                note: "traced".to_owned(),
                cells: vec![(AspectId("canon".to_owned()), CellVerdict::Defect(finding))],
            }],
        }];
        let verdict = reconcile(
            &reports,
            &wide,
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        let solo = verdict.solo.first().ok_or("the finding still surfaces")?;
        assert_eq!(
            solo.finding.claim,
            ClaimType::AdjacentPreexisting,
            "a cite on an unchanged context line relabels instead of riding as in-diff"
        );
        assert!(
            verdict.blocking_count() == 0,
            "pre-existing debt never blocks the round"
        );
        let second = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: "The added line itself drops the guard".to_owned(),
            mechanism: "src/a.rs:3 drops the guard early".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 3,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        });
        let reports = [JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![RegionReport {
                region: RegionId(0),
                note: "traced".to_owned(),
                cells: vec![(
                    AspectId("error-paths".to_owned()),
                    CellVerdict::Defect(second),
                )],
            }],
        }];
        let verdict = reconcile(
            &reports,
            &plan(),
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        let solo = verdict
            .solo
            .first()
            .ok_or("the added-line finding surfaces")?;
        assert_eq!(
            solo.finding.claim,
            ClaimType::InDiff,
            "a mechanism citing a genuinely added line keeps the in-diff claim"
        );
        Ok(())
    }

    #[test]
    fn a_finding_citing_a_tool_served_line_becomes_a_solo_finding()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let mut served = crate::contracts::ServedReads::new();
        served.insert("src/a.rs".to_owned(), vec![(800, 1200)]);
        let authority = CiteAuthority {
            index: &index,
            snapshots: &BTreeMap::new(),
            served: &served,
        };
        let finding = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("tests".to_owned()),
            title: "The converter branch has no per-action pin".to_owned(),
            mechanism: "src/a.rs:950 books the action without a test".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 950,
            }],
            severity: Severity::Suggestion,
            fix_hint: None,
        });
        let reports = [JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![RegionReport {
                region: RegionId(0),
                note: "traced".to_owned(),
                cells: vec![(AspectId("tests".to_owned()), CellVerdict::Defect(finding))],
            }],
        }];
        let verdict = reconcile(
            &reports,
            &plan(),
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        assert!(
            verdict.weak.is_empty() && verdict.dropped.is_empty(),
            "the served-read cite grounds the finding instead of demoting it: {:?} {:?}",
            verdict.weak,
            verdict.dropped
        );
        assert_eq!(
            verdict.solo.len(),
            1,
            "a grounded single-judge finding surfaces as solo, not as a weak lead"
        );
        Ok(())
    }

    #[test]
    fn a_failed_region_shortens_coverage_and_names_its_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut two_file_plan = plan();
        two_file_plan.regions.push(Region {
            file: "src/b.rs".to_owned(),
            start: 1,
            end: 2,
            kind: RegionKind::Added,
        });
        two_file_plan.cells = vec![
            (RegionId(0), AspectId("error-paths".to_owned())),
            (RegionId(1), AspectId("error-paths".to_owned())),
        ];
        let index = DiffIndex::parse(DIFF)?;
        let authority = CiteAuthority {
            index: &index,
            snapshots: &BTreeMap::new(),
            served: &SERVED,
        };
        let whole = JudgeReport {
            judge: "zai:glm-5.3".to_owned(),
            failed: Vec::new(),
            stats: Vec::new(),
            regions: vec![
                RegionReport {
                    region: RegionId(0),
                    note: "traced".to_owned(),
                    cells: vec![(AspectId("error-paths".to_owned()), CellVerdict::Clean)],
                },
                RegionReport {
                    region: RegionId(1),
                    note: "traced".to_owned(),
                    cells: vec![(AspectId("error-paths".to_owned()), CellVerdict::Clean)],
                },
            ],
        };
        let mut stalled = whole.clone();
        stalled.judge = "zai:glm-5.3-flash".to_owned();
        stalled.regions.pop();
        stalled.failed = vec![RegionId(1)];
        let verdict = reconcile(
            &[whole, stalled],
            &two_file_plan,
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["zai:glm-5.3".to_owned(), "zai:glm-5.3-flash".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        assert_eq!(
            verdict.coverage.cells_judged, 2,
            "the threshold still counts the one judge that finished"
        );
        assert_eq!(
            verdict.coverage.cells_short, 1,
            "the cell the stalled judge never judged is named, not silently clean"
        );
        let b_file = verdict
            .coverage
            .per_file
            .get("src/b.rs")
            .ok_or("per-file row missing")?;
        assert_eq!(b_file.short, 1, "the audit trail carries the short cell");
        let rendered =
            crate::render::console::render(&verdict, &crate::render::RenderOptions::default());
        assert!(
            rendered.contains("1 cell short of the full panel across 1 file: src/b.rs"),
            "the report names the unjudged file: {rendered}"
        );
        assert!(
            rendered.contains("src/b.rs 1/1 clean [1 short]"),
            "the per-file line marks the short cell: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn a_solo_finding_never_blocks_at_any_severity() -> Result<(), Box<dyn std::error::Error>> {
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Critical));
        let reports = [report("a", vec![defect(&finding)])];
        let verdict = drive(&reports, &["a"], &state(), &OracleResults::default(), &[])?;
        assert_eq!(verdict.solo.len(), 1);
        assert_eq!(verdict.blocking_count(), 0);
        let tools = [ToolFinding {
            tool: "clippy".to_owned(),
            file: "src/a.rs".to_owned(),
            line: 3,
            severity: Severity::Warning,
            message: "needless borrow".to_owned(),
        }];
        let with_tool = drive(
            &reports,
            &["a"],
            &state(),
            &OracleResults::default(),
            &tools,
        )?;
        assert_eq!(
            with_tool.blocking_count(),
            1,
            "a warning tool finding blocks on its own severity"
        );
        Ok(())
    }

    #[test]
    fn external_assumptions_never_block_and_are_labeled() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut assumption = draft(
            "Config file is ignored when present",
            3,
            Severity::Suggestion,
        );
        assumption.claim = ClaimType::ExternalAssumption;
        let finding = Finding::new(assumption);
        let reports = [
            report("a", vec![defect(&finding)]),
            report("b", vec![defect(&finding)]),
        ];
        let verdict = drive(
            &reports,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(verdict.confirmed.len(), 1);
        assert!(
            verdict
                .confirmed
                .first()
                .is_some_and(|entry| entry.severity == Severity::Suggestion
                    && entry.finding.claim == ClaimType::ExternalAssumption)
        );
        assert_eq!(verdict.blocking_count(), 0);
        Ok(())
    }

    #[test]
    fn two_judges_on_a_drifted_line_confirm_one_finding() -> Result<(), Box<dyn std::error::Error>>
    {
        let left = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let right = Finding::new(draft(
            "the lock is dropped early again",
            6,
            Severity::Warning,
        ));
        let reports = [
            report("a", vec![defect(&left)]),
            report("b", vec![defect(&right)]),
        ];
        let verdict = drive(
            &reports,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(verdict.confirmed.len(), 1);
        assert!(
            verdict
                .confirmed
                .first()
                .is_some_and(|entry| entry.votes.len() == 2)
        );
        assert!(verdict.solo.is_empty() && verdict.dropped.is_empty());
        Ok(())
    }

    #[test]
    fn two_of_three_confirmations_block() -> Result<(), Box<dyn std::error::Error>> {
        let first = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let second = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let reports = [
            report("a", vec![defect(&first)]),
            report("b", vec![defect(&second)]),
            report("c", vec![]),
        ];
        let verdict = drive(
            &reports,
            &["a", "b", "c"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.confirmed.len(),
            1,
            "two of three judges carrying the finding is a quorum"
        );
        let confirmed = verdict.confirmed.first().ok_or("confirmed entry")?;
        assert_eq!(
            confirmed.votes.len(),
            2,
            "the clean third judge neither votes nor demotes"
        );
        assert_eq!(
            verdict.blocking_count(),
            1,
            "a warning-grade confirmation blocks the round"
        );
        assert!(verdict.solo.is_empty(), "nothing stays unconfirmed");
        Ok(())
    }

    #[test]
    fn severity_is_the_vote_median_then_ceiling_then_claim_cap()
    -> Result<(), Box<dyn std::error::Error>> {
        let odd = [
            report(
                "a",
                vec![defect(&Finding::new(draft(
                    "Lock is dropped too early",
                    3,
                    Severity::Warning,
                )))],
            ),
            report(
                "b",
                vec![defect(&Finding::new(draft(
                    "Lock is dropped too early",
                    3,
                    Severity::Critical,
                )))],
            ),
            report(
                "c",
                vec![defect(&Finding::new(draft(
                    "Lock is dropped too early",
                    3,
                    Severity::Warning,
                )))],
            ),
        ];
        let verdict = drive(
            &odd,
            &["a", "b", "c"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.confirmed.first().map(|entry| entry.severity),
            Some(Severity::Warning),
            "warning, critical, warning reconciles to the middle vote"
        );
        let split = [
            report(
                "a",
                vec![defect(&Finding::new(draft(
                    "Buffer grows unbounded",
                    4,
                    Severity::Nitpick,
                )))],
            ),
            report(
                "b",
                vec![defect(&Finding::new(draft(
                    "Buffer grows unbounded",
                    4,
                    Severity::Suggestion,
                )))],
            ),
        ];
        let even = drive(
            &split,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            even.confirmed.first().map(|entry| entry.severity),
            Some(Severity::Nitpick),
            "an even split takes the lower middle, never the harsher"
        );
        Ok(())
    }

    #[test]
    fn merged_groups_cap_by_the_lowest_id_member_and_ignore_report_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let ceiling = [
            report(
                "a",
                vec![defect(&Finding::new(aspected(
                    "Lock is dropped too early",
                    3,
                    Severity::Warning,
                    "error-paths",
                    ClaimType::InDiff,
                )))],
            ),
            report(
                "b",
                vec![defect(&Finding::new(aspected(
                    "Lock is dropped too early",
                    3,
                    Severity::Critical,
                    "error-paths",
                    ClaimType::InDiff,
                )))],
            ),
            report(
                "c",
                vec![defect(&Finding::new(aspected(
                    "the lock is dropped early",
                    3,
                    Severity::Suggestion,
                    "docs",
                    ClaimType::InDiff,
                )))],
            ),
        ];
        let capped = drive(
            &ceiling,
            &["a", "b", "c"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            capped.confirmed.first().map(|entry| entry.severity),
            Some(Severity::Suggestion),
            "the docs member holds the lowest id, so its Suggestion ceiling caps the Warning median"
        );
        let reordered = [ceiling[2].clone(), ceiling[0].clone(), ceiling[1].clone()];
        let permuted = drive(
            &reordered,
            &["a", "b", "c"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert!(
            equivalent(&capped, &permuted),
            "a pure reordering of the reports cannot change the verdict"
        );
        let claim = [
            report(
                "a",
                vec![defect(&Finding::new(aspected(
                    "anchor lock dropped early",
                    3,
                    Severity::Suggestion,
                    "error-paths",
                    ClaimType::ExternalAssumption,
                )))],
            ),
            report(
                "b",
                vec![defect(&Finding::new(aspected(
                    "buffer overflow dropped early here",
                    3,
                    Severity::Warning,
                    "error-paths",
                    ClaimType::InDiff,
                )))],
            ),
            report(
                "c",
                vec![defect(&Finding::new(aspected(
                    "critical lock early failure",
                    3,
                    Severity::Critical,
                    "error-paths",
                    ClaimType::InDiff,
                )))],
            ),
        ];
        let assumption = drive(
            &claim,
            &["a", "b", "c"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            assumption.confirmed.first().map(|entry| entry.severity),
            Some(Severity::Suggestion),
            "the assumption member holds the lowest id, so its claim caps the Warning median"
        );
        Ok(())
    }

    #[test]
    fn an_oracle_verdict_keyed_by_any_member_of_the_group_counts()
    -> Result<(), Box<dyn std::error::Error>> {
        let primary = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let secondary = Finding::new(aspected(
            "the lock is dropped early",
            3,
            Severity::Suggestion,
            "docs",
            ClaimType::InDiff,
        ));
        let cells = vec![
            (
                AspectId("error-paths".to_owned()),
                CellVerdict::Defect(primary.clone()),
            ),
            (
                AspectId("docs".to_owned()),
                CellVerdict::Defect(secondary.clone()),
            ),
        ];
        let reports = [report("a", cells)];
        assert!(
            secondary.id < primary.id,
            "the fixture's representative must not be the keyed member"
        );
        let mut promoted = OracleResults::default();
        promoted
            .verdicts
            .insert(primary.id.clone(), OracleVerdict::Confirmed);
        let verdict = drive(&reports, &["a"], &state(), &promoted, &[])?;
        assert_eq!(
            verdict.confirmed.len(),
            1,
            "a confirmation keyed by a non-representative member still promotes"
        );
        let mut refuting = OracleResults::default();
        refuting.verdicts.insert(
            primary.id,
            OracleVerdict::Refuted {
                command: "cargo test -p dt".to_owned(),
            },
        );
        let refuted = drive(&reports, &["a"], &state(), &refuting, &[])?;
        assert_eq!(refuted.refuted.len(), 1);
        Ok(())
    }

    #[test]
    fn an_absent_judge_leaves_cells_counted_uncovered_for_their_minority()
    -> Result<(), Box<dyn std::error::Error>> {
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let reports = [report("a", vec![defect(&finding)]), report("b", vec![])];
        let verdict = drive(
            &reports,
            &["a", "b", "c"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(verdict.absent_judges, vec!["c".to_owned()]);
        assert_eq!(verdict.coverage.cells_total, 1);
        assert_eq!(
            verdict.coverage.cells_judged, 0,
            "one verdict of a three-judge panel is a minority"
        );
        assert_eq!(verdict.coverage.cells_defect, 0);
        let lone = drive(
            &[report("a", vec![])],
            &["a"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            lone.coverage.cells_judged, 0,
            "a sole judge skipping a cell leaves it uncovered"
        );
        Ok(())
    }

    #[test]
    fn repeated_judge_ids_list_incomplete_once() -> Result<(), Box<dyn std::error::Error>> {
        let mut first_pass = report("b", vec![]);
        first_pass.failed = vec![RegionId(0)];
        let mut second_pass = report("b", vec![]);
        second_pass.failed = vec![RegionId(0)];
        let reports = [
            report(
                "a",
                vec![(AspectId("error-paths".to_owned()), CellVerdict::Clean)],
            ),
            first_pass,
            second_pass,
        ];
        let verdict = drive(
            &reports,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.incomplete,
            vec!["b".to_owned()],
            "two passes of one judge id list one incomplete entry, not two"
        );
        Ok(())
    }

    #[test]
    fn a_judge_with_failed_regions_is_incomplete_and_the_round_partial()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut holes = report("b", vec![]);
        holes.failed = vec![RegionId(0)];
        let reports = [
            report(
                "a",
                vec![(AspectId("error-paths".to_owned()), CellVerdict::Clean)],
            ),
            holes,
        ];
        let verdict = drive(
            &reports,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.incomplete,
            vec!["b".to_owned()],
            "the judge's other regions still count; the failure is named, not fatal"
        );
        assert!(
            verdict.absent_judges.is_empty(),
            "a partially-answering judge is not absent"
        );
        assert_eq!(
            verdict.coverage.cells_judged, 1,
            "the surviving judge's verdict still covers the cell at the two-judge threshold"
        );
        Ok(())
    }

    #[test]
    fn a_suppression_match_removes_the_finding_from_voting()
    -> Result<(), Box<dyn std::error::Error>> {
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let reports = [
            report("a", vec![defect(&finding)]),
            report("b", vec![defect(&finding)]),
        ];
        let mut st = state();
        st.suppressions.push(Suppression {
            id: "sup-1".to_owned(),
            theme: "the lock is dropped too early".to_owned(),
            file_glob: "**/*.rs".to_owned(),
            reason: "accepted risk".to_owned(),
            created: "base".to_owned(),
            hits: 0,
        });
        let verdict = drive(&reports, &["a", "b"], &st, &OracleResults::default(), &[])?;
        assert!(
            verdict.confirmed.is_empty(),
            "the owner's suppression outranks two votes"
        );
        assert_eq!(verdict.suppressed.len(), 1);
        assert!(
            verdict
                .state
                .suppressions
                .first()
                .is_some_and(|rule| rule.hits == 1)
        );
        Ok(())
    }

    #[test]
    fn a_citation_failure_lands_in_weak_not_dropped() -> Result<(), Box<dyn std::error::Error>> {
        // An unanchorable cite is a weak lead, not fabrication — it
        // stays visible and non-blocking.
        let plan = plan();
        let state = state();
        let aspects = crate::aspects::AspectRegistry::new();
        let index = DiffIndex::parse(DIFF)?;
        let snapshots = BTreeMap::new();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("concurrency".to_owned()),
            title: "Buffer grows unbounded".to_owned(),
            mechanism: "The vec at src/other.rs:99 grows without bound".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/other.rs".to_owned(),
                line: 99,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        });
        let reports = vec![report("a", vec![defect(&finding)])];
        let verdict = reconcile(
            &reports,
            &plan,
            &state,
            &OracleResults::default(),
            &aspects,
            &authority,
            &["a".to_owned()],
            &ToolEvidence::default(),
        );
        assert_eq!(
            verdict.weak.len(),
            1,
            "the unanchorable finding survives as a weak lead"
        );
        assert!(
            verdict.dropped.is_empty(),
            "a citation failure never drops silently"
        );
        assert!(verdict.confirmed.is_empty(), "a weak lead never blocks");
        Ok(())
    }

    #[test]
    fn a_structural_failure_still_drops_not_weakens() -> Result<(), Box<dyn std::error::Error>> {
        // Fabrication guards drop one-way; anchoring gaps weaken.
        let plan = plan();
        let state = state();
        let aspects = crate::aspects::AspectRegistry::new();
        let index = DiffIndex::parse(DIFF)?;
        let snapshots = BTreeMap::new();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("concurrency".to_owned()),
            title: "   ".to_owned(),
            mechanism: "cite src/a.rs:2".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 2,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        });
        let reports = vec![report("a", vec![defect(&finding)])];
        let verdict = reconcile(
            &reports,
            &plan,
            &state,
            &OracleResults::default(),
            &aspects,
            &authority,
            &["a".to_owned()],
            &ToolEvidence::default(),
        );
        assert!(
            verdict.weak.len() == 1
                && verdict
                    .weak
                    .first()
                    .is_some_and(|lead| lead.finding.title == "cite src/a.rs:2"),
            "a blank title with a usable mechanism repairs from the mechanism: {:?}",
            verdict.weak
        );
        assert!(
            verdict.dropped.is_empty(),
            "title shape never destroys the finding"
        );
        let empty_mechanism = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("concurrency".to_owned()),
            title: "   ".to_owned(),
            mechanism: "   ".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 2,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        });
        let bare = vec![report("a", vec![defect(&empty_mechanism)])];
        let verdict = reconcile(
            &bare,
            &plan,
            &state,
            &OracleResults::default(),
            &aspects,
            &authority,
            &["a".to_owned()],
            &ToolEvidence::default(),
        );
        assert!(
            verdict.weak.is_empty() && verdict.dropped.len() == 1,
            "nothing to repair from still drops: {:?} {:?}",
            verdict.weak,
            verdict.dropped
        );
        Ok(())
    }

    #[test]
    fn invalid_findings_are_dropped_not_voted() -> Result<(), Box<dyn std::error::Error>> {
        let outside = Finding::new(draft(
            "Cite hangs outside the materials",
            99,
            Severity::Warning,
        ));
        let reports = [
            report("a", vec![defect(&outside)]),
            report("b", vec![defect(&outside)]),
        ];
        let verdict = drive(
            &reports,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.weak.len(),
            1,
            "the unanchorable twins weaken into one collapsed lead, neither drops"
        );
        assert!(
            verdict
                .weak
                .first()
                .is_some_and(|entry| entry.reason == DropReason::CiteOutsideMaterials
                    && entry.also_at.len() == 1),
            "the twin's site rides the collapsed lead's site list"
        );
        assert!(
            verdict.dropped.is_empty(),
            "a citation miss is a lead, never a silent discard"
        );
        assert!(verdict.solo.is_empty() && verdict.confirmed.is_empty());
        assert_eq!(
            verdict.coverage.cells_defect, 0,
            "an invalid judgment never counts as a defect"
        );
        assert_eq!(verdict.blocking_count(), 0);
        Ok(())
    }

    #[test]
    fn an_oracle_confirmation_promotes_a_solo_and_refutation_never_demotes()
    -> Result<(), Box<dyn std::error::Error>> {
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let solo = [report("a", vec![defect(&finding)])];
        let mut promoted = OracleResults::default();
        promoted
            .verdicts
            .insert(finding.id.clone(), OracleVerdict::Confirmed);
        let verdict = drive(&solo, &["a"], &state(), &promoted, &[])?;
        assert_eq!(
            verdict.confirmed.len(),
            1,
            "an oracle confirmation promotes the solo vote"
        );
        let mut refuted = OracleResults::default();
        refuted.verdicts.insert(
            finding.id.clone(),
            OracleVerdict::Refuted {
                command: "cargo test -p dt".to_owned(),
            },
        );
        let pair = [
            report("a", vec![defect(&finding)]),
            report("b", vec![defect(&finding)]),
        ];
        let confirmed = drive(&pair, &["a", "b"], &state(), &refuted, &[])?;
        assert_eq!(confirmed.confirmed.len(), 1);
        assert!(
            confirmed.refuted.is_empty(),
            "the oracle never demotes a quorum confirmation"
        );
        let lone = drive(&solo, &["a"], &state(), &refuted, &[])?;
        assert_eq!(lone.refuted.len(), 1);
        Ok(())
    }

    #[test]
    fn the_verdict_state_is_the_post_round_state() -> Result<(), Box<dyn std::error::Error>> {
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let mut st = state();
        st.issues.push(Issue {
            identity: IssueIdentity {
                file: "src/a.rs".to_owned(),
                line: 3,
                title_tokens: title_tokens("Lock is dropped too early")
                    .into_iter()
                    .collect(),
            },
            first_seen: "old".to_owned(),
            last_seen: "old".to_owned(),
            status: IssueStatus::Open,
            misses: 1,
            votes_seen: Vec::new(),
        });
        let reports = [
            report("a", vec![defect(&finding)]),
            report("b", vec![defect(&finding)]),
        ];
        let verdict = drive(&reports, &["a", "b"], &st, &OracleResults::default(), &[])?;
        assert_eq!(verdict.state.round, 2);
        assert!(
            verdict
                .state
                .issues
                .first()
                .is_some_and(|issue| issue.last_seen == "head" && issue.misses == 0)
        );
        Ok(())
    }

    #[test]
    fn coverage_counts_out_of_scope_as_covered_and_per_judge_verdicts()
    -> Result<(), Box<dyn std::error::Error>> {
        let reports = [
            report(
                "a",
                vec![(AspectId("error-paths".to_owned()), CellVerdict::OutOfScope)],
            ),
            report(
                "b",
                vec![(AspectId("error-paths".to_owned()), CellVerdict::Clean)],
            ),
        ];
        let verdict = drive(
            &reports,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.coverage.cells_judged, 1,
            "out-of-scope counts as covered"
        );
        assert_eq!(verdict.coverage.cells_clean, 1);
        assert_eq!(verdict.coverage.cells_defect, 0);
        assert_eq!(
            verdict.coverage.judged_by_judge,
            BTreeMap::from([("a".to_owned(), 1), ("b".to_owned(), 1)])
        );
        Ok(())
    }

    #[test]
    fn per_file_coverage_counts_cells_judges_and_defects() -> Result<(), Box<dyn std::error::Error>>
    {
        let index = DiffIndex::parse(DIFF)?;
        let snapshots = BTreeMap::new();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let reports = [
            report(
                "a",
                vec![
                    defect(&finding),
                    (AspectId("docs".to_owned()), CellVerdict::Clean),
                ],
            ),
            report("b", vec![defect(&finding)]),
        ];
        let coverage = CoveragePlan {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            regions: vec![
                Region {
                    file: "src/a.rs".to_owned(),
                    start: 3,
                    end: 4,
                    kind: RegionKind::Added,
                },
                Region {
                    file: "docs/guide.md".to_owned(),
                    start: 1,
                    end: 2,
                    kind: RegionKind::Added,
                },
            ],
            cells: vec![
                (RegionId(0), AspectId("error-paths".to_owned())),
                (RegionId(1), AspectId("docs".to_owned())),
            ],
            narrowed: false,
        };
        let verdict = reconcile(
            &reports,
            &coverage,
            &state(),
            &OracleResults::default(),
            &AspectRegistry::new(),
            &authority,
            &["a".to_owned(), "b".to_owned(), "c".to_owned()],
            &crate::contracts::ToolEvidence::default(),
        );
        let rust = verdict.coverage.per_file.get("src/a.rs");
        let docs = verdict.coverage.per_file.get("docs/guide.md");
        assert!(
            rust.is_some_and(|entry| *entry
                == FileCoverage {
                    cells: 1,
                    judged: 1,
                    clean: 0,
                    defect: 1,
                    short: 0,
                })
                && docs.is_some_and(|entry| *entry
                    == FileCoverage {
                        cells: 1,
                        judged: 0,
                        clean: 0,
                        defect: 0,
                        short: 1,
                    }),
            "the judged defect cell and the minority-uncovered cell land in their files: {:?}",
            verdict.coverage.per_file
        );
        assert_eq!(verdict.coverage.regions, 2);
        assert_eq!(
            verdict.coverage.cells_short, 1,
            "the docs cell one judge skipped is counted short, not silently clean"
        );
        Ok(())
    }
    #[test]
    fn a_profile_duplicated_across_two_panel_entries_carries_one_vote()
    -> Result<(), Box<dyn std::error::Error>> {
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let reports = [
            report("style", vec![defect(&finding)]),
            report("style#2", vec![defect(&finding)]),
        ];
        let verdict = drive(
            &reports,
            &["style", "style#2"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert!(
            verdict.confirmed.is_empty(),
            "the same profile twice is not a quorum"
        );
        assert_eq!(verdict.solo.len(), 1);
        assert_eq!(
            verdict.solo.first().map(|entry| entry.votes.len()),
            Some(2),
            "both votes are still recorded — the dedup weights, it does not erase"
        );
        Ok(())
    }

    #[test]
    fn two_distinct_profiles_meet_quorum_where_a_duplicate_does_not()
    -> Result<(), Box<dyn std::error::Error>> {
        let finding = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let pair = [
            report("style", vec![defect(&finding)]),
            report("correctness", vec![defect(&finding)]),
        ];
        let verdict = drive(
            &pair,
            &["style", "correctness"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.confirmed.len(),
            1,
            "two distinct profiles meet quorum"
        );
        Ok(())
    }

    #[test]
    fn the_severity_median_is_profile_weighted_not_replica_weighted()
    -> Result<(), Box<dyn std::error::Error>> {
        let loud = Finding::new(draft("Lock is dropped too early", 3, Severity::Critical));
        let calm = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let reports = [
            report("style", vec![defect(&loud)]),
            report("style#2", vec![defect(&loud)]),
            report("style#3", vec![defect(&loud)]),
            report("correctness", vec![defect(&calm)]),
        ];
        let verdict = drive(
            &reports,
            &["style", "style#2", "style#3", "correctness"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        let severity = verdict.confirmed.first().map(|entry| entry.severity);
        assert_eq!(
            severity,
            Some(Severity::Warning),
            "three replicas of one profile must not outvote one distinct dissent"
        );
        Ok(())
    }

    #[test]
    fn the_fold_is_insensitive_to_judge_completion_order() -> Result<(), Box<dyn std::error::Error>>
    {
        let first = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let second = Finding::new(draft("The cache grows unbounded", 6, Severity::Warning));
        let one_way = [
            report("a", vec![defect(&first)]),
            report("b", vec![defect(&second)]),
        ];
        let other_way = [
            report("b", vec![defect(&second)]),
            report("a", vec![defect(&first)]),
        ];
        let left = drive(
            &one_way,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        let right = drive(
            &other_way,
            &["a", "b"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(left.solo.len(), 2, "two distinct defects stay two solos");
        assert_eq!(
            left.state.issues.len(),
            2,
            "both defects open issues in the folded state"
        );
        assert!(
            equivalent(&left, &right),
            "completion order must not change a verdict: state order differs — {:?} vs {:?}",
            left.state.issues,
            right.state.issues
        );
        Ok(())
    }

    #[test]
    fn one_judges_dropped_instance_never_erases_another_valid_vote()
    -> Result<(), Box<dyn std::error::Error>> {
        let grounded = Finding::new(draft("Lock is dropped too early", 3, Severity::Warning));
        let mut loose = FindingDraft {
            mechanism: "the loop simply never terminates on its own".to_owned(),
            ..draft("Lock is dropped too early", 3, Severity::Warning)
        };
        loose.mechanism = "the loop simply never terminates on its own".to_owned();
        let ungrounded = Finding::new(loose);
        let reports = [
            report("a", vec![defect(&ungrounded)]),
            report("b", vec![defect(&grounded)]),
            report("c", vec![defect(&grounded)]),
        ];
        let verdict = drive(
            &reports,
            &["a", "b", "c"],
            &state(),
            &OracleResults::default(),
            &[],
        )?;
        assert_eq!(
            verdict.weak.len(),
            1,
            "the ungrounded instance weakens into a lead, never dropping"
        );
        assert_eq!(
            verdict.confirmed.len(),
            1,
            "the grounded twins still reach quorum: {:?}",
            verdict.solo
        );
        assert_eq!(
            verdict.coverage.cells_defect, 1,
            "coverage counts the cell as a defect, not clean, beside the confirmed list"
        );
        let defects = verdict
            .coverage
            .per_file
            .get("src/a.rs")
            .map_or(0, |entry| entry.defect);
        assert_eq!(defects, 1, "the per-file table agrees: {verdict:?}");
        Ok(())
    }

    #[test]
    fn a_profile_id_is_the_name_before_any_replica_suffix() {
        assert_eq!(judge_profile("style"), "style");
        assert_eq!(judge_profile("style#2"), "style");
    }
}
