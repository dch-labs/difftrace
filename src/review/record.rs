//! The findings-recording tool: the batch run's single output channel.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

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

/// Where a batch run's recorded findings land.
pub type FindingsSlot = Arc<Mutex<Option<Findings>>>;

#[derive(Deserialize)]
struct RecordInput {
    findings: Vec<Finding>,
}

pub struct RecordFindingsTool {
    slot: FindingsSlot,
}

impl RecordFindingsTool {
    #[must_use]
    pub fn empty_slot() -> FindingsSlot {
        Arc::new(Mutex::new(None))
    }

    #[must_use]
    pub fn new(slot: FindingsSlot) -> Self {
        Self { slot }
    }
}

impl Tool for RecordFindingsTool {
    fn name(&self) -> &'static str {
        "record_findings"
    }

    fn description(&self) -> &'static str {
        "Record this batch's findings. Call exactly once when the review of the listed files is complete; an empty findings list records a clean review."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            tool: self.name().to_owned(),
            description: self.description().to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "findings": findings_array_schema()
                },
                "required": ["findings"],
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
            let parsed: RecordInput = serde_json::from_value(input)
                .map_err(|err| ToolError::InvalidInput(err.to_string()))?;
            let count = parsed.findings.len();
            *self.slot.lock().map_err(|_| {
                ToolError::Execution("findings slot poisoned by a recording panic".into())
            })? = Some(Findings {
                findings: parsed.findings,
            });
            Ok(ToolOutput::text(format!(
                "Recorded {count} findings. The batch review is complete."
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recorded_findings_land_in_the_slot() -> Result<(), Box<dyn std::error::Error>> {
        let slot = RecordFindingsTool::empty_slot();
        let tool = RecordFindingsTool::new(Arc::clone(&slot));
        tool.call(
            json!({
                "findings": [{
                    "file": "src/lib.rs",
                    "line": 3,
                    "severity": "warning",
                    "title": "T",
                    "body": "B"
                }]
            }),
            &ToolContext::default(),
        )
        .await?;
        let snapshot = match slot.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return Err("findings slot poisoned".into()),
        };
        let recorded = snapshot.ok_or("expected recorded findings")?;
        assert_eq!(recorded.findings.len(), 1);
        assert_eq!(
            recorded.findings.first().ok_or("expected a value")?.file,
            "src/lib.rs"
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_input_leaves_the_slot_untouched() -> Result<(), Box<dyn std::error::Error>> {
        let slot = RecordFindingsTool::empty_slot();
        let tool = RecordFindingsTool::new(Arc::clone(&slot));
        let err = tool
            .call(json!({ "findings": "nope" }), &ToolContext::default())
            .await
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, ToolError::InvalidInput(_)));
        assert!(slot.lock().is_ok_and(|guard| guard.is_none()));
        Ok(())
    }
}
