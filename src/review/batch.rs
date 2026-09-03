//! Batch orchestration: split the changed files into batches, review each
//! with one agent run, aggregate and ground the findings, then either post
//! the review or hand back the identical rendered content for a dry run —
//! the render is the single code path, only the terminal step differs.

use loopctl::api::ApiClient;

use crate::error::DifftraceError;
use crate::findings::Findings;
use crate::findings::ReviewSummary;
use crate::github::CommentPosition;
use crate::github::ReviewSubmission;
use crate::review::ReviewRunner;
use crate::tools::submit::DroppedFinding;
use crate::tools::submit::ground_findings;

pub fn plan_batches(files: &[String], batch_size: usize) -> Vec<Vec<String>> {
    let mut sorted = files.to_vec();
    sorted.sort();
    let size = batch_size.max(1);
    sorted
        .chunks(size)
        .map(<[String]>::to_vec)
        .collect::<Vec<_>>()
}

pub struct ReviewOutcome {
    pub summary: ReviewSummary,
    pub comments: Vec<CommentPosition>,
    pub dropped: Vec<DroppedFinding>,
    pub posted: bool,
}

impl ReviewOutcome {
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let sections = [
            format!("## Summary\n{}", self.summary.summary),
            if self.summary.risk_notes.is_empty() {
                String::new()
            } else {
                let notes = self
                    .summary
                    .risk_notes
                    .iter()
                    .map(|note| format!("- {note}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("## Risks\n{notes}")
            },
            format!("## Tests\n{}", self.summary.tests),
            if self.dropped.is_empty() {
                String::new()
            } else {
                let drops = self
                    .dropped
                    .iter()
                    .map(|entry| {
                        format!("<!-- {}:{} — {} -->", entry.file, entry.line, entry.reason)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("<!-- difftrace: dropped findings (never posted) -->\n{drops}")
            },
        ];
        sections
            .iter()
            .filter(|section| !section.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n"
    }
}

impl<C: ApiClient + 'static> ReviewRunner<C> {
    pub async fn review_all(&self, dry_run: bool) -> Result<ReviewOutcome, DifftraceError> {
        let files = self.file_names();
        let mut aggregated = Findings::default();
        for batch in plan_batches(&files, self.settings().batch_files) {
            let findings = self.review_batch(&batch).await?;
            aggregated.findings.extend(findings.findings);
        }
        let grounded = ground_findings(
            self.index(),
            aggregated.findings,
            self.settings().max_findings_per_file,
        );
        let summary = self
            .summarize(&[Findings {
                findings: grounded.findings.clone(),
            }])
            .await?;
        let mut outcome = ReviewOutcome {
            summary,
            comments: grounded.comments,
            dropped: grounded.dropped,
            posted: false,
        };
        if dry_run {
            return Ok(outcome);
        }
        let submission = ReviewSubmission {
            head_sha: self.head_sha().to_owned(),
            summary: outcome.render_markdown(),
            comments: outcome.comments.clone(),
        };
        self.submit(submission).await?;
        outcome.posted = true;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::PrOverview;
    use crate::tools::fake_gateway::FakeGateway;
    use loopctl::testing::MockApiClient;
    use loopctl::testing::MockResponse;
    use loopctl::testing::MockToolCall;
    use serde_json::json;
    use std::sync::Arc;

    fn overview() -> PrOverview {
        PrOverview {
            number: 42,
            title: "Fix the worker".to_owned(),
            description: Some("Restarts consumers.".to_owned()),
            author: "dana".to_owned(),
            head_sha: "headsha".to_owned(),
            head_branch: "fix/worker".to_owned(),
            base_branch: "main".to_owned(),
            changed_files: 2,
            additions: 4,
            deletions: 2,
        }
    }

    fn diff_index() -> Result<crate::diff::DiffIndex, Box<dyn std::error::Error>> {
        let diff = "\
diff --git a/src/alpha.rs b/src/alpha.rs
--- a/src/alpha.rs
+++ b/src/alpha.rs
@@ -1,3 +1,3 @@
 ctx
-old
+new
 tail
diff --git a/src/beta.rs b/src/beta.rs
--- a/src/beta.rs
+++ b/src/beta.rs
@@ -10,3 +10,3 @@
 keep
-removed
+added
 done
";
        Ok(crate::diff::DiffIndex::parse(diff)?)
    }

    fn tool_call(id: &str, name: &str, input: serde_json::Value) -> MockResponse {
        MockResponse {
            text: String::new(),
            tool_call: Some(MockToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
            }),
            stop_reason: "tool_use".to_owned(),
        }
    }

    fn text_response(text: &str) -> MockResponse {
        MockResponse {
            text: text.to_owned(),
            tool_call: None,
            stop_reason: "end_turn".to_owned(),
        }
    }

    fn finding_json(file: &str, line: usize) -> serde_json::Value {
        json!({
            "file": file,
            "line": line,
            "severity": "warning",
            "title": "Title",
            "body": "Body"
        })
    }

    fn scripted_client() -> MockApiClient {
        let summary = json!({
            "summary": "Two files reviewed.",
            "risk_notes": ["Retry can outlive shutdown."],
            "tests": "Covered by integration tests."
        });
        MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({ "findings": [finding_json("src/alpha.rs", 2)] }),
            ),
            text_response("Batch one done."),
            tool_call(
                "call_2",
                "record_findings",
                json!({
                    "findings": [
                        finding_json("src/beta.rs", 11),
                        finding_json("src/beta.rs", 999)
                    ]
                }),
            ),
            text_response("Batch two done."),
            text_response(&summary.to_string()),
        ])
    }

    fn make_runner<C: ApiClient + 'static>(
        client: Arc<C>,
        gateway: Arc<FakeGateway>,
    ) -> Result<ReviewRunner<C>, Box<dyn std::error::Error>> {
        Ok(ReviewRunner::new(
            client,
            gateway,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                ..crate::config::ReviewSettings::default()
            },
            None,
        ))
    }

    #[test]
    fn batches_are_sorted_and_sized() {
        let files: Vec<String> = [
            "c.rs", "a.rs", "b.rs", "d.rs", "e.rs", "f.rs", "g.rs", "h.rs", "i.rs",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let batches = plan_batches(&files, 4);
        assert_eq!(batches.len(), 3);
        let expected: Vec<Vec<String>> = vec![
            vec!["a.rs", "b.rs", "c.rs", "d.rs"],
            vec!["e.rs", "f.rs", "g.rs", "h.rs"],
            vec!["i.rs"],
        ]
        .into_iter()
        .map(|batch| batch.into_iter().map(String::from).collect())
        .collect();
        assert_eq!(batches, expected);
    }

    #[test]
    fn a_zero_batch_size_is_treated_as_one() {
        let files = vec!["a.rs".to_owned(), "b.rs".to_owned()];
        let batches = plan_batches(&files, 0);
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|batch| batch.len() == 1));
    }

    #[tokio::test]
    async fn a_dry_run_aggregates_grounds_and_renders_without_posting()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let client = scripted_client();
        let runner = make_runner(Arc::new(client), Arc::clone(&gateway))?;
        let outcome = runner.review_all(true).await?;
        assert!(!outcome.posted);
        assert_eq!(gateway.submit_calls(), 0);
        assert_eq!(outcome.comments.len(), 2);
        assert_eq!(outcome.dropped.len(), 1);
        assert_eq!(
            outcome.dropped.first().ok_or("expected a drop")?.file,
            "src/beta.rs"
        );
        let rendered = outcome.render_markdown();
        assert!(rendered.contains("## Summary\nTwo files reviewed."));
        assert!(rendered.contains("- Retry can outlive shutdown."));
        assert!(rendered.contains("## Tests\nCovered by integration tests."));
        assert!(rendered.contains("src/beta.rs:999"));
        Ok(())
    }

    #[tokio::test]
    async fn the_summary_describes_only_the_grounded_findings()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = json!({
            "summary": "One finding: the beta anchor.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({
                    "findings": [
                        finding_json("src/alpha.rs", 2),
                        finding_json("src/alpha.rs", 999)
                    ]
                }),
            ),
            text_response("Batch done."),
            text_response(&summary.to_string()),
        ]);
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(client), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(submission.comments.len(), 1);
        assert_eq!(outcome.dropped.len(), 1);
        assert!(submission.summary.contains("One finding"));
        assert!(submission.summary.contains("alpha.rs:999"));
        let overstatement = "two findings";
        assert!(
            !submission.summary.contains(overstatement),
            "the summary must not count dropped findings"
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_posted_review_matches_the_dry_run_content()
    -> Result<(), Box<dyn std::error::Error>> {
        let post_gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&post_gateway))?;
        let dry = {
            let dry_gateway = Arc::new(FakeGateway::empty());
            let dry_runner = make_runner(Arc::new(scripted_client()), dry_gateway)?;
            dry_runner.review_all(true).await?
        };
        let posted = runner.review_all(false).await?;
        assert!(posted.posted);
        let submission = post_gateway
            .submitted()
            .ok_or("expected a submission to reach the gateway")?;
        assert_eq!(submission.head_sha, "headsha");
        assert_eq!(submission.comments, dry.comments);
        assert_eq!(submission.summary, dry.render_markdown());
        assert_eq!(posted.dropped, dry.dropped);
        Ok(())
    }
}
