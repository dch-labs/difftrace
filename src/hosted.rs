//! The hosted surface: actor authorization, the /review trigger
//! contract, and the pure posting plan that turns a verdict plus the
//! open review threads into one review submission, thread replies, and
//! resolutions — executed by the hosted arm through a sync port.

use crate::contracts::{
    CheckRun, InlineComment, Reconciled, ReviewEvent, ReviewSubmission, RoundState, RoundVerdict,
    Side, Thread,
};
use crate::error::DtError;
use crate::identity;
use crate::render::{escape_inline, severity_word};

pub trait HostedOps {
    fn permission(&self, actor: &str) -> Result<String, DtError>;
    fn pr_base_head(&self) -> Result<(String, String), DtError>;
    fn marker_comment(&self) -> Result<Option<(u64, String)>, DtError>;
    fn update_issue_comment(&self, comment_id: u64, body: &str) -> Result<(), DtError>;
    fn post_issue_comment(&self, body: &str) -> Result<(), DtError>;
    fn own_threads(&self) -> Result<Vec<Thread>, DtError>;
    fn submit_review(&self, submission: &ReviewSubmission) -> Result<(), DtError>;
    fn reply_to_thread(&self, comment_id: u64, body: &str) -> Result<(), DtError>;
    fn resolve_thread(&self, thread_id: &str) -> Result<(), DtError>;
    fn fetch_check_runs(&self, sha: &str) -> Result<Vec<CheckRun>, DtError>;
}

pub fn submit_with_fallback(
    hosted: &dyn HostedOps,
    submission: &ReviewSubmission,
) -> Result<(), DtError> {
    if let Err(err) = hosted.submit_review(submission) {
        if submission.event == ReviewEvent::Comment {
            return Err(err);
        }
        tracing::warn!(
            target: "difftrace::hosted",
            cause = %err,
            "the review event was rejected; retrying once as a neutral comment so the verdict still lands"
        );
        let neutral = ReviewSubmission {
            event: ReviewEvent::Comment,
            ..submission.clone()
        };
        hosted.submit_review(&neutral)?;
    }
    Ok(())
}

#[must_use]
pub fn permission_allows(permission: &str) -> bool {
    matches!(permission.trim(), "admin" | "write")
}

#[must_use]
pub fn is_review_command(body: &str) -> bool {
    body.lines()
        .next()
        .is_some_and(|line| line.trim() == "/review")
}

pub struct PostingPlan {
    pub review: ReviewSubmission,
    pub replies: Vec<ReplyPlan>,
    pub resolutions: Vec<ResolutionPlan>,
}

pub struct ReplyPlan {
    pub comment_id: u64,
    pub body: String,
}

pub struct ResolutionPlan {
    pub thread_id: String,
    pub comment_id: u64,
    pub note: String,
}

#[must_use]
pub fn review_body(head_sha: &str) -> String {
    let short = head_sha.get(..7).unwrap_or(head_sha);
    format!(
        "difftrace reviewed `{short}` — the verdict and state live in the difftrace comment on this pull request."
    )
}

#[must_use]
pub fn plan_posting(
    verdict: &RoundVerdict,
    threads: &[Thread],
    partial_round: bool,
    index: &crate::diff::DiffIndex,
) -> PostingPlan {
    let panel = verdict
        .coverage
        .judged_by_judge
        .len()
        .saturating_add(verdict.absent_judges.len());
    let event = if partial_round {
        ReviewEvent::Comment
    } else if verdict.blocking_count() > 0 {
        ReviewEvent::ChangesRequested
    } else {
        ReviewEvent::Approved
    };
    let mut plan = PostingPlan {
        review: ReviewSubmission {
            head_sha: verdict.range.head_sha.clone(),
            event,
            body: review_body(&verdict.range.head_sha),
            comments: Vec::new(),
        },
        replies: Vec::new(),
        resolutions: Vec::new(),
    };
    let mut claimed: Vec<&Thread> = Vec::new();
    for entry in &verdict.confirmed {
        let Some(cite) = entry.finding.evidence.first() else {
            continue;
        };
        let matched = threads.iter().find(|thread| {
            thread_matches(thread, &cite.file, cite.line) && !claimed.contains(thread)
        });
        let body = entry_body(entry, panel, &verdict.range.head_sha, matched.is_some());
        match matched {
            Some(thread) => {
                claimed.push(thread);
                plan.replies.push(ReplyPlan {
                    comment_id: thread.comment_id,
                    body,
                });
            }
            None => {
                if index.resolve(cite) {
                    plan.review.comments.push(InlineComment {
                        path: cite.file.clone(),
                        line: cite.line,
                        side: Side::Right,
                        body,
                    });
                }
            }
        }
    }
    for thread in threads {
        if claimed.contains(&thread) {
            continue;
        }
        let Some(note) = resolution_note(&verdict.state, thread) else {
            continue;
        };
        plan.resolutions.push(ResolutionPlan {
            thread_id: thread.id.clone(),
            comment_id: thread.comment_id,
            note,
        });
    }
    plan
}

fn thread_matches(thread: &Thread, file: &str, line: u32) -> bool {
    if thread.path != file {
        return false;
    }
    let anchors = [Some(thread.line), thread.original_line];
    anchors.iter().flatten().any(|anchor| {
        identity::lines_within(
            u64::from(*anchor),
            u64::from(line),
            identity::LINE_DRIFT_TOLERANCE,
        )
    })
}

fn resolution_note(state: &RoundState, thread: &Thread) -> Option<String> {
    let matched: Vec<&crate::contracts::Issue> = state
        .issues
        .iter()
        .filter(|issue| thread_matches(thread, &issue.identity.file, issue.identity.line))
        .collect();
    if matched
        .iter()
        .any(|issue| matches!(issue.status, crate::contracts::IssueStatus::Open))
    {
        return None;
    }
    let chosen = matched
        .iter()
        .find(|issue| issue.identity.line == thread.line)
        .or_else(|| {
            matched
                .iter()
                .find(|issue| Some(issue.identity.line) == thread.original_line)
        })
        .or_else(|| matched.first())?;
    match &chosen.status {
        crate::contracts::IssueStatus::Retired { round, .. } => Some(format!(
            "difftrace: resolving — the issue retired after repeated clean rounds (round {round})."
        )),
        crate::contracts::IssueStatus::Rejected { id } => Some(format!(
            "difftrace: resolving — the owner rejected this theme (suppression {id})."
        )),
        crate::contracts::IssueStatus::Open => None,
    }
}

fn entry_body(entry: &Reconciled, panel: usize, head_sha: &str, re_raise: bool) -> String {
    let mut lines = vec![format!(
        "[{}|{}|{}/{}] {}{}",
        severity_word(entry.severity),
        entry.finding.aspect.0,
        entry.votes.len(),
        panel,
        escape_inline(&entry.finding.title),
        anchor(&entry.finding.evidence)
    )];
    lines.push(format!(
        "    mechanism: {}",
        escape_inline(&entry.finding.mechanism)
    ));
    if re_raise {
        let short = head_sha.get(..7).unwrap_or(head_sha);
        lines.push(format!("Re-raised in the review of commit `{short}`."));
    }
    lines.join("\n")
}

fn anchor(evidence: &[crate::contracts::Cite]) -> String {
    match evidence.first() {
        Some(cite) => format!(" — {}:{}", escape_inline(&cite.file), cite.line),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::contracts::{
        AspectId, Cite, ClaimType, CoverageSummary, Finding, FindingDraft, Issue, IssueIdentity,
        IssueStatus, PinnedRange, Severity,
    };

    fn empty_index() -> crate::diff::DiffIndex {
        crate::diff::DiffIndex::parse("").unwrap_or_default()
    }

    fn diff_index() -> crate::diff::DiffIndex {
        let mut body = " ctx\n".repeat(50);
        body.push_str("+new");
        crate::diff::DiffIndex::parse(&format!(
            "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,50 +1,51 @@\n{body}\n"
        ))
        .unwrap_or_default()
    }

    fn confirmed(title: &str, file: &str, line: u32) -> Reconciled {
        Reconciled {
            finding: Finding::new(FindingDraft {
                region: crate::contracts::RegionId(0),
                aspect: AspectId("error-paths".to_owned()),
                title: title.to_owned(),
                mechanism: "path → line → failure".to_owned(),
                claim: ClaimType::InDiff,
                evidence: vec![Cite {
                    file: file.to_owned(),
                    line,
                }],
                severity: Severity::Warning,
                fix_hint: None,
            }),
            votes: vec!["a".to_owned(), "b".to_owned()],
            families: vec!["a".to_owned(), "b".to_owned()],
            severity: Severity::Warning,
        }
    }

    fn thread(id: &str, comment_id: u64, file: &str, line: u32) -> Thread {
        Thread {
            id: id.to_owned(),
            path: file.to_owned(),
            line,
            original_line: None,
            comment_id,
        }
    }

    fn issue(file: &str, line: u32, title: &str, status: IssueStatus) -> Issue {
        Issue {
            identity: IssueIdentity {
                file: file.to_owned(),
                line,
                title_tokens: identity::title_tokens(title).into_iter().collect(),
            },
            first_seen: "s0".to_owned(),
            last_seen: "s1".to_owned(),
            status,
            misses: 0,
            votes_seen: Vec::new(),
        }
    }

    fn verdict(
        confirmed_entries: Vec<Reconciled>,
        solo_entries: Vec<Reconciled>,
        issues: Vec<Issue>,
    ) -> RoundVerdict {
        RoundVerdict {
            range: PinnedRange {
                base_sha: "base0000".to_owned(),
                head_sha: "head123456789".to_owned(),
            },
            coverage: CoverageSummary {
                cells_total: 1,
                cells_judged: 1,
                cells_clean: 1,
                cells_defect: 0,
                judged_by_judge: [("a".to_owned(), 1), ("b".to_owned(), 1)]
                    .into_iter()
                    .collect(),
                regions: 1,
                per_file: BTreeMap::default(),
                cells_short: 0,
            },
            tool_findings: Vec::new(),
            confirmed: confirmed_entries,
            solo: solo_entries,
            weak: Vec::new(),
            refuted: Vec::new(),
            suppressed: Vec::new(),
            dropped: Vec::new(),
            absent_judges: Vec::new(),
            incomplete: Vec::new(),
            tools_unavailable: Vec::new(),
            stats: Vec::new(),
            lead_verdicts: BTreeMap::default(),
            state: RoundState {
                checkout: "work".to_owned(),
                round: 1,
                issues,
                suppressions: Vec::new(),
            },
        }
    }

    #[test]
    fn a_confirmed_finding_opens_a_thread() -> Result<(), Box<dyn std::error::Error>> {
        let entry = confirmed("Lock is dropped too early", "src/a.rs", 42);
        let verdict = verdict(vec![entry], Vec::new(), Vec::new());
        let plan = plan_posting(&verdict, &[], false, &diff_index());
        assert_eq!(plan.review.comments.len(), 1);
        let comment = plan
            .review
            .comments
            .first()
            .ok_or("the inline comment is missing")?;
        assert_eq!(comment.path, "src/a.rs");
        assert_eq!(comment.line, 42);
        assert_eq!(comment.side, Side::Right);
        assert!(comment.body.contains("[warning|error-paths|2/2]"));
        assert!(!comment.body.contains("Re-raised"));
        assert!(plan.replies.is_empty() && plan.resolutions.is_empty());
        Ok(())
    }

    #[test]
    fn a_snapshot_only_cite_never_mints_an_inline_comment() {
        let entry = confirmed("Lock is dropped too early", "src/a.rs", 42);
        let verdict = verdict(vec![entry], Vec::new(), Vec::new());
        let plan = plan_posting(&verdict, &[], false, &empty_index());
        assert!(
            plan.review.comments.is_empty(),
            "a cite the pinned diff does not contain can never anchor an inline comment: {:?}",
            plan.review.comments
        );
        assert!(
            plan.replies.is_empty() && plan.resolutions.is_empty(),
            "the finding rides the standing verdict comment instead"
        );
    }

    #[test]
    fn a_diff_pinned_cite_mints_the_inline_comment() {
        let entry = confirmed("Lock is dropped too early", "src/a.rs", 2);
        let verdict = verdict(vec![entry], Vec::new(), Vec::new());
        let plan = plan_posting(&verdict, &[], false, &diff_index());
        assert_eq!(
            plan.review.comments.len(),
            1,
            "a cite inside the diff hunks anchors safely"
        );
    }

    #[test]
    fn a_snapshot_only_cite_still_replies_into_a_matched_thread() {
        let entry = confirmed("Lock is dropped too early", "src/a.rs", 60);
        let shifted = thread("T_9", 21, "src/a.rs", 62);
        let verdict = verdict(vec![entry], Vec::new(), Vec::new());
        let plan = plan_posting(&verdict, &[shifted], false, &empty_index());
        assert!(
            plan.review.comments.is_empty(),
            "no inline comment for a snapshot-only cite"
        );
        assert_eq!(
            plan.replies.len(),
            1,
            "a thread reply carries the re-raise — replies anchor to threads, not diff lines"
        );
    }

    #[test]
    fn a_refind_replies_into_the_identity_matched_thread() -> Result<(), Box<dyn std::error::Error>>
    {
        let entry = confirmed("Lock is dropped too early", "src/a.rs", 44);
        let existing = thread("T_1", 11, "src/a.rs", 42);
        let verdict = verdict(vec![entry], Vec::new(), Vec::new());
        let plan = plan_posting(&verdict, &[existing], false, &diff_index());
        assert!(
            plan.review.comments.is_empty(),
            "a matched thread never also gets a duplicate inline comment"
        );
        let reply = plan.replies.first().ok_or("the reply is missing")?;
        assert_eq!(reply.comment_id, 11);
        assert!(
            reply
                .body
                .contains("Re-raised in the review of commit `head123`."),
            "the reply names the reviewed commit: {}",
            reply.body
        );
        assert!(plan.resolutions.is_empty());
        Ok(())
    }

    #[test]
    fn the_original_line_is_an_anchor_too() {
        let entry = confirmed("Lock is dropped too early", "src/a.rs", 60);
        let existing = thread("T_2", 12, "src/a.rs", 999);
        let mut shifted = existing;
        shifted.original_line = Some(58);
        let verdict = verdict(vec![entry], Vec::new(), Vec::new());
        let plan = plan_posting(&verdict, &[shifted], false, &empty_index());
        assert_eq!(plan.replies.len(), 1, "the original line anchors the match");
    }

    #[test]
    fn resolution_waits_for_the_second_miss() -> Result<(), Box<dyn std::error::Error>> {
        let standing = issue("src/a.rs", 10, "old finding", IssueStatus::Open);
        let existing = thread("T_3", 13, "src/a.rs", 10);
        let open_verdict = verdict(Vec::new(), Vec::new(), vec![standing]);
        let plan = plan_posting(
            &open_verdict,
            std::slice::from_ref(&existing),
            false,
            &empty_index(),
        );
        assert!(
            plan.resolutions.is_empty(),
            "an open issue's thread stays open — resolution follows retirement, not a single absence"
        );
        let mut retired_state = verdict(Vec::new(), Vec::new(), Vec::new());
        retired_state.state.issues = vec![issue(
            "src/a.rs",
            10,
            "old finding",
            IssueStatus::Retired {
                round: 3,
                sha: "s".to_owned(),
            },
        )];
        let plan = plan_posting(&retired_state, &[existing], false, &empty_index());
        let resolution = plan
            .resolutions
            .first()
            .ok_or("the resolution is missing")?;
        assert_eq!(resolution.thread_id, "T_3");
        assert!(resolution.note.contains("round 3"));
        Ok(())
    }

    #[test]
    fn a_rejected_issue_resolves_with_a_note() -> Result<(), Box<dyn std::error::Error>> {
        let existing = thread("T_4", 14, "src/b.rs", 7);
        let mut rejected = verdict(Vec::new(), Vec::new(), Vec::new());
        rejected.state.issues = vec![issue(
            "src/b.rs",
            7,
            "hallucinated theme",
            IssueStatus::Rejected {
                id: "s1".to_owned(),
            },
        )];
        let plan = plan_posting(&rejected, &[existing], false, &empty_index());
        let resolution = plan
            .resolutions
            .first()
            .ok_or("the resolution is missing")?;
        assert!(resolution.note.contains("s1"));
        assert!(resolution.note.contains("owner rejected"));
        Ok(())
    }

    #[test]
    fn a_thread_matching_any_open_issue_never_resolves() {
        let open = issue("src/a.rs", 12, "still standing", IssueStatus::Open);
        let retired = issue(
            "src/a.rs",
            9,
            "adjacent old thing",
            IssueStatus::Retired {
                round: 2,
                sha: "s".to_owned(),
            },
        );
        let existing = thread("T_7", 15, "src/a.rs", 12);
        let mut state = verdict(Vec::new(), Vec::new(), Vec::new());
        state.state.issues = vec![retired, open];
        let plan = plan_posting(&state, &[existing], false, &empty_index());
        assert!(
            plan.resolutions.is_empty(),
            "the retired first match must not resolve a thread whose own issue is still open"
        );
    }

    #[test]
    fn resolution_prefers_the_exact_line_match_among_resolved_issues() {
        let rejected_neighbor = issue(
            "src/a.rs",
            9,
            "adjacent old thing",
            IssueStatus::Rejected {
                id: "s2".to_owned(),
            },
        );
        let retired_exact = issue(
            "src/a.rs",
            12,
            "the real one",
            IssueStatus::Retired {
                round: 4,
                sha: "s".to_owned(),
            },
        );
        let existing = thread("T_8", 16, "src/a.rs", 12);
        let mut state = verdict(Vec::new(), Vec::new(), Vec::new());
        state.state.issues = vec![rejected_neighbor, retired_exact];
        let plan = plan_posting(&state, &[existing], false, &empty_index());
        let note = plan
            .resolutions
            .first()
            .map(|resolution| resolution.note.clone())
            .unwrap_or_default();
        assert!(
            note.contains("round 4") && !note.contains("s2"),
            "the exact-line match wins over the drifted first match: {note}"
        );
    }

    #[test]
    fn solo_findings_never_open_threads() {
        let solo_entry = confirmed("Unconfirmed hunch", "src/c.rs", 5);
        let verdict = verdict(Vec::new(), vec![solo_entry], Vec::new());
        let plan = plan_posting(&verdict, &[], false, &empty_index());
        assert!(
            plan.review.comments.is_empty(),
            "solo findings render in the verdict body only"
        );
        assert!(plan.replies.is_empty());
    }

    #[test]
    fn the_review_event_pairs_blocking_and_clean() {
        let blocking = confirmed("Hard failure", "src/a.rs", 1);
        let clean = verdict(Vec::new(), Vec::new(), Vec::new());
        assert_eq!(
            plan_posting(&clean, &[], false, &empty_index())
                .review
                .event,
            ReviewEvent::Approved
        );
        let blocking_verdict = verdict(vec![blocking], Vec::new(), Vec::new());
        assert_eq!(
            plan_posting(&blocking_verdict, &[], false, &empty_index())
                .review
                .event,
            ReviewEvent::ChangesRequested
        );
    }

    #[test]
    fn a_partial_round_withholds_both_approval_and_rejection() {
        let clean = verdict(Vec::new(), Vec::new(), Vec::new());
        assert_eq!(
            plan_posting(&clean, &[], true, &empty_index()).review.event,
            ReviewEvent::Comment,
            "a clean-looking partial round never posts approval"
        );
        let blocking = confirmed("Hard failure", "src/a.rs", 1);
        let blocking_verdict = verdict(vec![blocking], Vec::new(), Vec::new());
        assert_eq!(
            plan_posting(&blocking_verdict, &[], true, &empty_index())
                .review
                .event,
            ReviewEvent::Comment,
            "an incomplete round posts neutral commentary, never a verdict event"
        );
    }

    #[test]
    fn the_review_body_is_one_neutral_line_naming_the_commit() {
        let body = review_body("head123456789");
        assert_eq!(body.lines().count(), 1);
        assert!(body.contains("`head123`"));
        assert!(body.contains("difftrace comment"));
    }

    #[test]
    fn an_unauthorized_actor_is_refused() {
        assert!(permission_allows("admin"));
        assert!(permission_allows("write"));
        assert!(!permission_allows("read"));
        assert!(!permission_allows("none"));
        assert!(!permission_allows(""));
    }

    #[test]
    fn a_quoted_or_second_line_review_mention_never_triggers() {
        assert!(is_review_command("/review"));
        assert!(is_review_command("  /review  \nplease"));
        assert!(!is_review_command("\"/review\""));
        assert!(!is_review_command("looks good\n/review"));
        assert!(!is_review_command("please /review"));
    }

    #[test]
    fn state_marker_round_trips_through_the_comment() -> Result<(), Box<dyn std::error::Error>> {
        let state = RoundState {
            checkout: "work".to_owned(),
            round: 4,
            issues: vec![issue("src/a.rs", 9, "standing issue", IssueStatus::Open)],
            suppressions: Vec::new(),
        };
        let body = crate::state::marker::embed("the verdict text", &state)?;
        assert!(body.starts_with("the verdict text"));
        let back = crate::state::marker::extract(&body)?;
        assert_eq!(back, Some(state));
        Ok(())
    }
}
