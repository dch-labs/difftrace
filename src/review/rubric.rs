//! The rubric contributor: the review rules and pull-request frame,
//! re-emitted at every turn boundary so the run cannot drift off-goal,
//! plus the cross-round issue history when the registry has one.

use loopctl::contributor::ContextContributor;
use loopctl::contributor::ContributorContext;
use loopctl::message::Message;

use crate::github::PrOverview;
use crate::review::registry::Registry;

const RULES: &str = "\
You are reviewing one batch of the pull request's changed files.
Rules:
- Cite only lines inside the changed hunks of each file; a finding outside
  them will be dropped.
- Severity: nitpick, suggestion, warning, critical — reserve critical for
  defects that will bite in production.
- Complexity: rate each finding 1-5, where 1 is a one-liner and 5 needs
  restructuring.
- Report defects and concrete improvements; do not restate the diff.
- Do not report a finding whose own analysis concludes the code is
  deliberate, acceptable, or needs no change.
- State only facts you can verify from the pull request's materials
  (diffs, files, prior comments); anything about other repos, unseen
  files, or external conventions must be marked as an assumption.
- Before flagging code, check whether the file, the changelog, or prior
  comments document why it is the way it is; engage with a documented
  rationale instead of re-flagging it.
- When the same issue occurs at several locations, raise one finding per
  location and reuse the exact same title and severity for each.
- Use the tools to read context you need (file diff sections, full files,
  prior comments), then call record_findings exactly once with every
  finding, or with an empty list for a clean batch.";

// Only the rubric carries these: the reviewer must keep the registry's
// open issues alive by re-reporting them, and must not reverse settled
// fixes. The verifier gets the bare list — its drop criteria live in
// its own instructions.
const REVIEWER_HISTORY_RULES: &str = "\
Issues listed as still open are already tracked: if the problem is
still present, report it again at the same location with the same
title, and it will land in its existing thread. Do not open a separate
differently-titled finding for it. A suggestion that reverses one of
the fixed items needs explicit justification in its body; otherwise
skip it.";

const FIX_HISTORY_CAP: usize = 12;

// The registry as a compact cross-round context: still-open issues,
// then the fix history (most recent first, capped). Empty when the
// registry has nothing yet.
#[must_use]
pub(crate) fn cross_round_section(registry: &Registry) -> String {
    let open = registry.unresolved();
    if open.is_empty() && registry.issues.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec!["Issues already raised on this pull request:".to_owned()];
    for issue in &open {
        let location = issue
            .line
            .map_or_else(String::new, |line| format!(":{line}"));
        lines.push(format!(
            "- Still open: \"{}\" ({}{location})",
            issue.title, issue.file
        ));
    }
    for issue in registry.fixed_history().iter().rev().take(FIX_HISTORY_CAP) {
        let round = issue.resolved_round.unwrap_or(0);
        let line = if issue.status == crate::review::registry::IssueStatus::ManuallyResolved {
            format!("- Manually resolved in round {round}: \"{}\"", issue.title)
        } else {
            let sha = issue.resolved_sha.as_deref().unwrap_or_default();
            let short = crate::review::registry::short_sha(sha);
            format!(
                "- Fixed in round {round} (commit `{short}`): \"{}\"",
                issue.title
            )
        };
        lines.push(line);
    }
    lines.join("\n")
}

// The reviewer's variant: the bare list plus the mechanism-compatible
// re-raise rules.
#[must_use]
pub(crate) fn rubric_history(registry: &Registry) -> String {
    let list = cross_round_section(registry);
    if list.is_empty() {
        return list;
    }
    format!("{list}\n\n{REVIEWER_HISTORY_RULES}")
}

pub struct ReviewRubric {
    frame: String,
    history: Option<String>,
}

pub(crate) fn render_frame(overview: &PrOverview) -> String {
    let description = overview
        .description
        .as_deref()
        .unwrap_or("(no description)");
    format!(
        "Pull request #{} \"{}\" by {} ({} -> {}, {} files, +{}/-{}):\n{}",
        overview.number,
        overview.title,
        overview.author,
        overview.head_branch,
        overview.base_branch,
        overview.changed_files,
        overview.additions,
        overview.deletions,
        description
    )
}

impl ReviewRubric {
    #[must_use]
    pub fn new(overview: &PrOverview) -> Self {
        Self {
            frame: render_frame(overview),
            history: None,
        }
    }

    #[must_use]
    pub fn with_history(mut self, history: String) -> Self {
        if !history.is_empty() {
            self.history = Some(history);
        }
        self
    }
}

impl ContextContributor for ReviewRubric {
    fn contribute(&self, _ctx: &ContributorContext<'_>) -> Option<Message> {
        let base = format!("{RULES}\n\n{}", self.frame);
        match &self.history {
            Some(history) => Some(Message::user(format!("{base}\n\n{history}"))),
            None => Some(Message::user(base)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::PrOverview;

    #[test]
    fn the_rubric_carries_rules_and_frame_every_turn() -> Result<(), Box<dyn std::error::Error>> {
        let overview = PrOverview {
            number: 42,
            title: "Fix the worker".to_owned(),
            description: Some("Restarts consumers.".to_owned()),
            author: "dana".to_owned(),
            head_sha: "abc".to_owned(),
            head_branch: "fix/worker".to_owned(),
            base_branch: "main".to_owned(),
            changed_files: 3,
            additions: 10,
            deletions: 2,
        };
        let rubric = ReviewRubric::new(&overview);
        let conversation: Vec<Message> = Vec::new();
        let ctx = ContributorContext {
            turn: 7,
            conversation: &conversation,
        };
        let message = rubric.contribute(&ctx).ok_or("expected a value")?;
        let text = message
            .parts
            .iter()
            .find_map(|part| match part {
                loopctl::message::MessagePart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .ok_or("expected a value")?;
        assert!(text.contains("record_findings exactly once"));
        assert!(text.contains("rate each finding 1-5"));
        assert!(text.contains("#42 \"Fix the worker\""));
        assert!(text.contains("Restarts consumers."));
        assert!(
            text.contains("Do not report a finding whose own analysis concludes"),
            "the rubric forbids self-refuting findings"
        );
        assert!(
            text.contains("marked as an assumption"),
            "the rubric requires unverifiable claims to be labelled"
        );
        assert!(
            text.contains("document why it is the way it is"),
            "the rubric requires checking documented rationale"
        );
        assert!(
            text.contains("reuse the exact same title"),
            "the rubric asks the model to reuse titles so grouping can pair locations"
        );
        assert!(
            !text.contains("Issues already raised"),
            "without history the rubric stays silent about prior rounds"
        );
        Ok(())
    }

    #[test]
    fn the_rubric_carries_the_cross_round_history_when_set()
    -> Result<(), Box<dyn std::error::Error>> {
        let overview = PrOverview {
            number: 7,
            title: "T".to_owned(),
            description: None,
            author: "dana".to_owned(),
            head_sha: "abc".to_owned(),
            head_branch: "h".to_owned(),
            base_branch: "m".to_owned(),
            changed_files: 1,
            additions: 1,
            deletions: 0,
        };
        let rubric = ReviewRubric::new(&overview)
            .with_history("Issues already raised on this pull request:\n- Fixed in round 1 (commit `abc1234`): \"Old issue\"".to_owned());
        let conversation: Vec<Message> = Vec::new();
        let ctx = ContributorContext {
            turn: 1,
            conversation: &conversation,
        };
        let message = rubric.contribute(&ctx).ok_or("expected a value")?;
        let text = message
            .parts
            .iter()
            .find_map(|part| match part {
                loopctl::message::MessagePart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .ok_or("expected a value")?;
        assert!(text.contains("Issues already raised on this pull request:"));
        assert!(text.contains("Fixed in round 1"));
        Ok(())
    }

    #[test]
    fn the_cross_round_section_lists_open_and_fixed_issues() {
        use crate::review::registry::IssueStatus;
        let mut registry = Registry {
            round: 3,
            issues: vec![
                crate::review::registry::Issue {
                    title: "Still broken".to_owned(),
                    file: "src/a.rs".to_owned(),
                    line: Some(4),
                    severity: crate::findings::Severity::Warning,
                    complexity: 2,
                    anchored: true,
                    status: IssueStatus::Open,
                    thread_id: None,
                    raised_round: 1,
                    raised_sha: String::new(),
                    last_round: 2,
                    resolved_round: None,
                    resolved_sha: None,
                },
                crate::review::registry::Issue {
                    title: "Already fixed".to_owned(),
                    file: "src/b.rs".to_owned(),
                    line: Some(8),
                    severity: crate::findings::Severity::Nitpick,
                    complexity: 1,
                    anchored: true,
                    status: IssueStatus::Fixed,
                    thread_id: None,
                    raised_round: 1,
                    raised_sha: String::new(),
                    last_round: 1,
                    resolved_round: Some(2),
                    resolved_sha: Some("b2bb699dc1c7c4cb28db9adf619b58ce7d965d52".to_owned()),
                },
            ],
        };
        let section = cross_round_section(&registry);
        assert!(section.contains("- Still open: \"Still broken\" (src/a.rs:4)"));
        assert!(section.contains("- Fixed in round 2 (commit `b2bb699`): \"Already fixed\""));
        assert!(
            !section.contains("reverses one of the fixed items"),
            "the bare list carries no rules — the verifier supplies its own"
        );
        registry.issues.clear();
        assert!(
            cross_round_section(&registry).is_empty(),
            "a fresh registry renders no history"
        );
    }

    #[test]
    fn the_rubric_history_adds_the_re_raise_rules_to_the_bare_list() {
        use crate::review::registry::IssueStatus;
        let mut registry = Registry {
            round: 2,
            issues: vec![crate::review::registry::Issue {
                title: "Manually closed".to_owned(),
                file: "src/c.rs".to_owned(),
                line: Some(6),
                severity: crate::findings::Severity::Suggestion,
                complexity: 1,
                anchored: true,
                status: IssueStatus::ManuallyResolved,
                thread_id: None,
                raised_round: 1,
                raised_sha: String::new(),
                last_round: 1,
                resolved_round: Some(2),
                resolved_sha: None,
            }],
        };
        let history = rubric_history(&registry);
        assert!(
            history.contains("- Manually resolved in round 2: \"Manually closed\""),
            "a manually-resolved issue does not render as fixed with a commit"
        );
        assert!(
            history.contains("report it again at the same location"),
            "open issues must be re-reported, not starved"
        );
        assert!(history.contains("reverses one of"));
        registry.issues.clear();
        assert!(
            rubric_history(&registry).is_empty(),
            "a fresh registry renders no history and no rules"
        );
    }
}
