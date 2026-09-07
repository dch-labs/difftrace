//! The review runner: one `BareLoop` per batch, plus the summary pass.

use std::path::PathBuf;
use std::sync::Arc;

use loopctl::engine::BareLoop;
use loopctl::engine::Loop;
use loopctl::engine::RunConfig;
use loopctl::error::LoopError;
use loopctl::managers::LoopManagers;
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
use crate::findings::Finding;
use crate::findings::Findings;
use crate::findings::ReviewSummary;
use crate::findings::Verification;
use crate::findings::VerificationVerdict;
use crate::github::PrGateway;
use crate::github::PrOverview;
use crate::review::RecordFindingsTool;
use crate::review::logging::LoggingObserver;
use crate::review::rubric::ReviewRubric;
use crate::tools::ReviewScope;
use loopctl::tool::ToolRegistry;

pub(crate) const TOOL_OUTPUT_MAX_CHARS: usize = 128 * 1024;

pub(crate) fn production_managers(stream_timeout_secs: u64) -> LoopManagers {
    let handler = loopctl::stream::handler::StreamHandler::new().with_timeout_config(
        loopctl::stream::handler::StreamTimeoutConfig {
            total_stream_timeout: std::time::Duration::from_secs(stream_timeout_secs),
            ..loopctl::stream::handler::StreamTimeoutConfig::default()
        },
    );
    LoopManagers::new().with_stream_handler(handler)
}

pub async fn recheck_pinned_head(
    gateway: &dyn PrGateway,
    pr: u64,
    requested: Option<&str>,
) -> Result<(), DifftraceError> {
    let Some(requested) = requested else {
        return Ok(());
    };
    let latest = gateway.pr_overview(pr).await?;
    crate::cli::check_sha(&latest.head_sha, Some(requested)).map_err(DifftraceError::Cli)
}

pub async fn fetch_pinned_diff(
    gateway: &dyn PrGateway,
    pr: u64,
    requested: Option<&str>,
) -> Result<(PrOverview, String), DifftraceError> {
    let overview = gateway.pr_overview(pr).await?;
    crate::cli::check_sha(&overview.head_sha, requested).map_err(DifftraceError::Cli)?;
    let diff = gateway.pr_diff(pr).await?;
    recheck_pinned_head(gateway, pr, requested).await?;
    Ok((overview, diff))
}

fn check_truncation(
    budget: Option<u32>,
    turns: &[loopctl::engine::Turn],
) -> Result<(), DifftraceError> {
    let Some(budget) = budget else {
        return Ok(());
    };
    let Some(final_turn) = turns.last() else {
        return Ok(());
    };
    tracing::info!(
        target: "difftrace::review",
        output_tokens = final_turn.output_tokens,
        tool_calls = final_turn.tool_calls.len(),
        "batch run ended"
    );
    if final_turn.tool_calls.is_empty() && final_turn.output_tokens >= u64::from(budget) {
        tracing::warn!(
            target: "difftrace::review",
            output_tokens = final_turn.output_tokens,
            budget,
            "the batch's final turn was truncated at the output budget"
        );
        return Err(DifftraceError::ReviewTruncated { budget });
    }
    Ok(())
}

fn recorded_findings(
    slot: &std::sync::Mutex<Option<Findings>>,
) -> Result<Option<Findings>, DifftraceError> {
    let guard = slot.lock().map_err(|_| DifftraceError::ReviewRun {
        source: LoopError::ToolExecution {
            tool: "record_findings".to_owned(),
            message: "findings slot poisoned".to_owned(),
        },
    })?;
    Ok(guard.clone())
}

const SUMMARY_SYSTEM: &str = "\
You are writing the summary of a completed code review. Given every finding
the review recorded, write the summary body, the risk notes (one per line),
and one sentence on test coverage. Be specific and calm; never invent
findings that are not in the list.";

const VERIFY_SYSTEM: &str = "\
You are the verifier of a completed code review. Cross-examine every
finding and drop the ones that do not stand:
- the finding's own text concedes the code is deliberate, acceptable, or
  needs no change;
- the finding asserts facts about files, repos, or conventions it cannot
  verify from the pull request's materials;
- the finding reverses a fix from an earlier round without explicit
  justification in its body;
- the code's own documentation already answers the complaint.
Keep every finding that survives. Return exactly one verdict per finding
index.";

pub struct ReviewRunner<C: loopctl::api::ApiClient> {
    client: Arc<C>,
    scope: ReviewScope,
    overview: PrOverview,
    settings: ReviewSettings,
    trajectory_dir: Option<PathBuf>,
    output_budget: Option<u32>,
}

fn batch_prompt(files: &[String], evidence: &str, hunt: bool) -> String {
    let list = files.join("\n");
    let ask = if hunt {
        "A first pass recorded no blocking findings in these files. Argue against that: hunt for what it missed."
    } else {
        "Review these changed files:"
    };
    format!("{ask}\n{list}\n\n{evidence}")
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

fn plain_summary(findings: &Findings) -> ReviewSummary {
    let count = findings.findings.len();
    ReviewSummary {
        summary: format!(
            "The generated summary is unavailable this round (provider \
             errors after every retry); all {count} recorded finding(s) \
             are intact below."
        ),
        risk_notes: vec![
            "Risk notes unavailable: the summary pass could not reach the \
             provider."
                .to_owned(),
        ],
        tests: "Test-coverage note unavailable: the summary pass could not \
                reach the provider."
            .to_owned(),
    }
}

fn verify_prompt(findings: &[Finding], history: &str) -> Result<String, DifftraceError> {
    let payload = serde_json::to_string(findings).map_err(|err| DifftraceError::Verify {
        source: loopctl::structured::StructuredError::Deserialize(err),
    })?;
    let history_block = if history.is_empty() {
        String::new()
    } else {
        format!("{history}\n\n")
    };
    Ok(format!(
        "The review recorded these findings:\n{payload}\n\n{history_block}Cross-examine every finding and return one verdict per index."
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

    pub(crate) fn client(&self) -> &Arc<C> {
        &self.client
    }

    pub(crate) fn gateway(&self) -> Arc<dyn PrGateway> {
        Arc::clone(&self.scope.gateway)
    }

    pub(crate) fn index_arc(&self) -> Arc<DiffIndex> {
        Arc::clone(&self.scope.index)
    }

    pub(crate) fn overview_ref(&self) -> &PrOverview {
        &self.overview
    }

    pub(crate) fn trajectory_dir_ref(&self) -> Option<&PathBuf> {
        self.trajectory_dir.as_ref()
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

    pub(crate) async fn own_threads(
        &self,
    ) -> Result<Vec<crate::github::ReviewThread>, DifftraceError> {
        self.scope.gateway.own_threads(self.scope.pr).await
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
            output_budget: None,
        }
    }

    #[must_use]
    pub fn with_output_budget(mut self, budget: Option<u32>) -> Self {
        self.output_budget = budget;
        self
    }

    pub async fn review_batch(
        &self,
        files: &[String],
        history: &str,
        evidence: &str,
        hunt: bool,
    ) -> Result<Findings, DifftraceError> {
        let slot = RecordFindingsTool::empty_slot();
        let registry = self
            .scope
            .batch_registry(Arc::clone(&slot), self.settings.max_findings_per_file);
        let mut agent = self.assemble_batch_agent(registry, history, hunt)?;
        let mut run_config = RunConfig::default();
        run_config.max_turns = self.settings.max_turns;
        let prompt = batch_prompt(files, evidence, hunt);
        let run = match agent.run(&prompt, &run_config).await {
            Ok(run) => Some(run),
            Err(LoopError::MaxTurnsExceeded { .. }) => None,
            Err(source) => return Err(DifftraceError::ReviewRun { source }),
        };
        check_truncation(
            self.output_budget,
            run.as_ref().map_or(&[], |run| run.turns.as_slice()),
        )?;
        if let Some(findings) = recorded_findings(&slot)? {
            return Ok(findings);
        }
        tracing::warn!(
            target: "difftrace::review",
            "the batch run ended without a single record_findings call"
        );
        Err(DifftraceError::ReviewNoVerdict)
    }

    fn assemble_batch_agent(
        &self,
        registry: ToolRegistry,
        history: &str,
        hunt: bool,
    ) -> Result<BareLoop<C>, DifftraceError> {
        let mut agent = BareLoop::new_with_managers(
            Arc::clone(&self.client),
            registry,
            loopctl::config::SessionConfig::default(),
            production_managers(self.settings.stream_timeout_secs),
        );
        let rubric = if hunt {
            ReviewRubric::hunt(&self.overview)
        } else {
            ReviewRubric::new(&self.overview)
        };
        agent.add_contributor(Box::new(
            rubric
                .with_history(history.to_owned())
                .with_memory(self.settings.memory_content.clone()),
        ));
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
        Ok(agent)
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
        let mut api_attempt: u64 = 0;
        loop {
            let request = loopctl::api::StreamRequest {
                messages: messages.clone(),
                system: system.clone(),
                tools: None,
            };
            let response = match self
                .client
                .create_message_with_options(&request, options.clone())
                .await
            {
                Ok(response) => response,
                Err(source) => {
                    api_attempt = api_attempt.saturating_add(1);
                    if api_attempt >= 3 {
                        tracing::warn!(
                            target: "difftrace::review",
                            error = %source,
                            "summary generation failed after every retry; posting a plain summary"
                        );
                        return Ok(plain_summary(&merged));
                    }
                    let wait = std::time::Duration::from_secs(2_u64.saturating_mul(api_attempt));
                    tracing::warn!(
                        target: "difftrace::review",
                        error = %source,
                        ?wait,
                        "summary request failed; retrying"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
            };
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
                        "summary schema mismatch; retrying with the parse error fed back"
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

    pub(crate) async fn verify(
        &self,
        findings: &[Finding],
        history: &str,
    ) -> Result<Vec<VerificationVerdict>, DifftraceError> {
        let mut messages = vec![Message::user(verify_prompt(findings, history)?)];
        let system = Some(VERIFY_SYSTEM.to_owned());
        let mut response_format = ResponseFormat::from_type::<Verification>();
        response_format.strict = false;
        let options = RequestOptions::new().with_response_format(response_format);
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
                .map_err(|source| DifftraceError::Verify {
                    source: loopctl::structured::StructuredError::Api(source),
                })?;
            let value = self.client.extract_structured(&response.message);
            match Verification::from_value(value) {
                Ok(verification) => return Ok(verification.verdicts),
                Err(source) if attempts_left == 0 => {
                    tracing::warn!(
                        target: "difftrace::review",
                        error = %source,
                        "verification schema mismatch persisted; keeping every finding"
                    );
                    return Ok(Vec::new());
                }
                Err(source) => {
                    tracing::warn!(
                        target: "difftrace::review",
                        error = %source,
                        "verification schema mismatch; retrying with the parse error fed back"
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
pub(crate) mod test_support {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use futures::Stream;
    use loopctl::api::StreamRequest;
    use loopctl::api::error::ApiError;
    use loopctl::stream::StreamEvent;
    use loopctl::testing::MockApiClient;

    pub(crate) struct FlakyClient {
        inner: MockApiClient,
        fail_first: AtomicUsize,
    }

    impl FlakyClient {
        pub(crate) fn new(inner: MockApiClient, fail_first: usize) -> Self {
            Self {
                inner,
                fail_first: AtomicUsize::new(fail_first),
            }
        }
    }

    impl loopctl::api::ApiClient for FlakyClient {
        fn model(&self) -> String {
            self.inner.model()
        }

        fn stream_messages(
            &self,
            request: &StreamRequest,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ApiError>> + Send + 'static>> {
            let failed = self
                .fail_first
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
                .is_ok_and(|previous| previous > 0);
            if failed {
                return Box::pin(futures::stream::once(async move {
                    Err(ApiError::api("stream reset mid-flight"))
                }));
            }
            self.inner.stream_messages(request)
        }

        fn stream_messages_with_options(
            &self,
            request: &StreamRequest,
            _options: loopctl::structured::RequestOptions,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ApiError>> + Send + 'static>> {
            self.stream_messages(request)
        }

        fn create_message(
            &self,
            request: &StreamRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<loopctl::api::NonStreamingResponse, ApiError>>
                    + Send
                    + '_,
            >,
        > {
            self.inner.create_message(request)
        }

        fn create_message_with_options(
            &self,
            request: &StreamRequest,
            options: loopctl::structured::RequestOptions,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<loopctl::api::NonStreamingResponse, ApiError>>
                    + Send
                    + '_,
            >,
        > {
            let failed = self
                .fail_first
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
                .is_ok_and(|previous| previous > 0);
            if failed {
                return Box::pin(
                    async move { Err(ApiError::api("network error during the request")) },
                );
            }
            self.inner.create_message_with_options(request, options)
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
    use test_support::FlakyClient;

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

    #[tokio::test]
    async fn a_pinned_run_rechecks_the_head_after_the_diff_is_fetched()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_overview_queue(vec![overview()]));
        recheck_pinned_head(gateway.as_ref(), 42, Some("headsha")).await?;
        assert_eq!(gateway.overview_calls(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_head_that_moves_mid_fetch_fails_the_pinned_run()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut moved = overview();
        moved.head_sha = "newerhead".to_owned();
        let gateway = Arc::new(FakeGateway::with_overview_queue(vec![moved]));
        let err = recheck_pinned_head(gateway.as_ref(), 42, Some("headsha"))
            .await
            .err()
            .ok_or("expected the moved head to fail the pin")?;
        let message = err.to_string();
        assert!(
            message.contains("headsha"),
            "the pinned commit is named: {message}"
        );
        assert!(
            message.contains("newerhead"),
            "the actual head is named: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_unpinned_run_does_not_refetch_the_head() -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        recheck_pinned_head(gateway.as_ref(), 42, None).await?;
        assert_eq!(gateway.overview_calls(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn a_pinned_run_fetches_overview_diff_then_rechecks_the_head()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = FakeGateway::with_overview_queue(vec![overview()])
            .and_diff("diff --git a/src/lib.rs b/src/lib.rs");
        let (fetched, diff) = fetch_pinned_diff(&gateway, 42, Some("headsha")).await?;
        assert_eq!(fetched.head_sha, "headsha");
        assert!(!diff.is_empty());
        assert_eq!(
            gateway.call_order(),
            vec!["overview", "diff", "overview"],
            "the pin is checked before the fetch and re-checked after it"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_failing_pin_skips_the_diff_fetch() -> Result<(), Box<dyn std::error::Error>> {
        let gateway =
            FakeGateway::with_overview_queue(vec![overview()]).and_diff("diff --git a/x b/x");
        let err = fetch_pinned_diff(&gateway, 42, Some("otherpin"))
            .await
            .err()
            .ok_or("expected the pin check to fail before the fetch")?;
        assert_eq!(
            gateway.call_order(),
            vec!["overview"],
            "nothing else runs once the pin fails"
        );
        assert!(err.to_string().contains("otherpin"));
        Ok(())
    }

    #[tokio::test]
    async fn a_head_moving_after_the_diff_fails_the_composed_fetch()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut moved = overview();
        moved.head_sha = "newerhead".to_owned();
        let gateway = FakeGateway::with_overview_queue(vec![overview(), moved])
            .and_diff("diff --git a/x b/x");
        let err = fetch_pinned_diff(&gateway, 42, Some("headsha"))
            .await
            .err()
            .ok_or("expected the mid-fetch move to fail the run")?;
        assert_eq!(
            gateway.call_order(),
            vec!["overview", "diff", "overview"],
            "the diff is fetched before the re-check fails"
        );
        assert!(err.to_string().contains("newerhead"));
        Ok(())
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

    fn turn_with(output_tokens: u64, tools: &[&str]) -> loopctl::engine::Turn {
        loopctl::engine::Turn {
            turn: 0,
            input: String::new(),
            output: String::new(),
            tool_calls: tools
                .iter()
                .map(|tool| loopctl::engine::ToolCall {
                    id: format!("call_{tool}"),
                    tool: (*tool).to_owned(),
                    input: serde_json::json!({}),
                })
                .collect(),
            input_tokens: 0,
            output_tokens,
        }
    }

    #[test]
    fn the_truncation_check_fires_on_a_tool_free_final_turn_at_the_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        let turns = vec![turn_with(25, &[])];
        let err = check_truncation(Some(10), &turns)
            .err()
            .ok_or("an over-budget tool-free final turn is truncation")?;
        assert!(
            err.to_string().contains("10-token output budget"),
            "the failure names the budget: {err}"
        );
        Ok(())
    }

    #[test]
    fn the_truncation_check_passes_under_the_budget_or_with_tool_calls() {
        assert!(check_truncation(Some(64), &[turn_with(25, &[])]).is_ok());
        assert!(check_truncation(Some(10), &[turn_with(25, &["record_findings"])]).is_ok());
    }

    #[test]
    fn the_truncation_check_needs_both_a_budget_and_a_final_turn() {
        assert!(check_truncation(None, &[]).is_ok(), "no budget, no check");
        assert!(
            check_truncation(Some(10), &[]).is_ok(),
            "a max-turns run has no final turn to judge"
        );
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
        let findings = runner
            .review_batch(&["src/lib.rs".to_owned()], "", "", false)
            .await?;
        assert_eq!(findings.findings.len(), 1);
        assert_eq!(findings.findings.first().ok_or("expected a value")?.line, 2);
        Ok(())
    }

    #[test]
    fn the_verify_prompt_carries_the_findings_and_the_cross_round_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let findings = vec![crate::findings::Finding {
            file: "src/lib.rs".to_owned(),
            line: 2,
            severity: crate::findings::Severity::Warning,
            complexity: 3,
            title: "Lock dropped early".to_owned(),
            body: "The guard is dropped.".to_owned(),
        }];
        let prompt = verify_prompt(&findings, "Issues already raised on this pull request:")?;
        assert!(prompt.contains("Lock dropped early"));
        assert!(prompt.contains("Issues already raised on this pull request:"));
        assert!(prompt.contains("one verdict per index"));
        let bare = verify_prompt(&findings, "")?;
        assert!(
            !bare.contains("Issues already raised"),
            "a fresh registry renders no history block"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_run_out_of_turns_without_a_verdict_is_not_a_clean_review()
    -> Result<(), Box<dyn std::error::Error>> {
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
        let err = runner
            .review_batch(&["src/lib.rs".to_owned()], "", "", false)
            .await
            .err()
            .ok_or("a run that never recorded findings must fail the batch")?;
        assert!(err.to_string().contains("without ever recording findings"));
        Ok(())
    }

    #[tokio::test]
    async fn a_final_turn_truncated_at_the_output_budget_fails_the_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        // The mock reports 25 output tokens per turn; 10 reads as truncated.
        let client = MockApiClient::new("review-model").with_responses(vec![text_response("…")]);
        let runner =
            runner(Arc::new(client), ReviewSettings::default(), None)?.with_output_budget(Some(10));
        let err = runner
            .review_batch(&["src/lib.rs".to_owned()], "", "", false)
            .await
            .err()
            .ok_or("a final turn at the budget must fail the batch")?;
        assert!(
            err.to_string()
                .contains("truncated at the 10-token output budget"),
            "the failure names the truncation: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_final_turn_under_the_budget_with_a_verdict_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call("call_1", "record_findings", json!({ "findings": [] })),
            text_response("Clean batch."),
        ]);
        let runner =
            runner(Arc::new(client), ReviewSettings::default(), None)?.with_output_budget(Some(64));
        let findings = runner
            .review_batch(&["src/lib.rs".to_owned()], "", "", false)
            .await?;
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
        runner
            .review_batch(&["src/lib.rs".to_owned()], "", "", false)
            .await?;
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
    fn the_production_managers_carry_a_real_retry_ladder() {
        use loopctl::managers::StreamCapable as _;
        let managers = production_managers(900);
        let handler = managers.stream_handler();
        assert_eq!(
            handler.rate_limit_config().max_retries,
            5,
            "rate limits back off and retry instead of failing the run"
        );
        assert_eq!(handler.retry_config().max_retries, 3);
        assert!(handler.rate_limit_config().respect_retry_after);
        assert_eq!(
            handler.timeout_config().total_stream_timeout,
            std::time::Duration::from_mins(15),
            "the configured stream budget reaches the handler"
        );
    }

    #[tokio::test]
    async fn a_transient_stream_failure_is_retried_by_the_production_ladder()
    -> Result<(), Box<dyn std::error::Error>> {
        let inner = MockApiClient::new("review-model").with_responses(vec![
            tool_call("call_1", "record_findings", json!({ "findings": [] })),
            text_response("Batch complete."),
        ]);
        let client = Arc::new(FlakyClient::new(inner, 1));
        let runner = runner(client, ReviewSettings::default(), None)?;
        let findings = runner
            .review_batch(&["src/lib.rs".to_owned()], "", "", false)
            .await?;
        assert!(
            findings.findings.is_empty(),
            "the batch must complete after the ladder absorbs the transient failure"
        );
        Ok(())
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
    async fn a_summary_survives_transient_provider_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        let good = json!({
            "summary": "Adds retry with backoff.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let inner = MockApiClient::new("review-model")
            .with_responses(vec![text_response(&good.to_string())]);
        let client = Arc::new(FlakyClient::new(inner, 1));
        let runner = runner(client, ReviewSettings::default(), None)?;
        let summary = runner.summarize(&[Findings::default()]).await?;
        assert_eq!(summary.summary, "Adds retry with backoff.");
        Ok(())
    }

    #[tokio::test]
    async fn a_summary_that_never_arrives_falls_back_to_a_plain_summary()
    -> Result<(), Box<dyn std::error::Error>> {
        let inner = MockApiClient::new("review-model");
        let client = Arc::new(FlakyClient::new(inner, 9));
        let runner = runner(client, ReviewSettings::default(), None)?;
        let findings = Findings {
            findings: vec![crate::findings::Finding {
                file: "src/lib.rs".to_owned(),
                line: 2,
                severity: crate::findings::Severity::Warning,
                complexity: 3,
                title: "Lock dropped early".to_owned(),
                body: "The guard is dropped.".to_owned(),
            }],
        };
        let summary = runner.summarize(&[findings]).await?;
        assert!(
            summary.summary.contains("unavailable"),
            "the fallback states its limitation: {}",
            summary.summary
        );
        assert!(
            summary.summary.contains('1'),
            "the fallback names the intact finding count: {}",
            summary.summary
        );
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
        runner
            .review_batch(&["src/lib.rs".to_owned()], "", "", false)
            .await?;
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
