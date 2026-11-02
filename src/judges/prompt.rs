//! The judge prompt — the only system prompt — and the per-region
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

pub const REPORT_SCHEMA: &str = r#"{"type":"object","properties":{"region":{"type":"integer"},"note":{"type":"string"},"cells":{"type":"array","items":{"type":"object","properties":{"aspect":{"type":"string"},"verdict":{"enum":["clean","defect","out-of-scope"]},"finding":{"type":"object","properties":{"title":{"type":"string"},"mechanism":{"type":"string"},"claim":{"enum":["in-diff","needs-execution","external-assumption"]},"severity":{"enum":["nitpick","suggestion","warning","critical"]},"evidence":{"type":"array","items":{"type":"string","description":"path:line"}},"fix_hint":{"type":"string"}},"required":["title","mechanism","claim","severity","evidence"]}},"required":["aspect","verdict"]}}},"required":["region","note","cells"]}"#;

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
    let window = snapshot_window(content);
    if window.head >= window.total {
        return content.to_owned();
    }
    let lines: Vec<&str> = content.lines().collect();
    let head = lines
        .get(..window.head.min(window.total) as usize)
        .map(|slice| slice.join("\n"))
        .unwrap_or_default();
    let tail_start = window.tail_start.min(window.total) as usize;
    let tail = lines
        .get(tail_start..)
        .map(|slice| slice.join("\n"))
        .unwrap_or_default();
    let shown = window
        .head
        .saturating_add(window.total.saturating_sub(window.tail_start));
    let hidden = window.total.saturating_sub(shown);
    format!("{head}\n... [{hidden} lines truncated] ...\n{tail}")
}

pub const SNAPSHOT_BUDGET_BYTES: usize = 32 * 1024;

#[must_use]
pub fn cap_material_bytes(material: &str, budget: usize) -> String {
    if material.len() <= budget {
        return material.to_owned();
    }
    let mut cut = budget;
    while !material.is_char_boundary(cut) && cut > 0 {
        cut = cut.saturating_sub(1);
    }
    let kept = material.get(..cut).unwrap_or("");
    format!("{kept}\n… [region materials cut at the {budget}-byte budget] …")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotWindow {
    pub total: u32,
    pub head: u32,
    pub tail_start: u32,
}

impl SnapshotWindow {
    #[must_use]
    pub fn whole(total: u32) -> Self {
        Self {
            total,
            head: total,
            tail_start: total,
        }
    }

    #[must_use]
    pub fn line_window(total: u32) -> Self {
        let budget = u32::try_from(SNAPSHOT_BUDGET_LINES).unwrap_or(u32::MAX);
        let half = u32::try_from(SNAPSHOT_HALF).unwrap_or(u32::MAX);
        if total <= budget {
            return Self::whole(total);
        }
        Self {
            total,
            head: half,
            tail_start: total.saturating_sub(half),
        }
    }
}

#[must_use]
pub fn snapshot_admits(window: SnapshotWindow, line: u32) -> bool {
    line >= 1 && line <= window.total && (line <= window.head || line > window.tail_start)
}

fn shrink_halving(count: u32) -> u32 {
    count.div_ceil(2).min(count.saturating_sub(1))
}

#[must_use]
pub fn snapshot_window(content: &str) -> SnapshotWindow {
    let lines: Vec<&str> = content.lines().collect();
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let mut window = SnapshotWindow::line_window(total);
    if window.head >= window.total {
        let fits = content.len() <= SNAPSHOT_BUDGET_BYTES;
        if fits {
            return window;
        }
        window = SnapshotWindow {
            total,
            head: u32::try_from(SNAPSHOT_HALF).unwrap_or(u32::MAX),
            tail_start: total.saturating_sub(u32::try_from(SNAPSHOT_HALF).unwrap_or(u32::MAX)),
        };
    }
    let mut head = window.head.min(total);
    let mut tail_start = window.tail_start;
    while head > 0 || tail_start < total {
        let head_bytes = lines
            .get(..head as usize)
            .map(|slice| slice.join("\n").len())
            .unwrap_or_default();
        let tail_bytes = lines
            .get(tail_start as usize..)
            .map(|slice| slice.join("\n").len())
            .unwrap_or_default();
        if head_bytes.saturating_add(tail_bytes) <= SNAPSHOT_BUDGET_BYTES {
            break;
        }
        head = shrink_halving(head);
        let tail_count = total.saturating_sub(tail_start);
        let tail_count = shrink_halving(tail_count);
        tail_start = total.saturating_sub(tail_count);
        if head == 0 && tail_count == 0 {
            break;
        }
    }
    SnapshotWindow {
        total,
        head,
        tail_start,
    }
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
    untracked: &str,
) -> String {
    let names = aspects
        .iter()
        .map(|aspect| aspect.0.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let material = cap_material_bytes(diff_context, crate::diff::REGION_MATERIAL_BUDGET_BYTES);
    let mut out = format!(
        "Region {index} is {} lines {}-{} ({}). Aspects to judge: {names}.\n\nRegion diff with context lines:\n{material}\n\nThe file at the pinned head, truncated to {SNAPSHOT_BUDGET_LINES} lines:\n{snapshot}\n\n",
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
    if !untracked.is_empty() {
        out.push_str(untracked);
        out.push_str("\n\n");
    }
    out.push_str("Return one report for this region exactly per this schema:\n");
    out.push_str(schema);
    out.push('\n');
    out
}

#[must_use]
pub fn untracked_block(files: &[String]) -> String {
    const LISTED: usize = 50;
    if files.is_empty() {
        return String::new();
    }
    let listed: Vec<&str> = files.iter().take(LISTED).map(String::as_str).collect();
    let mut block = format!(
        "Working-tree files NOT tracked at head (a tracked file referencing one of these names a file the branch does not carry — that is a defect, cite the referencing line):\n{}\n",
        listed.join("\n")
    );
    if files.len() > LISTED {
        let more = files.len().saturating_sub(LISTED);
        block.push_str("(and ");
        block.push_str(&more.to_string());
        block.push_str(" more, truncated)\n");
    }
    block
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
            "the v1 pressure layer stays out of the judge prompt"
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
    fn the_snapshot_window_admits_only_its_own_lines() {
        let whole = SnapshotWindow::line_window(400);
        assert!(snapshot_admits(whole, 1) && snapshot_admits(whole, 400));
        assert!(
            !snapshot_admits(whole, 401) && !snapshot_admits(whole, 0),
            "a file that fits the budget whole shows exactly its own lines"
        );
        let split = SnapshotWindow::line_window(1000);
        assert!(
            snapshot_admits(split, 200) && snapshot_admits(split, 801),
            "the head and tail windows stay visible"
        );
        assert!(
            !snapshot_admits(split, 201) && !snapshot_admits(split, 800),
            "the truncated middle never validates a cite"
        );
        assert!(
            snapshot_admits(split, 1000) && !snapshot_admits(split, 1001),
            "the file's own last line is the tail window's edge"
        );
    }

    #[test]
    fn the_byte_shrink_halves_instead_of_stepping_one_line() {
        assert_eq!(shrink_halving(100), 50, "100 halves to 50, not to 99");
        assert_eq!(shrink_halving(101), 51);
        assert_eq!(shrink_halving(3), 2);
        assert_eq!(shrink_halving(2), 1);
        assert_eq!(shrink_halving(1), 0);
        assert_eq!(shrink_halving(0), 0);
    }

    #[test]
    fn a_snapshot_of_very_long_lines_shrinks_by_bytes() {
        let fat_line = "x".repeat(400);
        let content = (0..300)
            .map(|index| format!("{index}{fat_line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = snapshot_window(&content);
        assert!(
            window.head < 300 && window.tail_start > 0,
            "300 fat lines exceed the byte budget, so the window narrows past the line cap: {window:?}"
        );
        let shown = truncate_snapshot(&content);
        assert!(
            shown.len() <= SNAPSHOT_BUDGET_BYTES.saturating_add(200),
            "the rendered snapshot respects the byte budget: {} bytes",
            shown.len()
        );
        assert!(
            !snapshot_admits(window, 150),
            "a middle line the judge never saw never validates a cite"
        );
        assert!(
            snapshot_admits(window, 1) || snapshot_admits(window, 300),
            "an edge line stays visible"
        );
    }

    #[test]
    fn a_short_fat_file_shrinks_by_bytes_below_the_line_cap() {
        let fat_line = "x".repeat(400);
        let content = (0..150)
            .map(|index| format!("{index}{fat_line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = snapshot_window(&content);
        assert!(
            window.head < window.total,
            "150 fat lines exceed the byte budget below the line cap, so the window narrows: {window:?}"
        );
        let shown = truncate_snapshot(&content);
        assert!(
            shown.len() <= SNAPSHOT_BUDGET_BYTES.saturating_add(200),
            "the rendered snapshot respects the byte budget under the line cap: {} bytes",
            shown.len()
        );
        assert!(
            !snapshot_admits(window, 75),
            "a middle line the judge never saw never validates a cite"
        );
        assert!(
            snapshot_admits(window, 1) || snapshot_admits(window, 150),
            "an edge line stays visible"
        );
    }

    #[test]
    fn oversized_region_material_ships_capped_with_a_marker()
    -> Result<(), Box<dyn std::error::Error>> {
        let budget = crate::diff::REGION_MATERIAL_BUDGET_BYTES;
        let whole = format!("+{}", "y".repeat(budget.saturating_mul(2)));
        let capped = cap_material_bytes(&whole, budget);
        assert!(
            capped.len() <= budget.saturating_add(64),
            "the cut keeps the material at the budget plus its marker: {}",
            capped.len()
        );
        assert!(
            capped.contains("… [region materials cut at the"),
            "the cut names itself: {capped}"
        );
        let prompt = region_prompt(
            &region(),
            0,
            &whole,
            "fn a() {}",
            None,
            &[AspectId("error-paths".to_owned())],
            REPORT_SCHEMA,
            "",
        );
        let at = prompt
            .find("region materials cut at the")
            .ok_or("the prompt carries the cut marker")?;
        assert!(
            at <= budget.saturating_add(200),
            "a single unsplittable line cannot ride the prompt whole: {at}"
        );
        Ok(())
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
            "",
        );
        assert!(prompt.contains("Region 0 is src/a.rs lines 1-4 (added lines)"));
        assert!(prompt.contains("Aspects to judge: error-paths."));
        assert!(prompt.contains("+fn a() {}"));
        assert!(prompt.contains("No flow map for this region kind."));
        assert!(prompt.contains("exactly per this schema:"));
        assert!(prompt.contains("\"verdict\":{\"enum\":[\"clean\",\"defect\",\"out-of-scope\"]}"));
        assert!(
            !prompt.contains("NOT tracked at head"),
            "an empty untracked list adds no block"
        );
        let with_untracked = region_prompt(
            &region(),
            0,
            "@@ ctx @@\n+fn a() {}",
            "fn a() {}",
            None,
            &[AspectId("error-paths".to_owned())],
            REPORT_SCHEMA,
            &untracked_block(&["example.config.toml".to_owned()]),
        );
        assert!(
            with_untracked.contains("NOT tracked at head")
                && with_untracked.contains("example.config.toml"),
            "the untracked list rides the region materials: {with_untracked}"
        );
        let many: Vec<String> = (0..60).map(|index| format!("file{index}.tmp")).collect();
        let block = untracked_block(&many);
        assert!(
            block.contains("10 more, truncated"),
            "the untracked list caps at the listed bound: {block}"
        );
    }
}
