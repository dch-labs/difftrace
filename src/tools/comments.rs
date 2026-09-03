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

fn render(comments: &[crate::github::ExistingComment]) -> String {
    if comments.is_empty() {
        return "No review comments yet.".to_owned();
    }
    comments
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
                comment.id, comment.author, comment.path, line, side, comment.body
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
        assert_eq!(output.text_content(), "No review comments yet.");
        Ok(())
    }
}
