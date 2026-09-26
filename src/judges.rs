//! The judge panel: every region judged per aspect, findings confirmed by quorum.

pub mod flow;
pub mod pass;
pub mod prompt;
pub mod provider;
pub mod tools;

use std::collections::BTreeSet;

use crate::aspects::AspectRegistry;
use crate::contracts::{
    AspectId, Cell, CoveragePlan, PinnedRange, Region, RegionId, RegionKind, Severity,
};
use crate::error::DtError;

pub const JUDGE_QUORUM: usize = 2;

const ESTIMATE_LINES_CAP: u32 = 2000;
const TOKENS_PER_LINE: u64 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanSummary {
    pub regions: usize,
    pub cells: usize,
    pub judge_calls: usize,
    pub est_tokens: u64,
    pub narrowed: bool,
}

pub fn plan(
    range: PinnedRange,
    regions: &[Region],
    aspects: &AspectRegistry,
    focus: &[AspectId],
    severity: Severity,
) -> Result<CoveragePlan, DtError> {
    for id in focus {
        if !aspects.contains(id) {
            return Err(DtError::Config(format!(
                "focus names unknown aspect {:?}",
                id.0
            )));
        }
    }
    let mut cells: Vec<Cell> = Vec::new();
    let mut narrowed = false;
    for (index, region) in regions.iter().enumerate() {
        let mut set = aspects.aspects_for(&region.file);
        set.sort_by(|a, b| a.0.cmp(&b.0));
        set.dedup();
        for aspect in set {
            if !focus.is_empty() && !focus.iter().any(|wanted| wanted == &aspect) {
                narrowed = true;
                continue;
            }
            if aspects.ceiling(&aspect) < severity {
                narrowed = true;
                continue;
            }
            let cell = (RegionId(index), aspect);
            if !cells.contains(&cell) {
                cells.push(cell);
            }
        }
    }
    Ok(CoveragePlan {
        range,
        regions: regions.to_vec(),
        cells,
        narrowed,
    })
}

#[must_use]
pub fn summarize(coverage: &CoveragePlan, judges: usize) -> PlanSummary {
    let called: BTreeSet<usize> = coverage.cells.iter().map(|(region, _)| region.0).collect();
    let tokens = called
        .iter()
        .filter_map(|idx| coverage.regions.get(*idx))
        .map(|region| {
            let lines = region
                .end
                .saturating_sub(region.start)
                .saturating_add(1)
                .min(ESTIMATE_LINES_CAP);
            u64::from(lines).saturating_mul(TOKENS_PER_LINE)
        })
        .fold(0u64, u64::saturating_add);
    let judge_count = u64::try_from(judges).unwrap_or(u64::MAX);
    PlanSummary {
        regions: coverage.regions.len(),
        cells: coverage.cells.len(),
        judge_calls: called.len().saturating_mul(judges),
        est_tokens: tokens.saturating_mul(judge_count),
        narrowed: coverage.narrowed,
    }
}

#[must_use]
pub fn render_dry_run(summary: &PlanSummary, coverage: &CoveragePlan) -> String {
    let mut lines = Vec::new();
    let mut headline = format!("plan: {} regions, {} cells", summary.regions, summary.cells);
    if summary.narrowed {
        headline.push_str(" (narrowed)");
    }
    lines.push(headline);
    lines.push(format!(
        "calls: {} judge calls, ~{} tokens",
        summary.judge_calls, summary.est_tokens
    ));
    lines.push(format!(
        "range: {}..{}",
        coverage.range.base_sha, coverage.range.head_sha
    ));
    for (index, region) in coverage.regions.iter().enumerate() {
        let aspects = coverage
            .cells
            .iter()
            .filter(|(RegionId(cell_region), _)| *cell_region == index)
            .map(|(_, aspect)| aspect.0.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "  {}. {}:{}-{} [{}] aspects: {}",
            index.saturating_add(1),
            region.file,
            region.start,
            region.end,
            region_kind(region.kind),
            aspects
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn region_kind(kind: RegionKind) -> &'static str {
    match kind {
        RegionKind::Added => "added",
        RegionKind::Hunk => "hunk",
        RegionKind::File => "file",
        RegionKind::Repo => "repo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_from(toml: &str) -> Result<AspectRegistry, DtError> {
        let config = crate::config::DifftraceConfig::from_toml_str(toml)?;
        AspectRegistry::from_config(&config.aspects)
    }

    fn ids(values: &[&str]) -> Vec<AspectId> {
        values.iter().map(|id| AspectId((*id).to_owned())).collect()
    }

    fn region(file: &str, start: u32, end: u32) -> Region {
        Region {
            file: file.to_owned(),
            start,
            end,
            kind: RegionKind::Hunk,
        }
    }

    fn range() -> PinnedRange {
        PinnedRange {
            base_sha: "base".to_owned(),
            head_sha: "head".to_owned(),
        }
    }

    fn cell_names(coverage: &CoveragePlan) -> Vec<String> {
        coverage
            .cells
            .iter()
            .map(|(RegionId(index), aspect)| format!("{index}:{}", aspect.0))
            .collect()
    }

    #[test]
    fn cells_partition_stably_and_deterministically() -> Result<(), DtError> {
        let registry = registry_from(
            "[aspects]\ndefault = [\"error-paths\", \"docs\"]\n\n[aspects.paths.\"*.md\"]\nreplace = [\"docs\"]\n",
        )?;
        let regions = vec![
            region("src/a.rs", 1, 10),
            region("guide.md", 1, 5),
            region("src/b.rs", 1, 5),
        ];
        let first = plan(range(), &regions, &registry, &[], Severity::Nitpick)?;
        let second = plan(range(), &regions, &registry, &[], Severity::Nitpick)?;
        assert_eq!(first, second, "identical inputs yield identical plans");
        assert_eq!(
            cell_names(&first),
            vec![
                "0:docs".to_owned(),
                "0:error-paths".to_owned(),
                "1:docs".to_owned(),
                "2:docs".to_owned(),
                "2:error-paths".to_owned(),
            ],
            "cells order by region index then aspect id; per-path overrides replace the set"
        );
        Ok(())
    }

    #[test]
    fn a_narrowed_plan_is_marked_and_never_affects_state() -> Result<(), DtError> {
        let registry = registry_from("[aspects]\ndefault = [\"error-paths\", \"docs\"]\n")?;
        let regions = vec![region("src/a.rs", 1, 10)];
        let full = plan(range(), &regions, &registry, &[], Severity::Nitpick)?;
        assert!(!full.narrowed, "an uncut plan is not narrowed");
        let focused = plan(
            range(),
            &regions,
            &registry,
            &ids(&["docs"]),
            Severity::Nitpick,
        )?;
        assert!(focused.narrowed, "a focus cut marks the plan narrowed");
        assert_eq!(focused.cells.len(), 1);
        let floored = plan(range(), &regions, &registry, &[], Severity::Critical)?;
        assert!(floored.narrowed, "a severity cut marks the plan narrowed");
        assert_eq!(floored.cells.len(), 1, "docs cannot clear a Critical floor");
        let empty = plan(range(), &[], &registry, &[], Severity::Nitpick)?;
        assert!(!empty.narrowed, "an empty plan from no regions cut nothing");
        Ok(())
    }

    #[test]
    fn an_unknown_focus_id_errors_naming_it() -> Result<(), DtError> {
        let registry = registry_from("[aspects]\ndefault = [\"error-paths\"]\n")?;
        let regions = vec![region("src/a.rs", 1, 10)];
        let outcome = plan(
            range(),
            &regions,
            &registry,
            &ids(&["nope"]),
            Severity::Nitpick,
        );
        assert!(
            outcome.is_err_and(|err| err.to_string().contains("nope")),
            "an unknown focus id fails closed instead of emptying the plan"
        );
        Ok(())
    }

    #[test]
    fn severity_cuts_cells_below_the_floor() -> Result<(), DtError> {
        let registry =
            registry_from("[aspects]\ndefault = [\"error-paths\", \"tests\", \"docs\"]\n")?;
        let regions = vec![region("src/a.rs", 1, 10)];
        let warned = plan(range(), &regions, &registry, &[], Severity::Warning)?;
        assert_eq!(
            cell_names(&warned),
            vec!["0:error-paths".to_owned(), "0:tests".to_owned()],
            "docs (Suggestion ceiling) cannot clear a Warning floor; equal ceilings stay"
        );
        Ok(())
    }

    #[test]
    fn the_estimate_is_capped_and_deterministic() -> Result<(), DtError> {
        let registry = registry_from("[aspects]\ndefault = [\"error-paths\"]\n")?;
        let regions = vec![region("all.rs", 1, u32::MAX)];
        let coverage = plan(range(), &regions, &registry, &[], Severity::Nitpick)?;
        let first = summarize(&coverage, 2);
        let second = summarize(&coverage, 2);
        assert_eq!(first, second);
        assert_eq!(first.judge_calls, 2, "one call per region per judge");
        assert_eq!(
            first.est_tokens, 48_000,
            "a repo-spanning region hits the line cap: 2000 x 12 tokens x 2 judges"
        );
        Ok(())
    }

    #[test]
    fn the_estimate_counts_only_regions_the_pass_will_call() -> Result<(), DtError> {
        let registry = registry_from(
            "[aspects]\ndefault = [\"error-paths\"]\n[aspects.paths.\"**/*.md\"]\nreplace = []\n",
        )?;
        let regions = vec![region("src/a.rs", 1, 10), region("README.md", 1, 10)];
        let coverage = plan(range(), &regions, &registry, &[], Severity::Nitpick)?;
        assert_eq!(
            coverage.cells.len(),
            1,
            "the replace-none override leaves the markdown region with no cells"
        );
        let summary = summarize(&coverage, 3);
        assert_eq!(summary.regions, 2, "the plan still reports both regions");
        assert_eq!(
            summary.judge_calls, 3,
            "the zero-cell region spends no call"
        );
        assert_eq!(
            summary.est_tokens, 360,
            "tokens count the called region only: 10 lines x 12 tokens x 3 judges"
        );
        Ok(())
    }
}
