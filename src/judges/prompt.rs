//! The judge prompt — the only system prompt in v2 — and the per-region
//! materials rendered in front of it.

use crate::contracts::{FlowMap, IssueStatus, Region, RegionKind, RoundState, SearchHit};

pub const SYSTEM_PROMPT: &str = "You are judge {name} in a review panel. You are given regions of a
change and, for each region, aspects to judge. For every (region,
aspect) pair return exactly one verdict.

For each region:
1. Read the region, its file at the pinned head, and its flow map —
   the callers, callees, and trigger sites that reach it. Judge the
   change along those flows: what each trigger now produces, and what
   the change alters for its callers. A verdict made in isolation from
   the flows is not a verdict.
2. For every aspect: decide — clean, defect, or out-of-scope.
3. \"Clean\" means you traced the aspect through the region's code and
   found no defect. Write one note per region saying what you traced.
4. A defect requires a mechanism you can walk end to end in the
   materials in front of you: the input or execution path, the exact
   lines, and the failure that follows. If you cannot walk it, it is
   not a defect.
5. Claim type: InDiff if the whole mechanism is visible in these
   materials; NeedsExecution if proving it requires running something;
   ExternalAssumption if it relies on code or conventions not in these
   materials. Never state an ExternalAssumption as fact.
6. Cite only lines that exist in the given materials.
7. A previous round left these issues open in this region: {issues}.
   Re-judge them on the current code: report again only if the defect
   still stands.
8. These themes are suppressed by the owner: {suppressions}. Do not
   report them again.
Return the structured report exactly per the schema. An all-clean
report is a fully successful outcome.";

pub const SNAPSHOT_BUDGET_LINES: usize = 400;
pub const REGION_CONTEXT_LINES: u32 = 10;
const SNAPSHOT_HALF: usize = 200;

pub const REPORT_SCHEMA: &str = r#"{"type":"object","properties":{"region":{"type":"integer"},"note":{"type":"string"},"cells":{"type":"array","items":{"type":"object","properties":{"aspect":{"type":"string"},"verdict":{"enum":["clean","defect","out-of-scope"]},"finding":{"type":"object","properties":{"title":{"type":"string"},"mechanism":{"type":"string"},"claim":{"enum":["in-diff","needs-execution","external-assumption"]},"severity":{"enum":["nitpick","suggestion","warning","critical"]},"evidence":{"type":"array","items":{"type":"string"}},"fix_hint":{"type":"string"}},"required":["title","mechanism","claim","severity","evidence"]}},"required":["aspect","verdict"]}}},"required":["region","note","cells"]}"#;

#[must_use]
pub fn system(judge: &str, issues: &str, suppressions: &str, definitions: &str) -> String {
    let body = interpolate(
        SYSTEM_PROMPT,
        &[
            ("{name}", judge),
            ("{issues}", issues),
            ("{suppressions}", suppressions),
        ],
    );
    format!("{body}\n\nAspect definitions:\n{definitions}\n")
}

fn interpolate(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    for (placeholder, value) in values {
        if let Some((before, after)) = rest.split_once(placeholder) {
            out.push_str(before);
            out.push_str(value);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

#[must_use]
pub fn open_issues_block(state: &RoundState, file: &str) -> String {
    let lines: Vec<String> = state
        .issues
        .iter()
        .filter(|issue| matches!(issue.status, IssueStatus::Open) && issue.identity.file == file)
        .map(|issue| {
            format!(
                "{}:{} {}",
                issue.identity.file,
                issue.identity.line,
                issue.identity.title_tokens.join(" ")
            )
        })
        .collect();
    if lines.is_empty() {
        "(none)".to_owned()
    } else {
        lines.join("; ")
    }
}

#[must_use]
pub fn suppressions_block(state: &RoundState, file: &str) -> String {
    let lines: Vec<String> = state
        .suppressions
        .iter()
        .filter(|suppression| crate::aspects::glob_matches(&suppression.file_glob, file))
        .map(|suppression| {
            format!(
                "{} ({}): {}",
                suppression.theme, suppression.file_glob, suppression.reason
            )
        })
        .collect();
    if lines.is_empty() {
        "(none)".to_owned()
    } else {
        lines.join("; ")
    }
}

#[must_use]
pub fn truncate_snapshot(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= SNAPSHOT_BUDGET_LINES {
        return content.to_owned();
    }
    let head = lines
        .get(..SNAPSHOT_HALF)
        .map(|slice| slice.join("\n"))
        .unwrap_or_default();
    let tail_start = lines.len().saturating_sub(SNAPSHOT_HALF);
    let tail = lines
        .get(tail_start..)
        .map(|slice| slice.join("\n"))
        .unwrap_or_default();
    let hidden = lines.len().saturating_sub(SNAPSHOT_BUDGET_LINES);
    format!("{head}\n... [{hidden} lines truncated] ...\n{tail}")
}

#[must_use]
pub fn region_prompt(
    region: &Region,
    index: usize,
    diff_context: &str,
    snapshot: &str,
    flow: Option<&FlowMap>,
    aspects: &[crate::contracts::AspectId],
    schema: &str,
) -> String {
    let names = aspects
        .iter()
        .map(|aspect| aspect.0.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = format!(
        "Region {index} is {} lines {}-{} ({}). Aspects to judge: {names}.\n\nRegion diff with context lines:\n{diff_context}\n\nThe file at the pinned head, truncated to {SNAPSHOT_BUDGET_LINES} lines:\n{snapshot}\n\n",
        region.file,
        region.start,
        region.end,
        kind_name(region.kind)
    );
    match flow {
        Some(map) => {
            out.push_str("Flow map reaching this region:\n");
            out.push_str(&flow_block(map));
            out.push_str("\n\n");
        }
        None => out.push_str("No flow map for this region kind.\n\n"),
    }
    out.push_str("Return one report for this region exactly per this schema:\n");
    out.push_str(schema);
    out.push('\n');
    out
}

fn kind_name(kind: RegionKind) -> &'static str {
    match kind {
        RegionKind::Added => "added lines",
        RegionKind::Hunk => "hunk",
        RegionKind::File => "whole file",
        RegionKind::Repo => "repository",
    }
}

fn flow_block(map: &FlowMap) -> String {
    let anchors = |hits: &[SearchHit]| {
        hits.iter()
            .map(|hit| format!("{}:{}", hit.file, hit.line))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "defines: {}\ncallers: {}\ncallees: {}\ntriggers: {}",
        map.defines.join(", "),
        anchors(&map.callers),
        map.callees.join(", "),
        anchors(&map.triggers)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{AspectId, RegionKind};
    use std::fmt::Write;

    fn region() -> Region {
        Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 4,
            kind: RegionKind::Added,
        }
    }

    #[test]
    fn the_prompt_contains_the_flow_and_clean_success_lines_and_no_argue_against() {
        let prompt = system(
            "alpha",
            "src/a.rs:2 stale debt",
            "tests (**): generated code",
            "error-paths: find the failure paths",
        );
        assert!(prompt.contains("You are judge alpha in a review panel."));
        assert!(prompt.contains("A verdict made in isolation from"));
        assert!(prompt.contains("a fully successful outcome."));
        assert!(prompt.contains("src/a.rs:2 stale debt"));
        assert!(prompt.contains("tests (**): generated code"));
        assert!(prompt.contains("Aspect definitions:\nerror-paths: find the failure paths"));
        assert!(
            !prompt.contains("argue against"),
            "the v1 pressure layer stays out of the v2 prompt"
        );
        assert!(
            !prompt.contains("{name}")
                && !prompt.contains("{issues}")
                && !prompt.contains("{suppressions}"),
            "every placeholder is interpolated"
        );
    }

    #[test]
    fn empty_state_blocks_read_as_none() {
        let state = RoundState {
            checkout: "difftrace".to_owned(),
            round: 1,
            issues: Vec::new(),
            suppressions: Vec::new(),
        };
        assert_eq!(open_issues_block(&state, "src/a.rs"), "(none)");
        assert_eq!(suppressions_block(&state, "src/a.rs"), "(none)");
    }

    #[test]
    fn placeholder_literals_inside_state_text_never_inject_another_block() {
        let prompt = system(
            "alpha",
            "src/a.rs:2 title says {suppressions} verbatim",
            "tests (**): generated {issues} code",
            "error-paths: find the failure paths",
        );
        assert!(prompt.contains("src/a.rs:2 title says {suppressions} verbatim"));
        assert!(prompt.contains("tests (**): generated {issues} code"));
        assert_eq!(
            prompt.matches("{suppressions}").count(),
            1,
            "only the literal from the title survives; no block is injected into it"
        );
        assert_eq!(prompt.matches("{issues}").count(), 1);
        assert_eq!(
            prompt.matches("generated").count(),
            1,
            "the suppressions text appears once, in its own block"
        );
    }

    #[test]
    fn region_materials_truncate_the_snapshot_head_and_tail() {
        let mut long = String::new();
        for index in 0..500 {
            let _unused = writeln!(long, "line{index}");
        }
        let truncated = truncate_snapshot(&long);
        assert!(truncated.contains("line0\n") && truncated.contains("line499"));
        assert!(truncated.contains("line199"));
        assert!(truncated.contains("[100 lines truncated]"));
        assert!(
            !truncated.contains("\nline200\n") && !truncated.contains("\nline299\n"),
            "the middle stays hidden"
        );
        let prompt = region_prompt(
            &region(),
            0,
            "@@ ctx @@\n+fn a() {}",
            "fn a() {}",
            None,
            &[AspectId("error-paths".to_owned())],
            REPORT_SCHEMA,
        );
        assert!(prompt.contains("Region 0 is src/a.rs lines 1-4 (added lines)"));
        assert!(prompt.contains("Aspects to judge: error-paths."));
        assert!(prompt.contains("+fn a() {}"));
        assert!(prompt.contains("No flow map for this region kind."));
        assert!(prompt.contains("exactly per this schema:"));
        assert!(prompt.contains("\"verdict\":{\"enum\":[\"clean\",\"defect\",\"out-of-scope\"]}"));
    }
}
