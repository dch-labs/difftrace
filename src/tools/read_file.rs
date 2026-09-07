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
            let hint = module_root_hint(&path);
            let content = self
                .scope
                .gateway
                .file_at_ref(path, self.scope.head_sha.clone())
                .await
                .map_err(|err| {
                    let mut message = err.to_string();
                    if let Some(hint) = hint {
                        message.push_str(&hint);
                    }
                    ToolError::Execution(message)
                })?;
            Ok(ToolOutput::text(content))
        })
    }
}

fn module_root_hint(path: &str) -> Option<String> {
    if !path.ends_with("mod.rs") {
        return None;
    }
    let parent = path
        .strip_suffix("mod.rs")?
        .trim_end_matches('/')
        .to_owned();
    if parent.is_empty() {
        return Some(
            " (no mod.rs here: this repo uses the `foo.rs` + `foo/` layout \
             — do not retry this path)"
                .to_owned(),
        );
    }
    Some(format!(
        " (no mod.rs here: this repo uses the `foo.rs` + `foo/` layout; the \
         module root is {parent}.rs — read that instead of retrying)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffIndex;
    use crate::tools::fake_gateway::FakeGateway;

    #[tokio::test]
    async fn a_file_is_read_at_the_head_sha() -> Result<(), Box<dyn std::error::Error>> {
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
            .await?;
        assert_eq!(output.text_content(), "fn main() {}");
        assert_eq!(
            gateway.requested_reads(),
            vec![("src/lib.rs".to_owned(), "headsha".to_owned())]
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_empty_path_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
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
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, ToolError::InvalidInput(_)));
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_mod_rs_read_names_the_sibling_module_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let scope = ReviewScope::new(
            Arc::new(FakeGateway::empty()),
            Arc::new(DiffIndex::empty()),
            7,
            "h",
        );
        let tool = ReadFileTool::new(Arc::new(scope));
        let err = tool
            .call(
                json!({ "path": "src/memory/mod.rs" }),
                &ToolContext::default(),
            )
            .await
            .err()
            .ok_or("expected an error")?;
        let ToolError::Execution(message) = err else {
            return Err("expected an execution error".into());
        };
        assert!(
            message.contains("src/memory.rs"),
            "the error names the sibling module root: {message}"
        );
        assert!(
            message.contains("instead of retrying"),
            "the error tells the model to stop retrying: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_read_of_a_normal_file_carries_no_layout_hint()
    -> Result<(), Box<dyn std::error::Error>> {
        let scope = ReviewScope::new(
            Arc::new(FakeGateway::empty()),
            Arc::new(DiffIndex::empty()),
            7,
            "h",
        );
        let tool = ReadFileTool::new(Arc::new(scope));
        let err = tool
            .call(json!({ "path": "src/gone.rs" }), &ToolContext::default())
            .await
            .err()
            .ok_or("expected an error")?;
        let ToolError::Execution(message) = err else {
            return Err("expected an execution error".into());
        };
        assert!(
            !message.contains("foo.rs"),
            "only mod.rs misses carry the layout hint: {message}"
        );
        Ok(())
    }
}
