//! The `GitHub` REST layer: everything behind the [`PrGateway`] trait in
//! difftrace's own types, so the agent layers never see octocrab or the
//! wire models. [`GitHubClient`] is the PAT-authenticated reference
//! implementation.

mod review;

use std::future::Future;
use std::pin::Pin;

use base64::Engine as _;
use octocrab::Octocrab;

pub use review::CommentPosition;
pub use review::ReviewEvent;
pub use review::ReviewSubmission;
pub use review::Side;

use crate::error::DifftraceError;

const THREADS_QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!, $cursor: String) {\
  repository(owner: $owner, name: $name) {\
    pullRequest(number: $number) {\
      reviewThreads(first: 100, after: $cursor) {\
        nodes {\
          id\
          isResolved\
          comments(first: 1) {\
            nodes {\
              author { login }\
              path\
              line\
              originalLine\
            }\
          }\
        }\
        pageInfo { hasNextPage endCursor }\
      }\
    }\
  }\
}";

const RESOLVE_MUTATION: &str = "\
mutation($threadId: ID!) {\
  resolveReviewThread(input: { threadId: $threadId }) {\
    thread { isResolved }\
  }\
}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrOverview {
    pub number: u64,
    pub title: String,
    pub description: Option<String>,
    pub author: String,
    pub head_sha: String,
    pub head_branch: String,
    pub base_branch: String,
    pub changed_files: u64,
    pub additions: u64,
    pub deletions: u64,
}

impl From<octocrab::models::pulls::PullRequest> for PrOverview {
    fn from(pr: octocrab::models::pulls::PullRequest) -> Self {
        Self {
            number: pr.number,
            title: pr.title.unwrap_or_default(),
            description: pr.body,
            author: pr.user.map(|user| user.login).unwrap_or_default(),
            head_sha: pr.head.sha.clone(),
            head_branch: pr.head.ref_field.clone(),
            base_branch: pr.base.ref_field.clone(),
            changed_files: pr.changed_files.unwrap_or_default(),
            additions: pr.additions.unwrap_or_default(),
            deletions: pr.deletions.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingComment {
    pub id: u64,
    pub path: String,
    pub line: Option<u64>,
    pub side: Option<Side>,
    pub body: String,
    pub author: String,
}

impl From<octocrab::models::pulls::Comment> for ExistingComment {
    fn from(comment: octocrab::models::pulls::Comment) -> Self {
        Self {
            id: *comment.id,
            path: comment.path,
            line: comment.line,
            side: comment.side.as_deref().and_then(Side::from_wire),
            body: comment.body,
            author: comment.user.map(|user| user.login).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub original_line: Option<u64>,
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
    #[serde(default)]
    author: Option<AuthorWire>,
    path: String,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    original_line: Option<u64>,
}

#[derive(serde::Deserialize)]
struct AuthorWire {
    login: String,
}

fn own_threads_page(wire: ThreadsWire, own_login: &str) -> (Vec<ReviewThread>, Option<String>) {
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
        .filter(|thread| !thread.is_resolved)
        .filter_map(|thread| {
            let comment = thread.comments?.nodes.into_iter().next()?;
            let login = comment.author.map(|author| author.login)?;
            if login != own_login {
                return None;
            }
            Some(ReviewThread {
                id: thread.id,
                path: comment.path,
                line: comment.line,
                original_line: comment.original_line,
            })
        })
        .collect();
    (mapped, next)
}

pub trait PrGateway: Send + Sync {
    fn pr_overview(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<PrOverview, DifftraceError>> + Send + '_>>;

    fn pr_diff(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>>;

    fn file_at_ref(
        &self,
        path: String,
        git_ref: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>>;

    fn existing_review_comments(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingComment>, DifftraceError>> + Send + '_>>;

    fn submit_review(
        &self,
        pr: u64,
        submission: ReviewSubmission,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>>;

    fn own_open_threads(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ReviewThread>, DifftraceError>> + Send + '_>>;

    fn resolve_thread(
        &self,
        thread_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>>;
}

pub struct GitHubClient {
    crab: Octocrab,
    repo: RepoRef,
}

impl std::fmt::Debug for GitHubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubClient")
            .field("repo", &self.repo)
            .finish_non_exhaustive()
    }
}

impl GitHubClient {
    pub fn new(
        token: String,
        repo: RepoRef,
        api_base_url: Option<&str>,
    ) -> Result<Self, DifftraceError> {
        let mut builder = Octocrab::builder().personal_token(token);
        if let Some(base_url) = api_base_url {
            builder =
                builder
                    .base_uri(base_url)
                    .map_err(|source| DifftraceError::InvalidBaseUrl {
                        url: base_url.to_owned(),
                        source: Box::new(source),
                    })?;
        }
        let crab = builder
            .build()
            .map_err(|source| DifftraceError::GitHubInit { source })?;
        Ok(Self { crab, repo })
    }

    #[must_use]
    pub fn repo(&self) -> &RepoRef {
        &self.repo
    }

    fn map_github_error(source: octocrab::Error) -> DifftraceError {
        DifftraceError::GitHubApi { source }
    }

    fn decode_content(
        path: &str,
        content: octocrab::models::repos::Content,
    ) -> Result<String, DifftraceError> {
        if content.encoding.as_deref() != Some("base64") {
            return Err(DifftraceError::ContentTooLarge {
                path: path.to_owned(),
                size: content.size,
            });
        }
        let encoded = content.content.ok_or_else(|| DifftraceError::NotAFile {
            path: path.to_owned(),
        })?;
        let mut raw = encoded.into_bytes();
        raw.retain(|byte| !b" \n\t\r\x0b\x0c".contains(byte));
        let bytes = base64::prelude::BASE64_STANDARD
            .decode(raw)
            .map_err(|source| DifftraceError::ContentDecode {
                path: path.to_owned(),
                source,
            })?;
        String::from_utf8(bytes).map_err(|_| DifftraceError::BinaryContent {
            path: path.to_owned(),
        })
    }
}

impl PrGateway for GitHubClient {
    fn pr_overview(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<PrOverview, DifftraceError>> + Send + '_>> {
        let owner = self.repo.owner.clone();
        let repo = self.repo.repo.clone();
        Box::pin(async move {
            let model = self
                .crab
                .pulls(owner.as_str(), repo.as_str())
                .get(pr)
                .await
                .map_err(Self::map_github_error)?;
            Ok(PrOverview::from(model))
        })
    }

    fn pr_diff(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        let owner = self.repo.owner.clone();
        let repo = self.repo.repo.clone();
        Box::pin(async move {
            self.crab
                .pulls(owner.as_str(), repo.as_str())
                .get_diff(pr)
                .await
                .map_err(Self::map_github_error)
        })
    }

    fn file_at_ref(
        &self,
        path: String,
        git_ref: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        let owner = self.repo.owner.clone();
        let repo = self.repo.repo.clone();
        Box::pin(async move {
            let mut items = self
                .crab
                .repos(owner.as_str(), repo.as_str())
                .get_content()
                .path(path.as_str())
                .r#ref(git_ref.as_str())
                .send()
                .await
                .map_err(Self::map_github_error)?;
            let mut contents = items.take_items();
            if contents.len() != 1 {
                return Err(DifftraceError::NotAFile { path });
            }
            let Some(content) = contents.pop() else {
                return Err(DifftraceError::NotAFile { path });
            };
            Self::decode_content(&path, content)
        })
    }

    fn existing_review_comments(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingComment>, DifftraceError>> + Send + '_>>
    {
        let owner = self.repo.owner.clone();
        let repo = self.repo.repo.clone();
        Box::pin(async move {
            let page = self
                .crab
                .pulls(owner.as_str(), repo.as_str())
                .list_comments(Some(pr))
                .send()
                .await
                .map_err(Self::map_github_error)?;
            let comments = self
                .crab
                .all_pages(page)
                .await
                .map_err(Self::map_github_error)?;
            Ok(comments.into_iter().map(ExistingComment::from).collect())
        })
    }

    fn submit_review(
        &self,
        pr: u64,
        submission: ReviewSubmission,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        let owner = self.repo.owner.clone();
        let repo = self.repo.repo.clone();
        Box::pin(
            async move { review::post_review(&self.crab, &owner, &repo, pr, submission).await },
        )
    }

    fn own_open_threads(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ReviewThread>, DifftraceError>> + Send + '_>> {
        let owner = self.repo.owner.clone();
        let repo = self.repo.repo.clone();
        Box::pin(async move {
            let login = self
                .crab
                .current()
                .user()
                .await
                .map_err(Self::map_github_error)?
                .login;
            let mut threads = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let payload = serde_json::json!({
                    "query": THREADS_QUERY,
                    "variables": {
                        "owner": owner.as_str(),
                        "name": repo.as_str(),
                        "number": pr,
                        "cursor": cursor,
                    },
                });
                let wire: ThreadsWire = self
                    .crab
                    .graphql(&payload)
                    .await
                    .map_err(Self::map_github_error)?;
                let (page, next) = own_threads_page(wire, &login);
                threads.extend(page);
                match next {
                    Some(next_cursor) => cursor = Some(next_cursor),
                    None => break,
                }
            }
            Ok(threads)
        })
    }

    fn resolve_thread(
        &self,
        thread_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        Box::pin(async move {
            let payload = serde_json::json!({
                "query": RESOLVE_MUTATION,
                "variables": { "threadId": thread_id },
            });
            let _: serde_json::Value = self
                .crab
                .graphql(&payload)
                .await
                .map_err(Self::map_github_error)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn author_json(login: &str) -> serde_json::Value {
        json!({
            "login": login,
            "id": 7,
            "node_id": "MDQ6VXNlcjc=",
            "avatar_url": "https://avatars.githubusercontent.com/u/7?v=4",
            "gravatar_id": "",
            "url": format!("https://api.github.com/users/{login}"),
            "html_url": format!("https://github.com/{login}"),
            "followers_url": format!("https://api.github.com/users/{login}/followers"),
            "following_url": format!("https://api.github.com/users/{login}/following{{/other_user}}"),
            "gists_url": format!("https://api.github.com/users/{login}/gists{{/gist_id}}"),
            "starred_url": format!("https://api.github.com/users/{login}/starred{{/owner}}{{/repo}}"),
            "subscriptions_url": format!("https://api.github.com/users/{login}/subscriptions"),
            "organizations_url": format!("https://api.github.com/users/{login}/orgs"),
            "repos_url": format!("https://api.github.com/users/{login}/repos"),
            "events_url": format!("https://api.github.com/users/{login}/events{{/privacy}}"),
            "received_events_url": format!("https://api.github.com/users/{login}/received_events"),
            "type": "User",
            "site_admin": false
        })
    }

    #[test]
    fn pr_overview_maps_the_wire_fields() -> Result<(), Box<dyn std::error::Error>> {
        let model: octocrab::models::pulls::PullRequest = serde_json::from_value(json!({
            "id": 1001,
            "number": 42,
            "url": "https://api.github.com/repos/acme/app/pulls/42",
            "title": "Fix the flaky worker",
            "body": "Restarts consumers on backpressure.",
            "user": author_json("dana"),
            "head": { "ref": "fix/worker", "sha": "abc123fullsha0000000000000000000000000000" },
            "base": { "ref": "main", "sha": "def456fullsha0000000000000000000000000000" },
            "changed_files": 3,
            "additions": 120,
            "deletions": 15
        }))?;
        let overview = PrOverview::from(model);
        assert_eq!(overview.number, 42);
        assert_eq!(overview.title, "Fix the flaky worker");
        assert_eq!(
            overview.description.as_deref(),
            Some("Restarts consumers on backpressure.")
        );
        assert_eq!(overview.author, "dana");
        assert_eq!(overview.head_branch, "fix/worker");
        assert_eq!(overview.base_branch, "main");
        assert_eq!(
            overview.head_sha,
            "abc123fullsha0000000000000000000000000000"
        );
        assert_eq!(overview.changed_files, 3);
        assert_eq!(overview.additions, 120);
        assert_eq!(overview.deletions, 15);
        Ok(())
    }

    #[test]
    fn absent_optional_fields_default_to_empty() -> Result<(), Box<dyn std::error::Error>> {
        let model: octocrab::models::pulls::PullRequest = serde_json::from_value(json!({
            "id": 1002,
            "number": 43,
            "url": "https://api.github.com/repos/acme/app/pulls/43",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" }
        }))?;
        let overview = PrOverview::from(model);
        assert_eq!(overview.title, "");
        assert_eq!(overview.description, None);
        assert_eq!(overview.author, "");
        assert_eq!(overview.changed_files, 0);
        Ok(())
    }

    #[test]
    fn existing_comment_maps_the_wire_fields() -> Result<(), Box<dyn std::error::Error>> {
        let model: octocrab::models::pulls::Comment = serde_json::from_value(json!({
            "url": "https://api.github.com/repos/acme/app/pulls/comments/9",
            "id": 9,
            "node_id": "MDI0OlB1bGxSZXF1ZXN0UmV2aWV3Q29tbWVudDk=",
            "diff_hunk": "@@ -1,2 +1,3 @@\n context\n-old\n+new",
            "path": "src/main.rs",
            "position": 4,
            "original_position": 4,
            "commit_id": "ccc",
            "original_commit_id": "ccc",
            "user": author_json("dana"),
            "body": "This drops the lock too early.",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "html_url": "https://github.com/acme/app/pull/42#discussion_r9",
            "pull_request_url": "https://api.github.com/repos/acme/app/pulls/42",
            "_links": {},
            "line": 12,
            "side": "LEFT"
        }))?;
        let comment = ExistingComment::from(model);
        assert_eq!(comment.id, 9);
        assert_eq!(comment.path, "src/main.rs");
        assert_eq!(comment.line, Some(12));
        assert_eq!(comment.side, Some(Side::Left));
        assert_eq!(comment.body, "This drops the lock too early.");
        assert_eq!(comment.author, "dana");
        Ok(())
    }

    #[test]
    fn an_absent_wire_side_maps_to_none() -> Result<(), Box<dyn std::error::Error>> {
        let model: octocrab::models::pulls::Comment = serde_json::from_value(json!({
            "url": "https://api.github.com/repos/acme/app/pulls/comments/10",
            "id": 10,
            "node_id": "MDI0OlB1bGxSZXF1ZXN0UmV2aWV3Q29tbWVudDEw",
            "diff_hunk": "@@ -1 +1 @@\n-a\n+b",
            "path": "src/lib.rs",
            "commit_id": "ddd",
            "original_commit_id": "ddd",
            "body": "Note.",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "html_url": "https://github.com/acme/app/pull/42#discussion_r10",
            "pull_request_url": "https://api.github.com/repos/acme/app/pulls/42",
            "_links": {}
        }))?;
        let comment = ExistingComment::from(model);
        assert_eq!(comment.side, None);
        Ok(())
    }

    fn content_json(
        encoding: &str,
        content: &str,
        size: i64,
    ) -> Result<octocrab::models::repos::Content, Box<dyn std::error::Error>> {
        serde_json::from_value(json!({
            "name": "file.txt",
            "path": "file.txt",
            "sha": "abc",
            "encoding": encoding,
            "content": content,
            "size": size,
            "url": "https://api.github.com/repos/acme/app/contents/file.txt",
            "html_url": "https://github.com/acme/app/blob/main/file.txt",
            "git_url": "https://api.github.com/repos/acme/app/git/blobs/abc",
            "download_url": "https://raw.githubusercontent.com/acme/app/main/file.txt",
            "type": "file",
            "_links": {
                "self": "https://api.github.com/repos/acme/app/contents/file.txt"
            }
        }))
        .map_err(Into::into)
    }

    #[test]
    fn content_above_the_endpoint_limit_is_rejected_not_emptied()
    -> Result<(), Box<dyn std::error::Error>> {
        let err =
            GitHubClient::decode_content("vendored/big.log", content_json("none", "", 5_242_880)?)
                .err()
                .ok_or("expected an error")?;
        assert!(
            err.to_string().contains("5242880"),
            "error reports the size: {err}"
        );
        assert!(matches!(err, DifftraceError::ContentTooLarge { .. }));
        Ok(())
    }

    #[test]
    fn base64_content_round_trips_through_the_decode() -> Result<(), Box<dyn std::error::Error>> {
        let text = GitHubClient::decode_content(
            "src/lib.rs",
            content_json("base64", "Zm4gbWFpbigpIHt9Cg==\n", 12)?,
        )?;
        assert_eq!(text, "fn main() {}\n");
        Ok(())
    }

    #[test]
    fn non_utf8_content_is_rejected_as_binary() -> Result<(), Box<dyn std::error::Error>> {
        let err = GitHubClient::decode_content("logo.dat", content_json("base64", "/w==", 1)?)
            .err()
            .ok_or("expected an error")?;
        assert!(matches!(err, DifftraceError::BinaryContent { .. }));
        Ok(())
    }

    #[test]
    fn enterprise_base_url_must_parse() -> Result<(), Box<dyn std::error::Error>> {
        let err = GitHubClient::new(
            "token".to_owned(),
            RepoRef {
                owner: "acme".to_owned(),
                repo: "app".to_owned(),
            },
            Some("not a url"),
        )
        .err()
        .ok_or("expected an error")?;
        assert!(
            err.to_string().contains("base URL"),
            "error names the problem: {err}"
        );
        Ok(())
    }

    #[test]
    fn own_threads_come_from_the_wire_filtered_by_author_and_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let wire: ThreadsWire = serde_json::from_value(json!({
            "repository": { "pullRequest": { "reviewThreads": { "nodes": [
                {
                    "id": "T_KEEP",
                    "isResolved": false,
                    "comments": { "nodes": [ {
                        "author": { "login": "difftrace[bot]" },
                        "path": "src/a.rs", "line": 2, "originalLine": 2
                    } ] }
                },
                {
                    "id": "T_DONE",
                    "isResolved": true,
                    "comments": { "nodes": [ {
                        "author": { "login": "difftrace[bot]" },
                        "path": "src/a.rs", "line": 5, "originalLine": 5
                    } ] }
                },
                {
                    "id": "T_FOREIGN",
                    "isResolved": false,
                    "comments": { "nodes": [ {
                        "author": { "login": "dana" },
                        "path": "src/b.rs", "line": 7, "originalLine": 7
                    } ] }
                },
                {
                    "id": "T_OUTDATED",
                    "isResolved": false,
                    "comments": { "nodes": [ {
                        "author": { "login": "difftrace[bot]" },
                        "path": "src/c.rs", "line": null, "originalLine": 30
                    } ] }
                }
            ] , "pageInfo": { "hasNextPage": false } } } }
        }))?;
        let (threads, next) = own_threads_page(wire, "difftrace[bot]");
        assert_eq!(threads.len(), 2);
        assert!(threads.iter().any(|thread| {
            thread.id == "T_KEEP" && thread.path == "src/a.rs" && thread.line == Some(2)
        }));
        assert!(
            threads
                .iter()
                .any(|thread| thread.id == "T_OUTDATED" && thread.line.is_none())
        );
        assert_eq!(next, None);
        Ok(())
    }

    #[test]
    fn a_missing_repository_section_yields_no_threads() -> Result<(), Box<dyn std::error::Error>> {
        let wire: ThreadsWire = serde_json::from_value(json!({}))?;
        let (threads, next) = own_threads_page(wire, "difftrace[bot]");
        assert!(threads.is_empty());
        assert_eq!(next, None);
        Ok(())
    }

    #[test]
    fn thread_pages_carry_the_next_cursor_only_when_one_exists()
    -> Result<(), Box<dyn std::error::Error>> {
        let page = |has_next: bool, cursor: serde_json::Value| {
            json!({
                "repository": { "pullRequest": { "reviewThreads": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": has_next, "endCursor": cursor }
                } } }
            })
        };
        let more: ThreadsWire = serde_json::from_value(page(true, json!("CURSOR_1")))?;
        let (_, next) = own_threads_page(more, "difftrace[bot]");
        assert_eq!(next, Some("CURSOR_1".to_owned()));
        let last: ThreadsWire = serde_json::from_value(page(false, json!("CURSOR_1")))?;
        let (_, next) = own_threads_page(last, "difftrace[bot]");
        assert_eq!(next, None);
        let guarded: ThreadsWire = serde_json::from_value(page(true, json!(null)))?;
        let (_, next) = own_threads_page(guarded, "difftrace[bot]");
        assert_eq!(
            next, None,
            "a page claiming more without a cursor must end the loop"
        );
        Ok(())
    }

    #[test]
    fn the_resolve_mutation_targets_the_thread() {
        assert!(RESOLVE_MUTATION.contains("resolveReviewThread"));
        assert!(RESOLVE_MUTATION.contains("$threadId"));
    }
}
