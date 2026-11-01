//! The hosted gateway: hermetic reads over the checked-out repository
//! beside the GitHub REST and GraphQL surface — check runs, review
//! threads, review posting, the standing marker comment, and actor
//! permissions.

use std::path::Path;

use octocrab::Octocrab;

use crate::contracts::{
    CheckRun, DiffSource, FileSnapshot, PinnedRange, ReviewSubmission, SearchHit, Severity, Thread,
    ToolFinding,
};
use crate::error::DtError;
use crate::gateway::PrGateway;
use crate::gateway::local::LocalGateway;

const THREADS_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        nodes {
          id
          isResolved
          comments(first: 1) {
            nodes {
              databaseId
              author { login }
              path
              line
              originalLine
            }
          }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
"#;

const VIEWER_QUERY: &str = "query { viewer { login } }";

const RESOLVE_MUTATION: &str = r#"
mutation($threadId: ID!) {
  resolveReviewThread(input: { threadId: $threadId }) {
    thread { isResolved }
  }
}
"#;

pub struct GitHubGateway {
    reads: LocalGateway,
    crab: Octocrab,
    runtime: tokio::runtime::Runtime,
    owner: String,
    repo: String,
    pr: u64,
}

impl GitHubGateway {
    pub fn open_at(
        start: &Path,
        token: &str,
        owner: &str,
        repo: &str,
        pr: u64,
        base_sha: &str,
    ) -> Result<Self, DtError> {
        let reads = LocalGateway::open_at(start, base_sha)?;
        let crab = Octocrab::builder()
            .personal_token(token.to_owned())
            .build()
            .map_err(|err| DtError::Gateway(format!("the GitHub client cannot start: {err}")))?;
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|err| DtError::Gateway(format!("the hosted runtime cannot start: {err}")))?;
        Ok(Self {
            reads,
            crab,
            runtime,
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            pr,
        })
    }

    #[must_use]
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.reads.root()
    }

    pub fn open_pr(
        start: &Path,
        token: &str,
        owner: &str,
        repo: &str,
        pr: u64,
    ) -> Result<Self, DtError> {
        let probe = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|err| DtError::Gateway(format!("the probe runtime cannot start: {err}")))?;
        let crab = Octocrab::builder()
            .personal_token(token.to_owned())
            .build()
            .map_err(|err| DtError::Gateway(format!("the GitHub client cannot start: {err}")))?;
        let pull = probe.block_on(async {
            crab.pulls(owner, repo)
                .get(pr)
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))
        })?;
        let gateway = Self::open_at(start, token, owner, repo, pr, &pull.base.sha)?;
        let pinned = gateway.pinned_range();
        if pinned.head_sha != pull.head.sha {
            return Err(DtError::Gateway(format!(
                "the checkout is at {} but the pull request head is {} — check out the pull request before reviewing it",
                pinned.head_sha, pull.head.sha
            )));
        }
        Ok(gateway)
    }

    fn run<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, DtError>>,
    ) -> Result<T, DtError> {
        self.runtime.block_on(future)
    }

    pub fn pr_base_head(&self) -> Result<(String, String), DtError> {
        self.run(async {
            let pr = self
                .crab
                .pulls(self.owner.as_str(), self.repo.as_str())
                .get(self.pr)
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            Ok((pr.base.sha, pr.head.sha))
        })
    }

    pub fn own_login(&self) -> Result<String, DtError> {
        self.run(self.viewer_login())
    }

    async fn viewer_login(&self) -> Result<String, DtError> {
        let payload = serde_json::json!({ "query": VIEWER_QUERY });
        let wire: ViewerWire = self
            .crab
            .graphql(&payload)
            .await
            .map_err(|err| DtError::Gateway(chain_text(&err)))?;
        wire.viewer
            .map(|viewer| viewer.login)
            .ok_or_else(|| DtError::Gateway("the viewer query returned no login".to_owned()))
    }

    pub fn own_threads(&self) -> Result<Vec<Thread>, DtError> {
        self.run(async {
            let login = self.viewer_login().await?;
            let mut threads = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let payload = serde_json::json!({
                    "query": THREADS_QUERY,
                    "variables": {
                        "owner": self.owner.as_str(),
                        "name": self.repo.as_str(),
                        "number": self.pr,
                        "cursor": cursor,
                    },
                });
                let wire: ThreadsWire = self
                    .crab
                    .graphql(&payload)
                    .await
                    .map_err(|err| DtError::Gateway(chain_text(&err)))?;
                let (page, next) = own_threads_page(wire, &login);
                threads.extend(page);
                match next {
                    Some(next_cursor) => cursor = Some(next_cursor),
                    None => return Ok(threads),
                }
            }
        })
    }

    pub fn submit_review(&self, submission: &ReviewSubmission) -> Result<(), DtError> {
        self.run(async {
            post_review(&self.crab, &self.owner, &self.repo, self.pr, submission).await
        })
    }

    pub fn reply_to_thread(&self, comment_id: u64, body: &str) -> Result<(), DtError> {
        self.run(async {
            self.crab
                .pulls(self.owner.as_str(), self.repo.as_str())
                .reply_to_comment(self.pr, comment_id.into(), body.to_owned())
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            Ok(())
        })
    }

    pub fn resolve_thread(&self, thread_id: &str) -> Result<(), DtError> {
        self.run(async {
            let payload = serde_json::json!({
                "query": RESOLVE_MUTATION,
                "variables": { "threadId": thread_id },
            });
            let _: serde_json::Value = self
                .crab
                .graphql(&payload)
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            Ok(())
        })
    }

    pub fn marker_comment(&self) -> Result<Option<(u64, String)>, DtError> {
        self.run(async {
            let login = self.viewer_login().await?;
            let page = self
                .crab
                .issues(self.owner.as_str(), self.repo.as_str())
                .list_comments(self.pr)
                .send()
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            let comments = self
                .crab
                .all_pages(page)
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            Ok(own_marker_comment(
                &comments
                    .into_iter()
                    .map(|comment| IssueCommentWire {
                        id: comment.id.0,
                        author: comment.user.login,
                        body: comment.body.unwrap_or_default(),
                    })
                    .collect::<Vec<_>>(),
                &login,
            ))
        })
    }

    pub fn update_issue_comment(&self, comment_id: u64, body: &str) -> Result<(), DtError> {
        self.run(async {
            let route = format!(
                "/repos/{}/{}/issues/comments/{comment_id}",
                self.owner, self.repo
            );
            let payload = serde_json::json!({ "body": body });
            let _: octocrab::models::issues::Comment = self
                .crab
                .patch(&route, Some(&payload))
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            Ok(())
        })
    }

    pub fn post_issue_comment(&self, body: &str) -> Result<(), DtError> {
        self.run(async {
            self.crab
                .issues(self.owner.as_str(), self.repo.as_str())
                .create_comment(self.pr, body.to_owned())
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            Ok(())
        })
    }

    pub fn permission(&self, user: &str) -> Result<String, DtError> {
        self.run(async {
            let route = format!(
                "/repos/{}/{}/collaborators/{user}/permission",
                self.owner, self.repo
            );
            let permission: PermissionWire = self
                .crab
                .get(route, None::<&()>)
                .await
                .map_err(|err| DtError::Gateway(chain_text(&err)))?;
            Ok(permission.permission)
        })
    }

    pub fn fetch_check_runs(&self, sha: &str) -> Result<Vec<CheckRun>, DtError> {
        self.run(async {
            let mut wire_runs = Vec::new();
            let mut page: u32 = 1;
            loop {
                let wire: CheckRunsWire = self
                    .crab
                    .get(
                        check_runs_route(&self.owner, &self.repo, sha, page),
                        None::<&()>,
                    )
                    .await
                    .map_err(|err| DtError::Gateway(chain_text(&err)))?;
                let received = wire.check_runs.len();
                wire_runs.extend(wire.check_runs);
                if received < CHECK_RUNS_PER_PAGE {
                    break;
                }
                page = page.saturating_add(1);
            }
            let mut runs = Vec::new();
            for run in wire_runs {
                let findings = if run.finished_unsuccessfully() {
                    let mut annotations: Vec<AnnotationWire> = Vec::new();
                    let mut page: u32 = 1;
                    loop {
                        let batch: Vec<AnnotationWire> = self
                            .crab
                            .get(
                                annotations_route(&self.owner, &self.repo, run.id, page),
                                None::<&()>,
                            )
                            .await
                            .map_err(|err| DtError::Gateway(chain_text(&err)))?;
                        let received = batch.len();
                        annotations.extend(batch);
                        if received < ANNOTATIONS_PER_PAGE {
                            break;
                        }
                        page = page.saturating_add(1);
                    }
                    annotations
                        .iter()
                        .filter_map(|annotation| annotation_to_finding(&run.name, annotation))
                        .collect()
                } else {
                    Vec::new()
                };
                runs.push(CheckRun {
                    name: run.name,
                    conclusion: run.conclusion.unwrap_or_default(),
                    summary: run
                        .output
                        .and_then(|output| output.summary)
                        .unwrap_or_default(),
                    findings,
                });
            }
            Ok(runs)
        })
    }
}

impl PrGateway for GitHubGateway {
    fn pinned_range(&self) -> PinnedRange {
        self.reads.pinned_range()
    }
    fn diff(&self) -> DiffSource {
        self.reads.diff()
    }
    fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, DtError> {
        self.reads.read_file_at_head(path)
    }
    fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
        self.reads.list_files_at_head()
    }
    fn search_at_head(&self, pattern: &str, cap: usize) -> Result<Vec<SearchHit>, DtError> {
        self.reads.search_at_head(pattern, cap)
    }
    fn untracked_files(&self) -> Result<Vec<String>, DtError> {
        self.reads.untracked_files()
    }
}

async fn post_review(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    pr: u64,
    submission: &ReviewSubmission,
) -> Result<(), DtError> {
    let route = format!("/repos/{owner}/{repo}/pulls/{pr}/reviews");
    let wire = WireReview::from_submission(submission);
    crab.post::<_, octocrab::models::pulls::Review>(&route, Some(&wire))
        .await
        .map_err(|err| DtError::Gateway(chain_text(&err)))?;
    Ok(())
}

#[derive(serde::Serialize)]
struct WireReview<'a> {
    commit_id: &'a str,
    body: &'a str,
    event: &'static str,
    comments: Vec<WireComment<'a>>,
}

impl<'a> WireReview<'a> {
    fn from_submission(submission: &'a ReviewSubmission) -> Self {
        Self {
            commit_id: submission.head_sha.as_str(),
            body: submission.body.as_str(),
            event: submission.event.as_str(),
            comments: submission
                .comments
                .iter()
                .map(|comment| WireComment {
                    path: comment.path.as_str(),
                    line: u64::from(comment.line),
                    side: comment.side.as_str(),
                    body: comment.body.as_str(),
                })
                .collect(),
        }
    }
}

#[derive(serde::Serialize)]
struct WireComment<'a> {
    path: &'a str,
    line: u64,
    side: &'static str,
    body: &'a str,
}

#[derive(serde::Deserialize)]
struct PermissionWire {
    #[serde(default)]
    permission: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsWire {
    #[serde(default)]
    repository: Option<RepositoryWire>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryWire {
    #[serde(default)]
    pull_request: Option<PullRequestWire>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestWire {
    #[serde(default)]
    review_threads: Option<ReviewThreadsWire>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThreadsWire {
    #[serde(default)]
    nodes: Vec<ThreadWire>,
    #[serde(default)]
    page_info: Option<PageInfoWire>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfoWire {
    #[serde(default)]
    has_next_page: bool,
    #[serde(default)]
    end_cursor: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadWire {
    id: String,
    is_resolved: bool,
    #[serde(default)]
    comments: Option<CommentsWire>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentsWire {
    #[serde(default)]
    nodes: Vec<ThreadCommentWire>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadCommentWire {
    database_id: u64,
    #[serde(default)]
    author: Option<AuthorWire>,
    path: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    original_line: Option<u32>,
}

#[derive(serde::Deserialize)]
struct AuthorWire {
    login: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewerWire {
    #[serde(default)]
    viewer: Option<AuthorWire>,
}

struct IssueCommentWire {
    id: u64,
    author: String,
    body: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CheckRunsWire {
    #[serde(default)]
    check_runs: Vec<CheckRunWire>,
}

#[derive(serde::Deserialize)]
struct CheckRunWire {
    id: u64,
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    output: Option<CheckOutputWire>,
}

impl CheckRunWire {
    fn finished_unsuccessfully(&self) -> bool {
        !matches!(
            self.conclusion.as_deref(),
            None | Some("success" | "skipped")
        )
    }
}

#[derive(serde::Deserialize)]
struct CheckOutputWire {
    #[serde(default)]
    summary: Option<String>,
}

#[derive(serde::Deserialize)]
struct AnnotationWire {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    annotation_level: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

const CHECK_RUNS_PER_PAGE: usize = 100;
const ANNOTATIONS_PER_PAGE: usize = 50;

fn check_runs_route(owner: &str, repo: &str, sha: &str, page: u32) -> String {
    format!(
        "/repos/{owner}/{repo}/commits/{sha}/check-runs?per_page={CHECK_RUNS_PER_PAGE}&page={page}"
    )
}

fn annotations_route(owner: &str, repo: &str, id: u64, page: u32) -> String {
    format!(
        "/repos/{owner}/{repo}/check-runs/{id}/annotations?per_page={ANNOTATIONS_PER_PAGE}&page={page}"
    )
}

fn annotation_to_finding(tool: &str, annotation: &AnnotationWire) -> Option<ToolFinding> {
    let path = annotation.path.as_deref()?;
    Some(ToolFinding {
        tool: tool.to_owned(),
        file: path.to_owned(),
        line: annotation.start_line.unwrap_or(0),
        severity: match annotation.annotation_level.as_deref() {
            Some("failure") => Severity::Critical,
            Some("notice") => Severity::Suggestion,
            _ => Severity::Warning,
        },
        message: annotation.message.clone().unwrap_or_default(),
    })
}

fn bot_normalized(login: &str) -> &str {
    login.strip_suffix("[bot]").unwrap_or(login)
}

fn own_marker_comment(comments: &[IssueCommentWire], own_login: &str) -> Option<(u64, String)> {
    let own_login = bot_normalized(own_login);
    comments
        .iter()
        .filter(|comment| {
            bot_normalized(&comment.author) == own_login
                && comment.body.contains(crate::state::marker::marker_start())
        })
        .map(|comment| (comment.id, comment.body.clone()))
        .next_back()
}

fn own_threads_page(wire: ThreadsWire, own_login: &str) -> (Vec<Thread>, Option<String>) {
    let own_login = bot_normalized(own_login);
    let Some(threads) = wire
        .repository
        .and_then(|repository| repository.pull_request)
        .and_then(|pull_request| pull_request.review_threads)
    else {
        return (Vec::new(), None);
    };
    let next = match threads.page_info {
        Some(ref info) if info.has_next_page => info.end_cursor.clone(),
        _ => None,
    };
    let mapped = threads
        .nodes
        .into_iter()
        .filter_map(|thread| {
            if thread.is_resolved {
                return None;
            }
            let comment = thread.comments?.nodes.into_iter().next()?;
            let login = comment.author.map(|author| author.login)?;
            if bot_normalized(&login) != own_login {
                return None;
            }
            Some(Thread {
                id: thread.id,
                path: comment.path,
                line: comment.line.unwrap_or(0),
                original_line: comment.original_line,
                comment_id: comment.database_id,
            })
        })
        .collect();
    (mapped, next)
}

fn chain_text<E: std::error::Error + ?Sized>(err: &E) -> String {
    let mut text = format!("{err}");
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

impl crate::hosted::HostedOps for GitHubGateway {
    fn permission(&self, actor: &str) -> Result<String, DtError> {
        GitHubGateway::permission(self, actor)
    }
    fn pr_base_head(&self) -> Result<(String, String), DtError> {
        GitHubGateway::pr_base_head(self)
    }
    fn marker_comment(&self) -> Result<Option<(u64, String)>, DtError> {
        GitHubGateway::marker_comment(self)
    }
    fn update_issue_comment(&self, comment_id: u64, body: &str) -> Result<(), DtError> {
        GitHubGateway::update_issue_comment(self, comment_id, body)
    }
    fn post_issue_comment(&self, body: &str) -> Result<(), DtError> {
        GitHubGateway::post_issue_comment(self, body)
    }
    fn own_threads(&self) -> Result<Vec<Thread>, DtError> {
        GitHubGateway::own_threads(self)
    }
    fn submit_review(&self, submission: &ReviewSubmission) -> Result<(), DtError> {
        GitHubGateway::submit_review(self, submission)
    }
    fn reply_to_thread(&self, comment_id: u64, body: &str) -> Result<(), DtError> {
        GitHubGateway::reply_to_thread(self, comment_id, body)
    }
    fn resolve_thread(&self, thread_id: &str) -> Result<(), DtError> {
        GitHubGateway::resolve_thread(self, thread_id)
    }
    fn fetch_check_runs(&self, sha: &str) -> Result<Vec<CheckRun>, DtError> {
        GitHubGateway::fetch_check_runs(self, sha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Side;

    #[test]
    fn the_graphql_documents_keep_every_field_a_separate_token() {
        for token in [
            "id",
            "isResolved",
            "comments",
            "databaseId",
            "author",
            "login",
        ] {
            assert!(
                THREADS_QUERY.contains(token),
                "{token} must stay a separate token in the threads query"
            );
        }
        for token in ["threadId", "isResolved", "thread"] {
            assert!(
                RESOLVE_MUTATION.contains(token),
                "{token} must stay a separate token in the resolve mutation"
            );
        }
        assert!(
            THREADS_QUERY.contains("pageInfo { hasNextPage endCursor }"),
            "the threads query paginates by cursor"
        );
    }

    #[test]
    fn the_login_comes_from_the_graphql_viewer_query() -> Result<(), Box<dyn std::error::Error>> {
        assert!(VIEWER_QUERY.contains("viewer"));
        let wire: ViewerWire =
            serde_json::from_str(r#"{ "viewer": { "login": "difftrace-app" } }"#)?;
        assert_eq!(
            wire.viewer.map(|viewer| viewer.login),
            Some("difftrace-app".to_owned())
        );
        Ok(())
    }

    #[test]
    fn the_wire_review_is_built_from_the_submission() -> Result<(), Box<dyn std::error::Error>> {
        let submission = ReviewSubmission {
            head_sha: "abc123".to_owned(),
            event: crate::contracts::ReviewEvent::ChangesRequested,
            body: "difftrace reviewed abc123.".to_owned(),
            comments: vec![crate::contracts::InlineComment {
                path: "src/main.rs".to_owned(),
                line: 12,
                side: Side::Right,
                body: "Unwrapped lock.".to_owned(),
            }],
        };
        let value = serde_json::to_value(WireReview::from_submission(&submission))?;
        assert_eq!(
            value.get("commit_id").and_then(serde_json::Value::as_str),
            Some("abc123")
        );
        assert_eq!(
            value.get("event").and_then(serde_json::Value::as_str),
            Some("REQUEST_CHANGES")
        );
        let comment = value
            .pointer("/comments/0")
            .ok_or("the inline comment is missing from the wire")?;
        assert_eq!(comment.get("line"), Some(&serde_json::json!(12)));
        assert_eq!(
            comment.get("side").and_then(serde_json::Value::as_str),
            Some("RIGHT")
        );
        assert!(
            comment.get("position").is_none(),
            "the modern wire shape anchors by line and side, never position"
        );
        Ok(())
    }

    #[test]
    fn approved_maps_to_approve_not_approved() {
        assert_eq!(
            crate::contracts::ReviewEvent::Approved.as_str(),
            "APPROVE",
            "APPROVED is the response state, not a request value"
        );
    }

    #[test]
    fn bot_logins_normalize_before_the_own_thread_filter() -> Result<(), Box<dyn std::error::Error>>
    {
        let page: ThreadsWire = serde_json::from_str(
            r#"{
                "repository": { "pullRequest": { "reviewThreads": { "nodes": [
                    { "id": "T_1", "isResolved": false, "comments": { "nodes": [
                        { "databaseId": 11, "author": { "login": "difftrace" },
                          "path": "src/a.rs", "line": 10, "originalLine": 8 } ] } },
                    { "id": "T_2", "isResolved": false, "comments": { "nodes": [
                        { "databaseId": 12, "author": { "login": "difftrace[bot]" },
                          "path": "src/b.rs", "line": 4, "originalLine": null } ] } },
                    { "id": "T_3", "isResolved": false, "comments": { "nodes": [
                        { "databaseId": 13, "author": { "login": "someone-else" },
                          "path": "src/c.rs", "line": 2, "originalLine": null } ] } },
                    { "id": "T_4", "isResolved": true, "comments": { "nodes": [
                        { "databaseId": 14, "author": { "login": "difftrace" },
                          "path": "src/d.rs", "line": 9, "originalLine": null } ] } }
                ], "pageInfo": { "hasNextPage": false } } } }
            }"#,
        )?;
        let (threads, next) = own_threads_page(page, "difftrace[bot]");
        assert_eq!(
            threads.len(),
            2,
            "own open threads survive, foreign and resolved drop"
        );
        assert!(next.is_none());
        let first = threads.first().ok_or("first thread missing")?;
        assert_eq!(first.id, "T_1");
        assert_eq!(first.line, 10);
        assert_eq!(first.original_line, Some(8));
        Ok(())
    }

    #[test]
    fn a_cursor_page_drives_the_next_query() -> Result<(), Box<dyn std::error::Error>> {
        let page: ThreadsWire = serde_json::from_str(
            r#"{ "repository": { "pullRequest": { "reviewThreads": {
                "nodes": [], "pageInfo": { "hasNextPage": true, "endCursor": "CURSOR_1" } } } } }"#,
        )?;
        let (threads, next) = own_threads_page(page, "difftrace");
        assert!(threads.is_empty());
        assert_eq!(next, Some("CURSOR_1".to_owned()));
        let truncated: ThreadsWire = serde_json::from_str(
            r#"{ "repository": { "pullRequest": { "reviewThreads": {
                "nodes": [], "pageInfo": { "hasNextPage": true } } } } }"#,
        )?;
        let (_, no_cursor) = own_threads_page(truncated, "difftrace");
        assert!(
            no_cursor.is_none(),
            "a page claiming more without a cursor ends the walk"
        );
        Ok(())
    }

    #[test]
    fn the_marker_comment_is_found_by_login_and_marker_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let marker = crate::state::marker::marker_start();
        let comments = vec![
            IssueCommentWire {
                id: 1,
                author: "someone-else".to_owned(),
                body: format!("also mentions {marker} but is foreign"),
            },
            IssueCommentWire {
                id: 2,
                author: "difftrace".to_owned(),
                body: format!("an older verdict\n{marker}{{}}"),
            },
            IssueCommentWire {
                id: 3,
                author: "difftrace[bot]".to_owned(),
                body: format!("the newest verdict\n{marker}{{\"round\":2}}"),
            },
        ];
        let (id, body) = own_marker_comment(&comments, "difftrace").ok_or("must be found")?;
        assert_eq!(
            id, 3,
            "the newest own match wins and bot suffixes normalize"
        );
        assert!(body.contains("\"round\":2"));
        let foreign = [IssueCommentWire {
            id: 1,
            author: "someone-else".to_owned(),
            body: format!("also mentions {marker} but is foreign"),
        }];
        assert!(
            own_marker_comment(&foreign, "difftrace").is_none(),
            "foreign comments never match"
        );
        Ok(())
    }

    #[test]
    fn annotations_map_to_tool_findings_by_severity() -> Result<(), Box<dyn std::error::Error>> {
        let failure: AnnotationWire = serde_json::from_str(
            r#"{ "path": "src/a.rs", "start_line": 3, "annotation_level": "failure", "message": "boom" }"#,
        )?;
        let mapped = annotation_to_finding("ci", &failure).ok_or("must map")?;
        assert_eq!(mapped.severity, Severity::Critical);
        assert_eq!(mapped.tool, "ci");
        assert_eq!(mapped.line, 3);
        let warning: AnnotationWire = serde_json::from_str(
            r#"{ "path": "src/a.rs", "start_line": 4, "annotation_level": "warning", "message": "w" }"#,
        )?;
        assert_eq!(
            annotation_to_finding("ci", &warning)
                .ok_or("must map")?
                .severity,
            Severity::Warning
        );
        let notice: AnnotationWire = serde_json::from_str(
            r#"{ "path": "src/a.rs", "annotation_level": "notice", "message": "n" }"#,
        )?;
        let mapped = annotation_to_finding("ci", &notice).ok_or("must map")?;
        assert_eq!(mapped.severity, Severity::Suggestion);
        assert_eq!(
            mapped.line, 0,
            "a file-level annotation anchors at line zero"
        );
        let pathless: AnnotationWire = serde_json::from_str(
            r#"{ "start_line": 9, "annotation_level": "failure", "message": "x" }"#,
        )?;
        assert!(
            annotation_to_finding("ci", &pathless).is_none(),
            "an annotation without a path cannot anchor and drops"
        );
        Ok(())
    }

    #[test]
    fn check_run_wire_parses_and_skips_annotations_for_green_runs()
    -> Result<(), Box<dyn std::error::Error>> {
        let wire: CheckRunsWire = serde_json::from_str(
            r#"{ "check_runs": [
                { "id": 1, "name": "clippy", "conclusion": "failure",
                  "output": { "summary": "2 findings" } },
                { "id": 2, "name": "fmt", "conclusion": "success" }
            ] }"#,
        )?;
        assert_eq!(wire.check_runs.len(), 2);
        let clippy = wire.check_runs.first().ok_or("first run missing")?;
        assert!(clippy.finished_unsuccessfully());
        let fmt = wire.check_runs.get(1).ok_or("second run missing")?;
        assert!(!fmt.finished_unsuccessfully());
        Ok(())
    }

    #[test]
    fn check_runs_and_annotations_request_full_pages_and_follow_them() {
        assert!(
            check_runs_route("o", "r", "deadbee", 1).contains("per_page=100&page=1"),
            "the check-runs listing asks for the maximum page"
        );
        assert!(check_runs_route("o", "r", "deadbee", 3).contains("&page=3"));
        assert!(
            annotations_route("o", "r", 7, 2).contains("per_page=50&page=2"),
            "the annotation endpoint caps at 50 — the loop must use its real maximum"
        );
        assert_eq!(ANNOTATIONS_PER_PAGE, 50);
        assert_eq!(CHECK_RUNS_PER_PAGE, 100);
    }

    #[test]
    fn the_error_chain_text_walks_every_source_level() {
        let root: Box<dyn std::error::Error> = "root".into();
        let inner: Level = Level {
            text: "inner".to_owned(),
            source: Some(root),
        };
        let outer: Level = Level {
            text: "outer".to_owned(),
            source: Some(Box::new(inner)),
        };
        assert_eq!(chain_text(&outer), "outer: inner: root");
    }

    #[derive(Debug)]
    struct Level {
        text: String,
        source: Option<Box<dyn std::error::Error>>,
    }

    impl std::fmt::Display for Level {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.text)
        }
    }

    impl std::error::Error for Level {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source.as_deref()
        }
    }
}
