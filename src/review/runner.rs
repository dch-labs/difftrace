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
use loopctl::structured::RequestOptions;
use loopctl::structured::ResponseFormat;
use loopctl::structured::StructuredOutput;

use crate::config::ReviewSettings;
use crate::diff::DiffIndex;
use crate::error::DifftraceError;
use crate::findings::Findings;
use crate::findings::ReviewSummary;
use crate::github::PrGateway;
use crate::github::PrOverview;
use crate::review::RecordFindingsTool;
use crate::review::logging::LoggingObserver;
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

fn summary_response_format() -> ResponseFormat {
    let mut response_format = ResponseFormat::from_type::<ReviewSummary>();
    response_format.strict = false;
    response_format
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
    pub(crate) fn file_names(&self) -> Vec<String> {
        self.scope
            .index
            .file_names()
            .into_iter()
            .map(String::from)
            .collect()
    }

    pub(crate) fn index(&self) -> &crate::diff::DiffIndex {
        &self.scope.index
    }

    pub(crate) fn head_sha(&self) -> &str {
        &self.scope.head_sha
    }

    pub(crate) fn pr(&self) -> u64 {
        self.scope.pr
    }

    pub(crate) fn settings(&self) -> &ReviewSettings {
        &self.settings
    }

    pub(crate) async fn submit(
        &self,
        submission: crate::github::ReviewSubmission,
    ) -> Result<(), DifftraceError> {
        self.scope
            .gateway
            .submit_review(self.scope.pr, submission)
            .await
    }

    pub(crate) async fn own_open_threads(
        &self,
    ) -> Result<Vec<crate::github::ReviewThread>, DifftraceError> {
        self.scope.gateway.own_open_threads(self.scope.pr).await
    }

    pub(crate) async fn resolve_thread(&self, thread_id: String) -> Result<(), DifftraceError> {
        self.scope.gateway.resolve_thread(thread_id).await
    }

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
        agent.register_observer(Arc::new(LoggingObserver));
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
        let mut messages = vec![Message::user(summary_prompt(&merged)?)];
        let system = Some(SUMMARY_SYSTEM.to_owned());
        let options = RequestOptions::new().with_response_format(summary_response_format());
        let mut attempts_left: u8 = 2;
        loop {
            let request = loopctl::api::StreamRequest {
                messages: messages.clone(),
                system: system.clone(),
                tools: None,
            };
            let response = self
                .client
                .create_message_with_options(&request, options.clone())
                .await
                .map_err(|source| DifftraceError::Summary {
                    source: loopctl::structured::StructuredError::Api(source),
                })?;
            let value = self.client.extract_structured(&response.message);
            match ReviewSummary::from_value(value) {
                Ok(summary) => return Ok(summary),
                Err(source) if attempts_left == 0 => {
                    return Err(DifftraceError::Summary { source });
                }
                Err(source) => {
                    tracing::warn!(
                        target: "difftrace::review",
                        error = %source,
                        "summary schema mismatch; retrying once with the parse error fed back"
                    );
                    attempts_left = attempts_left.saturating_sub(1);
                    messages.push(Message::user(format!(
                        "That JSON did not match the schema: {source}. \
Return the corrected JSON now, matching every field type exactly."
                    )));
                }
            }
        }
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
                        "complexity": 3,
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

    #[test]
    fn the_summary_request_is_explicitly_non_strict() {
        let response_format = summary_response_format();
        assert!(
            !response_format.strict,
            "strict = true is refused by Anthropic-protocol endpoints at request time"
        );
        assert_eq!(response_format.name, ReviewSummary::name());
        assert_eq!(response_format.schema, ReviewSummary::schema());
    }

    #[test]
    fn the_summary_options_carry_the_response_format() -> Result<(), Box<dyn std::error::Error>> {
        let options = RequestOptions::new().with_response_format(summary_response_format());
        let response_format = options
            .response_format
            .as_ref()
            .ok_or("expected a response format on the options")?;
        assert!(!response_format.strict);
        assert_eq!(response_format.name, "difftrace_review_summary");
        Ok(())
    }

    #[tokio::test]
    async fn a_malformed_summary_gets_one_corrective_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let good = json!({
            "summary": "Adds retry with backoff to the worker loop.",
            "risk_notes": ["Retry can now outlive the shutdown signal."],
            "tests": "Covered by the new integration test."
        });
        let bad = json!({
            "summary": { "text": "nested where a string belongs" },
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            text_response(&bad.to_string()),
            text_response(&good.to_string()),
        ]);
        let runner = runner(Arc::new(client.clone()), ReviewSettings::default(), None)?;
        let summary = runner.summarize(&[Findings::default()]).await?;
        assert_eq!(
            summary.summary,
            "Adds retry with backoff to the worker loop."
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_summary_retry_is_visible_in_the_logs() -> Result<(), Box<dyn std::error::Error>> {
        let (logs, _guard) = crate::review::logging::test_support::install();
        let good = json!({
            "summary": "Adds retry with backoff.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let bad = json!({
            "summary": { "text": "nested where a string belongs" },
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            text_response(&bad.to_string()),
            text_response(&good.to_string()),
        ]);
        let runner = runner(Arc::new(client), ReviewSettings::default(), None)?;
        runner.summarize(&[Findings::default()]).await?;
        assert!(
            logs.text().contains("summary schema mismatch"),
            "the retry warning must reach an installed subscriber"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_summary_still_malformed_after_the_retries_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let bad = json!({
            "summary": { "text": "nested where a string belongs" },
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            text_response(&bad.to_string()),
            text_response(&bad.to_string()),
            text_response(&bad.to_string()),
        ]);
        let runner = runner(Arc::new(client.clone()), ReviewSettings::default(), None)?;
        let err = runner
            .summarize(&[Findings::default()])
            .await
            .err()
            .ok_or("expected the retry exhaustion to fail")?;
        assert!(
            err.to_string().contains("expected a string"),
            "names the schema breach: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_summary_recovering_on_the_final_attempt_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let good = json!({
            "summary": "Adds retry with backoff.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let bad = json!({
            "summary": { "text": "nested where a string belongs" },
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            text_response(&bad.to_string()),
            text_response(&bad.to_string()),
            text_response(&good.to_string()),
        ]);
        let runner = runner(Arc::new(client), ReviewSettings::default(), None)?;
        let summary = runner.summarize(&[Findings::default()]).await?;
        assert_eq!(summary.summary, "Adds retry with backoff.");
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
