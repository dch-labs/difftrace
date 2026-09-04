//! The chat reply: answer an authorized question about the review and
//! post the answer back where it was asked — inline for review threads,
//! as a mentioning comment for conversation questions.

use std::path::Path;
use std::sync::Arc;

use loopctl::engine::BareLoop;
use loopctl::engine::Loop;
use loopctl::engine::RunConfig;
use loopctl::error::LoopError;
use loopctl::memory::trajectory::TrajectoryObserver;
use loopctl::message::Message;
use loopctl::middleware::OutputLimitMiddleware;
use loopctl::middleware::ToolPipelineBuilder;

use crate::config::ReviewSettings;
use crate::diff::DiffIndex;
use crate::error::DifftraceError;
use crate::github::PrGateway;
use crate::github::PrOverview;
use crate::review::logging::LoggingObserver;
use crate::review::rubric::render_frame;
use crate::review::runner::ReviewRunner;
use crate::review::runner::TOOL_OUTPUT_MAX_CHARS;

const REPLY_SYSTEM: &str = "\
You are difftrace, the code-review bot. A collaborator asked you a \
question about your review of this pull request. Answer concisely in \
markdown without headings; cite file paths and lines when helpful. Use \
the tools to read the diff or the files when the answer needs more \
context. Never invent findings — discuss the review's existing \
findings and the code shown to you.";

const REFUSAL: &str = "Only collaborators or the pull request's author can run difftrace commands.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyTarget {
    IssueComment { id: u64 },
    ReviewComment { id: u64 },
}

impl ReplyTarget {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::IssueComment { .. } => "pull request conversation",
            Self::ReviewComment { .. } => "review thread",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyOutcome {
    pub refused: bool,
    pub target: &'static str,
}

struct ReplyContext {
    author: String,
    question: String,
    finding: Option<String>,
    path: Option<String>,
}

pub struct ReplyRubric {
    frame: String,
}

impl ReplyRubric {
    #[must_use]
    pub fn new(overview: &PrOverview) -> Self {
        Self {
            frame: render_frame(overview),
        }
    }
}

impl loopctl::contributor::ContextContributor for ReplyRubric {
    fn contribute(&self, _ctx: &loopctl::contributor::ContributorContext<'_>) -> Option<Message> {
        Some(Message::user(format!("{REPLY_SYSTEM}\n\n{}", self.frame)))
    }
}

pub(crate) struct ReplyInputs<'a, C> {
    pub(crate) client: &'a Arc<C>,
    pub(crate) gateway: Arc<dyn PrGateway>,
    pub(crate) index: Arc<DiffIndex>,
    pub(crate) overview: &'a PrOverview,
    pub(crate) settings: &'a ReviewSettings,
    pub(crate) trajectory_dir: Option<&'a Path>,
    pub(crate) pr: u64,
}

pub(crate) async fn run_reply<C: loopctl::api::ApiClient + 'static>(
    inputs: ReplyInputs<'_, C>,
    target: ReplyTarget,
) -> Result<ReplyOutcome, DifftraceError> {
    let context = reply_context(&inputs.gateway, &target).await?;
    if !authorized(&inputs.gateway, inputs.overview, &context.author).await? {
        post_answer(
            &inputs.gateway,
            inputs.pr,
            &target,
            &context.author,
            REFUSAL.to_owned(),
        )
        .await?;
        tracing::warn!(
            target: "difftrace::review",
            author = %context.author,
            "reply refused: not a collaborator or the pull request author"
        );
        return Ok(ReplyOutcome {
            refused: true,
            target: target.label(),
        });
    }
    let answer = answer(&inputs, &context).await?;
    post_answer(&inputs.gateway, inputs.pr, &target, &context.author, answer).await?;
    Ok(ReplyOutcome {
        refused: false,
        target: target.label(),
    })
}

async fn reply_context(
    gateway: &Arc<dyn PrGateway>,
    target: &ReplyTarget,
) -> Result<ReplyContext, DifftraceError> {
    match target {
        ReplyTarget::IssueComment { id } => {
            let comment = gateway.fetch_issue_comment(*id).await?;
            Ok(ReplyContext {
                author: comment.author,
                question: comment.body,
                finding: None,
                path: None,
            })
        }
        ReplyTarget::ReviewComment { id } => {
            let comment = gateway.fetch_review_comment(*id).await?;
            let path = comment.path.clone();
            match comment.in_reply_to {
                Some(root_id) => {
                    let root = gateway.fetch_review_comment(root_id).await?;
                    Ok(ReplyContext {
                        author: comment.author,
                        question: comment.body,
                        finding: Some(root.body),
                        path: Some(path),
                    })
                }
                None => Ok(ReplyContext {
                    author: comment.author,
                    question: comment.body,
                    finding: None,
                    path: Some(path),
                }),
            }
        }
    }
}

async fn authorized(
    gateway: &Arc<dyn PrGateway>,
    overview: &PrOverview,
    author: &str,
) -> Result<bool, DifftraceError> {
    if author == overview.author {
        return Ok(true);
    }
    let permission = gateway.commenter_permission(author.to_owned()).await?;
    Ok(matches!(permission.as_str(), "admin" | "write"))
}

async fn answer<C: loopctl::api::ApiClient + 'static>(
    inputs: &ReplyInputs<'_, C>,
    context: &ReplyContext,
) -> Result<String, DifftraceError> {
    let scope = Arc::new(crate::tools::ReviewScope::new(
        Arc::clone(&inputs.gateway),
        Arc::clone(&inputs.index),
        inputs.pr,
        inputs.overview.head_sha.clone(),
    ));
    let registry = scope.chat_registry();
    let mut agent = BareLoop::new(
        Arc::clone(inputs.client),
        registry,
        loopctl::config::SessionConfig::default(),
    );
    agent.add_contributor(Box::new(ReplyRubric::new(inputs.overview)));
    agent.register_observer(Arc::new(match inputs.trajectory_dir {
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
    run_config.max_turns = inputs.settings.reply_max_turns;
    let prompt = reply_prompt(context);
    match agent.run(&prompt, &run_config).await {
        Ok(run) => run.output.ok_or_else(|| DifftraceError::Reply {
            message: "the reply loop produced no answer".to_owned(),
        }),
        Err(LoopError::MaxTurnsExceeded { .. }) => Err(DifftraceError::Reply {
            message: "the reply loop exceeded its turn budget without finishing".to_owned(),
        }),
        Err(source) => Err(DifftraceError::ReviewRun { source }),
    }
}

fn reply_prompt(context: &ReplyContext) -> String {
    let mut parts = Vec::new();
    if let Some(body) = context.finding.as_deref() {
        parts.push(format!("The finding being discussed:\n\n{body}"));
    }
    if let Some(path) = context.path.as_deref() {
        parts.push(format!("It concerns the file `{path}`."));
    }
    parts.push(format!("The question:\n\n{}", context.question));
    parts.join("\n\n") + "\n\nAnswer it."
}

async fn post_answer(
    gateway: &Arc<dyn PrGateway>,
    pr: u64,
    target: &ReplyTarget,
    asker: &str,
    answer: String,
) -> Result<(), DifftraceError> {
    match target {
        ReplyTarget::ReviewComment { id } => gateway.reply_to_review_comment(pr, *id, answer).await,
        ReplyTarget::IssueComment { .. } => {
            gateway
                .post_pr_comment(pr, format!("@{asker} {answer}"))
                .await
        }
    }
}

impl<C: loopctl::api::ApiClient + 'static> ReviewRunner<C> {
    pub async fn reply(&self, target: ReplyTarget) -> Result<ReplyOutcome, DifftraceError> {
        let inputs = ReplyInputs {
            client: self.client(),
            gateway: self.gateway(),
            index: self.index_arc(),
            overview: self.overview_ref(),
            settings: self.settings(),
            trajectory_dir: self.trajectory_dir_ref().map(std::path::PathBuf::as_path),
            pr: self.pr(),
        };
        run_reply(inputs, target).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::ExistingComment;
    use crate::github::ExistingIssueComment;
    use crate::github::Side;
    use crate::tools::fake_gateway::FakeGateway;
    use loopctl::testing::MockApiClient;
    use loopctl::testing::MockResponse;
    use loopctl::testing::MockToolCall;
    use serde_json::json;

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

    fn review_comment(id: u64, in_reply_to: Option<u64>, author: &str) -> ExistingComment {
        ExistingComment {
            id,
            path: "src/lib.rs".to_owned(),
            line: Some(2),
            side: Some(Side::Right),
            body: "@difftrace why is this a warning?".to_owned(),
            author: author.to_owned(),
            in_reply_to,
        }
    }

    fn issue_comment(id: u64, author: &str) -> ExistingIssueComment {
        ExistingIssueComment {
            id,
            body: "@difftrace explain the verdict".to_owned(),
            author: author.to_owned(),
        }
    }

    fn text_response(text: &str) -> MockResponse {
        MockResponse {
            text: text.to_owned(),
            tool_call: None,
            stop_reason: "end_turn".to_owned(),
        }
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

    async fn run(
        gateway: Arc<FakeGateway>,
        client: MockApiClient,
    ) -> Result<ReplyOutcome, DifftraceError> {
        let client = Arc::new(client);
        let settings = ReviewSettings::default();
        let overview = overview();
        let inputs = ReplyInputs {
            client: &client,
            gateway: gateway_as_trait(gateway),
            index: Arc::new(DiffIndex::empty()),
            overview: &overview,
            settings: &settings,
            trajectory_dir: None,
            pr: 42,
        };
        run_reply(inputs, ReplyTarget::ReviewComment { id: 7 }).await
    }

    fn gateway_as_trait(gateway: Arc<FakeGateway>) -> Arc<dyn PrGateway> {
        gateway as Arc<dyn PrGateway>
    }

    #[tokio::test]
    async fn an_authorized_thread_question_replies_into_the_same_thread()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(
            FakeGateway::empty()
                .with_review_comment(review_comment(7, Some(3), "bobrykov"))
                .with_review_comment(ExistingComment {
                    id: 3,
                    path: "src/lib.rs".to_owned(),
                    line: Some(2),
                    side: Some(Side::Right),
                    body: "![warning](…) **Lock dropped early**".to_owned(),
                    author: "difftrace[bot]".to_owned(),
                    in_reply_to: None,
                })
                .with_permission("bobrykov", "write"),
        );
        let client = MockApiClient::new("review-model")
            .with_responses(vec![text_response("The guard is dropped before the read.")]);
        let outcome = run(std::sync::Arc::clone(&gateway), client).await?;
        assert!(!outcome.refused);
        assert_eq!(outcome.target, "review thread");
        let replies = gateway.posted_replies();
        assert_eq!(
            replies.first().ok_or("expected a reply")?.0,
            7,
            "the answer must reply to the question comment"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_conversation_question_replies_top_level_mentioning_the_asker()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(
            FakeGateway::empty()
                .with_issue_comment(issue_comment(9, "bobrykov"))
                .with_permission("bobrykov", "admin"),
        );
        let client = Arc::new(
            MockApiClient::new("review-model")
                .with_responses(vec![text_response("The verdict is red because…")]),
        );
        let settings = ReviewSettings::default();
        let overview = overview();
        let fake = std::sync::Arc::clone(&gateway);
        let inputs = ReplyInputs {
            client: &client,
            gateway: gateway_as_trait(gateway),
            index: Arc::new(DiffIndex::empty()),
            overview: &overview,
            settings: &settings,
            trajectory_dir: None,
            pr: 42,
        };
        let outcome = run_reply(inputs, ReplyTarget::IssueComment { id: 9 }).await?;
        assert!(!outcome.refused);
        assert_eq!(outcome.target, "pull request conversation");
        let posted = fake.posted_comments();
        let body = posted.first().ok_or("expected a comment")?.1.clone();
        assert!(body.starts_with("@bobrykov "), "mentions the asker: {body}");
        assert!(body.contains("The verdict is red because"));
        Ok(())
    }

    #[tokio::test]
    async fn an_unauthorized_commenter_gets_the_refusal() -> Result<(), Box<dyn std::error::Error>>
    {
        let gateway = Arc::new(
            FakeGateway::empty()
                .with_review_comment(review_comment(7, None, "rando"))
                .with_permission("rando", "read"),
        );
        let client = MockApiClient::new("review-model").with_responses(vec![]);
        let outcome = run(std::sync::Arc::clone(&gateway), client).await?;
        assert!(outcome.refused);
        let replies = gateway.posted_replies();
        assert!(
            replies
                .first()
                .ok_or("expected a refusal")?
                .1
                .contains("Only collaborators")
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_pr_author_is_always_authorized() -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(
            FakeGateway::empty()
                .with_review_comment(review_comment(7, None, "dana"))
                .with_permission("dana", "read"),
        );
        let client = MockApiClient::new("review-model")
            .with_responses(vec![text_response("Because the guard is dropped.")]);
        let outcome = run(std::sync::Arc::clone(&gateway), client).await?;
        assert!(!outcome.refused);
        assert_eq!(gateway.posted_replies().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn exhausting_the_reply_budget_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(
            FakeGateway::empty()
                .with_review_comment(review_comment(7, None, "bobrykov"))
                .with_permission("bobrykov", "write"),
        );
        let client = Arc::new(
            MockApiClient::new("review-model").with_responses(vec![tool_call(
                "call_1",
                "get_file_diff",
                json!({ "path": "src/lib.rs" }),
            )]),
        );
        let settings = ReviewSettings {
            reply_max_turns: 1,
            ..ReviewSettings::default()
        };
        let overview = overview();
        let fake = std::sync::Arc::clone(&gateway);
        let inputs = ReplyInputs {
            client: &client,
            gateway: gateway_as_trait(gateway),
            index: Arc::new(DiffIndex::empty()),
            overview: &overview,
            settings: &settings,
            trajectory_dir: None,
            pr: 42,
        };
        let err = run_reply(inputs, ReplyTarget::ReviewComment { id: 7 })
            .await
            .err()
            .ok_or("expected the budget exhaustion to fail")?;
        assert!(err.to_string().contains("turn budget"));
        assert!(fake.posted_replies().is_empty());
        Ok(())
    }
}
