//! The review runner: one `BareLoop` per batch, plus the summary pass.

use std::path::PathBuf;
use std::sync::Arc;

use loopctl::engine::BareLoop;
use loopctl::engine::Loop;
use loopctl::engine::RunConfig;
use loopctl::error::LoopError;
use loopctl::memory::trajectory::TrajectoryObserver;
use loopctl::message::Message;
use loopctl::middleware::OutputLimitMiddleware;
use loopctl::middleware::ToolPipelineBuilder;
use loopctl::structured::request_structured;

use crate::config::ReviewSettings;
use crate::diff::DiffIndex;
use crate::error::DifftraceError;
use crate::findings::Findings;
use crate::findings::ReviewSummary;
use crate::github::PrGateway;
use crate::github::PrOverview;
use crate::review::RecordFindingsTool;
use crate::review::rubric::ReviewRubric;
use crate::tools::ReviewScope;

const TOOL_OUTPUT_MAX_CHARS: usize = 16 * 1024;

const SUMMARY_SYSTEM: &str = "\
You are writing the summary of a completed code review. Given every finding
the review recorded, write the summary body, the risk notes (one per line),
and one sentence on test coverage. Be specific and calm; never invent
findings that are not in the list.";

pub struct ReviewRunner<C: loopctl::api::ApiClient> {
    client: Arc<C>,
    scope: ReviewScope,
    overview: PrOverview,
    settings: ReviewSettings,
    trajectory_dir: Option<PathBuf>,
}

fn batch_prompt(files: &[String]) -> String {
    let list = files.join("\n");
    format!("Review these changed files:\n{list}")
}

fn summary_prompt(findings: &Findings) -> Result<String, DifftraceError> {
    let payload = serde_json::to_string(&findings).map_err(|err| DifftraceError::Summary {
        source: loopctl::structured::StructuredError::Deserialize(err),
    })?;
    Ok(format!(
        "The review recorded these findings:\n{payload}\n\nWrite the review summary."
    ))
}

impl<C: loopctl::api::ApiClient + 'static> ReviewRunner<C> {
    #[must_use]
    pub fn new(
        client: Arc<C>,
        gateway: Arc<dyn PrGateway>,
        index: Arc<DiffIndex>,
        overview: PrOverview,
        settings: ReviewSettings,
        trajectory_dir: Option<PathBuf>,
    ) -> Self {
        let scope = ReviewScope::new(gateway, index, overview.number, overview.head_sha.clone());
        Self {
            client,
            scope,
            overview,
            settings,
            trajectory_dir,
        }
    }

    pub async fn review_batch(&self, files: &[String]) -> Result<Findings, DifftraceError> {
        let slot = RecordFindingsTool::empty_slot();
        let registry = self
            .scope
            .batch_registry(Arc::clone(&slot), self.settings.max_findings_per_file);
        let mut agent = BareLoop::new(
            Arc::clone(&self.client),
            registry,
            loopctl::config::SessionConfig::default(),
        );
        agent.add_contributor(Box::new(ReviewRubric::new(&self.overview)));
        agent.register_observer(Arc::new(match &self.trajectory_dir {
            Some(dir) => TrajectoryObserver::writing_to(dir),
            None => TrajectoryObserver::in_memory(),
        }));
        let pipeline = ToolPipelineBuilder::new()
            .with_middleware(OutputLimitMiddleware::new(TOOL_OUTPUT_MAX_CHARS));
        agent
            .set_pipeline(pipeline)
            .map_err(|source| DifftraceError::ReviewRun { source })?;

        let mut run_config = RunConfig::default();
        run_config.max_turns = self.settings.max_turns;
        match agent.run(&batch_prompt(files), &run_config).await {
            Ok(_) | Err(LoopError::MaxTurnsExceeded { .. }) => {}
            Err(source) => return Err(DifftraceError::ReviewRun { source }),
        }
        let recorded = slot
            .lock()
            .map_err(|_| DifftraceError::ReviewRun {
                source: LoopError::ToolExecution {
                    tool: "record_findings".to_owned(),
                    message: "findings slot poisoned".to_owned(),
                },
            })?
            .clone();
        Ok(recorded.unwrap_or_default())
    }

    pub async fn summarize(&self, batches: &[Findings]) -> Result<ReviewSummary, DifftraceError> {
        let mut merged = Findings::default();
        for batch in batches {
            merged.findings.extend(batch.findings.iter().cloned());
        }
        let prompt = summary_prompt(&merged)?;
        let system = Some(SUMMARY_SYSTEM.to_owned());
        request_structured::<ReviewSummary>(
            self.client.as_ref(),
            vec![Message::user(prompt)],
            system,
        )
        .await
        .map_err(|source| DifftraceError::Summary { source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopctl::testing::MockApiClient;
    use loopctl::testing::MockResponse;
    use loopctl::testing::MockToolCall;
    use serde_json::json;
    use std::sync::Arc;

    use crate::github::PrOverview;
    use crate::tools::fake_gateway::FakeGateway;

    fn overview() -> PrOverview {
        PrOverview {
            number: 42,
            title: "Fix the worker".to_owned(),
            description: Some("Restarts consumers.".to_owned()),
            author: "dana".to_owned(),
            head_sha: "headsha".to_owned(),
            head_branch: "fix/worker".to_owned(),
            base_branch: "main".to_owned(),
            changed_files: 1,
            additions: 3,
            deletions: 1,
        }
    }

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

    fn runner<C: loopctl::api::ApiClient + 'static>(
        client: Arc<C>,
        settings: ReviewSettings,
        trajectory_dir: Option<PathBuf>,
    ) -> Result<ReviewRunner<C>, Box<dyn std::error::Error>> {
        Ok(ReviewRunner::new(
            client,
            Arc::new(FakeGateway::empty()),
            Arc::new(diff_index()?),
            overview(),
            settings,
            trajectory_dir,
        ))
    }

    #[tokio::test]
    async fn a_batch_run_records_its_findings() -> Result<(), Box<dyn std::error::Error>> {
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({
                    "findings": [{
                        "file": "src/lib.rs",
                        "line": 2,
                        "severity": "warning",
                        "title": "Lock dropped early",
                        "body": "The guard is dropped before the read completes."
                    }]
                }),
            ),
            text_response("Batch review complete."),
        ]);
        let runner = runner(Arc::new(client), ReviewSettings::default(), None)?;
        let findings = runner.review_batch(&["src/lib.rs".to_owned()]).await?;
        assert_eq!(findings.findings.len(), 1);
        assert_eq!(findings.findings.first().ok_or("expected a value")?.line, 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_budget_exhausted_batch_stops_cleanly() -> Result<(), Box<dyn std::error::Error>> {
        let client = MockApiClient::new("review-model").with_responses(vec![tool_call(
            "call_1",
            "get_file_diff",
            json!({ "path": "src/lib.rs" }),
        )]);
        let settings = ReviewSettings {
            max_turns: 1,
            ..ReviewSettings::default()
        };
        let runner = runner(Arc::new(client), settings, None)?;
        let findings = runner.review_batch(&["src/lib.rs".to_owned()]).await?;
        assert!(findings.findings.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn the_trajectory_is_captured_to_the_configured_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("difftrace-traj-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call("call_1", "record_findings", json!({ "findings": [] })),
            text_response("Clean batch."),
        ]);
        let runner = runner(
            Arc::new(client),
            ReviewSettings::default(),
            Some(dir.clone()),
        )?;
        runner.review_batch(&["src/lib.rs".to_owned()]).await?;
        let entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(Result::ok).collect();
        let jsonl: Vec<_> = entries
            .iter()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        assert!(!jsonl.is_empty(), "expected a JSONL trajectory in {dir:?}");
        for entry in entries {
            let _unused = std::fs::remove_file(entry.path());
        }
        let _unused = std::fs::remove_dir(&dir);
        Ok(())
    }

    #[tokio::test]
    async fn the_summary_is_generated_from_recorded_findings()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary_json = json!({
            "summary": "Adds retry with backoff to the worker loop.",
            "risk_notes": ["Retry can now outlive the shutdown signal."],
            "tests": "Covered by the new integration test."
        });
        let client = MockApiClient::new("review-model")
            .with_responses(vec![text_response(&summary_json.to_string())]);
        let runner = runner(Arc::new(client), ReviewSettings::default(), None)?;
        let batches = vec![Findings::default()];
        let summary = runner.summarize(&batches).await?;
        assert_eq!(
            summary.summary,
            "Adds retry with backoff to the worker loop."
        );
        assert_eq!(summary.risk_notes.len(), 1);
        assert_eq!(summary.tests, "Covered by the new integration test.");
        Ok(())
    }

    #[tokio::test]
    async fn an_oversized_tool_output_is_truncated_for_the_model()
    -> Result<(), Box<dyn std::error::Error>> {
        let big = "a".repeat(TOOL_OUTPUT_MAX_CHARS.saturating_add(4_096));
        let gateway = FakeGateway::with_file("src/lib.rs", &big);
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "read_file_at_head",
                json!({ "path": "src/lib.rs" }),
            ),
            tool_call("call_2", "record_findings", json!({ "findings": [] })),
            text_response("Clean batch."),
        ]);
        let dir = std::env::temp_dir().join(format!("difftrace-traj-limit-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let runner = ReviewRunner::new(
            Arc::new(client),
            Arc::new(gateway),
            Arc::new(diff_index()?),
            overview(),
            ReviewSettings::default(),
            Some(dir.clone()),
        );
        runner.review_batch(&["src/lib.rs".to_owned()]).await?;
        let mut trajectory = String::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|ext| ext == "jsonl") {
                trajectory.push_str(&std::fs::read_to_string(entry.path())?);
            }
        }
        for entry in std::fs::read_dir(&dir)? {
            let _unused = std::fs::remove_file(entry?.path());
        }
        let _unused = std::fs::remove_dir(&dir);
        assert!(
            trajectory.contains("[truncated]"),
            "the oversized tool output must carry the truncation marker in the trajectory"
        );
        let untruncated = format!(r#""text": "aaaa{}"#, "a".repeat(TOOL_OUTPUT_MAX_CHARS));
        assert!(
            !trajectory.contains(&untruncated),
            "the trajectory must not carry an untruncated {TOOL_OUTPUT_MAX_CHARS}-char payload"
        );
        Ok(())
    }
}
