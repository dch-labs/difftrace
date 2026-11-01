//! Verdict rendering: the hosted comment, the console report, the JSON dump.

pub mod chat;
pub mod comment;
pub mod console;
pub mod json;

use crate::contracts::{
    ClaimType, DropReason, Finding, IssueStatus, OracleVerdict, RoundVerdict, Severity,
};

#[derive(Default)]
pub struct RenderOptions {
    pub fix_prompts: bool,
}

const INDENT: &str = "    ";

pub(crate) fn escape_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '['
                | ']'
                | '('
                | ')'
                | '!'
                | '<'
                | '>'
                | '`'
                | '*'
                | '_'
                | '#'
                | '~'
                | '|'
                | '&'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

pub(crate) fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Nitpick => "nitpick",
        Severity::Suggestion => "suggestion",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

fn anchor(finding: &Finding) -> String {
    finding.evidence.first().map_or_else(String::new, |cite| {
        format!(" — {}:{}", escape_inline(&cite.file), cite.line)
    })
}

fn claim_label(claim: ClaimType) -> &'static str {
    match claim {
        ClaimType::InDiff => "in-diff",
        ClaimType::AdjacentPreexisting => "adjacent-preexisting (pre-existing)",
        ClaimType::NeedsExecution => "needs-execution",
        ClaimType::ExternalAssumption => "external-assumption (assumption)",
    }
}

fn indent_block(text: &str) -> String {
    let mut out = String::new();
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(INDENT);
        out.push_str(line);
    }
    out
}

pub(crate) fn sections(verdict: &RoundVerdict, options: &RenderOptions) -> String {
    let mut blocks = vec![
        verdict_section(verdict),
        coverage_section(verdict),
        confirmed_section(verdict),
        tool_section(verdict),
        solo_section(verdict),
        leads_section(verdict),
        footer_section(verdict),
        state_section(verdict),
    ];
    if options.fix_prompts {
        blocks.push(fix_prompts_section(verdict));
    }
    let joined = blocks
        .into_iter()
        .filter(|block| !block.is_empty())
        .map(|block| block.join("\n"))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("{joined}\n")
}

fn verdict_section(verdict: &RoundVerdict) -> Vec<String> {
    let mut lines = vec!["## Verdict".to_owned()];
    let blocking = verdict.blocking_count();
    if blocking == 0 {
        lines.push("🟢 Good to go — no blocking findings".to_owned());
    } else {
        lines.push(format!(
            "🔴 Not good to go — {blocking} confirmed blocking {}",
            plural(blocking, "finding")
        ));
    }
    let solo = verdict.solo.len();
    let tools = verdict.tool_findings.len();
    if solo > 0 || tools > 0 {
        let mut parts: Vec<String> = Vec::new();
        if solo > 0 {
            parts.push(format!(
                "🟡 {solo} {} (unconfirmed — suggestion only)",
                plural(solo, "solo finding")
            ));
        }
        if tools > 0 {
            parts.push(format!("{} {}", tools, plural(tools, "tool finding")));
        }
        lines.push(parts.join(" · "));
    }
    let absent = verdict.absent_judges.len();
    if absent > 0 {
        lines.push(format!(
            "⚠ {absent} {} absent — partial round",
            plural(absent, "judge")
        ));
    }
    let incomplete = verdict.incomplete.len();
    if incomplete > 0 {
        lines.push(format!(
            "⚠ {incomplete} {} incomplete — partial round",
            plural(incomplete, "judge")
        ));
    }
    let unavailable = verdict.tools_unavailable.len();
    if unavailable > 0 {
        lines.push(format!(
            "⚠ {unavailable} {} unavailable — the deterministic layer was partial",
            plural(unavailable, "tool")
        ));
    }
    lines
}

fn coverage_section(verdict: &RoundVerdict) -> Vec<String> {
    let coverage = &verdict.coverage;
    let mut lines = vec![
        "## Coverage".to_owned(),
        format!(
            "regions {} · cells {} · judged {}/{} by {} {} · clean {} · defect {}",
            coverage.regions,
            coverage.cells_total,
            coverage.cells_judged,
            coverage.cells_total,
            coverage.judged_by_judge.len(),
            plural(coverage.judged_by_judge.len(), "judge"),
            coverage.cells_clean,
            coverage.cells_defect
        ),
    ];
    if coverage.cells_short > 0 {
        let short_files: Vec<&str> = coverage
            .per_file
            .iter()
            .filter(|(_, entry)| entry.short > 0)
            .map(|(file, _)| file.as_str())
            .collect();
        lines.push(format!(
            "⚠ {} {} short of the full panel across {} {}: {}",
            coverage.cells_short,
            plural(coverage.cells_short, "cell"),
            short_files.len(),
            plural(short_files.len(), "file"),
            short_files.join(", ")
        ));
    }
    if !coverage.per_file.is_empty() {
        let files = coverage
            .per_file
            .iter()
            .map(|(file, entry)| {
                let base = format!(
                    "{} {}/{} clean",
                    escape_inline(file),
                    entry.clean,
                    entry.judged
                );
                let defect = if entry.defect > 0 {
                    format!(" ({} {})", entry.defect, plural(entry.defect, "defect"))
                } else {
                    String::new()
                };
                let short = if entry.short > 0 {
                    format!(" [{} short]", entry.short)
                } else {
                    String::new()
                };
                format!("{base}{defect}{short}")
            })
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(format!("per-file: {files}"));
    }
    lines
}

fn confirmed_section(verdict: &RoundVerdict) -> Vec<String> {
    if verdict.confirmed.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["## Confirmed findings".to_owned()];
    let panel = verdict
        .coverage
        .judged_by_judge
        .len()
        .saturating_add(verdict.absent_judges.len());
    for (index, entry) in verdict.confirmed.iter().enumerate() {
        let number = index.saturating_add(1);
        let families = if !entry.families.is_empty() && entry.families.len() < entry.votes.len() {
            format!(" ({} families)", entry.families.len())
        } else {
            String::new()
        };
        lines.push(format!(
            "{}. [{}|{}|{}/{}{}] {}{}",
            number,
            severity_word(entry.severity),
            entry.finding.aspect.0,
            entry.votes.len(),
            panel,
            families,
            escape_inline(&entry.finding.title),
            anchor(&entry.finding)
        ));
        lines.push(indent_block(&format!(
            "mechanism: {}",
            escape_inline(&entry.finding.mechanism)
        )));
        let claim = match &entry.finding.fix_hint {
            Some(hint) => format!(
                "{}claim: {} · fix hint: {}",
                INDENT,
                claim_label(entry.finding.claim),
                escape_inline(hint)
            ),
            None => format!("{}claim: {}", INDENT, claim_label(entry.finding.claim)),
        };
        lines.push(claim);
    }
    lines
}

fn tool_section(verdict: &RoundVerdict) -> Vec<String> {
    if verdict.tool_findings.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["## Tool findings (deterministic)".to_owned()];
    for finding in &verdict.tool_findings {
        lines.push(format!(
            "- {}: {}:{} — {} [{}]",
            escape_inline(&finding.tool),
            escape_inline(&finding.file),
            finding.line,
            escape_inline(&finding.message),
            severity_word(finding.severity)
        ));
    }
    lines
}

fn solo_section(verdict: &RoundVerdict) -> Vec<String> {
    if verdict.solo.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["## Solo (unconfirmed — never blocking)".to_owned()];
    for entry in &verdict.solo {
        lines.push(format!(
            "- {}{} [{}]",
            escape_inline(&entry.finding.title),
            anchor(&entry.finding),
            severity_word(entry.severity)
        ));
    }
    lines
}

fn footer_section(verdict: &RoundVerdict) -> Vec<String> {
    vec![format!(
        "## Suppressed by owner: {} · Refuted by oracle: {} · Weak leads: {} · Dropped as invalid: {}",
        verdict.suppressed.len(),
        verdict.refuted.len(),
        verdict.weak.len(),
        verdict.dropped.len()
    )]
}

fn leads_section(verdict: &RoundVerdict) -> Vec<String> {
    if verdict.dropped.is_empty() && verdict.weak.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["## Unverified leads (not findings)".to_owned()];
    for entry in &verdict.weak {
        let cite = entry
            .finding
            .evidence
            .first()
            .map_or_else(String::new, |cite| {
                format!(" \u{00b7} {}:{}", escape_inline(&cite.file), cite.line)
            });
        let mut line = format!(
            "- [{}] {} ({}){}",
            drop_reason_label(entry.reason),
            escape_inline(&entry.finding.title),
            severity_word(entry.finding.severity),
            cite
        );
        if entry.repaired.is_some() {
            line.push_str(" [title repaired]");
        }
        if !entry.also_at.is_empty() {
            let sites = entry
                .also_at
                .iter()
                .map(|site| escape_inline(site))
                .collect::<Vec<_>>()
                .join(", ");
            line.push_str(" — also at ");
            line.push_str(&sites);
        }
        if let Some(hint) = &entry.finding.fix_hint {
            line.push_str(" \u{2014} fix: ");
            line.push_str(&escape_inline(hint));
        }
        lines.push(line);
    }
    for entry in &verdict.dropped {
        let cite = entry
            .finding
            .evidence
            .first()
            .map_or_else(String::new, |cite| {
                format!(" · {}:{}", escape_inline(&cite.file), cite.line)
            });
        let oracle = match verdict.lead_verdicts.get(&entry.finding.id.0) {
            Some(OracleVerdict::Confirmed) => " · oracle-confirmed",
            Some(OracleVerdict::Refuted { .. }) => " · oracle-refuted",
            _ => "",
        };
        let mut line = format!(
            "- [{}] {} ({}){}",
            drop_reason_label(entry.reason),
            escape_inline(&entry.finding.title),
            severity_word(entry.finding.severity),
            cite
        );
        line.push_str(oracle);
        lines.push(line);
    }
    lines
}

fn drop_reason_label(reason: DropReason) -> &'static str {
    match reason {
        DropReason::CiteOutsideMaterials => "cite-outside-materials",
        DropReason::UnparseableEvidence => "unparseable-evidence",
        DropReason::MechanismCitesNoRegionLine => "mechanism-cites-no-region-line",
        DropReason::AssumptionNotCapped => "assumption-not-capped",
        DropReason::SeverityAboveAspectCeiling => "severity-above-aspect-ceiling",
        DropReason::MalformedTitle => "malformed-title",
        DropReason::NeedsRepair => "needs-repair",
    }
}

fn state_section(verdict: &RoundVerdict) -> Vec<String> {
    let state = &verdict.state;
    let standing = state
        .issues
        .iter()
        .filter(|issue| issue.status == IssueStatus::Open)
        .count();
    let retired = state
        .issues
        .iter()
        .filter(|issue| {
            matches!(&issue.status, IssueStatus::Retired { round, .. } if *round == state.round)
        })
        .count();
    vec![
        "## State".to_owned(),
        format!(
            "round {} · standing {} · retired this round {} · suppressions {}",
            state.round,
            standing,
            retired,
            state.suppressions.len()
        ),
    ]
}

fn fix_prompts_section(verdict: &RoundVerdict) -> Vec<String> {
    if verdict.confirmed.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["## Fix prompts".to_owned()];
    for (index, entry) in verdict.confirmed.iter().enumerate() {
        let number = index.saturating_add(1);
        lines.push(format!(
            "### {}. {}{}",
            number,
            escape_inline(&entry.finding.title),
            anchor(&entry.finding)
        ));
        for line in entry.finding.mechanism.lines() {
            lines.push(format!("{INDENT}{line}"));
        }
        if let Some(hint) = &entry.finding.fix_hint {
            lines.push(format!("{INDENT}Fix: {}", escape_inline(hint)));
        }
    }
    lines
}

#[must_use]
pub fn fmt_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let minutes = secs / 60;
        let rest = secs % 60;
        if rest == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m{rest:02}s")
        }
    } else {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{minutes:02}m")
        }
    }
}

#[must_use]
pub fn region_stats_section(verdict: &RoundVerdict) -> String {
    if verdict.stats.is_empty() {
        return String::new();
    }
    let mut lines = vec!["## Region stats".to_owned()];
    for entry in &verdict.stats {
        let outcome = match (&entry.cause, entry.judged) {
            (Some(cause), _) => format!("failed — {cause}"),
            (None, true) => "ok".to_owned(),
            (None, false) => "failed".to_owned(),
        };
        lines.push(format!(
            "- {} · region {} · {} · {} turn{} · {} tool call{} · {}/{} tok · {}",
            entry.judge,
            entry.region.0,
            fmt_duration(entry.secs),
            entry.turns,
            plural(entry.turns, ""),
            entry.tool_calls,
            plural(entry.tool_calls, ""),
            entry.input_tokens,
            entry.output_tokens,
            outcome
        ));
    }
    let judged = verdict.stats.iter().filter(|entry| entry.judged).count();
    let secs: u64 = verdict.stats.iter().map(|entry| entry.secs).sum();
    let turns: usize = verdict.stats.iter().map(|entry| entry.turns).sum();
    let tool_calls: usize = verdict.stats.iter().map(|entry| entry.tool_calls).sum();
    let tokens_in: u64 = verdict.stats.iter().map(|entry| entry.input_tokens).sum();
    let tokens_out: u64 = verdict.stats.iter().map(|entry| entry.output_tokens).sum();
    lines.push(format!(
        "totals: {judged} ok · {} failed · {} · {turns} turn{} · {tool_calls} tool call{} · {tokens_in}/{tokens_out} tok",
        verdict.stats.len().saturating_sub(judged),
        fmt_duration(secs),
        plural(turns, ""),
        plural(tool_calls, ""),
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        AspectId, Cite, ClaimType, CoverageSummary, DropReason, Dropped, FileCoverage, Finding,
        FindingDraft, Issue, IssueIdentity, IssueStatus, PinnedRange, Reconciled, RegionId,
        RoundState, RoundVerdict, Severity, Suppression, ToolFinding,
    };
    use crate::identity::title_tokens;
    use std::collections::BTreeMap;

    fn draft_finding(
        title: &str,
        aspect: &str,
        file: &str,
        line: u32,
        severity: Severity,
        claim: ClaimType,
        fix_hint: Option<&str>,
        mechanism: &str,
    ) -> Finding {
        Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId(aspect.to_owned()),
            title: title.to_owned(),
            mechanism: mechanism.to_owned(),
            claim,
            evidence: vec![Cite {
                file: file.to_owned(),
                line,
            }],
            severity,
            fix_hint: fix_hint.map(str::to_owned),
        })
    }

    fn reconciled(votes: &[&str], severity: Severity, finding: Finding) -> Reconciled {
        Reconciled {
            finding,
            votes: votes.iter().map(|judge| (*judge).to_owned()).collect(),
            families: Vec::new(),
            severity,
        }
    }

    fn state() -> RoundState {
        RoundState {
            checkout: "trunk".to_owned(),
            round: 2,
            issues: vec![
                Issue {
                    identity: IssueIdentity {
                        file: "src/a.rs".to_owned(),
                        line: 3,
                        title_tokens: title_tokens("Lock is dropped too early")
                            .into_iter()
                            .collect(),
                    },
                    first_seen: "base".to_owned(),
                    last_seen: "head".to_owned(),
                    status: IssueStatus::Open,
                    misses: 0,
                    votes_seen: Vec::new(),
                },
                Issue {
                    identity: IssueIdentity {
                        file: "src/b.rs".to_owned(),
                        line: 9,
                        title_tokens: title_tokens("Buffer grows unbounded").into_iter().collect(),
                    },
                    first_seen: "base".to_owned(),
                    last_seen: "base".to_owned(),
                    status: IssueStatus::Retired {
                        round: 2,
                        sha: "head".to_owned(),
                    },
                    misses: 2,
                    votes_seen: Vec::new(),
                },
            ],
            suppressions: vec![Suppression {
                id: "sup-1".to_owned(),
                theme: "generated code".to_owned(),
                file_glob: "tests/**".to_owned(),
                reason: "accepted".to_owned(),
                created: "base".to_owned(),
                hits: 3,
            }],
        }
    }

    fn blocking_verdict() -> RoundVerdict {
        RoundVerdict {
            weak: Vec::new(),
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            coverage: CoverageSummary {
                cells_total: 5,
                cells_judged: 4,
                cells_clean: 3,
                cells_defect: 1,
                judged_by_judge: BTreeMap::from([("a".to_owned(), 3), ("b".to_owned(), 3)]),
                regions: 2,
                per_file: BTreeMap::from([
                    (
                        "docs/guide.md".to_owned(),
                        FileCoverage {
                            cells: 1,
                            judged: 1,
                            clean: 1,
                            defect: 0,
                            short: 0,
                        },
                    ),
                    (
                        "src/a.rs".to_owned(),
                        FileCoverage {
                            cells: 4,
                            judged: 3,
                            clean: 2,
                            defect: 1,
                            short: 0,
                        },
                    ),
                ]),
                cells_short: 0,
            },
            tool_findings: vec![ToolFinding {
                tool: "clippy".to_owned(),
                file: "src/a.rs".to_owned(),
                line: 12,
                severity: Severity::Warning,
                message: "needless borrow".to_owned(),
            }],
            confirmed: vec![
                reconciled(
                    &["a", "b"],
                    Severity::Warning,
                    draft_finding(
                        "Lock is dropped too early",
                        "concurrency",
                        "src/a.rs",
                        3,
                        Severity::Warning,
                        ClaimType::InDiff,
                        Some("hold the guard across the await"),
                        "the walk reaches src/a.rs:3\nand the guard fails",
                    ),
                ),
                reconciled(
                    &["a"],
                    Severity::Suggestion,
                    draft_finding(
                        "The guide overstates the guarantee",
                        "docs",
                        "docs/guide.md",
                        2,
                        Severity::Suggestion,
                        ClaimType::ExternalAssumption,
                        None,
                        "the guide claims the lock is held",
                    ),
                ),
            ],
            solo: vec![reconciled(
                &["b"],
                Severity::Suggestion,
                draft_finding(
                    "Buffer grows unbounded",
                    "lifecycle",
                    "src/a.rs",
                    9,
                    Severity::Suggestion,
                    ClaimType::InDiff,
                    None,
                    "the buffer never drains",
                ),
            )],
            refuted: vec![reconciled(
                &["a"],
                Severity::Warning,
                draft_finding(
                    "The build does not compile",
                    "error-paths",
                    "src/a.rs",
                    4,
                    Severity::Warning,
                    ClaimType::NeedsExecution,
                    None,
                    "cargo check passes",
                ),
            )],
            suppressed: vec![reconciled(
                &["a", "b"],
                Severity::Warning,
                draft_finding(
                    "Generated fixtures drift",
                    "fixtures",
                    "tests/golden.rs",
                    7,
                    Severity::Warning,
                    ClaimType::InDiff,
                    None,
                    "the golden regenerates differently",
                ),
            )],
            dropped: vec![Dropped {
                judge: "zai:glm-5.3".to_owned(),
                finding: draft_finding(
                    "Cite hangs outside the materials",
                    "error-paths",
                    "src/a.rs",
                    99,
                    Severity::Warning,
                    ClaimType::InDiff,
                    None,
                    "the cite floats",
                ),
                reason: DropReason::CiteOutsideMaterials,
            }],
            absent_judges: Vec::new(),
            incomplete: Vec::new(),
            tools_unavailable: Vec::new(),
            stats: Vec::new(),
            lead_verdicts: std::collections::BTreeMap::new(),
            state: state(),
        }
    }

    #[test]
    fn a_clean_verdict_renders_the_good_to_go_layout() {
        let mut verdict = blocking_verdict();
        verdict.confirmed = Vec::new();
        verdict.solo = Vec::new();
        verdict.tool_findings = Vec::new();
        verdict.refuted = Vec::new();
        verdict.suppressed = Vec::new();
        verdict.dropped = Vec::new();
        verdict.coverage = CoverageSummary {
            cells_total: 1,
            cells_judged: 1,
            cells_clean: 1,
            cells_defect: 0,
            judged_by_judge: BTreeMap::from([("a".to_owned(), 1)]),
            regions: 1,
            per_file: BTreeMap::from([(
                "src/a.rs".to_owned(),
                FileCoverage {
                    cells: 1,
                    judged: 1,
                    clean: 1,
                    defect: 0,
                    short: 0,
                },
            )]),
            cells_short: 0,
        };
        verdict.state.issues = Vec::new();
        verdict.state.suppressions = Vec::new();
        verdict.state.round = 1;
        let report = console::render(&verdict, &RenderOptions::default());
        assert_eq!(
            report,
            "## Verdict\n\
             🟢 Good to go — no blocking findings\n\
             \n\
             ## Coverage\n\
             regions 1 · cells 1 · judged 1/1 by 1 judge · clean 1 · defect 0\n\
             per-file: src/a.rs 1/1 clean\n\
             \n\
             ## Suppressed by owner: 0 · Refuted by oracle: 0 · Weak leads: 0 · Dropped as invalid: 0\n\
             \n\
             ## State\n\
             round 1 · standing 0 · retired this round 0 · suppressions 0\n"
        );
    }

    #[test]
    fn a_blocking_verdict_renders_every_section_exactly() {
        let report = console::render(&blocking_verdict(), &RenderOptions::default());
        assert_eq!(
            report,
            "## Verdict\n\
             🔴 Not good to go — 2 confirmed blocking findings\n\
             🟡 1 solo finding (unconfirmed — suggestion only) · 1 tool finding\n\
             \n\
             ## Coverage\n\
             regions 2 · cells 5 · judged 4/5 by 2 judges · clean 3 · defect 1\n\
             per-file: docs/guide.md 1/1 clean · src/a.rs 2/3 clean (1 defect)\n\
             \n\
             ## Confirmed findings\n\
             1. [warning|concurrency|2/2] Lock is dropped too early — src/a.rs:3\n\
             \x20   mechanism: the walk reaches src/a.rs:3\n\
             \x20   and the guard fails\n\
             \x20   claim: in-diff · fix hint: hold the guard across the await\n\
             2. [suggestion|docs|1/2] The guide overstates the guarantee — docs/guide.md:2\n\
             \x20   mechanism: the guide claims the lock is held\n\
             \x20   claim: external-assumption (assumption)\n\
             \n\
             ## Tool findings (deterministic)\n\
             - clippy: src/a.rs:12 — needless borrow [warning]\n\
             \n\
             ## Solo (unconfirmed — never blocking)\n\
             - Buffer grows unbounded — src/a.rs:9 [suggestion]\n\
             \n\
             ## Unverified leads (not findings)\n\
             - [cite-outside-materials] Cite hangs outside the materials (warning) · src/a.rs:99\n\
             \n\
             ## Suppressed by owner: 1 · Refuted by oracle: 1 · Weak leads: 0 · Dropped as invalid: 1\n\
             \n\
             ## State\n\
             round 2 · standing 1 · retired this round 1 · suppressions 1\n"
        );
    }

    #[test]
    fn a_partial_verdict_marks_the_absent_judges() {
        let mut verdict = blocking_verdict();
        verdict.absent_judges = vec!["c".to_owned()];
        let report = console::render(&verdict, &RenderOptions::default());
        assert!(
            report
                .lines()
                .any(|line| line == "⚠ 1 judge absent — partial round"),
            "the absent judge surfaces in the verdict block: {report}"
        );
        assert!(
            report.contains("[warning|concurrency|2/3]"),
            "the badge panel counts present plus absent judges"
        );
    }

    #[test]
    fn json_mode_round_trips_the_verdict() {
        let verdict = blocking_verdict();
        let dumped = json::render(&verdict);
        assert!(
            serde_json::from_str::<RoundVerdict>(&dumped).is_ok_and(|back| back == verdict),
            "the JSON sink reproduces the verdict: {dumped}"
        );
        assert!(dumped.starts_with('{') && dumped.ends_with("}\n"));
    }

    #[test]
    fn fix_prompts_are_absent_by_default_and_present_under_the_flag() {
        let verdict = blocking_verdict();
        let plain = console::render(&verdict, &RenderOptions::default());
        assert!(
            !plain.contains("## Fix prompts"),
            "fix prompts are opt-in: {plain}"
        );
        let with_prompts = console::render(&verdict, &RenderOptions { fix_prompts: true });
        assert!(with_prompts.contains("## Fix prompts"));
        assert!(with_prompts.contains("### 1. Lock is dropped too early — src/a.rs:3"));
        assert!(with_prompts.contains(
            "    the walk reaches src/a.rs:3\n    and the guard fails\n    Fix: hold the guard across the await"
        ));
        assert!(
            !with_prompts.contains("### 3."),
            "solo and refuted findings never get fix prompts"
        );
    }

    #[test]
    fn model_controlled_text_renders_inert_everywhere() {
        let mut verdict = blocking_verdict();
        let hostile = draft_finding(
            "[build log](https://evil.example)",
            "concurrency",
            "src/a.rs",
            3,
            Severity::Critical,
            ClaimType::InDiff,
            Some("run `cargo` --fix"),
            "see [exploit](https://evil.example) and ![img](https://evil.example/x.png) — walk ``` rust\n# not a heading\n## a real fence above",
        );
        verdict.confirmed = vec![reconciled(&["a", "b"], Severity::Critical, hostile)];
        let crafted = "![x](https://evil.example).rs";
        verdict.tool_findings = vec![ToolFinding {
            tool: "clippy".to_owned(),
            file: crafted.to_owned(),
            line: 1,
            severity: Severity::Warning,
            message: "see [docs](https://evil.example)".to_owned(),
        }];
        verdict.coverage.per_file.insert(
            crafted.to_owned(),
            crate::contracts::FileCoverage {
                cells: 2,
                judged: 2,
                clean: 1,
                defect: 1,
                short: 0,
            },
        );
        let comment = comment::render(&verdict, &RenderOptions { fix_prompts: true });
        assert!(
            comment.contains("\\[build log\\]\\(https://evil.example\\)"),
            "the disguised link renders as inert text: {comment}"
        );
        assert!(
            !comment.contains("[build log](https://evil.example)"),
            "the raw link never ships: {comment}"
        );
        assert!(
            !comment.contains("![x](https://evil.example).rs"),
            "a crafted file name never renders live: {comment}"
        );
        assert!(
            comment.contains("\\!\\[x\\]\\(https://evil.example\\).rs"),
            "crafted paths arrive escaped in every inline position: {comment}"
        );
        assert!(
            !comment.contains("mechanism: see [exploit](https://evil.example)")
                && !comment.contains("mechanism: see ![img]"),
            "the confirmed list never carries a live link or image in the mechanism: {comment}"
        );
        assert!(
            comment.contains("mechanism: see \\[exploit\\]\\(https://evil.example\\)"),
            "the confirmed list carries the mechanism escaped, not fenced: {comment}"
        );
        assert!(
            comment.contains("\\[docs\\]\\(https://evil.example\\)"),
            "tool messages escape too: {comment}"
        );
        assert!(
            comment.contains(
                "    see [exploit](https://evil.example) and ![img](https://evil.example/x.png) — walk ``` rust"
            ) && comment.contains("    # not a heading"),
            "the fix-prompt block carries the raw mechanism as inert indented code: {comment}"
        );
        assert!(
            !comment.contains("\n```"),
            "no literal fence line exists to close: {comment}"
        );
        assert!(
            comment.contains("Fix: run \\`cargo\\` --fix"),
            "hints escape inline markdown: {comment}"
        );
    }

    #[test]
    fn the_hosted_comment_carries_the_sections_and_the_run_range() {
        let comment = comment::render(&blocking_verdict(), &RenderOptions::default());
        assert!(comment.contains("## Confirmed findings"));
        assert!(comment.contains("## Suppressed by owner: 1"));
        assert!(
            comment.ends_with("<sub>difftrace review of base..head</sub>\n"),
            "the hosted framing names the reviewed range: {comment}"
        );
    }

    #[test]
    fn dropped_claims_render_as_leads_with_reasons_and_oracle_marks() {
        let mut verdict = blocking_verdict();
        let finding = Finding {
            id: crate::contracts::FindingId("0/error-paths/lead".to_owned()),
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: "The dependency is not rev-pinned".to_owned(),
            mechanism: "m".to_owned(),
            claim: ClaimType::NeedsExecution,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 2,
            }],
            severity: Severity::Warning,
            fix_hint: None,
            discarded_cites: 0,
        };
        verdict.dropped = vec![Dropped {
            judge: "zai:glm-5.3".to_owned(),
            finding: finding.clone(),
            reason: DropReason::CiteOutsideMaterials,
        }];
        let text = sections(&verdict, &RenderOptions::default());
        assert!(
            text.contains("## Unverified leads (not findings)"),
            "the section lands: {text}"
        );
        assert!(
            text.contains("[cite-outside-materials] The dependency is not rev-pinned (warning)"),
            "each lead names its reason and severity: {text}"
        );
        let lead_id = finding.id.0.as_str().to_owned();
        verdict
            .lead_verdicts
            .insert(lead_id, OracleVerdict::Confirmed);
        let marked = sections(&verdict, &RenderOptions::default());
        assert!(
            marked.contains("oracle-confirmed"),
            "a mechanically confirmed lead is visibly marked, still not a finding: {marked}"
        );
    }

    #[test]
    fn fmt_duration_speaks_minutes_and_hours() {
        assert_eq!(fmt_duration(42), "42s");
        assert_eq!(fmt_duration(60), "1m");
        assert_eq!(fmt_duration(252), "4m12s");
        assert_eq!(fmt_duration(3600), "1h");
        assert_eq!(fmt_duration(4516), "1h15m");
        assert_eq!(fmt_duration(239_938), "66h38m");
    }

    #[test]
    fn the_region_stats_section_names_causes_and_minutes() {
        use crate::contracts::{RegionId, RegionStats};
        let mut verdict = blocking_verdict();
        assert!(
            region_stats_section(&verdict).is_empty(),
            "a verdict without stats has no section"
        );
        verdict.stats = vec![RegionStats {
            judge: "zai:glm-5.3-flash".to_owned(),
            region: RegionId(26),
            file: "src/config.rs".to_owned(),
            attempts: 2,
            judged: false,
            secs: 271,
            turns: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            run_secs: vec![271],
            cause: Some("API error: stream failed: rate limit".to_owned()),
        }];
        let section = region_stats_section(&verdict);
        assert!(
            section.contains("region 26 · 4m31s ·"),
            "durations render in minutes: {section}"
        );
        assert!(
            section.contains("failed — API error: stream failed: rate limit"),
            "the failure cause rides the section: {section}"
        );
        assert!(
            section.contains("totals: 0 ok · 1 failed · 4m31s ·"),
            "the totals line carries the final time in minutes: {section}"
        );
    }

    #[test]
    fn unavailable_tools_are_disclosed_in_the_verdict() {
        let mut verdict = blocking_verdict();
        verdict.tools_unavailable =
            vec!["clippy head side: binary clippy is not installed".to_owned()];
        let text = sections(&verdict, &RenderOptions::default());
        assert!(
            text.contains("1 tool unavailable"),
            "a partial deterministic layer never passes silently: {text}"
        );
        let clean = sections(&blocking_verdict(), &RenderOptions::default());
        assert!(
            !clean.contains("tool unavailable"),
            "a complete tool layer stays quiet"
        );
    }
}
