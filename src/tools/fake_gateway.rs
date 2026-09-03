//! An in-memory `PrGateway` for tool tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use crate::error::DifftraceError;
use crate::github::ExistingComment;
use crate::github::PrGateway;
use crate::github::PrOverview;
use crate::github::ReviewSubmission;

#[derive(Clone, Default)]
pub(crate) struct FakeGateway {
    inner: std::sync::Arc<Inner>,
}

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
}
