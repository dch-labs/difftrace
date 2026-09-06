//! The prior-review-comments tool.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use loopctl::tool::Tool;
use loopctl::tool::ToolContext;
use loopctl::tool::ToolError;
use loopctl::tool::ToolOutput;
use loopctl::tool::ToolSchema;
use serde_json::Value;
use serde_json::json;

use crate::tools::ReviewScope;

pub struct ListCommentsTool {
    scope: Arc<ReviewScope>,
}

const COMMENT_EXCERPT_CHARS: usize = 600;

fn excerpt(body: &str) -> String {
    if body.chars().count() <= COMMENT_EXCERPT_CHARS {
        return body.to_owned();
    }
    let cut: String = body.chars().take(COMMENT_EXCERPT_CHARS).collect();
    format!("{cut}\n… (excerpt truncated)")
}

fn render(comments: &[crate::github::ExistingComment]) -> String {
    let visible: Vec<&crate::github::ExistingComment> = comments
        .iter()
        .filter(|comment| !comment.author.ends_with("[bot]"))
        .collect();
    if visible.is_empty() {
        return "No human review comments yet.".to_owned();
    }
    visible
        .iter()
        .map(|comment| {
            let line = comment
                .line
                .map_or_else(|| "-".to_owned(), |l| l.to_string());
            let side = comment
                .side
                .map_or_else(|| "-".to_owned(), |s| s.as_str().to_owned());
            format!(
                "#{} {} {}:{} {}\n{}",
                comment.id,
                comment.author,
                comment.path,
                line,
                side,
                excerpt(&comment.body)
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}

impl ListCommentsTool {
    #[must_use]
    pub fn new(scope: Arc<ReviewScope>) -> Self {
        Self { scope }
    }
}

impl Tool for ListCommentsTool {
    fn name(&self) -> &'static str {
        "list_review_comments"
    }

    fn description(&self) -> &'static str {
        "List the review comments already posted on the pull request, so earlier feedback is not repeated."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            tool: self.name().to_owned(),
            description: self.description().to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    fn call(
        &self,
        _input: Value,
        _ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let comments = self
                .scope
                .gateway
                .existing_review_comments(self.scope.pr)
                .await
                .map_err(|err| ToolError::Execution(err.to_string()))?;
            Ok(ToolOutput::text(render(&comments)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffIndex;
    use crate::github::ExistingComment;
    use crate::tools::fake_gateway::FakeGateway;

    #[tokio::test]
    async fn prior_comments_are_listed_with_their_anchors() -> Result<(), Box<dyn std::error::Error>>
    {
        let gateway = FakeGateway::with_comments(vec![ExistingComment {
            id: 9,
            path: "src/lib.rs".to_owned(),
            line: Some(12),
            side: Some(crate::github::Side::Left),
            body: "Too early.".to_owned(),
            author: "dana".to_owned(),
            in_reply_to: None,
        }]);
        let scope = ReviewScope::new(
            Arc::new(gateway.clone()),
            Arc::new(DiffIndex::empty()),
            42,
            "h",
        );
        let tool = ListCommentsTool::new(Arc::new(scope));
        let output = tool.call(json!({}), &ToolContext::default()).await?;
        let text = output.text_content();
        assert!(text.contains("#9 dana src/lib.rs:12 LEFT"));
        assert!(text.contains("Too early."));
        assert_eq!(gateway.requested_comment_lists(), vec![42]);
        Ok(())
    }

    #[tokio::test]
    async fn an_empty_comment_list_says_so() -> Result<(), Box<dyn std::error::Error>> {
        let scope = ReviewScope::new(
            Arc::new(FakeGateway::empty()),
            Arc::new(DiffIndex::empty()),
            42,
            "h",
        );
        let tool = ListCommentsTool::new(Arc::new(scope));
        let output = tool.call(json!({}), &ToolContext::default()).await?;
        assert_eq!(output.text_content(), "No human review comments yet.");
        Ok(())
    }

    #[tokio::test]
    async fn bot_comments_are_dropped_and_human_bodies_stay_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let long = "x".repeat(2_000);
        let gateway = FakeGateway::with_comments(vec![
            ExistingComment {
                id: 1,
                path: "src/lib.rs".to_owned(),
                line: Some(1),
                side: None,
                body: format!("Length of output: 30976 {long}"),
                author: "coderabbitai[bot]".to_owned(),
                in_reply_to: None,
            },
            ExistingComment {
                id: 2,
                path: "src/lib.rs".to_owned(),
                line: Some(2),
                side: None,
                body: long.clone(),
                author: "dana".to_owned(),
                in_reply_to: None,
            },
        ]);
        let scope = ReviewScope::new(Arc::new(gateway), Arc::new(DiffIndex::empty()), 42, "h");
        let tool = ListCommentsTool::new(Arc::new(scope));
        let output = tool.call(json!({}), &ToolContext::default()).await?;
        let text = output.text_content();
        assert!(
            !text.contains("coderabbitai[bot]") && !text.contains("Length of output"),
            "a competing bot's comment never rides the tool result"
        );
        assert!(text.contains("#2 dana"));
        assert!(
            text.chars().count() < 1_200,
            "a human body is a bounded excerpt, not the whole comment"
        );
        assert!(text.contains("(excerpt truncated)"));
        Ok(())
    }
}
