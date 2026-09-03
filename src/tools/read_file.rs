//! The file-content-at-head tool.

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

pub struct ReadFileTool {
    scope: Arc<ReviewScope>,
}

impl ReadFileTool {
    #[must_use]
    pub fn new(scope: Arc<ReviewScope>) -> Self {
        Self { scope }
    }

    fn input_path(input: &Value) -> Result<String, ToolError> {
        input
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ToolError::InvalidInput("path must be a non-empty string".into()))
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file_at_head"
    }

    fn description(&self) -> &'static str {
        "Read one file's full text content at the pull request's head commit, for context the diff does not show."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            tool: self.name().to_owned(),
            description: self.description().to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Repository-relative path of the file to read."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    fn call(
        &self,
        input: Value,
        _ctx: &ToolContext,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let path = Self::input_path(&input)?;
            let content = self
                .scope
                .gateway
                .file_at_ref(path, self.scope.head_sha.clone())
                .await
                .map_err(|err| ToolError::Execution(err.to_string()))?;
            Ok(ToolOutput::text(content))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffIndex;
    use crate::tools::fake_gateway::FakeGateway;

    #[tokio::test]
    async fn a_file_is_read_at_the_head_sha() {
        let gateway = FakeGateway::with_file("src/lib.rs", "fn main() {}");
        let scope = ReviewScope::new(
            Arc::new(gateway.clone()),
            Arc::new(DiffIndex::empty()),
            7,
            "headsha",
        );
        let tool = ReadFileTool::new(Arc::new(scope));
        let output = tool
            .call(json!({ "path": "src/lib.rs" }), &ToolContext::default())
            .await
            .unwrap();
        assert_eq!(output.text_content(), "fn main() {}");
        assert_eq!(
            gateway.requested_reads(),
            vec![("src/lib.rs".to_owned(), "headsha".to_owned())]
        );
    }

    #[tokio::test]
    async fn an_empty_path_is_rejected() {
        let scope = ReviewScope::new(
            Arc::new(FakeGateway::empty()),
            Arc::new(DiffIndex::empty()),
            7,
            "h",
        );
        let tool = ReadFileTool::new(Arc::new(scope));
        let err = tool
            .call(json!({ "path": "" }), &ToolContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}
