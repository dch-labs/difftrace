//! Fix-commit correlation, seed overrides, and repo resolution.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use crate::error::DtError;
use crate::extract::{Finding, Severity};

#[derive(Debug, Clone, serde::Serialize)]
pub struct GoldenFinding {
    pub run: String,
    pub sha: String,
    pub file: String,
    pub line: u64,
    pub title: String,
    pub severity: Severity,
    pub label: Label,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Label {
    TruePositive,
    FalsePositive,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct SeedEntry {
    pub file: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
}

#[must_use]
pub fn title_tokens(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect()
}

#[must_use]
pub fn titles_overlap_enough(a: &str, b: &str) -> bool {
    let left = title_tokens(a);
    let right = title_tokens(b);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let shared = left.intersection(&right).count();
    let total = left.union(&right).count();
    total > 0 && shared.saturating_mul(2) >= total
}

pub fn parse_seeds(text: &str) -> Result<Vec<SeedEntry>, DtError> {
    let mut seeds = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("- ") else {
            continue;
        };
        let (_, tail) = rest.split_once(" — ").ok_or_else(|| DtError::Parse {
            run: "seeds".to_owned(),
            message: format!("entry needs `<repo> @ <sha> — <path>:<line> — <title>`: {line}"),
        })?;
        let (anchor, title) = tail.split_once(" — ").ok_or_else(|| DtError::Parse {
            run: "seeds".to_owned(),
            message: format!("entry needs `<repo> @ <sha> — <path>:<line> — <title>`: {line}"),
        })?;
        let (file, _) = anchor
            .rsplit_once(':')
            .filter(|(_, number)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| DtError::Parse {
                run: "seeds".to_owned(),
                message: format!("anchor needs `<path>:<line>`: {line}"),
            })?;
        seeds.push(SeedEntry {
            file: file.to_owned(),
            title: title.trim().to_owned(),
        });
    }
    Ok(seeds)
}

pub struct Correlator {
    repos: Vec<Repo>,
    seeds: Vec<SeedEntry>,
    repo_cache: HashMap<String, Option<usize>>,
    log_cache: HashMap<(String, String), Vec<String>>,
}

impl Correlator {
    pub fn new(repos: Vec<Repo>, seeds: Vec<SeedEntry>) -> Self {
        Self {
            repos,
            seeds,
            repo_cache: HashMap::new(),
            log_cache: HashMap::new(),
        }
    }

    pub fn resolve(&mut self, sha: &str) -> Result<Option<usize>, DtError> {
        if let Some(hit) = self.repo_cache.get(sha) {
            return Ok(*hit);
        }
        let mut hit = None;
        for (idx, repo) in self.repos.iter().enumerate() {
            if git_ok(
                &repo.path,
                &["cat-file", "-e", &format!("{sha}^{{commit}}")],
            )? {
                hit = Some(idx);
                break;
            }
        }
        self.repo_cache.insert(sha.to_owned(), hit);
        Ok(hit)
    }

    pub fn repo(&self, idx: usize) -> Option<&Repo> {
        self.repos.get(idx)
    }

    pub fn label(
        &mut self,
        run: &str,
        sha: &str,
        repo: Option<usize>,
        findings: &[Finding],
    ) -> Result<Vec<GoldenFinding>, DtError> {
        let mut labeled = Vec::new();
        for finding in findings {
            let label = self.label_one(repo, sha, finding)?;
            labeled.push(GoldenFinding {
                run: run.to_owned(),
                sha: sha.to_owned(),
                file: finding.file.clone(),
                line: finding.line,
                title: finding.title.clone(),
                severity: finding.severity,
                label,
            });
        }
        Ok(labeled)
    }

    fn label_one(
        &mut self,
        repo: Option<usize>,
        sha: &str,
        finding: &Finding,
    ) -> Result<Label, DtError> {
        if self.seed_matches(finding) {
            return Ok(Label::FalsePositive);
        }
        let Some(idx) = repo else {
            return Ok(Label::Unknown);
        };
        if self.fix_commit_matches(idx, sha, finding)? {
            return Ok(Label::TruePositive);
        }
        Ok(Label::Unknown)
    }

    fn seed_matches(&self, finding: &Finding) -> bool {
        self.seeds.iter().any(|seed| {
            seed.file == finding.file && titles_overlap_enough(&finding.title, &seed.title)
        })
    }

    fn fix_commit_matches(
        &mut self,
        repo_idx: usize,
        sha: &str,
        finding: &Finding,
    ) -> Result<bool, DtError> {
        Ok(!self.candidates(repo_idx, sha, &finding.file)?.is_empty())
    }

    fn candidates(
        &mut self,
        repo_idx: usize,
        sha: &str,
        file: &str,
    ) -> Result<Vec<String>, DtError> {
        let key = (sha.to_owned(), file.to_owned());
        if let Some(hit) = self.log_cache.get(&key) {
            return Ok(hit.clone());
        }
        let repo = self.repos.get(repo_idx).ok_or_else(|| DtError::Git {
            repo: PathBuf::new(),
            message: "repo index out of range".to_owned(),
        })?;
        let out = git(
            &repo.path,
            &["log", "--format=%H", "--all", "--not", sha, "--", file],
        )?;
        let commits: Vec<String> = out
            .lines()
            .map(str::trim)
            .filter(|commit| !commit.is_empty())
            .filter(|commit| {
                git_ok(&repo.path, &["merge-base", "--is-ancestor", sha, commit])
                    .is_ok_and(|descendant| descendant)
            })
            .map(str::to_owned)
            .collect();
        Ok(self.log_cache.entry(key).or_insert(commits).clone())
    }
}

fn git(repo: &std::path::Path, args: &[&str]) -> Result<String, DtError> {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|err| DtError::Git {
            repo: repo.to_owned(),
            message: format!("spawn: {err}"),
        })?;
    if !output.status.success() {
        return Err(DtError::Git {
            repo: repo.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_ok(repo: &std::path::Path, args: &[&str]) -> Result<bool, DtError> {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|err| DtError::Git {
            repo: repo.to_owned(),
            message: format!("spawn: {err}"),
        })?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct ScratchRepo {
        dir: PathBuf,
    }

    impl Drop for ScratchRepo {
        fn drop(&mut self) {
            for entry in std::fs::read_dir(&self.dir).into_iter().flatten() {
                let Ok(entry) = entry else { continue };
                if entry.path().is_dir() {
                    drop_children(&entry.path());
                } else {
                    let _unused = std::fs::remove_file(entry.path());
                }
            }
            let _unused = std::fs::remove_dir(&self.dir);
        }
    }

    fn drop_children(dir: &Path) {
        for entry in std::fs::read_dir(dir).into_iter().flatten() {
            let Ok(entry) = entry else { continue };
            if entry.path().is_dir() {
                drop_children(&entry.path());
            } else {
                let _unused = std::fs::remove_file(entry.path());
            }
        }
        let _unused = std::fs::remove_dir(dir);
    }

    fn scratch_repo(tag: &str) -> Result<ScratchRepo, Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("dt-eval-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        git(&dir, &["init", "-b", "trunk"])?;
        git(&dir, &["config", "user.email", "eval@example.com"])?;
        git(&dir, &["config", "user.name", "dt eval"])?;
        let file = dir.join("src").join("anchor.rs");
        std::fs::create_dir_all(file.parent().ok_or("parent")?)?;
        std::fs::write(&file, numbered_lines(30))?;
        git(&dir, &["add", "."])?;
        git(&dir, &["commit", "-m", "base"])?;
        Ok(ScratchRepo { dir })
    }

    fn numbered_lines(count: usize) -> String {
        let mut out = String::new();
        for number in 1..=count {
            out.push_str("line ");
            out.push_str(&number.to_string());
            out.push('\n');
        }
        out
    }

    fn rewrite_line(
        repo: &ScratchRepo,
        line: usize,
        text: &str,
        message: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = repo.dir.join("src").join("anchor.rs");
        let content = std::fs::read_to_string(&file)?;
        let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
        let Some(slot) = lines.get_mut(line.saturating_sub(1)) else {
            return Err(format!("line {line} out of range").into());
        };
        *slot = text.to_owned();
        let mut joined = lines.join("\n");
        joined.push('\n');
        std::fs::write(&file, joined)?;
        git(&repo.dir, &["add", "."])?;
        git(&repo.dir, &["commit", "-m", message])?;
        Ok(())
    }

    fn finding(line: u64, title: &str) -> Finding {
        Finding {
            file: "src/anchor.rs".to_owned(),
            line,
            title: title.to_owned(),
            severity: Severity::Warning,
        }
    }

    fn head_sha(repo: &ScratchRepo) -> Result<String, Box<dyn std::error::Error>> {
        Ok(git(&repo.dir, &["rev-parse", "HEAD"])?.trim().to_owned())
    }

    fn base_sha(repo: &ScratchRepo) -> Result<String, Box<dyn std::error::Error>> {
        Ok(git(&repo.dir, &["rev-parse", "HEAD~1"])?.trim().to_owned())
    }

    fn correlate_one(
        repo: &ScratchRepo,
        name: &str,
        sha: &str,
        seeds: &str,
        line: u64,
        title: &str,
    ) -> Result<GoldenFinding, Box<dyn std::error::Error>> {
        let repos = vec![Repo {
            name: name.to_owned(),
            path: repo.dir.clone(),
        }];
        let mut correlator = Correlator::new(repos, parse_seeds(seeds)?);
        let idx = correlator.resolve(sha)?.ok_or("repo not resolved")?;
        let mut labeled = correlator.label("run.md", sha, Some(idx), &[finding(line, title)])?;
        labeled.pop().ok_or_else(|| "no labeled finding".into())
    }

    #[test]
    fn a_fix_commit_touching_the_file_marks_a_true_positive()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = scratch_repo("fix")?;
        rewrite_line(&repo, 26, "line 26 changed", "chore: tidy whitespace")?;
        let sha = base_sha(&repo)?;
        let labeled = correlate_one(&repo, "demo", &sha, "", 10, "off-by-one breaks the clamp")?;
        assert_eq!(
            labeled.label,
            Label::TruePositive,
            "subject wording and hunk distance carry no weight"
        );
        Ok(())
    }

    #[test]
    fn a_commit_touching_only_other_files_stays_unknown() -> Result<(), Box<dyn std::error::Error>>
    {
        let repo = scratch_repo("other-file")?;
        let other = repo.dir.join("src").join("other.rs");
        std::fs::write(&other, "unrelated\n")?;
        git(&repo.dir, &["add", "."])?;
        git(
            &repo.dir,
            &["commit", "-m", "fix: clamp the off-by-one elsewhere"],
        )?;
        let sha = base_sha(&repo)?;
        let labeled = correlate_one(&repo, "demo", &sha, "", 10, "off-by-one breaks the clamp")?;
        assert_eq!(labeled.label, Label::Unknown);
        Ok(())
    }

    #[test]
    fn a_seed_entry_forces_false_positive() -> Result<(), Box<dyn std::error::Error>> {
        let repo = scratch_repo("seed")?;
        rewrite_line(
            &repo,
            10,
            "line 10 fixed",
            "fix: clamp the off-by-one in the anchor walk",
        )?;
        let sha = base_sha(&repo)?;
        let seeds = "- demo @ 0000000000000000000000000000000000000000 — src/anchor.rs:10 — off-by-one breaks the clamp\n";
        let labeled = correlate_one(
            &repo,
            "demo",
            &sha,
            seeds,
            10,
            "off-by-one breaks the clamp",
        )?;
        assert_eq!(
            labeled.label,
            Label::FalsePositive,
            "the verified refutation outranks fix correlation"
        );
        Ok(())
    }

    #[test]
    fn an_unmatched_finding_stays_unknown() -> Result<(), Box<dyn std::error::Error>> {
        let repo = scratch_repo("unmatched")?;
        let sha = head_sha(&repo)?;
        let labeled = correlate_one(&repo, "demo", &sha, "", 10, "off-by-one breaks the clamp")?;
        assert_eq!(labeled.label, Label::Unknown);
        Ok(())
    }

    #[test]
    fn a_sha_resolves_to_the_repo_containing_it() -> Result<(), Box<dyn std::error::Error>> {
        let left = scratch_repo("resolve-left")?;
        let right = scratch_repo("resolve-right")?;
        let sha = head_sha(&left)?;
        let repos = vec![
            Repo {
                name: "left".to_owned(),
                path: left.dir.clone(),
            },
            Repo {
                name: "right".to_owned(),
                path: right.dir.clone(),
            },
        ];
        let mut correlator = Correlator::new(repos, Vec::new());
        let idx = correlator.resolve(&sha)?.ok_or("unresolved")?;
        assert_eq!(
            correlator.repo(idx).map(|repo| repo.name.as_str()),
            Some("left")
        );
        Ok(())
    }

    #[test]
    fn token_overlap_pins_the_identity_semantics() {
        assert!(titles_overlap_enough(
            "off-by-one breaks the clamp",
            "the clamp breaks with an off-by-one"
        ));
        assert!(!titles_overlap_enough(
            "off-by-one breaks the clamp",
            "unrelated wording entirely"
        ));
        assert!(!titles_overlap_enough("— …", "( …)"));
        assert_eq!(
            title_tokens("Clamp the 42 (…) — guard")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![
                "42".to_owned(),
                "clamp".to_owned(),
                "guard".to_owned(),
                "the".to_owned(),
            ],
            "tokens are lowercased words containing an alphanumeric char"
        );
    }
}
