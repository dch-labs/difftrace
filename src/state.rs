//! Cross-round issue state: identity, misses, retirement, suppressions.

pub mod file;
pub mod marker;

use crate::aspects::glob_matches;
use crate::contracts::{
    Cite, Finding, Issue, IssueIdentity, IssueStatus, Reconciled, RoundState, RoundVerdict,
    Suppression,
};
use crate::identity::{same_spot, title_tokens, titles_overlap_enough};

const RETIRE_AFTER_CONSECUTIVE_MISSES: u32 = 2;
const VOTES_HISTORY: usize = 20;

#[must_use]
pub fn matches_suppression(suppression: &Suppression, finding: &Finding) -> bool {
    titles_overlap_enough(&suppression.theme, &finding.title)
        && finding
            .evidence
            .iter()
            .any(|cite| glob_matches(&suppression.file_glob, &cite.file))
}

pub fn apply_round(
    state: &mut RoundState,
    round: &RoundVerdict,
    covered_gaps: bool,
    narrowed: bool,
) {
    let number = state.round.saturating_add(1);
    state.round = number;
    let head = round.range.head_sha.clone();
    let mut matched = vec![false; state.issues.len()];
    let mut rejected_now = vec![false; state.issues.len()];
    fold_refinds(
        state,
        round.suppressed.iter(),
        number,
        &head,
        &mut matched,
        &mut rejected_now,
        true,
    );
    fold_refinds(
        state,
        round.confirmed.iter().chain(&round.solo),
        number,
        &head,
        &mut matched,
        &mut rejected_now,
        false,
    );
    if covered_gaps && !narrowed {
        grow_misses(state, &matched, number, &head);
    }
}

pub fn apply_partial_round(state: &mut RoundState, round: &RoundVerdict) {
    for entry in &round.suppressed {
        for rule in &mut state.suppressions {
            if matches_suppression(rule, &entry.finding) {
                rule.hits = rule.hits.saturating_add(1);
                break;
            }
        }
    }
}

#[must_use]
pub fn reject_finding(
    state: &mut RoundState,
    file: &str,
    line: u32,
    title: &str,
    theme: bool,
    reason: &str,
    sha: &str,
) -> String {
    let file_glob = if theme {
        directory_glob(file)
    } else {
        file.to_owned()
    };
    let next = state
        .suppressions
        .iter()
        .filter_map(|rule| rule.id.strip_prefix('s'))
        .filter_map(|number| number.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let id = format!("s{next}");
    state.suppressions.push(Suppression {
        id: id.clone(),
        theme: title_tokens(title)
            .into_iter()
            .collect::<Vec<_>>()
            .join(" "),
        file_glob,
        reason: reason.to_owned(),
        created: sha.to_owned(),
        hits: 0,
    });
    let cite = Cite {
        file: file.to_owned(),
        line,
    };
    for issue in &mut state.issues {
        if same_issue(&issue.identity, &cite, title) {
            issue.status = IssueStatus::Rejected { id: id.clone() };
        }
    }
    id
}

pub fn clear_suppression(state: &mut RoundState, id: Option<&str>) -> usize {
    let before = state.suppressions.len();
    match id {
        Some(wanted) => state.suppressions.retain(|rule| rule.id != wanted),
        None => state.suppressions.clear(),
    }
    let removed = before.saturating_sub(state.suppressions.len());
    for issue in &mut state.issues {
        let orphaned = match (&issue.status, id) {
            (IssueStatus::Rejected { id: rejected }, Some(wanted)) => rejected.as_str() == wanted,
            (IssueStatus::Rejected { .. }, None) => true,
            _ => false,
        };
        if orphaned {
            issue.status = IssueStatus::Open;
        }
    }
    removed
}

fn directory_glob(file: &str) -> String {
    let parent = std::path::Path::new(file)
        .parent()
        .map_or_else(String::new, |dir| dir.to_string_lossy().into_owned());
    if parent.is_empty() {
        "**".to_owned()
    } else {
        format!("{parent}/**")
    }
}

fn fold_refinds<'a>(
    state: &mut RoundState,
    entries: impl Iterator<Item = &'a Reconciled>,
    number: u32,
    head: &str,
    matched: &mut Vec<bool>,
    rejected_now: &mut Vec<bool>,
    suppressed: bool,
) {
    for entry in entries {
        let Some(cite) = entry.finding.evidence.first() else {
            continue;
        };
        let rejection = if suppressed {
            firing_suppression(state, &entry.finding)
        } else {
            None
        };
        let votes = entry.votes.len();
        let target = state
            .issues
            .iter()
            .enumerate()
            .find(|(index, issue)| {
                !matched.get(*index).copied().unwrap_or(false)
                    && same_issue(&issue.identity, cite, &entry.finding.title)
            })
            .map(|(index, _)| index)
            .or_else(|| {
                state
                    .issues
                    .iter()
                    .position(|issue| same_issue(&issue.identity, cite, &entry.finding.title))
            });
        if let Some(index) = target {
            if let Some(flag) = matched.get_mut(index) {
                *flag = true;
            }
            let sticky = rejected_now.get(index).copied().unwrap_or(false);
            if let Some(issue) = state.issues.get_mut(index) {
                record_refind(issue, number, head, votes, rejection.as_ref(), sticky);
            }
            if rejection.is_some()
                && let Some(flag) = rejected_now.get_mut(index)
            {
                *flag = true;
            }
        } else {
            let status = rejection
                .as_ref()
                .map_or(IssueStatus::Open, |id| IssueStatus::Rejected {
                    id: id.clone(),
                });
            state.issues.push(Issue {
                identity: IssueIdentity {
                    file: cite.file.clone(),
                    line: cite.line,
                    title_tokens: title_tokens(&entry.finding.title).into_iter().collect(),
                },
                first_seen: head.to_owned(),
                last_seen: head.to_owned(),
                status,
                misses: 0,
                votes_seen: vec![(number, votes)],
            });
            matched.push(true);
            rejected_now.push(rejection.is_some());
        }
    }
}

fn firing_suppression(state: &mut RoundState, finding: &Finding) -> Option<String> {
    state
        .suppressions
        .iter_mut()
        .find(|suppression| matches_suppression(suppression, finding))
        .map(|suppression| {
            suppression.hits = suppression.hits.saturating_add(1);
            suppression.id.clone()
        })
}

fn record_refind(
    issue: &mut Issue,
    number: u32,
    head: &str,
    votes: usize,
    rejection: Option<&String>,
    sticky: bool,
) {
    head.clone_into(&mut issue.last_seen);
    issue.misses = 0;
    let keep_rejected = sticky && matches!(issue.status, IssueStatus::Rejected { .. });
    issue.status = match rejection.as_ref() {
        Some(id) => IssueStatus::Rejected { id: (*id).clone() },
        None if keep_rejected => issue.status.clone(),
        None => IssueStatus::Open,
    };
    match issue.votes_seen.last_mut() {
        Some(entry) if entry.0 == number => entry.1 = entry.1.max(votes),
        _ => issue.votes_seen.push((number, votes)),
    }
    let excess = issue.votes_seen.len().saturating_sub(VOTES_HISTORY);
    if excess > 0 {
        let _unused = issue.votes_seen.drain(..excess);
    }
}

fn grow_misses(state: &mut RoundState, matched: &[bool], number: u32, head: &str) {
    for (issue, seen) in state.issues.iter_mut().zip(matched.iter()) {
        if *seen || issue.status != IssueStatus::Open {
            continue;
        }
        issue.misses = issue.misses.saturating_add(1);
        if issue.misses >= RETIRE_AFTER_CONSECUTIVE_MISSES {
            issue.status = IssueStatus::Retired {
                round: number,
                sha: head.to_owned(),
            };
        }
    }
}

fn same_issue(identity: &IssueIdentity, cite: &Cite, title: &str) -> bool {
    identity.file == cite.file
        && same_spot(
            u64::from(identity.line),
            &identity.title_tokens.join(" "),
            u64::from(cite.line),
            title,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        AspectId, ClaimType, CoverageSummary, Finding, FindingDraft, Issue, IssueIdentity,
        IssueStatus, PinnedRange, RegionId, Severity, ToolFinding,
    };
    use std::collections::BTreeMap;

    fn plain_state() -> RoundState {
        RoundState {
            checkout: "trunk".to_owned(),
            round: 3,
            issues: Vec::new(),
            suppressions: Vec::new(),
        }
    }

    fn open_issue(file: &str, line: u32, title: &str) -> Issue {
        Issue {
            identity: IssueIdentity {
                file: file.to_owned(),
                line,
                title_tokens: title_tokens(title).into_iter().collect(),
            },
            first_seen: "base".to_owned(),
            last_seen: "head".to_owned(),
            status: IssueStatus::Open,
            misses: 0,
            votes_seen: vec![(1, 2)],
        }
    }

    #[test]
    fn reject_theme_builds_the_glob_and_tokens_suppression()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = plain_state();
        let first = reject_finding(
            &mut state,
            "src/sub/a.rs",
            4,
            "Lock is dropped too early",
            false,
            "not like this",
            "head1",
        );
        let second = reject_finding(
            &mut state,
            "src/sub/a.rs",
            6,
            "Buffer grows unbounded",
            true,
            "known debt",
            "head2",
        );
        let root = reject_finding(
            &mut state,
            "README.md",
            1,
            "Docs claim too much",
            true,
            "docs",
            "head3",
        );
        assert_eq!(
            (first.as_str(), second.as_str(), root.as_str()),
            ("s1", "s2", "s3"),
            "ids increment over the existing set"
        );
        assert_eq!(
            state.suppressions.first().ok_or("first")?.file_glob,
            "src/sub/a.rs",
            "a plain reject keys the finding's own file"
        );
        assert_eq!(
            state.suppressions.get(1).ok_or("second")?.file_glob,
            "src/sub/**",
            "the theme arm widens to the directory glob"
        );
        assert_eq!(
            state.suppressions.get(2).ok_or("third")?.file_glob,
            "**",
            "a root-level file themes the whole tree"
        );
        assert_eq!(
            state.suppressions.first().ok_or("first")?.theme,
            "dropped early is lock too",
            "the theme is the normalized title tokens as the identity set holds them"
        );
        assert_eq!(state.suppressions.first().ok_or("first")?.created, "head1");
        Ok(())
    }

    #[test]
    fn a_rejected_issue_leaves_the_standing_set() -> Result<(), Box<dyn std::error::Error>> {
        let mut state = plain_state();
        state
            .issues
            .push(open_issue("src/a.rs", 3, "Lock is dropped too early"));
        let standing = |state: &RoundState| {
            state
                .issues
                .iter()
                .filter(|issue| issue.status == IssueStatus::Open)
                .count()
        };
        assert_eq!(standing(&state), 1);
        let id = reject_finding(
            &mut state,
            "src/a.rs",
            3,
            "Lock is dropped too early",
            false,
            "owner rejected",
            "head",
        );
        let issue = state.issues.first().ok_or("issue")?;
        assert_eq!(
            issue.status,
            IssueStatus::Rejected { id },
            "the same-identity issue flips to name its suppression"
        );
        assert_eq!(
            standing(&state),
            0,
            "a rejected issue leaves the standing set"
        );
        Ok(())
    }

    #[test]
    fn clearing_a_suppression_reopens_its_rejected_issues() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut state = plain_state();
        state
            .issues
            .push(open_issue("src/a.rs", 3, "Lock is dropped too early"));
        state
            .issues
            .push(open_issue("src/b.rs", 5, "Buffer grows unbounded"));
        let kept = reject_finding(
            &mut state,
            "src/a.rs",
            3,
            "Lock is dropped too early",
            false,
            "owner rejected",
            "head",
        );
        let gone = reject_finding(
            &mut state,
            "src/b.rs",
            5,
            "Buffer grows unbounded",
            false,
            "owner rejected",
            "head",
        );
        assert_eq!(
            state
                .issues
                .iter()
                .filter(|issue| matches!(issue.status, IssueStatus::Rejected { .. }))
                .count(),
            2
        );
        let removed = clear_suppression(&mut state, Some(&gone));
        assert_eq!(removed, 1, "only the named suppression goes");
        assert_eq!(state.suppressions.len(), 1);
        assert_eq!(
            state.suppressions.first().ok_or("kept")?.id,
            kept,
            "the survivor keeps its id"
        );
        let rejected: Vec<&Issue> = state
            .issues
            .iter()
            .filter(|issue| matches!(issue.status, IssueStatus::Rejected { .. }))
            .collect();
        assert_eq!(
            rejected.len(),
            1,
            "the orphaned rejection reopens, the kept one stays"
        );
        let removed_all = clear_suppression(&mut state, None);
        assert_eq!(removed_all, 1, "clearing without an id empties the set");
        assert!(state.suppressions.is_empty());
        assert!(
            state
                .issues
                .iter()
                .all(|issue| issue.status == IssueStatus::Open),
            "no dangling rejections survive a full clear"
        );
        Ok(())
    }

    fn draft(title: &str, file: &str, line: u32) -> FindingDraft {
        FindingDraft {
            region: RegionId(0),
            aspect: AspectId("concurrency".to_owned()),
            title: title.to_owned(),
            mechanism: "path line failure".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: file.to_owned(),
                line,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        }
    }

    fn entry(title: &str, file: &str, line: u32, votes: usize) -> Reconciled {
        Reconciled {
            finding: Finding::new(draft(title, file, line)),
            votes: vec!["zai:glm-5.3".to_owned(); votes],
            families: vec!["glm-5.3".to_owned()],
            severity: Severity::Warning,
        }
    }

    fn verdict_with(
        confirmed: Vec<Reconciled>,
        solo: Vec<Reconciled>,
        suppressed: Vec<Reconciled>,
    ) -> RoundVerdict {
        RoundVerdict {
            weak: Vec::new(),
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            coverage: CoverageSummary {
                cells_total: 1,
                cells_judged: 1,
                cells_clean: 1,
                cells_defect: 0,
                judged_by_judge: BTreeMap::new(),
                regions: 1,
                per_file: BTreeMap::new(),
                cells_short: 0,
            },
            tool_findings: Vec::new(),
            confirmed,
            solo,
            refuted: Vec::new(),
            suppressed,
            dropped: Vec::new(),
            absent_judges: Vec::new(),
            incomplete: Vec::new(),
            tools_unavailable: Vec::new(),
            stats: Vec::new(),
            lead_verdicts: std::collections::BTreeMap::new(),
            state: fresh(),
        }
    }

    fn fresh() -> RoundState {
        RoundState {
            checkout: "work".to_owned(),
            round: 0,
            issues: Vec::new(),
            suppressions: Vec::new(),
        }
    }

    fn suppression(theme: &str, file_glob: &str) -> Suppression {
        Suppression {
            id: "sup-1".to_owned(),
            theme: theme.to_owned(),
            file_glob: file_glob.to_owned(),
            reason: "owner rejected the theme".to_owned(),
            created: "2026-09-23".to_owned(),
            hits: 0,
        }
    }

    #[test]
    fn a_partial_round_bumps_only_suppression_hits() {
        let mut hot = suppression("Buffer grows unbounded", "src/*.rs");
        hot.id = "sup-hit".to_owned();
        let mut cold = suppression("Unrelated theme", "docs/*");
        cold.id = "sup-cold".to_owned();
        let mut state = fresh();
        state.suppressions = vec![hot, cold];
        let mut partial = state.clone();
        apply_partial_round(
            &mut partial,
            &verdict_with(
                vec![],
                vec![],
                vec![entry("Buffer grows unbounded", "src/a.rs", 2, 1)],
            ),
        );
        let hits = |id: &str| {
            partial
                .suppressions
                .iter()
                .find(|rule| rule.id == id)
                .map_or(0, |rule| rule.hits)
        };
        assert_eq!(
            hits("sup-hit"),
            1,
            "a suppression that fired in the partial round counts its hit"
        );
        assert_eq!(hits("sup-cold"), 0, "cold suppressions stay cold");
        assert_eq!(
            partial.round, state.round,
            "the round clock does not move on a partial round"
        );
        assert_eq!(partial.issues, state.issues, "issues are untouched");
    }

    #[test]
    fn a_partial_round_without_a_firing_suppression_changes_nothing() {
        let mut state = fresh();
        state
            .suppressions
            .push(suppression("Unrelated theme", "docs/*"));
        let mut partial = state.clone();
        apply_partial_round(
            &mut partial,
            &verdict_with(
                vec![],
                vec![],
                vec![entry("Buffer grows unbounded", "src/a.rs", 2, 1)],
            ),
        );
        assert_eq!(
            partial, state,
            "a partial round with no firing suppression writes nothing new"
        );
    }

    #[test]
    fn a_new_finding_opens_an_issue_at_round_one() {
        let mut state = fresh();
        apply_round(
            &mut state,
            &verdict_with(vec![entry("Off by one", "src/a.rs", 10, 2)], vec![], vec![]),
            true,
            false,
        );
        assert_eq!(state.round, 1);
        assert!(matches!(
            state.issues.first(),
            Some(issue) if issue.status == IssueStatus::Open
                && issue.misses == 0
                && issue.first_seen == "head"
                && issue.last_seen == "head"
                && issue.votes_seen == vec![(1, 2)]
        ));
    }

    #[test]
    fn two_consecutive_full_rounds_retire() {
        let mut state = fresh();
        apply_round(
            &mut state,
            &verdict_with(vec![entry("Off by one", "src/a.rs", 10, 2)], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            true,
            false,
        );
        assert!(
            matches!(state.issues.first(), Some(issue)
                if issue.status == IssueStatus::Open && issue.misses == 1),
            "one absence is not evidence: per-pass recall misses real issues"
        );
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            true,
            false,
        );
        assert!(matches!(
            state.issues.first(),
            Some(issue) if matches!(issue.status, IssueStatus::Retired { round: 3, .. })
        ));
    }

    #[test]
    fn partial_never_grows_misses() {
        let mut state = fresh();
        apply_round(
            &mut state,
            &verdict_with(vec![entry("Off by one", "src/a.rs", 10, 2)], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            false,
            false,
        );
        assert!(
            matches!(state.issues.first(), Some(issue)
                if issue.status == IssueStatus::Open && issue.misses == 0),
            "absence without coverage is not a fix"
        );
    }

    #[test]
    fn narrowed_never_grows_misses() {
        let mut state = fresh();
        apply_round(
            &mut state,
            &verdict_with(vec![entry("Off by one", "src/a.rs", 10, 2)], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            true,
            true,
        );
        assert!(
            matches!(state.issues.first(), Some(issue)
                if issue.status == IssueStatus::Open && issue.misses == 0),
            "a narrowed round cannot re-raise out-of-scope issues, so absence is not evidence"
        );
    }

    #[test]
    fn a_refind_reopens_a_retired_issue() {
        let mut state = fresh();
        apply_round(
            &mut state,
            &verdict_with(vec![entry("Off by one", "src/a.rs", 10, 2)], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(
                vec![entry("Off by one again", "src/a.rs", 12, 2)],
                vec![],
                vec![],
            ),
            true,
            false,
        );
        assert!(matches!(
            state.issues.first(),
            Some(issue) if issue.status == IssueStatus::Open
                && issue.misses == 0
                && issue.votes_seen.len() == 2
        ));
    }

    #[test]
    fn two_same_spot_findings_in_one_round_are_one_issue() {
        let mut state = fresh();
        let verdict = verdict_with(
            vec![
                entry("Lock is dropped too early", "src/a.rs", 10, 2),
                entry("The lock is dropped early", "src/a.rs", 12, 3),
            ],
            vec![],
            vec![],
        );
        apply_round(&mut state, &verdict, true, false);
        assert_eq!(state.issues.len(), 1);
        assert!(
            matches!(state.issues.first(), Some(issue) if issue.votes_seen == vec![(1, 3)]),
            "the second finding records into the same issue, keeping the stronger vote count"
        );
    }

    #[test]
    fn a_confirmed_twin_cannot_unreject_a_same_round_suppression() {
        let mut state = fresh();
        state
            .suppressions
            .push(suppression("api.max_tokens is ignored", "src/provider.rs"));
        let twin = |votes| {
            entry(
                "api.max_tokens is ignored for profiles",
                "src/provider.rs",
                394,
                votes,
            )
        };
        let mut verdict = verdict_with(vec![], vec![], vec![twin(2)]);
        verdict.confirmed.push(twin(3));
        apply_round(&mut state, &verdict, true, false);
        assert!(
            matches!(state.issues.first(), Some(issue)
                if matches!(&issue.status, IssueStatus::Rejected { id } if id == "sup-1")),
            "the owner's rejection holds for the round it fired in, whatever bucket the twin rode"
        );
        assert!(matches!(&state.suppressions.first(), Some(rule) if rule.hits == 1));
        assert!(
            matches!(state.issues.first(), Some(issue) if issue.votes_seen == vec![(1, 3)]),
            "the twin still counts as presence with its votes recorded"
        );
    }

    #[test]
    fn solo_findings_track_and_retire_like_confirmed() {
        let mut state = fresh();
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![entry("Off by one", "src/a.rs", 10, 1)], vec![]),
            true,
            false,
        );
        assert!(matches!(
            state.issues.first(),
            Some(issue) if issue.status == IssueStatus::Open && issue.votes_seen == vec![(1, 1)]
        ));
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![]),
            true,
            false,
        );
        assert!(matches!(
            state.issues.first(),
            Some(issue) if matches!(issue.status, IssueStatus::Retired { .. })
        ));
    }

    #[test]
    fn refuted_and_tool_findings_leave_state_untouched() {
        let mut state = fresh();
        let mut verdict = verdict_with(vec![], vec![], vec![]);
        verdict
            .refuted
            .push(entry("Hallucinated race", "src/a.rs", 10, 2));
        verdict.tool_findings.push(ToolFinding {
            tool: "cargo-clippy".to_owned(),
            file: "src/a.rs".to_owned(),
            line: 4,
            severity: Severity::Warning,
            message: "needless borrow".to_owned(),
        });
        apply_round(&mut state, &verdict, true, false);
        assert!(
            state.issues.is_empty(),
            "refuted claims and deterministic tools do not join the issue lifecycle"
        );
    }

    #[test]
    fn votes_seen_records_the_round_and_the_vote_count() {
        let mut state = fresh();
        apply_round(
            &mut state,
            &verdict_with(vec![entry("Off by one", "src/a.rs", 10, 2)], vec![], vec![]),
            true,
            false,
        );
        apply_round(
            &mut state,
            &verdict_with(
                vec![entry("Off by one again", "src/a.rs", 10, 3)],
                vec![],
                vec![],
            ),
            true,
            false,
        );
        assert!(matches!(
            state.issues.first(),
            Some(issue) if issue.votes_seen == vec![(1, 2), (2, 3)]
        ));
    }

    #[test]
    fn votes_seen_keeps_only_the_recent_entries() {
        let mut state = fresh();
        for _ in 0..21 {
            apply_round(
                &mut state,
                &verdict_with(vec![entry("Off by one", "src/a.rs", 10, 2)], vec![], vec![]),
                true,
                false,
            );
        }
        assert!(matches!(
            state.issues.first(),
            Some(issue) if issue.votes_seen.len() == 20
                && issue.votes_seen.first() == Some(&(2, 2))
                && issue.votes_seen.last() == Some(&(21, 2))
        ));
    }

    #[test]
    fn suppressed_theme_fires_repeatedly_with_hits() {
        let mut state = fresh();
        state
            .suppressions
            .push(suppression("api.max_tokens is ignored", "src/**/*.rs"));
        let round = || {
            verdict_with(
                vec![],
                vec![],
                vec![entry(
                    "api.max_tokens is ignored for profiles",
                    "src/provider.rs",
                    394,
                    2,
                )],
            )
        };
        apply_round(&mut state, &round(), true, false);
        apply_round(&mut state, &round(), true, false);
        assert!(
            matches!(&state.suppressions.first(), Some(rule) if rule.hits == 2),
            "every suppressed re-raise counts a hit"
        );
        assert!(
            matches!(state.issues.first(), Some(issue)
                if matches!(&issue.status, IssueStatus::Rejected { id } if id == "sup-1")
                    && issue.misses == 0),
            "a rejected issue never accumulates misses"
        );
    }

    #[test]
    fn a_refind_reopens_a_rejected_issue_when_no_suppression_fires() {
        let mut state = fresh();
        state
            .suppressions
            .push(suppression("api.max_tokens is ignored", "src/**/*.rs"));
        let finding = || {
            entry(
                "api.max_tokens is ignored for profiles",
                "src/provider.rs",
                394,
                2,
            )
        };
        apply_round(
            &mut state,
            &verdict_with(vec![], vec![], vec![finding()]),
            true,
            false,
        );
        state.suppressions.clear();
        apply_round(
            &mut state,
            &verdict_with(vec![finding()], vec![], vec![]),
            true,
            false,
        );
        assert!(
            matches!(state.issues.first(), Some(issue) if issue.status == IssueStatus::Open),
            "the owner clearing the rejection lets the issue stand again"
        );
    }

    #[test]
    fn a_theme_with_half_the_wording_in_the_globbed_file_fires() {
        let rule = suppression("The lock is dropped too early", "src/**/*.rs");
        assert!(matches_suppression(
            &rule,
            &Finding::new(draft("lock is dropped early", "src/a.rs", 10))
        ));
    }

    #[test]
    fn a_theme_outside_the_globbed_file_never_fires() {
        let rule = suppression("The lock is dropped too early", "src/provider.rs");
        assert!(!matches_suppression(
            &rule,
            &Finding::new(draft("lock is dropped early", "src/a.rs", 10))
        ));
    }

    #[test]
    fn an_empty_evidence_finding_never_fires() {
        let rule = suppression("The lock is dropped too early", "**");
        let mut bare = draft("lock is dropped early", "src/a.rs", 10);
        bare.evidence = Vec::new();
        assert!(
            !matches_suppression(&rule, &Finding::new(bare)),
            "a finding with no evidence has no file to anchor the glob"
        );
    }

    #[test]
    fn a_finding_cited_in_any_evidence_file_fires() {
        let rule = suppression("The lock is dropped too early", "src/provider.rs");
        let mut wandering = draft("lock is dropped early", "src/a.rs", 1);
        wandering.evidence.push(Cite {
            file: "src/provider.rs".to_owned(),
            line: 2,
        });
        assert!(matches_suppression(&rule, &Finding::new(wandering)));
    }
}
