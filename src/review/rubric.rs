//! The rubric contributor: the review rules and pull-request frame,
//! re-emitted at every turn boundary so the run cannot drift off-goal.

use loopctl::contributor::ContextContributor;
use loopctl::contributor::ContributorContext;
use loopctl::message::Message;

use crate::github::PrOverview;

const RULES: &str = "\
You are reviewing one batch of the pull request's changed files.
Rules:
- Cite only lines inside the changed hunks of each file; a finding outside
  them will be dropped.
- Severity: nitpick, suggestion, warning, critical — reserve critical for
  defects that will bite in production.
- Report defects and concrete improvements; do not restate the diff.
- Use the tools to read context you need (file diff sections, full files,
  prior comments), then call record_findings exactly once with every
  finding, or with an empty list for a clean batch.";

pub struct ReviewRubric {
    frame: String,
}

fn render_frame(overview: &PrOverview) -> String {
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
        }
    }
}

impl ContextContributor for ReviewRubric {
    fn contribute(&self, _ctx: &ContributorContext<'_>) -> Option<Message> {
        Some(Message::user(format!("{RULES}\n\n{}", self.frame)))
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
        assert!(text.contains("#42 \"Fix the worker\""));
        assert!(text.contains("Restarts consumers."));
        Ok(())
    }
}
