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
use crate::findings::reject_zero_lines;

pub type FindingsSlot = Arc<Mutex<Option<Findings>>>;

#[derive(Deserialize)]
struct RecordInput {
    findings: Vec<Finding>,
}

pub struct RecordFindingsTool {
    slot: FindingsSlot,
    max_per_file: usize,
}

impl RecordFindingsTool {
    #[must_use]
    pub fn empty_slot() -> FindingsSlot {
        Arc::new(Mutex::new(None))
    }

    #[must_use]
    pub fn new(slot: FindingsSlot, max_per_file: usize) -> Self {
        Self { slot, max_per_file }
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
            reject_zero_lines(&parsed.findings).map_err(ToolError::InvalidInput)?;
            let mut per_file = std::collections::BTreeMap::new();
            let mut accepted = Vec::new();
            let mut cap_drops: Vec<(String, usize)> = Vec::new();
            for finding in parsed.findings {
                let count = per_file.entry(finding.file.clone()).or_insert(0usize);
                if *count >= self.max_per_file {
                    if let Some((_, dropped)) =
                        cap_drops.iter_mut().find(|(f, _)| *f == finding.file)
                    {
                        *dropped = (*dropped).saturating_add(1);
                    } else {
                        cap_drops.push((finding.file, 1));
                    }
                    continue;
                }
                *count = (*count).saturating_add(1);
                accepted.push(finding);
            }
            let count = accepted.len();
            *self.slot.lock().map_err(|_| {
                ToolError::Execution("findings slot poisoned by a recording panic".into())
            })? = Some(Findings { findings: accepted });
            let mut receipt = format!("Recorded {count} findings.");
            if !cap_drops.is_empty() {
                let drop_lines = cap_drops
                    .iter()
                    .map(|(file, dropped)| format!("- {file}: {dropped} over the per-file cap"))
                    .collect::<Vec<_>>()
                    .join("\n");
                receipt.push_str("\nDropped findings over the per-file cap:\n");
                receipt.push_str(&drop_lines);
            }
            Ok(ToolOutput::text(receipt))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recorded_findings_land_in_the_slot() -> Result<(), Box<dyn std::error::Error>> {
        let slot = RecordFindingsTool::empty_slot();
        let tool = RecordFindingsTool::new(Arc::clone(&slot), 5);
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
        let tool = RecordFindingsTool::new(Arc::clone(&slot), 5);
        let err = tool
            .call(json!({ "findings": "nope" }), &ToolContext::default())
            .await
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, ToolError::InvalidInput(_)));
        assert!(slot.lock().is_ok_and(|guard| guard.is_none()));
        Ok(())
    }

    #[tokio::test]
    async fn findings_beyond_the_per_file_cap_are_dropped_with_a_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let slot = RecordFindingsTool::empty_slot();
        let tool = RecordFindingsTool::new(Arc::clone(&slot), 1);
        let output = tool
            .call(
                json!({
                    "findings": [
                        { "file": "src/lib.rs", "line": 1, "severity": "warning", "title": "T", "body": "B" },
                        { "file": "src/lib.rs", "line": 2, "severity": "warning", "title": "T", "body": "B" }
                    ]
                }),
                &ToolContext::default(),
            )
            .await?;
        let text = output.text_content();
        assert!(text.contains("Recorded 1 findings."));
        assert!(text.contains("src/lib.rs: 1 over the per-file cap"));
        let snapshot = match slot.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return Err("findings slot poisoned".into()),
        };
        assert_eq!(
            snapshot.ok_or("expected recorded findings")?.findings.len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_zero_line_finding_is_rejected_at_entry() -> Result<(), Box<dyn std::error::Error>> {
        let slot = RecordFindingsTool::empty_slot();
        let tool = RecordFindingsTool::new(Arc::clone(&slot), 5);
        let err = tool
            .call(
                json!({
                    "findings": [
                        { "file": "src/lib.rs", "line": 0, "severity": "warning", "title": "T", "body": "B" }
                    ]
                }),
                &ToolContext::default(),
            )
            .await
            .err()
            .ok_or("expected an error")?;
        assert!(err.to_string().contains("at least 1"));
        assert!(slot.lock().is_ok_and(|guard| guard.is_none()));
        Ok(())
    }
}
