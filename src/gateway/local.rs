//! Local gateway: hermetic git with every read pinned to one range.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::contracts::{DiffSource, FileSnapshot, PinnedRange, SearchHit};
use crate::error::DtError;

use super::PrGateway;

const HERMETIC: [&str; 6] = [
    "-c",
    "color.ui=never",
    "-c",
    "core.quotePath=false",
    "-c",
    "diff.noprefix=false",
];

pub struct LocalGateway {
    root: PathBuf,
    range: PinnedRange,
    diff: DiffSource,
    tracked: BTreeSet<String>,
}

impl LocalGateway {
    pub fn open(base: &str) -> Result<Self, DtError> {
        let cwd = std::env::current_dir()
            .map_err(|err| DtError::Git(format!("cannot read the working directory: {err}")))?;
        Self::open_at(&cwd, base)
    }

    pub fn open_at(start: &Path, base: &str) -> Result<Self, DtError> {
        let root = toplevel(start)?;
        let head = run_git(&root, &["rev-parse", "HEAD"])?
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let merge_base = run_git(&root, &["merge-base", "--", base, "HEAD"])?
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if head.is_empty() || merge_base.is_empty() {
            return Err(DtError::Git(
                "could not resolve the pinned range".to_owned(),
            ));
        }
        let diff = run_git(&root, &["diff", "--no-renames", &merge_base, &head])?;
        let tracked = run_git(&root, &["ls-tree", "-r", "--name-only", &head])?
            .lines()
            .map(str::to_owned)
            .collect();
        Ok(Self {
            root,
            range: PinnedRange {
                base_sha: merge_base,
                head_sha: head,
            },
            diff: DiffSource::Unified(diff),
            tracked,
        })
    }
}

impl PrGateway for LocalGateway {
    fn pinned_range(&self) -> PinnedRange {
        self.range.clone()
    }

    fn diff(&self) -> DiffSource {
        self.diff.clone()
    }

    fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, DtError> {
        let at = format!("{}:{path}", self.range.head_sha);
        let output = Command::new("git")
            .args(HERMETIC)
            .args(["cat-file", "blob", &at])
            .current_dir(&self.root)
            .output()
            .map_err(|err| DtError::Git(format!("could not run git: {err}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(DtError::Gateway(format!(
                "no file {path:?} at {}: {stderr}",
                self.range.head_sha
            )));
        }
        let content = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(FileSnapshot {
            path: path.to_owned(),
            content,
        })
    }

    fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
        let listing = run_git(
            &self.root,
            &["ls-tree", "-r", "--name-only", &self.range.head_sha],
        )?;
        Ok(listing.lines().map(str::to_owned).collect())
    }

    fn search_at_head(&self, pattern: &str, cap: usize) -> Result<Vec<SearchHit>, DtError> {
        let output = Command::new("git")
            .args(HERMETIC)
            .args(["grep", "-n", "-I", "-e", pattern, &self.range.head_sha])
            .current_dir(&self.root)
            .output()
            .map_err(|err| DtError::Git(format!("could not run git: {err}")))?;
        let status = output.status.code().unwrap_or(2);
        if status >= 2 {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(DtError::Gateway(format!(
                "search pattern {pattern:?} failed: {stderr}"
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let sha = self.range.head_sha.as_str();
        let mut hits: Vec<SearchHit> = stdout
            .lines()
            .filter_map(|line| parse_hit(line, sha, &self.tracked))
            .collect();
        hits.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        hits.truncate(cap);
        Ok(hits)
    }
}

fn toplevel(start: &Path) -> Result<PathBuf, DtError> {
    let text = run_git(start, &["rev-parse", "--show-toplevel"])?;
    let line = text.lines().next().unwrap_or_default().trim().to_owned();
    if line.is_empty() {
        return Err(DtError::Git("not inside a git repository".to_owned()));
    }
    Ok(PathBuf::from(line))
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, DtError> {
    let output = Command::new("git")
        .args(HERMETIC)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| DtError::Git(format!("could not run git: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(DtError::Git(format!(
            "git {} failed: {stderr}",
            args.first().copied().unwrap_or_default()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_hit(line: &str, sha: &str, tracked: &BTreeSet<String>) -> Option<SearchHit> {
    let rest = line.strip_prefix(sha)?.strip_prefix(':')?;
    let parts: Vec<&str> = rest.split(':').collect();
    let is_number =
        |segment: &str| !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit());
    let anchor_at = |index: usize| -> Option<SearchHit> {
        Some(SearchHit {
            file: parts.get(..index).map(|head| head.join(":"))?,
            line: parts
                .get(index)
                .and_then(|segment| segment.parse::<u32>().ok())?,
            text: parts
                .get(index.saturating_add(1)..)
                .map(|tail| tail.join(":"))?,
        })
    };
    let mut unverified: Option<SearchHit> = None;
    for index in 1..parts.len() {
        let digits = is_number(parts.get(index).copied().unwrap_or_default());
        if !digits || parts.get(index.saturating_add(1)).is_none() {
            continue;
        }
        let Some(hit) = anchor_at(index) else {
            continue;
        };
        if hit.file.is_empty() {
            continue;
        }
        if tracked.contains(&hit.file) {
            return Some(hit);
        }
        if unverified.is_none() {
            unverified = Some(hit);
        }
    }
    unverified
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> Result<PathBuf, DtError> {
        let dir =
            std::env::temp_dir().join(format!("difftrace-gateway-{}-{name}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|err| DtError::Git(err.to_string()))?;
        run_git(&dir, &["init", "-b", "main"])?;
        Ok(dir)
    }

    fn commit(dir: &Path, files: &[(&str, &str)], message: &str) -> Result<String, DtError> {
        for (path, body) in files {
            let file = dir.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|err| DtError::Git(err.to_string()))?;
            }
            std::fs::write(&file, body).map_err(|err| DtError::Git(err.to_string()))?;
        }
        run_git(dir, &["add", "-A"])?;
        run_git(
            dir,
            &[
                "-c",
                "user.email=difftrace@example.invalid",
                "-c",
                "user.name=difftrace",
                "commit",
                "-m",
                message,
            ],
        )?;
        Ok(run_git(dir, &["rev-parse", "HEAD"])?
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned())
    }

    #[test]
    fn a_commit_after_open_does_not_change_the_reviewed_diff() -> Result<(), DtError> {
        let dir = scratch("pin")?;
        commit(&dir, &[("src/lib.rs", "fn one() {}\n")], "base")?;
        commit(
            &dir,
            &[("src/lib.rs", "fn one() {}\nfn two() {}\n")],
            "head",
        )?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let pinned = gateway.pinned_range();
        let before = gateway.diff();
        commit(
            &dir,
            &[("src/lib.rs", "fn one() {}\nfn two() {}\nfn three() {}\n")],
            "late",
        )?;
        assert_eq!(
            gateway.pinned_range(),
            pinned,
            "the pin never moves after open"
        );
        assert_eq!(
            gateway.diff(),
            before,
            "the diff is frozen at the pin despite later commits"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn reads_from_a_subdirectory_still_pin_the_repo_root() -> Result<(), DtError> {
        let dir = scratch("subdir")?;
        commit(
            &dir,
            &[
                ("src/deep/inner.rs", "fn deep() {}\n"),
                ("root.rs", "fn root() {}\n"),
            ],
            "seed",
        )?;
        let start = dir.join("src").join("deep");
        let gateway = LocalGateway::open_at(&start, "main")?;
        assert_eq!(
            gateway.read_file_at_head("root.rs")?.content,
            "fn root() {}\n",
            "paths stay repo-relative from any starting directory"
        );
        assert_eq!(
            gateway.list_files_at_head()?,
            vec!["root.rs".to_owned(), "src/deep/inner.rs".to_owned()],
            "listing runs at the repo root, not the starting subtree"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn the_pin_is_the_merge_base_not_the_base_tip() -> Result<(), DtError> {
        let dir = scratch("diverge")?;
        let base = commit(&dir, &[("lib.rs", "fn base() {}\n")], "base")?;
        run_git(&dir, &["checkout", "-b", "topic"])?;
        let head = commit(&dir, &[("topic.rs", "fn topic() {}\n")], "topic work")?;
        run_git(&dir, &["checkout", "main"])?;
        commit(
            &dir,
            &[("main_only.rs", "fn main_only() {}\n")],
            "base moved on",
        )?;
        run_git(&dir, &["checkout", "topic"])?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let range = gateway.pinned_range();
        assert_eq!(
            range.base_sha, base,
            "the pin is the merge base, not the base tip"
        );
        assert_eq!(range.head_sha, head);
        let DiffSource::Unified(diff) = gateway.diff();
        assert!(
            diff.contains("topic.rs"),
            "the pinned diff carries head-side work"
        );
        assert!(
            !diff.contains("main_only.rs"),
            "post-merge-base base work stays outside the pin"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn reading_a_missing_file_errors_naming_the_path() -> Result<(), DtError> {
        let dir = scratch("missing")?;
        commit(&dir, &[("real.rs", "fn real() {}\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let outcome = gateway.read_file_at_head("ghost.rs");
        assert!(
            outcome.is_err_and(|err| err.to_string().contains("ghost.rs")),
            "the error names the missing path"
        );
        assert_eq!(
            gateway.read_file_at_head("real.rs")?.content,
            "fn real() {}\n"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn listing_names_tracked_files_at_head_only() -> Result<(), DtError> {
        let dir = scratch("list")?;
        commit(&dir, &[("a.rs", "one\n"), ("nested/b.rs", "two\n")], "seed")?;
        std::fs::write(dir.join("untracked.rs"), "three\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        assert_eq!(
            gateway.list_files_at_head()?,
            vec!["a.rs".to_owned(), "nested/b.rs".to_owned()],
            "listing is the sorted tracked set at head; worktree noise stays out"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn opening_outside_a_repository_errors() -> Result<(), DtError> {
        let dir =
            std::env::temp_dir().join(format!("difftrace-gateway-{}-norepo", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|err| DtError::Git(err.to_string()))?;
        let outside = LocalGateway::open_at(&dir, "main");
        assert!(
            outside.is_err_and(|err| err.to_string().contains("not a git repository")),
            "opening outside a repository is a git error"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        let repo = scratch("badbase")?;
        commit(&repo, &[("a.rs", "one\n")], "seed")?;
        let bad = LocalGateway::open_at(&repo, "no-such-branch");
        assert!(
            bad.is_err_and(|err| err.to_string().contains("merge-base")),
            "an unresolvable base names the failing operation"
        );
        let _unused = std::fs::remove_dir_all(&repo);
        Ok(())
    }

    #[test]
    fn empty_diffs_and_no_match_searches_read_as_empty() -> Result<(), DtError> {
        let dir = scratch("empty")?;
        commit(&dir, &[("a.rs", "fn a() {}\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let DiffSource::Unified(diff) = gateway.diff();
        assert!(
            diff.trim().is_empty(),
            "a head at the merge base reviews nothing"
        );
        assert!(
            gateway.search_at_head("zzz_no_such_token", 10)?.is_empty(),
            "no matches read as an empty search"
        );
        let bad = gateway.search_at_head("[unclosed", 10);
        assert!(
            bad.is_err_and(|err| err.to_string().contains("search pattern")),
            "a broken pattern is an error, never a silent clean search"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn search_results_are_sorted_and_capped() -> Result<(), DtError> {
        let dir = scratch("grep")?;
        commit(
            &dir,
            &[
                ("z.rs", "needle here\nplain\nneedle again\n"),
                ("a.rs", "needle one\nneedle two\n"),
                ("b.txt", "no match at all\n"),
            ],
            "seeds",
        )?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let hits = gateway.search_at_head("needle", 3)?;
        let summary = hits
            .iter()
            .map(|hit| format!("{}:{}", hit.file, hit.line))
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                "a.rs:1".to_owned(),
                "a.rs:2".to_owned(),
                "z.rs:1".to_owned()
            ],
            "hits sort by file then line and the cap drops the tail"
        );
        assert!(
            hits.first()
                .is_some_and(|hit| hit.text.contains("needle one")),
            "the hit carries the matching line's text"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn directory_paths_error_instead_of_reading_tree_listings() -> Result<(), DtError> {
        let dir = scratch("tree")?;
        commit(&dir, &[("src/a.rs", "fn a() {}\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        for path in ["", "src"] {
            let outcome = gateway.read_file_at_head(path);
            assert!(
                outcome.is_err_and(|err| err.to_string().contains(path)),
                "an empty or directory path is an error, not tree text read as content"
            );
        }
        assert_eq!(
            gateway.read_file_at_head("src/a.rs")?.content,
            "fn a() {}\n"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn hits_in_files_with_colons_in_their_names_survive_parsing() -> Result<(), DtError> {
        let dir = scratch("colon")?;
        commit(&dir, &[("we:ird.rs", "needle colon\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let hits = gateway.search_at_head("needle", 10)?;
        let summary = hits
            .iter()
            .map(|hit| format!("{}:{}", hit.file, hit.line))
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec!["we:ird.rs:1".to_owned()],
            "a colon in the file name does not silently drop the hit"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_digit_only_name_chunk_is_not_the_line_number() -> Result<(), DtError> {
        let dir = scratch("digit-name")?;
        commit(&dir, &[("x:007:y.rs", "needle digits\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let hits = gateway.search_at_head("needle", 10)?;
        let hit = hits
            .first()
            .ok_or_else(|| DtError::Git("the hit must survive parsing".to_owned()))?;
        assert_eq!(
            (hit.file.as_str(), hit.line, hit.text.as_str()),
            ("x:007:y.rs", 1, "needle digits"),
            "the anchor is the last number with content after it, never a name chunk"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn numeric_match_text_does_not_steal_the_anchor() -> Result<(), DtError> {
        let dir = scratch("digit-text")?;
        commit(&dir, &[("a.rs", "count\n42\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let hits = gateway.search_at_head("42", 10)?;
        let hit = hits
            .first()
            .ok_or_else(|| DtError::Git("the hit must parse".to_owned()))?;
        assert_eq!(
            (hit.file.as_str(), hit.line, hit.text.as_str()),
            ("a.rs", 2, "42"),
            "a digit-only match line is content, not the line number"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn digit_colon_match_text_keeps_the_real_line_anchor() -> Result<(), DtError> {
        let dir = scratch("digit-pair-text")?;
        commit(&dir, &[("clock.rs", "start\n10:30\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let hits = gateway.search_at_head("10:30", 10)?;
        let hit = hits
            .first()
            .ok_or_else(|| DtError::Git("the hit must parse".to_owned()))?;
        assert_eq!(
            (hit.file.as_str(), hit.line, hit.text.as_str()),
            ("clock.rs", 2, "10:30"),
            "a time-shaped match line is content: the anchor stays on the tracked file's real line"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_flag_shaped_base_fails_as_a_revision_not_as_a_git_mode() -> Result<(), DtError> {
        let dir = scratch("dash-base")?;
        commit(&dir, &[("a.rs", "one\n")], "seed")?;
        let outcome = LocalGateway::open_at(&dir, "--all");
        assert!(
            outcome
                .is_err_and(|err| err.to_string().contains("merge-base")
                    && err.to_string().contains("--all")),
            "a leading-dash base reaches git as a value and errors naming it"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_dash_leading_pattern_searches_instead_of_parsing_as_flags() -> Result<(), DtError> {
        let dir = scratch("dash-pattern")?;
        commit(&dir, &[("f.txt", "marker\n-needle here\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let hits = gateway.search_at_head("-needle", 10)?;
        assert_eq!(
            hits.first().map(|hit| (hit.file.as_str(), hit.line)),
            Some(("f.txt", 2)),
            "the pattern is passed as a value, so a dash-leading expression still matches"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_failed_listing_errors_instead_of_reading_empty() -> Result<(), DtError> {
        let dir = scratch("deadobject")?;
        commit(&dir, &[("a.rs", "one\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let sha = gateway.pinned_range().head_sha;
        let first = sha
            .get(..2)
            .ok_or_else(|| DtError::Git("short sha".to_owned()))?;
        let rest = sha
            .get(2..)
            .ok_or_else(|| DtError::Git("short sha".to_owned()))?;
        let object = dir.join(".git").join("objects").join(first).join(rest);
        std::fs::remove_file(&object).map_err(|err| DtError::Git(err.to_string()))?;
        assert!(
            gateway.list_files_at_head().is_err(),
            "a failed listing must error, never read as an empty repository"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
