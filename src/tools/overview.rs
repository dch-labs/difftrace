//! The pull request summary tool.

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

use crate::github::PrOverview;
use crate::tools::ReviewScope;

pub struct OverviewTool {
    scope: Arc<ReviewScope>,
}

fn render(overview: &PrOverview) -> String {
    let description = overview.description.as_deref().unwrap_or("-");
    format!(
        "#{} {} by {}\n{} -> {} | {} files changed (+{}/-{})\n\n{}",
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

impl OverviewTool {
    #[must_use]
    pub fn new(scope: Arc<ReviewScope>) -> Self {
        Self { scope }
    }
}

impl Tool for OverviewTool {
    fn name(&self) -> &'static str {
        "get_pr_overview"
    }

    fn description(&self) -> &'static str {
        "Fetch the pull request's title, description, author, branches, and change totals."
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
            let overview = self
                .scope
                .gateway
                .pr_overview(self.scope.pr)
                .await
                .map_err(|err| ToolError::Execution(err.to_string()))?;
            Ok(ToolOutput::text(render(&overview)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffIndex;
    use crate::github::PrOverview;
    use crate::tools::fake_gateway::FakeGateway;

    #[tokio::test]
    async fn the_overview_tool_renders_the_pull_request_summary()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = FakeGateway::with_overview(PrOverview {
            number: 42,
            title: "Fix the flaky worker".to_owned(),
            description: Some("Restarts consumers.".to_owned()),
            author: "dana".to_owned(),
            head_sha: "abc123".to_owned(),
            head_branch: "fix/worker".to_owned(),
            base_branch: "main".to_owned(),
            changed_files: 3,
            additions: 120,
            deletions: 15,
        });
        let scope = ReviewScope::new(
            Arc::new(gateway.clone()),
            Arc::new(DiffIndex::empty()),
            42,
            "abc123",
        );
        let tool = OverviewTool::new(Arc::new(scope));
        let output = tool.call(json!({}), &ToolContext::default()).await?;
        assert!(output.text_content().contains("#42 Fix the flaky worker"));
        assert!(output.text_content().contains("dana"));
        assert!(output.text_content().contains("Restarts consumers."));
        assert_eq!(gateway.requested_prs(), vec![42]);
        Ok(())
    }
}
