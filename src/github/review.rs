//! Review submission: one atomic POST carrying the summary and every
//! inline comment, anchored with the modern `line` + `side` fields.

use octocrab::Octocrab;

use crate::error::DifftraceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Right,
    Left,
}

impl Side {
    fn as_str(self) -> &'static str {
        match self {
            Self::Right => "RIGHT",
            Self::Left => "LEFT",
        }
    }

    pub(crate) fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "RIGHT" => Some(Self::Right),
            "LEFT" => Some(Self::Left),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentPosition {
    pub path: String,
    pub line: u64,
    pub side: Side,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSubmission {
    pub head_sha: String,
    pub summary: String,
    pub comments: Vec<CommentPosition>,
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
            body: submission.summary.as_str(),
            event: "COMMENT",
            comments: submission
                .comments
                .iter()
                .map(|comment| WireComment {
                    path: comment.path.as_str(),
                    line: comment.line,
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

pub(super) async fn post_review(
    crab: &Octocrab,
    owner: &str,
    repo: &str,
    pr: u64,
    submission: ReviewSubmission,
) -> Result<(), DifftraceError> {
    let route = format!("/repos/{owner}/{repo}/pulls/{pr}/reviews");
    let wire = WireReview::from_submission(&submission);
    crab.post::<_, octocrab::models::pulls::Review>(&route, Some(&wire))
        .await
        .map_err(|source| DifftraceError::GitHubApi { source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_review_is_built_from_the_submission() {
        let submission = ReviewSubmission {
            head_sha: "abc123".to_owned(),
            summary: "Two findings.".to_owned(),
            comments: vec![
                CommentPosition {
                    path: "src/main.rs".to_owned(),
                    line: 12,
                    side: Side::Right,
                    body: "Unwrapped lock.".to_owned(),
                },
                CommentPosition {
                    path: "old/module.rs".to_owned(),
                    line: 7,
                    side: Side::Left,
                    body: "Removed constant still referenced.".to_owned(),
                },
            ],
        };
        let value = serde_json::to_value(WireReview::from_submission(&submission)).unwrap();
        assert_eq!(value["commit_id"], "abc123");
        assert_eq!(value["body"], "Two findings.");
        assert_eq!(value["event"], "COMMENT");
        assert_eq!(value["comments"].as_array().map(Vec::len), Some(2));
        assert_eq!(value["comments"][0]["path"], "src/main.rs");
        assert_eq!(value["comments"][0]["line"], 12);
        assert_eq!(value["comments"][0]["side"], "RIGHT");
        assert_eq!(value["comments"][0]["body"], "Unwrapped lock.");
        assert_eq!(value["comments"][1]["path"], "old/module.rs");
        assert_eq!(value["comments"][1]["line"], 7);
        assert_eq!(value["comments"][1]["side"], "LEFT");
        assert_eq!(
            value["comments"][1]["body"],
            "Removed constant still referenced."
        );
    }

    #[test]
    fn an_empty_submission_posts_a_summary_only_review() {
        let submission = ReviewSubmission {
            head_sha: "abc123".to_owned(),
            summary: "Clean.".to_owned(),
            comments: Vec::new(),
        };
        let value = serde_json::to_value(WireReview::from_submission(&submission)).unwrap();
        assert_eq!(value["comments"].as_array().map(Vec::len), Some(0));
        assert_eq!(value["event"], "COMMENT");
    }

    #[test]
    fn side_wire_names_round_trip() {
        assert_eq!(Side::Right.as_str(), "RIGHT");
        assert_eq!(Side::Left.as_str(), "LEFT");
        assert_eq!(Side::from_wire("RIGHT"), Some(Side::Right));
        assert_eq!(Side::from_wire("LEFT"), Some(Side::Left));
        assert_eq!(Side::from_wire("weird"), None);
    }
}
