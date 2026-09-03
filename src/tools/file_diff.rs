//! The per-file diff tool.

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

pub struct FileDiffTool {
    scope: Arc<ReviewScope>,
}

impl FileDiffTool {
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

impl Tool for FileDiffTool {
    fn name(&self) -> &'static str {
        "get_file_diff"
    }

    fn description(&self) -> &'static str {
        "Fetch one changed file's unified-diff section, line for line as the pull request carries it."
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
                        "description": "Repository-relative path of the changed file."
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
            self.scope
                .index
                .file_section(&path)
                .map(ToolOutput::text)
                .ok_or_else(|| {
                    ToolError::InvalidInput(format!("{path} is not a file this diff changes"))
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffIndex;
    use crate::tools::fake_gateway::FakeGateway;

    fn scope() -> Arc<ReviewScope> {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-old
+new
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -2 +2 @@
-a
+b
";
        Arc::new(ReviewScope::new(
            Arc::new(FakeGateway::empty()),
            Arc::new(DiffIndex::parse(diff).unwrap()),
            7,
            "abc",
        ))
    }

    #[tokio::test]
    async fn a_files_section_is_served_verbatim() {
        let tool = FileDiffTool::new(scope());
        let output = tool
            .call(json!({ "path": "README.md" }), &ToolContext::default())
            .await
            .unwrap();
        let text = output.text_content();
        assert!(text.contains("diff --git a/README.md b/README.md"));
        assert!(text.contains("+b"));
        assert!(!text.contains("src/lib.rs"));
    }

    #[tokio::test]
    async fn an_absent_path_is_rejected() {
        let tool = FileDiffTool::new(scope());
        let err = tool
            .call(json!({ "path": "nope.rs" }), &ToolContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn a_missing_path_argument_is_rejected() {
        let tool = FileDiffTool::new(scope());
        let err = tool
            .call(json!({}), &ToolContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}
