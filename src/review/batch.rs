//! Batch orchestration: split the changed files into batches, review each
//! with one agent run, aggregate and ground the findings, then either post
//! the review or hand back the identical rendered content for a dry run —
//! the render is the single code path, only the terminal step differs.

use loopctl::api::ApiClient;

use crate::error::DifftraceError;
use crate::findings::Finding;
use crate::findings::Findings;
use crate::findings::ReviewSummary;
use crate::findings::Severity;
use crate::github::CommentPosition;
use crate::github::ReviewEvent;
use crate::github::ReviewSubmission;
use crate::github::ReviewThread;
use crate::prompts::fix_all_section;
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
    pub findings: Vec<Finding>,
    pub comments: Vec<CommentPosition>,
    pub dropped: Vec<DroppedFinding>,
    pub pr: u64,
    pub head_sha: String,
    pub posted: bool,
}

impl ReviewOutcome {
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let sections = [
            verdict_section(&self.findings),
            format!("## Summary\n{}", self.summary.summary),
            risks_section(&self.summary.risk_notes),
            format!("## Tests\n{}", self.summary.tests),
            fix_all_section(&self.findings, &self.dropped, self.pr, &self.head_sha),
            if self.dropped.is_empty() {
                String::new()
            } else {
                let drops = self
                    .dropped
                    .iter()
                    .map(|entry| {
                        format!(
                            "<!-- {}:{} — {} -->",
                            entry.finding.file, entry.finding.line, entry.reason
                        )
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

fn is_blocking(severity: Severity) -> bool {
    matches!(severity, Severity::Warning | Severity::Critical)
}

pub(crate) fn review_event(findings: &[Finding]) -> ReviewEvent {
    let blocking = findings.iter().any(|finding| is_blocking(finding.severity));
    if blocking {
        ReviewEvent::ChangesRequested
    } else {
        ReviewEvent::Approved
    }
}

fn threads_to_resolve(threads: &[ReviewThread], comments: &[CommentPosition]) -> Vec<String> {
    threads
        .iter()
        .filter_map(|thread| {
            let still_present = thread.line.or(thread.original_line).is_some_and(|anchor| {
                comments
                    .iter()
                    .any(|comment| comment.path == thread.path && comment.line == anchor)
            });
            if still_present {
                None
            } else {
                Some(thread.id.clone())
            }
        })
        .collect()
}

fn verdict_section(findings: &[Finding]) -> String {
    let blockers = findings
        .iter()
        .filter(|finding| is_blocking(finding.severity))
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        return "## Verdict\n\n🎉 Good to go — no blocking findings. Nitpicks and suggestions, if any, don't block."
            .to_owned();
    }
    let noun = if blockers.len() == 1 {
        "finding"
    } else {
        "findings"
    };
    let list = blockers
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            format!(
                "{}. `{}:{}` — {} {} ({})",
                index.saturating_add(1),
                finding.file,
                finding.line,
                finding.severity.glyph(),
                finding.title,
                crate::findings::complexity_glyph(finding.complexity),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Verdict\n\n🔴 Not good to go — {} blocking {}:\n\n{list}\n\nTo be good to go: fix the items above — each inline comment carries a fix prompt, or copy the fix-all prompt below. Nitpicks and suggestions don't block.",
        blockers.len(),
        noun,
    )
}

fn risks_section(notes: &[String]) -> String {
    if notes.is_empty() {
        return "## Risks\n\n(none flagged)".to_owned();
    }
    let list = notes
        .iter()
        .map(|note| format!("- {note}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("## Risks\n{list}")
}

impl<C: ApiClient + 'static> ReviewRunner<C> {
    pub async fn review_all(&self, dry_run: bool) -> Result<ReviewOutcome, DifftraceError> {
        let files = self.file_names();
        let batches = plan_batches(&files, self.settings().batch_files);
        tracing::info!(
            target: "difftrace::review",
            files = files.len(),
            batches = batches.len(),
            "reviewing"
        );
        let mut aggregated = Findings::default();
        for (index, batch) in batches.iter().enumerate() {
            tracing::info!(
                target: "difftrace::review",
                batch = index,
                files = batch.join(", "),
                "batch started"
            );
            let findings = self.review_batch(batch).await?;
            tracing::info!(
                target: "difftrace::review",
                batch = index,
                findings = findings.findings.len(),
                "batch finished"
            );
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
            findings: grounded.findings,
            comments: grounded.comments,
            dropped: grounded.dropped,
            pr: self.pr(),
            head_sha: self.head_sha().to_owned(),
            posted: false,
        };
        if dry_run {
            return Ok(outcome);
        }
        let previous_threads = match self.own_open_threads().await {
            Ok(threads) => threads,
            Err(err) => {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "could not list previous review threads; none will be resolved"
                );
                Vec::new()
            }
        };
        let submission = ReviewSubmission {
            head_sha: self.head_sha().to_owned(),
            event: review_event(&outcome.findings),
            summary: outcome.render_markdown(),
            comments: outcome.comments.clone(),
        };
        self.submit(submission).await?;
        let resolved = threads_to_resolve(&previous_threads, &outcome.comments);
        for id in &resolved {
            if let Err(err) = self.resolve_thread(id.clone()).await {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "could not resolve a previous review thread"
                );
            }
        }
        tracing::info!(
            target: "difftrace::review",
            resolved = resolved.len(),
            "resolved previous threads"
        );
        outcome.posted = true;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::PrOverview;
    use crate::github::Side;
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
        finding_json_severity(file, line, "warning")
    }

    fn finding_json_severity(file: &str, line: usize, severity: &str) -> serde_json::Value {
        json!({
            "file": file,
            "line": line,
            "severity": severity,
            "complexity": 3,
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

    #[test]
    fn only_unreposted_threads_are_slated_for_resolution() {
        let threads = vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                path: "src/beta.rs".to_owned(),
                line: Some(13),
                original_line: Some(13),
            },
            ReviewThread {
                id: "T_MOVED".to_owned(),
                path: "src/gone.rs".to_owned(),
                line: None,
                original_line: Some(40),
            },
        ];
        let comments = vec![
            CommentPosition {
                path: "src/alpha.rs".to_owned(),
                line: 2,
                side: Side::Right,
                body: String::new(),
            },
            CommentPosition {
                path: "src/beta.rs".to_owned(),
                line: 11,
                side: Side::Right,
                body: String::new(),
            },
        ];
        let slated = threads_to_resolve(&threads, &comments);
        assert_eq!(slated, vec!["T_FIXED".to_owned(), "T_MOVED".to_owned()]);
    }

    #[tokio::test]
    async fn a_re_review_resolves_unreposted_threads_and_requests_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                path: "src/beta.rs".to_owned(),
                line: Some(13),
                original_line: Some(13),
            },
        ]));
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert!(outcome.posted);
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(submission.event, ReviewEvent::ChangesRequested);
        assert_eq!(
            gateway.resolved_threads(),
            vec!["T_FIXED".to_owned()],
            "only the thread whose line was not reposted resolves"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stage_and_observer_logs_reach_an_installed_subscriber()
    -> Result<(), Box<dyn std::error::Error>> {
        let (logs, _guard) = crate::review::logging::test_support::install();
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(scripted_client()), gateway)?;
        let outcome = runner.review_all(true).await?;
        assert_eq!(outcome.comments.len(), 2);
        let text = logs.text();
        assert!(text.contains("reviewing"), "stage logs must emit");
        assert!(text.contains("batch started"));
        assert!(text.contains("batch finished"));
        assert!(
            text.contains("run started"),
            "the logging observer must be registered"
        );
        assert!(text.contains("tool call"));
        Ok(())
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
            outcome
                .dropped
                .first()
                .ok_or("expected a drop")?
                .finding
                .file,
            "src/beta.rs"
        );
        let rendered = outcome.render_markdown();
        assert!(rendered.starts_with("## Verdict"));
        assert!(rendered.contains("🔴 Not good to go — 2 blocking findings:"));
        assert!(rendered.contains("1. `src/alpha.rs:2` — ⚠️ Title (🟡)"));
        assert!(rendered.contains("2. `src/beta.rs:11` — ⚠️ Title (🟡)"));
        assert!(
            !rendered.contains("3 blocking"),
            "an unanchored finding must not join the verdict's blockers"
        );
        assert!(rendered.contains("To be good to go: fix the items above"));
        assert!(rendered.contains("## Summary\nTwo files reviewed."));
        assert!(rendered.contains("- Retry can outlive shutdown."));
        assert!(rendered.contains("## Tests\nCovered by integration tests."));
        assert!(rendered.contains("## 🤖 Fix all findings"));
        assert!(rendered.contains("1. `src/alpha.rs:2` — ⚠️ Title (🟡)"));
        assert!(rendered.contains("2. `src/beta.rs:11` — ⚠️ Title (🟡)"));
        assert!(
            rendered
                .contains("- `src/beta.rs:999` — ⚠️ Title (🟡, line outside the changed hunks)")
        );
        assert!(rendered.contains("PR #42"));
        assert!(rendered.contains("commit headsha"));
        assert!(rendered.contains("<summary>Copy the fix-all prompt</summary>"));
        assert!(rendered.contains("src/beta.rs:999"));
        Ok(())
    }

    #[tokio::test]
    async fn a_review_without_findings_renders_no_fix_all_section()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = json!({
            "summary": "Nothing to flag.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call("call_1", "record_findings", json!({ "findings": [] })),
            text_response("Batch one done."),
            tool_call("call_2", "record_findings", json!({ "findings": [] })),
            text_response("Batch two done."),
            text_response(&summary.to_string()),
        ]);
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(client), gateway)?;
        let outcome = runner.review_all(true).await?;
        assert!(outcome.comments.is_empty());
        assert!(outcome.dropped.is_empty());
        let rendered = outcome.render_markdown();
        assert!(rendered.starts_with("## Verdict"));
        assert!(rendered.contains("🎉 Good to go — no blocking findings."));
        assert!(rendered.contains("## Risks\n\n(none flagged)"));
        assert!(rendered.contains("## Summary\nNothing to flag."));
        assert!(
            !rendered.contains("Fix all findings"),
            "a clean review must not render a fix-all section"
        );
        Ok(())
    }

    #[tokio::test]
    async fn nitpicks_and_suggestions_do_not_block_the_verdict()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = json!({
            "summary": "Polish only.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({ "findings": [finding_json_severity("src/alpha.rs", 2, "suggestion")] }),
            ),
            text_response("Batch one done."),
            tool_call(
                "call_2",
                "record_findings",
                json!({ "findings": [finding_json_severity("src/beta.rs", 11, "nitpick")] }),
            ),
            text_response("Batch two done."),
            text_response(&summary.to_string()),
        ]);
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(client), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert_eq!(outcome.comments.len(), 2);
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(submission.event, ReviewEvent::Approved);
        let rendered = outcome.render_markdown();
        assert!(rendered.contains("🎉 Good to go — no blocking findings."));
        assert!(rendered.contains("1. `src/alpha.rs:2` — 💡 Title"));
        assert!(rendered.contains("2. `src/beta.rs:11` — 💬 Title"));
        assert!(gateway.resolved_threads().is_empty());
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
        assert!(submission.summary.starts_with("## Verdict"));
        assert!(
            submission
                .summary
                .contains("🔴 Not good to go — 1 blocking finding:")
        );
        assert!(submission.summary.contains("## Risks\n\n(none flagged)"));
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
