//! An in-memory `PrGateway` for tool tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use crate::error::DifftraceError;
use crate::github::ExistingComment;
use crate::github::ExistingIssueComment;
use crate::github::PrGateway;
use crate::github::PrOverview;
use crate::github::ReviewSubmission;
use crate::github::ReviewThread;

#[derive(Clone, Default)]
pub(crate) struct FakeGateway {
    inner: std::sync::Arc<Inner>,
}

const OWN_LOGIN: &str = "difftrace[bot]";

#[derive(Default)]
struct Inner {
    overview: Mutex<Option<PrOverview>>,
    file: Mutex<Option<(String, String)>>,
    comments: Mutex<Vec<ExistingComment>>,
    submitted: Mutex<Option<ReviewSubmission>>,
    submit_calls: Mutex<usize>,
    fail_next_submit: Mutex<bool>,
    requested_prs: Mutex<Vec<u64>>,
    requested_reads: Mutex<Vec<(String, String)>>,
    requested_comment_lists: Mutex<Vec<u64>>,
    threads: Mutex<Vec<ReviewThread>>,
    resolved: Mutex<Vec<String>>,
    issue_comment: Mutex<Option<ExistingIssueComment>>,
    review_comments: Mutex<Vec<ExistingComment>>,
    permissions: Mutex<Vec<(String, String)>>,
    posted_replies: Mutex<Vec<(u64, String)>>,
    posted_comments: Mutex<Vec<(u64, String)>>,
    issue_comments: Mutex<Vec<ExistingIssueComment>>,
    updated_comments: Mutex<Vec<(u64, String)>>,
    fail_comment_writes: Mutex<usize>,
    fail_reply_writes: Mutex<usize>,
    next_comment_id: Mutex<u64>,
}

impl FakeGateway {
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(crate) fn with_overview(overview: PrOverview) -> Self {
        let gateway = Self::empty();
        *gateway
            .inner
            .overview
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(overview);
        gateway
    }

    pub(crate) fn with_file(path: &str, content: &str) -> Self {
        let gateway = Self::empty();
        *gateway
            .inner
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((path.to_owned(), content.to_owned()));
        gateway
    }

    pub(crate) fn with_comments(comments: Vec<ExistingComment>) -> Self {
        let gateway = Self::empty();
        *gateway
            .inner
            .comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = comments;
        gateway
    }

    pub(crate) fn with_threads(threads: Vec<ReviewThread>) -> Self {
        let gateway = Self::empty();
        *gateway
            .inner
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = threads;
        gateway
    }

    pub(crate) fn with_issue_comment(self, comment: ExistingIssueComment) -> Self {
        *self
            .inner
            .issue_comment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(comment);
        self
    }

    pub(crate) fn with_review_comment(self, comment: ExistingComment) -> Self {
        self.inner
            .review_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(comment);
        self
    }

    pub(crate) fn with_permission(self, user: &str, permission: &str) -> Self {
        self.inner
            .permissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((user.to_owned(), permission.to_owned()));
        self
    }

    pub(crate) fn requested_prs(&self) -> Vec<u64> {
        self.inner
            .requested_prs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn requested_reads(&self) -> Vec<(String, String)> {
        self.inner
            .requested_reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn requested_comment_lists(&self) -> Vec<u64> {
        self.inner
            .requested_comment_lists
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn submitted(&self) -> Option<ReviewSubmission> {
        self.inner
            .submitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn submit_calls(&self) -> usize {
        *self
            .inner
            .submit_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn fail_next_submit(&self) {
        *self
            .inner
            .fail_next_submit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    }

    pub(crate) fn resolved_threads(&self) -> Vec<String> {
        self.inner
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn posted_replies(&self) -> Vec<(u64, String)> {
        self.inner
            .posted_replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn posted_comments(&self) -> Vec<(u64, String)> {
        self.inner
            .posted_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn updated_comments(&self) -> Vec<(u64, String)> {
        self.inner
            .updated_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn issue_comment_bodies(&self) -> Vec<String> {
        self.inner
            .issue_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|comment| comment.body.clone())
            .collect()
    }

    pub(crate) fn fail_comment_writes(&self, count: usize) {
        *self
            .inner
            .fail_comment_writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = count;
    }

    pub(crate) fn fail_reply_writes(&self, count: usize) {
        *self
            .inner
            .fail_reply_writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = count;
    }

    fn consume_comment_write_failure(&self) -> bool {
        let mut guard = self
            .inner
            .fail_comment_writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *guard == 0 {
            return false;
        }
        *guard = (*guard).saturating_sub(1);
        true
    }
}

fn missing(what: &str) -> DifftraceError {
    DifftraceError::NotAFile {
        path: format!("fake gateway has no {what} configured"),
    }
}

impl PrGateway for FakeGateway {
    fn pr_overview(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<PrOverview, DifftraceError>> + Send + '_>> {
        self.inner
            .requested_prs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pr);
        let overview = self
            .inner
            .overview
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move { overview.ok_or_else(|| missing("overview")) })
    }

    fn pr_diff(
        &self,
        _pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        let err = missing("diff");
        Box::pin(async move { Err(err) })
    }

    fn file_at_ref(
        &self,
        path: String,
        git_ref: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        self.inner
            .requested_reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((path.clone(), git_ref));
        let configured = self
            .inner
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|(p, content)| {
                if *p == path {
                    content.clone()
                } else {
                    String::new()
                }
            });
        Box::pin(async move {
            match configured {
                Some(content) if !content.is_empty() => Ok(content),
                _ => Err(missing("file content")),
            }
        })
    }

    fn existing_review_comments(
        &self,
        pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingComment>, DifftraceError>> + Send + '_>>
    {
        self.inner
            .requested_comment_lists
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pr);
        let comments = self
            .inner
            .comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move { Ok(comments) })
    }

    fn submit_review(
        &self,
        pr: u64,
        submission: ReviewSubmission,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        self.inner
            .requested_prs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pr);
        {
            let mut guard = self
                .inner
                .submit_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = (*guard).saturating_add(1);
        }
        let fail = {
            let mut guard = self
                .inner
                .fail_next_submit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let value = *guard;
            *guard = false;
            value
        };
        if fail {
            let err = DifftraceError::NotAFile {
                path: "fake gateway submit failure".to_owned(),
            };
            return Box::pin(async move { Err(err) });
        }
        *self
            .inner
            .submitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(submission);
        Box::pin(async move { Ok(()) })
    }

    fn own_open_threads(
        &self,
        _pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ReviewThread>, DifftraceError>> + Send + '_>> {
        let threads = self
            .inner
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move { Ok(threads) })
    }

    fn resolve_thread(
        &self,
        thread_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        self.inner
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(thread_id);
        Box::pin(async move { Ok(()) })
    }

    fn find_own_marker_comment(
        &self,
        _pr: u64,
        marker: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<u64>, DifftraceError>> + Send + '_>> {
        let found = crate::github::own_marker_comment_id(
            &self
                .inner
                .issue_comments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            OWN_LOGIN,
            &marker,
        );
        Box::pin(async move { Ok(found) })
    }

    fn update_issue_comment(
        &self,
        comment_id: u64,
        body: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        if self.consume_comment_write_failure() {
            let err = DifftraceError::NotAFile {
                path: "fake gateway comment-write failure".to_owned(),
            };
            return Box::pin(async move { Err(err) });
        }
        {
            let mut guard = self
                .inner
                .issue_comments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(stored) = guard.iter_mut().find(|comment| comment.id == comment_id) else {
                let err = DifftraceError::NotAFile {
                    path: format!("fake gateway has no issue comment {comment_id}"),
                };
                return Box::pin(async move { Err(err) });
            };
            stored.body.clone_from(&body);
        }
        self.inner
            .updated_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((comment_id, body));
        Box::pin(async move { Ok(()) })
    }

    fn fetch_issue_comment(
        &self,
        comment_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<ExistingIssueComment, DifftraceError>> + Send + '_>>
    {
        let comment = self
            .inner
            .issue_comment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(
            async move { comment.ok_or_else(|| missing(&format!("issue comment {comment_id}"))) },
        )
    }

    fn fetch_review_comment(
        &self,
        comment_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<ExistingComment, DifftraceError>> + Send + '_>> {
        let comment = self
            .inner
            .review_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|comment| comment.id == comment_id)
            .cloned();
        Box::pin(
            async move { comment.ok_or_else(|| missing(&format!("review comment {comment_id}"))) },
        )
    }

    fn commenter_permission(
        &self,
        user: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        let permission = self
            .inner
            .permissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(name, _)| *name == user)
            .map_or_else(|| "read".to_owned(), |(_, permission)| permission.clone());
        Box::pin(async move { Ok(permission) })
    }

    fn reply_to_review_comment(
        &self,
        _pr: u64,
        comment_id: u64,
        body: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        let mut guard = self
            .inner
            .fail_reply_writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *guard > 0 {
            *guard = (*guard).saturating_sub(1);
            let err = DifftraceError::NotAFile {
                path: "fake gateway reply-write failure".to_owned(),
            };
            return Box::pin(async move { Err(err) });
        }
        self.inner
            .posted_replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((comment_id, body));
        Box::pin(async move { Ok(()) })
    }

    fn post_pr_comment(
        &self,
        pr: u64,
        body: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        if self.consume_comment_write_failure() {
            let err = DifftraceError::NotAFile {
                path: "fake gateway comment-write failure".to_owned(),
            };
            return Box::pin(async move { Err(err) });
        }
        let id = {
            let mut guard = self
                .inner
                .next_comment_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = (*guard).saturating_add(1);
            *guard
        };
        self.inner
            .issue_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ExistingIssueComment {
                id,
                body: body.clone(),
                author: OWN_LOGIN.to_owned(),
            });
        self.inner
            .posted_comments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((pr, body));
        Box::pin(async move { Ok(()) })
    }
}
