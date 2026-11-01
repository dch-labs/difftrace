//! Live read-only verification of the hosted gateway against a real
//! pull request: viewer login, thread listing, check runs, and the
//! marker comment scan. Skipping silently unless `DIFFTRACE_E2E=1`,
//! `GITHUB_TOKEN`, `DIFFTRACE_TEST_REPO` (owner/name), and
//! `DIFFTRACE_TEST_PR` are all set — CI stays offline.

use difftrace::gateway::PrGateway;
use difftrace::gateway::github::GitHubGateway;

fn live_env() -> Option<(String, String, u64)> {
    if std::env::var("DIFFTRACE_E2E").ok().as_deref() != Some("1") {
        return None;
    }
    let token = std::env::var("GITHUB_TOKEN").ok()?;
    let repo = std::env::var("DIFFTRACE_TEST_REPO").ok()?;
    let pr = std::env::var("DIFFTRACE_TEST_PR")
        .ok()?
        .parse::<u64>()
        .ok()?;
    let (owner, name) = repo.split_once('/')?;
    if token.is_empty() || owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((token, format!("{owner}/{name}"), pr))
}

fn split(repo: &str) -> (&str, &str) {
    match repo.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => ("", ""),
    }
}

#[test]
fn live_gets_resolve_under_the_token() -> Result<(), Box<dyn std::error::Error>> {
    let Some((token, repo, pr)) = live_env() else {
        return Ok(());
    };
    let (owner, name) = split(&repo);
    let start = std::env::current_dir()?;
    let gateway = GitHubGateway::open_pr(&start, &token, owner, name, pr).map_err(|err| {
        format!(
            "the checkout must be the pull request head for the live read: {err} — run from a checkout of {repo}#{pr}"
        )
    })?;
    let login = gateway.own_login()?;
    assert!(!login.is_empty(), "the viewer query resolves a login");
    let threads = gateway.own_threads()?;
    println!(
        "live viewer={login} own open threads={} head={}",
        threads.len(),
        gateway.pinned_range().head_sha
    );
    let runs = gateway.fetch_check_runs(&gateway.pinned_range().head_sha)?;
    println!(
        "live check runs={} annotations={}",
        runs.len(),
        runs.iter().map(|run| run.findings.len()).sum::<usize>()
    );
    let marker = gateway.marker_comment()?;
    println!("live marker comment present={}", marker.is_some());
    Ok(())
}

#[test]
fn live_permission_matrix_reads() -> Result<(), Box<dyn std::error::Error>> {
    let Some((token, repo, _pr)) = live_env() else {
        return Ok(());
    };
    let (owner, name) = split(&repo);
    let start = std::env::current_dir()?;
    let gateway = GitHubGateway::open_at(&start, &token, owner, name, 0, "HEAD")?;
    let permission = gateway.permission(&gateway.own_login()?)?;
    assert!(
        !permission.is_empty(),
        "the collaborator permission endpoint answers for the viewer"
    );
    println!("live viewer permission={permission}");
    Ok(())
}
