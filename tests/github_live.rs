//! Live `GitHub` API tests.
//!
//! Run only when explicitly requested — set `DIFFTRACE_E2E=1`,
//! `GITHUB_TOKEN`, `DIFFTRACE_TEST_REPO` (`owner/repo`), and
//! `DIFFTRACE_TEST_PR`. Without them every test reports success by
//! skipping, so CI stays offline.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::missing_panics_doc))]

use difftrace::github::GitHubClient;
use difftrace::github::PrGateway;
use difftrace::github::RepoRef;

fn live_ready() -> Option<(String, String, u64)> {
    std::env::var("DIFFTRACE_E2E")
        .ok()
        .filter(|value| value == "1")?;
    let token = std::env::var("GITHUB_TOKEN").ok()?;
    let repo = std::env::var("DIFFTRACE_TEST_REPO").ok()?;
    let pr = std::env::var("DIFFTRACE_TEST_PR")
        .ok()
        .and_then(|v| v.parse().ok())?;
    Some((token, repo, pr))
}

#[tokio::test]
async fn live_pr_overview_and_diff_fetch() {
    let Some((token, repo, pr)) = live_ready() else {
        return;
    };
    let (owner, name) = repo.split_once('/').unwrap();
    let client = GitHubClient::new(
        token,
        RepoRef {
            owner: owner.to_owned(),
            repo: name.to_owned(),
        },
        None,
    )
    .unwrap();
    let overview = client.pr_overview(pr).await.unwrap();
    assert!(!overview.head_sha.is_empty());
    assert!(!overview.title.is_empty() || overview.number > 0);
    let diff = client.pr_diff(pr).await.unwrap();
    assert!(
        diff.contains("diff --git"),
        "expected a unified diff, got: {}",
        diff.chars().take(80).collect::<String>()
    );
}
