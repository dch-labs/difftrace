//! The review-submission tool — the run's only write-class operation,
//! guarded to fire at most once per run: findings are grounded through
//! the diff index, ungrounded or over-cap findings are dropped with a
//! per-finding receipt, and a second submission never reaches `GitHub`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use loopctl::structured::StructuredOutput;
use loopctl::tool::Tool;
use loopctl::tool::ToolContext;
use loopctl::tool::ToolError;
use loopctl::tool::ToolOutput;
use loopctl::tool::ToolSchema;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::findings::Finding;
use crate::findings::Findings;
use crate::findings::findings_array_schema;
use crate::github::CommentPosition;
use crate::github::ReviewSubmission;
use crate::github::Side;
use crate::tools::ReviewScope;

pub struct SubmitReviewTool {
    scope: Arc<ReviewScope>,
    max_per_file: usize,
    submitted: AtomicBool,
}

#[derive(Deserialize)]
struct SubmitInput {
    summary: String,
    findings: Vec<Finding>,
}

struct Dropped {
    file: String,
    line: usize,
    reason: &'static str,
}

fn render_receipt(posted: usize, dropped: &[Dropped]) -> String {
    let mut text = format!("Review posted: summary plus {posted} inline findings.");
    if dropped.is_empty() {
        return text;
    }
    let drop_lines = dropped
        .iter()
        .map(|entry| format!("- {}:{} — {}", entry.file, entry.line, entry.reason))
        .collect::<Vec<_>>()
        .join("\n");
    text.push_str("\nDropped findings (never posted):\n");
    text.push_str(&drop_lines);
    text
}

impl SubmitReviewTool {
    #[must_use]
    pub fn new(scope: Arc<ReviewScope>, max_per_file: usize) -> Self {
        Self {
            scope,
            max_per_file,
            submitted: AtomicBool::new(false),
        }
    }

    fn ground(&self, findings: Vec<Finding>) -> (Vec<CommentPosition>, Vec<Dropped>) {
        let mut accepted = Vec::new();
        let mut dropped = Vec::new();
        let mut per_file_counts = std::collections::BTreeMap::new();
        for finding in findings {
            let grounded = self.scope.index.clamp_to_hunk(&finding.file, finding.line);
            let Some(line) = grounded else {
                dropped.push(Dropped {
                    file: finding.file,
                    line: finding.line,
                    reason: "line outside the changed hunks",
                });
                continue;
            };
            let count = per_file_counts
                .entry(finding.file.clone())
                .or_insert(0usize);
            if *count >= self.max_per_file {
                dropped.push(Dropped {
                    file: finding.file,
                    line: finding.line,
                    reason: "per-file finding cap reached",
                });
                continue;
            }
            *count = (*count).saturating_add(1);
            accepted.push(CommentPosition {
                path: finding.file,
                line: line as u64,
                side: Side::Right,
                body: format!(
                    "**[{}] {}**\n\n{}",
                    finding.severity.as_str(),
                    finding.title,
                    finding.body
                ),
            });
        }
        (accepted, dropped)
    }
}

fn submit_input_schema() -> Value {
    let mut findings = Findings::schema()
        .get("properties")
        .and_then(|properties| properties.get("findings"))
        .cloned()
        .unwrap_or_else(findings_array_schema);
    if let Some(object) = findings.as_object_mut() {
        object.insert(
            "description".to_owned(),
            json!("Inline findings; cite lines inside the changed hunks."),
        );
    }
    json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "The review's summary body in Markdown."
            },
            "findings": findings
        },
        "required": ["summary", "findings"],
        "additionalProperties": false
    })
}

impl Tool for SubmitReviewTool {
    fn name(&self) -> &'static str {
        "submit_review"
    }

    fn description(&self) -> &'static str {
        "Submit the finished review: a summary plus inline findings. Each finding must cite a line inside the changed hunks of its file; ungrounded findings are dropped with a receipt."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            tool: self.name().to_owned(),
            description: self.description().to_owned(),
            input_schema: submit_input_schema(),
        }
    }

    fn call(
        &self,
        input: Value,
        _ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let parsed: SubmitInput = serde_json::from_value(input)
                .map_err(|err| ToolError::InvalidInput(err.to_string()))?;
            if self.submitted.swap(true, Ordering::SeqCst) {
                return Ok(ToolOutput::error_text(
                    "The review was already submitted; this run posts exactly one review.",
                ));
            }
            let (comments, dropped) = self.ground(parsed.findings);
            let posted = comments.len();
            let submission = ReviewSubmission {
                head_sha: self.scope.head_sha.clone(),
                summary: parsed.summary,
                comments,
            };
            if let Err(err) = self
                .scope
                .gateway
                .submit_review(self.scope.pr, submission)
                .await
            {
                self.submitted.store(false, Ordering::SeqCst);
                return Err(ToolError::Execution(err.to_string()));
            }
            Ok(ToolOutput::text(render_receipt(posted, &dropped)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffIndex;
    use crate::tools::fake_gateway::FakeGateway;

    fn diff_index() -> Result<DiffIndex, Box<dyn std::error::Error>> {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 ctx
-old
+new
 tail
";
        DiffIndex::parse(diff).map_err(Into::into)
    }

    fn tool(
        max_per_file: usize,
    ) -> Result<(SubmitReviewTool, FakeGateway), Box<dyn std::error::Error>> {
        let gateway = FakeGateway::empty();
        let scope = ReviewScope::new(
            Arc::new(gateway.clone()),
            Arc::new(diff_index()?),
            42,
            "headsha",
        );
        Ok((
            SubmitReviewTool::new(Arc::new(scope), max_per_file),
            gateway,
        ))
    }

    fn input(summary: &str, findings: &[Value]) -> Value {
        json!({ "summary": summary, "findings": findings })
    }

    fn finding_json(file: &str, line: usize) -> Value {
        json!({
            "file": file,
            "line": line,
            "severity": "warning",
            "title": "Title",
            "body": "Body"
        })
    }

    #[tokio::test]
    async fn a_grounded_finding_is_posted_at_its_anchor() -> Result<(), Box<dyn std::error::Error>>
    {
        let (tool, gateway) = tool(5)?;
        tool.call(
            input("Summary.", &[finding_json("src/lib.rs", 2)]),
            &ToolContext::default(),
        )
        .await?;
        let submission = gateway.submitted().ok_or("expected a value")?;
        assert_eq!(submission.head_sha, "headsha");
        assert_eq!(submission.summary, "Summary.");
        assert_eq!(submission.comments.len(), 1);
        let comment = submission.comments.first().ok_or("expected a value")?;
        assert_eq!(comment.path, "src/lib.rs");
        assert_eq!(comment.line, 2);
        assert_eq!(comment.side, Side::Right);
        assert!(comment.body.contains("[warning] Title"));
        Ok(())
    }

    #[tokio::test]
    async fn an_out_of_hunk_finding_is_dropped_and_receipted()
    -> Result<(), Box<dyn std::error::Error>> {
        let (tool, gateway) = tool(5)?;
        let output = tool
            .call(
                input(
                    "Summary.",
                    &[
                        finding_json("src/lib.rs", 2),
                        finding_json("src/lib.rs", 99),
                    ],
                ),
                &ToolContext::default(),
            )
            .await?;
        let text = output.text_content();
        assert!(text.contains("plus 1 inline findings"));
        assert!(text.contains("src/lib.rs:99"));
        assert!(text.contains("outside the changed hunks"));
        assert_eq!(
            gateway
                .submitted()
                .ok_or("expected a value")?
                .comments
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_finding_in_an_absent_file_is_dropped() -> Result<(), Box<dyn std::error::Error>> {
        let (tool, gateway) = tool(5)?;
        tool.call(
            input("Summary.", &[finding_json("nope.rs", 1)]),
            &ToolContext::default(),
        )
        .await?;
        assert_eq!(
            gateway
                .submitted()
                .ok_or("expected a value")?
                .comments
                .len(),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn findings_beyond_the_per_file_cap_are_dropped() -> Result<(), Box<dyn std::error::Error>>
    {
        let (tool, gateway) = tool(1)?;
        let output = tool
            .call(
                input(
                    "Summary.",
                    &[finding_json("src/lib.rs", 1), finding_json("src/lib.rs", 3)],
                ),
                &ToolContext::default(),
            )
            .await?;
        assert!(
            output
                .text_content()
                .contains("per-file finding cap reached")
        );
        assert_eq!(
            gateway
                .submitted()
                .ok_or("expected a value")?
                .comments
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_second_submission_never_reaches_the_gateway()
    -> Result<(), Box<dyn std::error::Error>> {
        let (tool, gateway) = tool(5)?;
        let input = input("Summary.", &[finding_json("src/lib.rs", 2)]);
        let first = tool.call(input.clone(), &ToolContext::default()).await?;
        assert!(!first.is_error);
        let second = tool.call(input, &ToolContext::default()).await?;
        assert!(second.is_error);
        assert!(second.text_content().contains("exactly one review"));
        assert_eq!(gateway.submit_calls(), 1);
        assert!(gateway.submitted().is_some());
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_submission_may_be_retried() -> Result<(), Box<dyn std::error::Error>> {
        let (tool, gateway) = tool(5)?;
        gateway.fail_next_submit();
        let err = tool
            .call(
                input("First try.", &[finding_json("src/lib.rs", 2)]),
                &ToolContext::default(),
            )
            .await
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, ToolError::Execution(_)));
        let second = tool
            .call(
                input("Retry.", &[finding_json("src/lib.rs", 2)]),
                &ToolContext::default(),
            )
            .await?;
        assert!(!second.is_error);
        let submission = gateway.submitted().ok_or("expected a value")?;
        assert_eq!(submission.summary, "Retry.");
        Ok(())
    }

    #[tokio::test]
    async fn invalid_input_does_not_consume_the_single_submission()
    -> Result<(), Box<dyn std::error::Error>> {
        let (tool, gateway) = tool(5)?;
        let err = tool
            .call(json!({ "summary": "s" }), &ToolContext::default())
            .await
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, ToolError::InvalidInput(_)));
        tool.call(
            input("Summary.", &[finding_json("src/lib.rs", 2)]),
            &ToolContext::default(),
        )
        .await?;
        assert!(gateway.submitted().is_some());
        Ok(())
    }

    #[test]
    fn the_tool_schema_and_the_findings_schema_share_one_source()
    -> Result<(), Box<dyn std::error::Error>> {
        let tool_schema = submit_input_schema();
        let tool_items = tool_schema
            .pointer("/properties/findings/items")
            .cloned()
            .ok_or("tool schema must carry the findings items")?;
        let model_items = Findings::schema()
            .pointer("/properties/findings/items")
            .cloned()
            .ok_or("model schema must carry the findings items")?;
        assert_eq!(tool_items, model_items);
        let severity = tool_schema
            .pointer("/properties/findings/items/properties/severity/enum")
            .cloned()
            .ok_or("severity enum must exist")?;
        assert_eq!(
            severity,
            json!(["nitpick", "suggestion", "warning", "critical"])
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_input_never_reaches_the_gateway() -> Result<(), Box<dyn std::error::Error>> {
        let (tool, gateway) = tool(5)?;
        let err = tool
            .call(json!({ "summary": "s" }), &ToolContext::default())
            .await
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, ToolError::InvalidInput(_)));
        assert!(gateway.submitted().is_none());
        Ok(())
    }
}
