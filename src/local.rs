//! A local-git gateway: review a branch's diff without GitHub.
//!
//! `--base <ref>` on the review command switches the run onto this
//! gateway. The review's materials come from a local checkout — the
//! diff is `git diff <merge-base>..HEAD`, file contents are `git show`
//! — every write is refused, and the round renders to stdout. This is
//! the pre-push loop the hosted rounds cannot offer: a review that
//! runs before anything is pushed, against a head that cannot move
//! mid-round, with no posted state to drift — so what a local round
//! clears stays cleared, and the pull-request round that follows is a
//! confirmation, not a lottery.

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;

use crate::error::DifftraceError;
use crate::github::ExistingComment;
use crate::github::ExistingIssueComment;
use crate::github::PrGateway;
use crate::github::PrOverview;
use crate::github::ReviewSubmission;
use crate::github::ReviewThread;

pub struct LocalGateway {
    dir: PathBuf,
    base: String,
    head: String,
    head_branch: String,
    author: String,
    merge_base: String,
}

fn git_in(dir: &Path, args: &[&str]) -> Result<String, DifftraceError> {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|err| {
            DifftraceError::Cli(format!("cannot run git in {}: {err}", dir.display()))
        })?;
    if !output.status.success() {
        return Err(DifftraceError::Cli(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

impl LocalGateway {
    pub fn new(dir: &Path, base: &str) -> Result<Self, DifftraceError> {
        let head = git_in(dir, &["rev-parse", "--verify", "HEAD"])?
            .trim_end()
            .to_owned();
        let head_branch = git_in(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim_end()
            .to_owned();
        let author = git_in(dir, &["config", "user.name"])
            .map_or_else(|_| "local".to_owned(), |name| name.trim_end().to_owned());
        git_in(dir, &["rev-parse", "--verify", base])?;
        let merge_base = git_in(dir, &["merge-base", base, "HEAD"])?
            .trim_end()
            .to_owned();
        Ok(Self {
            dir: dir.to_owned(),
            base: base.to_owned(),
            head,
            head_branch,
            author,
            merge_base,
        })
    }

    fn git(&self, args: &[&str]) -> Result<String, DifftraceError> {
        git_in(&self.dir, args)
    }

    fn pinned_range(&self) -> String {
        format!("{}..{}", self.merge_base, self.head)
    }

    fn numstat(&self) -> Result<Vec<(u64, u64)>, DifftraceError> {
        let range = self.pinned_range();
        let stat = self.git(&["diff", "--numstat", &range])?;
        let mut rows = Vec::new();
        for line in stat.lines() {
            let mut fields = line.split('\t');
            let (Some(added), Some(removed)) = (fields.next(), fields.next()) else {
                continue;
            };
            let parse = |value: &str| value.parse::<u64>().unwrap_or(0);
            rows.push((parse(added), parse(removed)));
        }
        Ok(rows)
    }
}

impl PrGateway for LocalGateway {
    fn pr_overview(
        &self,
        _pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<PrOverview, DifftraceError>> + Send + '_>> {
        let overview = (|| {
            let rows = self.numstat()?;
            Ok::<PrOverview, DifftraceError>(PrOverview {
                number: 0,
                title: format!("local review of {}..{}", self.base, self.head_branch),
                description: None,
                author: self.author.clone(),
                head_sha: self.head.clone(),
                head_branch: self.head_branch.clone(),
                base_branch: self.base.clone(),
                changed_files: u64::try_from(rows.len()).unwrap_or(0),
                additions: rows.iter().map(|row| row.0).sum(),
                deletions: rows.iter().map(|row| row.1).sum(),
            })
        })();
        Box::pin(async move { overview })
    }

    fn pr_diff(
        &self,
        _pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        let range = self.pinned_range();
        let diff = self.git(&["diff", &range]);
        Box::pin(async move { diff })
    }

    fn file_at_ref(
        &self,
        path: String,
        git_ref: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        let content = self.git(&["show", &format!("{git_ref}:{path}")]);
        Box::pin(async move { content })
    }

    fn list_files(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, DifftraceError>> + Send + '_>> {
        let files = self
            .git(&["ls-files"])
            .map(|out| out.lines().map(str::to_owned).collect());
        Box::pin(async move { files })
    }

    fn existing_review_comments(
        &self,
        _pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingComment>, DifftraceError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn submit_review(
        &self,
        _pr: u64,
        _submission: ReviewSubmission,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        Box::pin(async {
            Err(DifftraceError::Cli(
                "local reviews never post; pass --base only for the offline loop".to_owned(),
            ))
        })
    }

    fn own_threads(
        &self,
        _pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ReviewThread>, DifftraceError>> + Send + '_>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn resolve_thread(
        &self,
        _thread_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        Box::pin(async {
            Err(DifftraceError::Cli(
                "local reviews never resolve threads".to_owned(),
            ))
        })
    }

    fn find_own_marker_comment(
        &self,
        _pr: u64,
        _marker: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<u64>, DifftraceError>> + Send + '_>> {
        Box::pin(async { Ok(None) })
    }

    fn update_issue_comment(
        &self,
        _comment_id: u64,
        _body: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        Box::pin(async {
            Err(DifftraceError::Cli(
                "local reviews never write comments".to_owned(),
            ))
        })
    }

    fn fetch_issue_comment(
        &self,
        _comment_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<ExistingIssueComment, DifftraceError>> + Send + '_>>
    {
        Box::pin(async {
            Err(DifftraceError::Cli(
                "comment threads do not exist in local reviews".to_owned(),
            ))
        })
    }

    fn issue_comments(
        &self,
        _pr: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingIssueComment>, DifftraceError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_review_comment(
        &self,
        _comment_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<ExistingComment, DifftraceError>> + Send + '_>> {
        Box::pin(async {
            Err(DifftraceError::Cli(
                "comment threads do not exist in local reviews".to_owned(),
            ))
        })
    }

    fn commenter_permission(
        &self,
        _user: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, DifftraceError>> + Send + '_>> {
        Box::pin(async { Ok("write".to_owned()) })
    }

    fn reply_to_review_comment(
        &self,
        _pr: u64,
        _comment_id: u64,
        _body: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        Box::pin(async {
            Err(DifftraceError::Cli(
                "local reviews never write replies".to_owned(),
            ))
        })
    }

    fn post_pr_comment(
        &self,
        _pr: u64,
        _body: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), DifftraceError>> + Send + '_>> {
        Box::pin(async {
            Err(DifftraceError::Cli(
                "local reviews never post comments".to_owned(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::ReviewEvent;

    fn git(dir: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.email=t@local", "-c", "user.name=local"])
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn scratch_repo(tag: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let dir =
            std::env::temp_dir().join(format!("difftrace-local-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        git(&dir, &["init", "-b", "trunk"])?;
        std::fs::write(dir.join("lib.rs"), "old\n")?;
        git(&dir, &["add", "."])?;
        git(&dir, &["commit", "-m", "base"])?;
        git(&dir, &["switch", "-c", "feature"])?;
        std::fs::write(dir.join("lib.rs"), "old\nnew\n")?;
        std::fs::write(dir.join("extra.rs"), "extra\n")?;
        git(&dir, &["add", "."])?;
        git(&dir, &["commit", "-m", "branch work"])?;
        Ok(dir)
    }

    fn drop_repo(dir: &Path) {
        for entry in std::fs::read_dir(dir).into_iter().flatten() {
            let Ok(entry) = entry else { continue };
            if entry.path().is_dir() {
                drop_repo(&entry.path());
            } else {
                let _unused = std::fs::remove_file(entry.path());
            }
        }
        let _unused = std::fs::remove_dir(dir);
    }

    #[tokio::test]
    async fn the_local_diff_is_the_branch_delta() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch_repo("diff")?;
        let gateway = LocalGateway::new(&dir, "trunk")?;
        let diff = gateway.pr_diff(0).await?;
        drop_repo(&dir);
        assert!(diff.contains("+new"), "the branch line appears: {diff}");
        assert!(diff.contains("+extra"), "the added file appears: {diff}");
        assert!(!diff.contains("-old"), "the base content is not a deletion");
        Ok(())
    }

    #[tokio::test]
    async fn the_overview_counts_the_branch_delta() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch_repo("overview")?;
        let gateway = LocalGateway::new(&dir, "trunk")?;
        let overview = gateway.pr_overview(0).await?;
        let head = git(&dir, &["rev-parse", "HEAD"])?.trim_end().to_owned();
        drop_repo(&dir);
        assert_eq!(overview.changed_files, 2);
        assert_eq!(overview.additions, 2);
        assert_eq!(overview.deletions, 0);
        assert_eq!(overview.head_branch, "feature");
        assert_eq!(overview.base_branch, "trunk");
        assert_eq!(overview.head_sha, head);
        Ok(())
    }

    #[tokio::test]
    async fn file_at_ref_reads_the_named_commit() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch_repo("file-at-ref")?;
        let gateway = LocalGateway::new(&dir, "trunk")?;
        let base = git(&dir, &["rev-parse", "trunk"])?.trim_end().to_owned();
        let at_base = gateway.file_at_ref("lib.rs".to_owned(), base).await?;
        let at_head = gateway
            .file_at_ref("lib.rs".to_owned(), "HEAD".to_owned())
            .await?;
        drop_repo(&dir);
        assert_eq!(at_base, "old\n", "the base commit holds the base content");
        assert_eq!(at_head, "old\nnew\n");
        Ok(())
    }

    #[tokio::test]
    async fn a_commit_after_open_does_not_change_the_reviewed_diff()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch_repo("snapshot")?;
        let gateway = LocalGateway::new(&dir, "trunk")?;
        let head_at_open = gateway.pr_overview(0).await?.head_sha;
        std::fs::write(dir.join("lib.rs"), "old\nnew\nlate\n")?;
        git(&dir, &["add", "."])?;
        git(&dir, &["commit", "-m", "mid-round commit"])?;
        let diff = gateway.pr_diff(0).await?;
        let overview = gateway.pr_overview(0).await?;
        drop_repo(&dir);
        assert!(diff.contains("+new"), "the open-time delta stands: {diff}");
        assert!(
            !diff.contains("late"),
            "a commit landing after the open never enters the reviewed diff: {diff}"
        );
        assert_eq!(
            overview.head_sha, head_at_open,
            "the stamped head is the one resolved at open"
        );
        Ok(())
    }

    #[test]
    fn an_unresolvable_base_fails_before_any_spend() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch_repo("bad-base")?;
        let err = LocalGateway::new(&dir, "no-such-ref")
            .err()
            .ok_or("expected the missing base to fail the open")?;
        drop_repo(&dir);
        assert!(
            err.to_string().contains("no-such-ref"),
            "the failure names the ref: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn listing_names_every_tracked_file() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch_repo("list-files")?;
        let gateway = LocalGateway::new(&dir, "trunk")?;
        let files = gateway.list_files().await?;
        drop_repo(&dir);
        let mut names = files;
        names.sort();
        assert_eq!(names, vec!["extra.rs".to_owned(), "lib.rs".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn posting_is_refused_in_local_mode() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch_repo("no-post")?;
        let gateway = LocalGateway::new(&dir, "trunk")?;
        let err = gateway
            .submit_review(
                0,
                ReviewSubmission {
                    head_sha: "h".to_owned(),
                    event: ReviewEvent::Commented,
                    summary: String::new(),
                    comments: Vec::new(),
                },
            )
            .await
            .err()
            .ok_or("expected the write to be refused")?;
        drop_repo(&dir);
        assert!(
            err.to_string().contains("never post"),
            "the refusal explains local mode: {err}"
        );
        Ok(())
    }
}
