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
use crate::prompts::REVIEW_POINTER_BODY;
use crate::prompts::fix_all_section;
use crate::prompts::re_raised_reply_body;
use crate::review::ReviewRunner;
use crate::tools::submit::DroppedFinding;
use crate::tools::submit::ground_findings;

pub(crate) const VERDICT_MARKER: &str = "<!-- difftrace:verdict -->";
const VERDICT_WRITE_ATTEMPTS: u8 = 3;
const VERDICT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

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

fn verdict_comment_body(outcome: &ReviewOutcome) -> String {
    format!(
        "{VERDICT_MARKER}\n\n{}\n\n---\nReviewed commit: `{}`",
        outcome.render_markdown().trim_end(),
        outcome.head_sha
    )
}

fn review_body(outcome: &ReviewOutcome) -> String {
    format!(
        "{}\n\n{REVIEW_POINTER_BODY}",
        verdict_section(&outcome.findings)
    )
}

fn split_replies(
    threads: &[ReviewThread],
    comments: Vec<CommentPosition>,
    head_sha: &str,
) -> (Vec<CommentPosition>, Vec<(u64, String)>, Vec<String>) {
    let mut positions = Vec::new();
    let mut replies = Vec::new();
    let mut matched = Vec::new();
    for comment in comments {
        let thread = threads.iter().find(|thread| {
            thread.line.or(thread.original_line) == Some(comment.line)
                && thread.path == comment.path
                && !matched.contains(&thread.id)
        });
        match thread {
            Some(thread) => {
                matched.push(thread.id.clone());
                replies.push((
                    thread.comment_id,
                    re_raised_reply_body(&comment.body, head_sha),
                ));
            }
            None => positions.push(comment),
        }
    }
    (positions, replies, matched)
}

fn threads_to_resolve(threads: &[ReviewThread], matched: &[String]) -> Vec<String> {
    threads
        .iter()
        .filter(|thread| !matched.contains(&thread.id))
        .map(|thread| thread.id.clone())
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
        let (positions, replies, matched) =
            split_replies(&previous_threads, outcome.comments.clone(), self.head_sha());
        let submission = ReviewSubmission {
            head_sha: self.head_sha().to_owned(),
            event: review_event(&outcome.findings),
            summary: review_body(&outcome),
            comments: positions,
        };
        self.submit(submission).await?;
        self.post_re_raised_replies(replies).await;
        let resolved = threads_to_resolve(&previous_threads, &matched);
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
        self.upsert_verdict_comment(&outcome).await?;
        outcome.posted = true;
        Ok(outcome)
    }

    async fn post_re_raised_replies(&self, replies: Vec<(u64, String)>) {
        let mut posted = 0usize;
        for (comment_id, body) in replies {
            if let Err(err) = self
                .gateway()
                .reply_to_review_comment(self.pr(), comment_id, body)
                .await
            {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "could not reply into a previous review thread"
                );
                continue;
            }
            posted = posted.saturating_add(1);
        }
        tracing::info!(
            target: "difftrace::review",
            replied = posted,
            "replied into re-raised threads"
        );
    }

    async fn upsert_verdict_comment(&self, outcome: &ReviewOutcome) -> Result<(), DifftraceError> {
        let body = verdict_comment_body(outcome);
        let mut attempts_left = VERDICT_WRITE_ATTEMPTS;
        loop {
            let write = match self
                .gateway()
                .find_own_marker_comment(self.pr(), VERDICT_MARKER.to_owned())
                .await
            {
                Ok(Some(comment_id)) => {
                    self.gateway()
                        .update_issue_comment(comment_id, body.clone())
                        .await
                }
                Ok(None) => {
                    self.gateway()
                        .post_pr_comment(self.pr(), body.clone())
                        .await
                }
                Err(err) => Err(err),
            };
            match write {
                Ok(()) => {
                    tracing::info!(target: "difftrace::review", "verdict comment written");
                    return Ok(());
                }
                Err(err) if attempts_left <= 1 => {
                    tracing::error!(
                        target: "difftrace::review",
                        error = %crate::error::error_chain(&err),
                        "could not write the verdict comment; giving up after every retry"
                    );
                    return Err(err);
                }
                Err(err) => {
                    attempts_left = attempts_left.saturating_sub(1);
                    tracing::warn!(
                        target: "difftrace::review",
                        error = %crate::error::error_chain(&err),
                        "could not write the verdict comment; retrying"
                    );
                    tokio::time::sleep(VERDICT_RETRY_DELAY).await;
                }
            }
        }
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
        make_runner_with(client, gateway, overview())
    }

    fn make_runner_with<C: ApiClient + 'static>(
        client: Arc<C>,
        gateway: Arc<FakeGateway>,
        pr_overview: PrOverview,
    ) -> Result<ReviewRunner<C>, Box<dyn std::error::Error>> {
        Ok(ReviewRunner::new(
            client,
            gateway,
            Arc::new(diff_index()?),
            pr_overview,
            crate::config::ReviewSettings {
                batch_files: 1,
                ..crate::config::ReviewSettings::default()
            },
            None,
        ))
    }

    fn overview_at_sha(head_sha: &str) -> PrOverview {
        PrOverview {
            head_sha: head_sha.to_owned(),
            ..overview()
        }
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
    fn only_unmatched_threads_are_slated_for_resolution() {
        let threads = vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                comment_id: 501,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                comment_id: 502,
                path: "src/beta.rs".to_owned(),
                line: Some(13),
                original_line: Some(13),
            },
            ReviewThread {
                id: "T_MOVED".to_owned(),
                comment_id: 503,
                path: "src/gone.rs".to_owned(),
                line: None,
                original_line: Some(40),
            },
        ];
        let slated = threads_to_resolve(&threads, &["T_KEEP".to_owned()]);
        assert_eq!(slated, vec!["T_FIXED".to_owned(), "T_MOVED".to_owned()]);
    }

    #[test]
    fn reply_matching_prefers_the_current_line_then_the_original() {
        let threads = vec![
            ReviewThread {
                id: "T_CURRENT".to_owned(),
                comment_id: 601,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(9),
            },
            ReviewThread {
                id: "T_OUTDATED".to_owned(),
                comment_id: 602,
                path: "src/beta.rs".to_owned(),
                line: None,
                original_line: Some(11),
            },
        ];
        let comment = |path: &str, line: u64| CommentPosition {
            path: path.to_owned(),
            line,
            side: Side::Right,
            body: format!("{path}:{line}"),
        };
        let (positions, replies, matched) = split_replies(
            &threads,
            vec![
                comment("src/alpha.rs", 2),
                comment("src/beta.rs", 11),
                comment("src/gamma.rs", 5),
            ],
            "headsha",
        );
        assert_eq!(
            matched,
            vec!["T_CURRENT".to_owned(), "T_OUTDATED".to_owned()],
            "the current line matches first, the original line as fallback"
        );
        assert_eq!(positions.len(), 1, "only the unmatched comment stays fresh");
        assert_eq!(
            positions.first().map(|c| c.path.as_str()),
            Some("src/gamma.rs")
        );
        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies.first().map(|(id, _)| *id),
            Some(601),
            "the current-line match replies first"
        );
        assert_eq!(replies.get(1).map(|(id, _)| *id), Some(602));
        assert!(
            replies.first().is_some_and(
                |(_, body)| body.contains("Re-raised in the review of commit `headsha`.")
            ),
            "each reply names the reviewed commit"
        );
    }

    #[test]
    fn verdict_comment_body_wraps_the_render_with_marker_and_footer() {
        let outcome = ReviewOutcome {
            summary: crate::findings::ReviewSummary {
                summary: "One file.".to_owned(),
                risk_notes: Vec::new(),
                tests: "Covered.".to_owned(),
            },
            findings: Vec::new(),
            comments: Vec::new(),
            dropped: Vec::new(),
            pr: 42,
            head_sha: "9f3b2c1".to_owned(),
            posted: false,
        };
        let body = verdict_comment_body(&outcome);
        assert!(body.starts_with("<!-- difftrace:verdict -->\n"));
        assert!(body.contains("## Verdict"));
        assert!(body.ends_with("Reviewed commit: `9f3b2c1`"));
        let render = outcome.render_markdown();
        assert!(
            !render.contains("difftrace:verdict"),
            "the dry-run render must not carry the marker"
        );
        assert!(
            !render.contains("Reviewed commit"),
            "the dry-run render must not carry the footer"
        );
    }

    #[tokio::test]
    async fn a_re_review_resolves_unreposted_threads_and_requests_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                comment_id: 501,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                comment_id: 502,
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
    async fn a_re_raised_finding_replies_into_its_thread() -> Result<(), Box<dyn std::error::Error>>
    {
        let gateway = Arc::new(FakeGateway::with_threads(vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                comment_id: 501,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                comment_id: 502,
                path: "src/beta.rs".to_owned(),
                line: Some(13),
                original_line: Some(13),
            },
        ]));
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            gateway.posted_replies().len(),
            1,
            "the re-raised anchor replies into its thread via the replies endpoint"
        );
        let (comment_id, reply_body) = gateway
            .posted_replies()
            .first()
            .cloned()
            .ok_or("expected a reply")?;
        assert_eq!(comment_id, 501);
        assert!(reply_body.contains("Re-raised in the review of commit `headsha`."));
        assert!(
            reply_body.contains("**Title**"),
            "the reply keeps the finding body"
        );
        assert_eq!(
            submission.comments.len(),
            1,
            "only the fresh anchor posts a positioned comment"
        );
        assert_eq!(
            submission.comments.first().map(|c| c.line),
            Some(11),
            "the beta finding at its own anchor stays positioned"
        );
        assert_eq!(gateway.resolved_threads(), vec!["T_FIXED".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn a_shifted_finding_posts_a_new_comment_and_resolves_the_old()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![ReviewThread {
            id: "T_MOVED".to_owned(),
            comment_id: 503,
            path: "src/beta.rs".to_owned(),
            line: None,
            original_line: Some(13),
        }]));
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert!(
            gateway.posted_replies().is_empty(),
            "an anchor that moved does not reply into the outdated thread"
        );
        assert_eq!(
            submission
                .comments
                .iter()
                .filter(|c| c.path == "src/beta.rs")
                .count(),
            1,
            "the shifted finding posts a fresh positioned comment"
        );
        assert_eq!(gateway.resolved_threads(), vec!["T_MOVED".to_owned()]);
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
        let verdict = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the verdict comment must be posted")?;
        assert!(verdict.contains("🔴 Not good to go — 1 blocking finding:"));
        assert!(verdict.contains("## Risks\n\n(none flagged)"));
        assert!(verdict.contains("One finding"));
        assert!(verdict.contains("alpha.rs:999"));
        let overstatement = "two findings";
        assert!(
            !verdict.contains(overstatement),
            "the summary must not count dropped findings"
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_posted_review_body_carries_the_verdict_and_points_at_the_comment()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert!(
            submission.summary.starts_with("## Verdict"),
            "the round's verdict leads the review body — the newest timeline item"
        );
        assert!(
            submission
                .summary
                .contains("🔴 Not good to go — 2 blocking findings:"),
            "the blockers are visible in the review body"
        );
        assert!(
            submission.summary.contains(REVIEW_POINTER_BODY),
            "the body points at the single summary comment"
        );
        assert!(
            !submission.summary.contains("## Summary"),
            "the full summary stays in the comment, not the review body"
        );
        let verdict = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the verdict comment must be posted")?;
        assert!(verdict.contains("## Verdict"));
        assert!(verdict.starts_with(VERDICT_MARKER));
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
        assert_eq!(submission.summary, review_body(&dry));
        let verdict = post_gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the verdict comment must be posted")?;
        assert_eq!(verdict, verdict_comment_body(&dry));
        assert_eq!(posted.dropped, dry.dropped);
        Ok(())
    }

    #[tokio::test]
    async fn the_verdict_comment_is_created_then_edited_not_duplicated()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let first = make_runner_with(
            Arc::new(scripted_client()),
            Arc::clone(&gateway),
            overview_at_sha("firstsha"),
        )?;
        first.review_all(false).await?;
        assert_eq!(
            gateway.posted_comments().len(),
            1,
            "the first run creates exactly one verdict comment"
        );
        let second = make_runner_with(
            Arc::new(scripted_client()),
            Arc::clone(&gateway),
            overview_at_sha("secondsha"),
        )?;
        second.review_all(false).await?;
        assert_eq!(
            gateway.posted_comments().len(),
            1,
            "the second run must not create another comment"
        );
        let updated = gateway.updated_comments();
        assert_eq!(
            updated.len(),
            1,
            "the second run edits the comment in place"
        );
        let (_, body) = updated.first().cloned().ok_or("expected an update")?;
        assert!(body.starts_with(VERDICT_MARKER));
        assert!(
            body.contains("Reviewed commit: `secondsha`"),
            "the edit carries the new round's commit"
        );
        assert_eq!(gateway.issue_comment_bodies().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_verdict_upsert_exhausting_the_retries_fails_the_run_after_posting()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![ReviewThread {
            id: "T_FIXED".to_owned(),
            comment_id: 502,
            path: "src/beta.rs".to_owned(),
            line: Some(13),
            original_line: Some(13),
        }]));
        gateway.fail_comment_writes(9);
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let err = runner
            .review_all(false)
            .await
            .err()
            .ok_or("the exhausted verdict retries must fail the run")?;
        assert!(
            !err.to_string().is_empty(),
            "the failure names the underlying error"
        );
        assert!(
            gateway.submitted().is_some(),
            "the review itself stands posted; only the verdict comment failed"
        );
        assert_eq!(
            gateway.resolved_threads(),
            vec!["T_FIXED".to_owned()],
            "resolution completes even when the verdict write fails"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_reply_degrades_without_failing_the_run_or_resolving()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![ReviewThread {
            id: "T_KEEP".to_owned(),
            comment_id: 501,
            path: "src/alpha.rs".to_owned(),
            line: Some(2),
            original_line: Some(2),
        }]));
        gateway.fail_reply_writes(1);
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert!(outcome.posted, "a failed reply never fails the run");
        assert!(
            gateway.posted_replies().is_empty(),
            "the reply did not land"
        );
        assert!(
            gateway.resolved_threads().is_empty(),
            "the still-present finding's thread must stay open for the next re-review"
        );
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            submission
                .comments
                .iter()
                .filter(|c| c.path == "src/alpha.rs")
                .count(),
            0,
            "the re-raised anchor was carved out for the reply, not reposted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_verdict_upsert_recovering_on_a_later_retry_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        gateway.fail_comment_writes(2);
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert!(outcome.posted);
        assert_eq!(gateway.posted_comments().len(), 1);
        Ok(())
    }
}
