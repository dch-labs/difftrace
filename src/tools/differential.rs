//! Differential tool attribution: findings measured at both ends of
//! the pinned range, so repo debt renders as context and only what
//! the branch changed can block.

use std::collections::BTreeMap;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::contracts::{PinnedRange, Region, ToolFinding};
use crate::error::DtError;
use crate::gateway::local::hermetic_git;
use crate::identity::LINE_DRIFT_TOLERANCE;

use super::rust::{CacheKey, Executor, ToolOutcome, run_cached};
use super::{CommandSpec, ToolAdapter};

pub const CONTEXT_CAP: usize = 20;

pub struct DifferentialOutcome {
    pub new: Vec<ToolFinding>,
    pub context: Vec<ToolFinding>,
    pub unavailable: Vec<String>,
}

#[must_use]
pub fn attribute(
    head: &[ToolFinding],
    base: &[ToolFinding],
) -> (Vec<ToolFinding>, Vec<ToolFinding>) {
    let mut base_spots: BTreeMap<(&str, &str, &str), Vec<u32>> = BTreeMap::new();
    for finding in base {
        base_spots
            .entry((
                finding.tool.as_str(),
                finding.file.as_str(),
                finding.message.as_str(),
            ))
            .or_default()
            .push(finding.line);
    }
    let mut new: Vec<ToolFinding> = Vec::new();
    let mut context: Vec<ToolFinding> = Vec::new();
    for finding in head {
        let shifted = base_spots
            .get(&(
                finding.tool.as_str(),
                finding.file.as_str(),
                finding.message.as_str(),
            ))
            .is_some_and(|lines| {
                lines.iter().any(|base_line| {
                    let drift = finding
                        .line
                        .max(*base_line)
                        .saturating_sub(finding.line.min(*base_line));
                    u64::from(drift) <= LINE_DRIFT_TOLERANCE
                })
            });
        if shifted {
            context.push(finding.clone());
        } else {
            new.push(finding.clone());
        }
    }
    context.sort_by(|a, b| (&a.file, a.line, &a.message).cmp(&(&b.file, b.line, &b.message)));
    context.truncate(CONTEXT_CAP);
    (new, context)
}

pub fn export_tree(repo_root: &Path, sha: &str, dest: &Path) -> Result<(), DtError> {
    std::fs::create_dir_all(dest)
        .map_err(|err| DtError::Git(format!("cannot create the base export dir: {err}")))?;
    let mut archive = hermetic_git(repo_root)
        .args(["archive", "--format=tar", sha])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| DtError::Git(format!("cannot run git archive: {err}")))?;
    let pipe = archive.stdout.take();
    let extract = std::process::Command::new("tar")
        .args(["-x", "-C"])
        .arg(dest)
        .stdin(pipe.map_or(Stdio::null(), Stdio::from))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| DtError::Git(format!("cannot run tar: {err}")))?;
    let extract_out = extract
        .wait_with_output()
        .map_err(|err| DtError::Git(format!("cannot wait for tar: {err}")))?;
    let archive_out = archive
        .wait_with_output()
        .map_err(|err| DtError::Git(format!("cannot wait for git archive: {err}")))?;
    if !archive_out.status.success() || !extract_out.status.success() {
        return Err(DtError::Git(format!(
            "the base export of {sha:?} failed: git archive said {:?}, tar said {:?}",
            String::from_utf8_lossy(&archive_out.stderr).trim(),
            String::from_utf8_lossy(&extract_out.stderr).trim()
        )));
    }
    Ok(())
}

pub fn run_differential(
    adapter: &dyn ToolAdapter,
    targets: &[PathBuf],
    range: &PinnedRange,
    head_root: &Path,
    base_export: &Path,
    cache_dir: &Path,
    executor: &dyn Executor,
) -> DifferentialOutcome {
    run_differential_bounded(
        adapter,
        targets,
        range,
        head_root,
        base_export,
        cache_dir,
        executor,
        None,
    )
}

pub fn run_differential_bounded(
    adapter: &dyn ToolAdapter,
    targets: &[PathBuf],
    range: &PinnedRange,
    head_root: &Path,
    base_export: &Path,
    cache_dir: &Path,
    executor: &dyn Executor,
    deadline: Option<Instant>,
) -> DifferentialOutcome {
    let head = run_side(
        adapter,
        targets,
        head_root,
        &range.head_sha,
        cache_dir,
        executor,
        "head",
        deadline,
    );
    let base = run_side(
        adapter,
        targets,
        base_export,
        &range.base_sha,
        cache_dir,
        executor,
        "base",
        deadline,
    );
    let (head, base) = match (head, base) {
        (ToolOutcome::Ran(head), ToolOutcome::Ran(base)) => (head, base),
        sides => {
            let unavailable = [sides.0, sides.1]
                .into_iter()
                .filter_map(|side| match side {
                    ToolOutcome::Unavailable(reason) => Some(reason),
                    ToolOutcome::Ran(_) => None,
                })
                .collect();
            return DifferentialOutcome {
                new: Vec::new(),
                context: Vec::new(),
                unavailable,
            };
        }
    };
    let (new, context) = attribute(&head, &base);
    DifferentialOutcome {
        new,
        context,
        unavailable: Vec::new(),
    }
}

pub fn run_all(
    adapters: &[&dyn ToolAdapter],
    targets: &[PathBuf],
    range: &PinnedRange,
    repo_root: &Path,
    cache_dir: &Path,
    executor: &dyn Executor,
) -> Result<DifferentialOutcome, DtError> {
    run_all_bounded(
        adapters, targets, range, repo_root, cache_dir, executor, None,
    )
}

pub fn run_all_bounded(
    adapters: &[&dyn ToolAdapter],
    targets: &[PathBuf],
    range: &PinnedRange,
    repo_root: &Path,
    cache_dir: &Path,
    executor: &dyn Executor,
    deadline: Option<Instant>,
) -> Result<DifferentialOutcome, DtError> {
    if !head_tree_is_clean(repo_root)? {
        let unavailable = adapters
            .iter()
            .map(|adapter| {
                format!(
                    "{} head side: the working tree differs from {}; commit or stash before differential attribution",
                    adapter.name(),
                    range.head_sha
                )
            })
            .collect();
        return Ok(DifferentialOutcome {
            new: Vec::new(),
            context: Vec::new(),
            unavailable,
        });
    }
    let base_export = std::env::temp_dir().join(format!("difftrace-base-{}", unique_suffix()));
    if let Err(err) = export_tree(repo_root, &range.base_sha, &base_export) {
        let unavailable = adapters
            .iter()
            .map(|adapter| {
                format!(
                    "{} base side: the base export failed: {err}",
                    adapter.name()
                )
            })
            .collect();
        return Ok(DifferentialOutcome {
            new: Vec::new(),
            context: Vec::new(),
            unavailable,
        });
    }
    let mut combined = DifferentialOutcome {
        new: Vec::new(),
        context: Vec::new(),
        unavailable: Vec::new(),
    };
    for adapter in adapters {
        let outcome = run_differential_bounded(
            *adapter,
            targets,
            range,
            repo_root,
            &base_export,
            cache_dir,
            executor,
            deadline,
        );
        combined.new.extend(outcome.new);
        combined.context.extend(outcome.context);
        combined.unavailable.extend(outcome.unavailable);
    }
    let _unused = std::fs::remove_dir_all(&base_export);
    Ok(combined)
}

fn run_side(
    adapter: &dyn ToolAdapter,
    targets: &[PathBuf],
    root: &Path,
    sha: &str,
    cache_dir: &Path,
    executor: &dyn Executor,
    side: &str,
    deadline: Option<Instant>,
) -> ToolOutcome {
    let mut spec = adapter.command(targets);
    spec.cwd = Some(root.to_owned());
    let Some(share) = side_share(spec.timeout_secs, deadline, Instant::now()) else {
        return ToolOutcome::Unavailable(format!(
            "{} {side} side never ran: the shared tool-layer wall budget is spent",
            adapter.name()
        ));
    };
    spec.timeout_secs = share;
    let key = CacheKey::new(sha, &lock_digest(root), &spec);
    let scoped = RelativizingAdapter {
        inner: adapter,
        root,
    };
    run_cached(&scoped, &spec, &key, cache_dir, executor)
}

struct RelativizingAdapter<'a> {
    inner: &'a dyn ToolAdapter,
    root: &'a Path,
}

impl ToolAdapter for RelativizingAdapter<'_> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn should_run(&self, changed: &[Region]) -> bool {
        self.inner.should_run(changed)
    }

    fn command(&self, targets: &[PathBuf]) -> CommandSpec {
        self.inner.command(targets)
    }

    fn parse(&self, output: &str) -> Vec<ToolFinding> {
        relativize_findings(self.inner.parse(output), self.root)
    }
}

fn relativize_findings(findings: Vec<ToolFinding>, root: &Path) -> Vec<ToolFinding> {
    findings
        .into_iter()
        .map(|mut finding| {
            if let Ok(relative) = std::path::Path::new(&finding.file).strip_prefix(root) {
                finding.file = relative.to_string_lossy().into_owned();
            }
            finding
        })
        .collect()
}

#[must_use]
fn side_share(timeout_secs: u64, deadline: Option<Instant>, now: Instant) -> Option<u64> {
    let Some(deadline) = deadline else {
        return Some(timeout_secs);
    };
    let remaining_secs = deadline.saturating_duration_since(now).as_secs();
    if remaining_secs == 0 {
        return None;
    }
    Some(timeout_secs.min(remaining_secs))
}

fn lock_digest(root: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return "no-lock".to_owned();
    };
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(text.as_bytes());
    format!("{:016x}", hasher.finish())
}

fn head_tree_is_clean(repo_root: &Path) -> Result<bool, DtError> {
    let output = hermetic_git(repo_root)
        .args(["status", "--porcelain", "-uno"])
        .output()
        .map_err(|err| DtError::Git(format!("cannot run git status: {err}")))?;
    if !output.status.success() {
        return Err(DtError::Git(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout.is_empty())
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            since
                .as_secs()
                .wrapping_mul(1_000_000_000)
                .wrapping_add(u64::from(since.subsec_nanos()))
        });
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExternalTool;
    use crate::contracts::Severity;
    use crate::gateway::PrGateway;
    use crate::gateway::local::LocalGateway;
    use crate::tools::CommandSpec;
    use crate::tools::ExternalToolAdapter;
    use crate::tools::rust::{ExecOutput, ProcessExecutor, ToolRunError};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::time::Duration;

    fn finding(tool: &str, file: &str, line: u32, message: &str) -> ToolFinding {
        ToolFinding {
            tool: tool.to_owned(),
            file: file.to_owned(),
            line,
            severity: Severity::Warning,
            message: message.to_owned(),
        }
    }

    #[test]
    fn a_warning_present_at_base_is_context_not_a_blocker() {
        let shared = finding("clippy", "src/a.rs", 2, "unused variable");
        let escalated = ToolFinding {
            severity: Severity::Critical,
            ..shared.clone()
        };
        let (new, context) = attribute(&[escalated], &[shared]);
        assert!(
            new.is_empty() && context.len() == 1,
            "the same finding at both shas is context, whatever severity it renders at head"
        );
    }

    #[test]
    fn a_new_warning_at_head_blocks() {
        let head = finding("clippy", "src/a.rs", 2, "unused variable");
        let other_file = finding("clippy", "src/b.rs", 1, "unused variable");
        let (new, context) = attribute(&[head], &[other_file]);
        assert!(
            new.len() == 1 && context.is_empty(),
            "a head finding with no base twin is new and may block"
        );
    }

    #[test]
    fn a_finding_shifted_by_inserted_lines_stays_repo_debt() {
        let debt = finding("clippy", "src/a.rs", 120, "unused variable: x");
        let shifted = finding("clippy", "src/a.rs", 124, "unused variable: x");
        let far = finding("clippy", "src/a.rs", 131, "unused variable: x");
        let (new, context) = attribute(&[shifted, far], &[debt]);
        assert!(
            new.len() == 1 && context.len() == 1,
            "drift within tolerance is context; beyond it the finding is the branch's"
        );
        assert_eq!(context.first().map(|entry| entry.line), Some(124));
        assert_eq!(new.first().map(|entry| entry.line), Some(131));
    }

    #[test]
    fn an_unset_layer_deadline_leaves_each_side_timeout_alone() {
        let now = Instant::now();
        assert_eq!(
            side_share(30, None, now),
            Some(30),
            "no layer deadline means the per-tool budget stands as-is"
        );
    }

    #[test]
    fn a_layer_deadline_clamps_the_side_to_what_remains() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(10);
        assert_eq!(
            side_share(30, Some(deadline), now),
            Some(10),
            "the shared wall caps each side at the remainder, not at its own budget"
        );
    }

    #[test]
    fn a_spent_layer_deadline_skips_the_side() {
        let now = Instant::now();
        assert_eq!(
            side_share(30, Some(now), now),
            None,
            "a deadline already reached means the side never runs"
        );
        let sub_second = now + Duration::from_millis(500);
        assert_eq!(
            side_share(30, Some(sub_second), now),
            None,
            "a sub-second remainder cannot fund a side and must not become a zero timeout"
        );
    }

    #[test]
    fn a_failed_base_export_degrades_every_adapter_to_unavailable() -> Result<(), DtError> {
        let tool = diag_tool("eslint")?;
        let dir = scratch("export-degraded")?;
        let _head = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        let range = PinnedRange {
            base_sha: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
            head_sha: "head".to_owned(),
        };
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&cache_dir).map_err(|err| DtError::Git(err.to_string()))?;
        let outcome = run_all(
            &[&tool],
            &[PathBuf::from("src/a.js")],
            &range,
            &dir,
            &cache_dir,
            &SequenceExecutor::new(Vec::new()),
        )?;
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            outcome.new.is_empty() && outcome.context.is_empty(),
            "a failed export attributes nothing"
        );
        assert!(
            outcome
                .unavailable
                .first()
                .is_some_and(|reason| reason.contains("eslint base side")
                    && reason.contains("base export failed")),
            "the layer degrades with the export cause per adapter: {:?}",
            outcome.unavailable
        );
        Ok(())
    }

    #[test]
    fn a_head_side_that_spends_the_layer_budget_skips_the_base_side() -> Result<(), DtError> {
        let tool = ExternalToolAdapter::from_config(&ExternalTool {
            name: "sleepy".to_owned(),
            command: "sleep 2".to_owned(),
            format: "diag".to_owned(),
            run_on: "**".to_owned(),
            budget_secs: 30,
        })?;
        let dir = scratch("shared-budget")?;
        let base = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.js", "one\ntwo\n")], "head")?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&cache_dir).map_err(|err| DtError::Git(err.to_string()))?;
        let executor = ProcessExecutor;
        let deadline = Instant::now().checked_add(Duration::from_secs(1));
        let started = Instant::now();
        let outcome = run_all_bounded(
            &[&tool],
            &[PathBuf::from("src/a.js")],
            &range,
            &dir,
            &cache_dir,
            &executor,
            deadline,
        )?;
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the base side never sleeps its full two seconds"
        );
        assert!(
            outcome.new.is_empty() && outcome.context.is_empty(),
            "an unattributable layer still finds nothing"
        );
        assert!(
            outcome
                .unavailable
                .iter()
                .any(|reason| reason.contains("shared tool-layer wall budget")),
            "the skipped side names the shared budget: {:?}",
            outcome.unavailable
        );
        Ok(())
    }

    #[test]
    fn untracked_files_are_reviewable_state_and_keep_the_tool_layer_alive() -> Result<(), DtError> {
        let dir = scratch("untracked-clean")?;
        let _base = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        std::fs::write(dir.join("notes.txt"), "scratch\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        assert!(
            head_tree_is_clean(&dir)?,
            "untracked files are the normal, reviewable state of a tree"
        );
        std::fs::write(dir.join("src/a.js"), "one\nchanged\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        assert!(
            !head_tree_is_clean(&dir)?,
            "a modified tracked file still fails the head side closed"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_changed_finding_counts_as_new() {
        let head = finding("clippy", "src/a.rs", 2, "the message drifted");
        let base = finding("clippy", "src/a.rs", 2, "the original message");
        let (new, context) = attribute(&[head], &[base]);
        assert!(
            new.len() == 1 && context.is_empty(),
            "a changed message at the same spot is a changed finding"
        );
    }

    #[test]
    fn the_context_list_is_capped_and_sorted() {
        let head: Vec<ToolFinding> = (0..25)
            .map(|index| finding("clippy", &format!("src/{:03}.rs", 24 - index), 1, "debt"))
            .collect();
        let (new, context) = attribute(&head, &head);
        assert!(new.is_empty());
        assert_eq!(
            context.len(),
            CONTEXT_CAP,
            "context never floods the render"
        );
        assert!(
            context.iter().is_sorted_by(|a, b| a.file <= b.file),
            "the kept context is the sorted head of the debt"
        );
        assert!(
            context
                .first()
                .is_some_and(|finding| finding.file == "src/000.rs"),
            "the cap keeps the lowest-sorted entries"
        );
    }

    fn scratch(name: &str) -> Result<PathBuf, DtError> {
        let dir = std::env::temp_dir().join(format!(
            "difftrace-differential-{}-{name}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|err| DtError::Git(err.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        Ok(dir)
    }

    fn git(dir: &Path, args: &[&str]) -> Result<(), DtError> {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .map_err(|err| DtError::Git(err.to_string()))?;
        if !output.status.success() {
            return Err(DtError::Git(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(())
    }

    fn commit(dir: &Path, files: &[(&str, &str)], message: &str) -> Result<String, DtError> {
        for (path, body) in files {
            let file = dir.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|err| DtError::Git(err.to_string()))?;
            }
            std::fs::write(&file, body).map_err(|err| DtError::Git(err.to_string()))?;
        }
        git(dir, &["add", "-A"])?;
        git(
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
        let sha = std::process::Command::new("git")
            .current_dir(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|err| DtError::Git(err.to_string()))?;
        Ok(String::from_utf8_lossy(&sha.stdout).trim().to_owned())
    }

    #[test]
    fn the_base_export_materializes_the_pinned_tree_without_touching_the_repo()
    -> Result<(), DtError> {
        let dir = scratch("export")?;
        let base = commit(
            &dir,
            &[
                ("src/a.rs", "fn base() {}\n"),
                ("Cargo.lock", "base lock\n"),
            ],
            "base",
        )?;
        let head = commit(
            &dir,
            &[
                ("src/a.rs", "fn base() {}\nfn head() {}\n"),
                ("src/b.rs", "new\n"),
            ],
            "head",
        )?;
        let base_export = dir.join("base-export");
        export_tree(&dir, &base, &base_export)?;
        assert_eq!(
            std::fs::read_to_string(base_export.join("src/a.rs"))
                .map_err(|err| DtError::Git(err.to_string()))?,
            "fn base() {}\n",
            "the export is the base tree, not the working tree"
        );
        assert!(
            !base_export.join("src/b.rs").exists(),
            "head-only files stay out of the base export"
        );
        let head_export = dir.join("head-export");
        export_tree(&dir, &head, &head_export)?;
        assert!(head_export.join("src/b.rs").exists());
        assert!(
            !dir.join(".git").join("worktrees").exists(),
            "the export never writes worktree metadata into the repository"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    struct SequenceExecutor {
        calls: Cell<usize>,
        cwds: RefCell<Vec<PathBuf>>,
        results: RefCell<VecDeque<Result<ExecOutput, ToolRunError>>>,
    }

    impl SequenceExecutor {
        fn new(results: Vec<Result<ExecOutput, ToolRunError>>) -> Self {
            Self {
                calls: Cell::new(0),
                cwds: RefCell::new(Vec::new()),
                results: RefCell::new(results.into()),
            }
        }
    }

    impl Executor for SequenceExecutor {
        fn run(&self, spec: &CommandSpec) -> Result<ExecOutput, ToolRunError> {
            self.calls.set(self.calls.get().saturating_add(1));
            self.cwds
                .borrow_mut()
                .extend(spec.cwd.iter().map(PathBuf::from));
            self.results.borrow_mut().pop_front().unwrap_or_else(|| {
                Ok(ExecOutput {
                    text: String::new(),
                    code: Some(0),
                })
            })
        }
    }

    fn diag_tool(name: &str) -> Result<ExternalToolAdapter, DtError> {
        ExternalToolAdapter::from_config(&ExternalTool {
            name: name.to_owned(),
            command: format!("{name} \"{{files}}\""),
            format: "diag".to_owned(),
            run_on: "**".to_owned(),
            budget_secs: 60,
        })
    }

    #[test]
    fn absolute_tool_paths_relativize_so_both_sides_dedupe() -> Result<(), DtError> {
        let tool = diag_tool("rustfmt").map_err(|err| DtError::Git(err.to_string()))?;
        let dir = scratch("relativize")?;
        std::fs::write(dir.join("Cargo.lock"), "lock\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let base = commit(&dir, &[("src/a.rs", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.rs", "one\ntwo\n")], "head")?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let head_export = dir.join("head-side");
        let base_export = dir.join("base-side");
        std::fs::create_dir_all(&head_export).map_err(|err| DtError::Git(err.to_string()))?;
        std::fs::create_dir_all(&base_export).map_err(|err| DtError::Git(err.to_string()))?;
        std::fs::write(base_export.join("Cargo.lock"), "lock\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let executor = SequenceExecutor::new(vec![
            Ok(ExecOutput {
                text: format!(
                    "{}:1: error: debt\n",
                    head_export.join("src/a.rs").display()
                ),
                code: Some(0),
            }),
            Ok(ExecOutput {
                text: format!(
                    "{}:1: error: debt\n",
                    base_export.join("src/a.rs").display()
                ),
                code: Some(0),
            }),
        ]);
        let outcome = run_differential(
            &tool,
            &[PathBuf::from("src/a.rs")],
            &range,
            &head_export,
            &base_export,
            &dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            outcome.new.is_empty(),
            "the same finding under two absolute roots is shared debt, never branch-new: {:?}",
            outcome.new
        );
        assert_eq!(
            outcome.context.first().map(|finding| finding.file.clone()),
            Some("src/a.rs".to_owned()),
            "findings render repo-relative instead of absolute temp paths: {:?}",
            outcome.context
        );
        Ok(())
    }

    #[test]
    fn cached_base_findings_relativize_across_export_directories() -> Result<(), DtError> {
        let tool = diag_tool("rustfmt").map_err(|err| DtError::Git(err.to_string()))?;
        let dir = scratch("cache-reuse")?;
        std::fs::write(dir.join("Cargo.lock"), "lock\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let base = commit(&dir, &[("src/a.rs", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.rs", "one\ntwo\n")], "head")?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let head_export = dir.join("head-side");
        let first_export = dir.join("base-side-a");
        let second_export = dir.join("base-side-b");
        for export in [&head_export, &first_export, &second_export] {
            std::fs::create_dir_all(export).map_err(|err| DtError::Git(err.to_string()))?;
            std::fs::write(export.join("Cargo.lock"), "lock\n")
                .map_err(|err| DtError::Git(err.to_string()))?;
        }
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&cache_dir).map_err(|err| DtError::Git(err.to_string()))?;
        let executor = SequenceExecutor::new(vec![
            Ok(ExecOutput {
                text: format!(
                    "{}:1: error: debt\n",
                    head_export.join("src/a.rs").display()
                ),
                code: Some(0),
            }),
            Ok(ExecOutput {
                text: format!(
                    "{}:1: error: debt\n",
                    first_export.join("src/a.rs").display()
                ),
                code: Some(0),
            }),
        ]);
        let first = run_differential(
            &tool,
            &[PathBuf::from("src/a.rs")],
            &range,
            &head_export,
            &first_export,
            &dir,
            &executor,
        );
        assert!(
            first.new.is_empty(),
            "the fresh run dedupes the shared debt: {:?}",
            first.new
        );
        let second = run_differential(
            &tool,
            &[PathBuf::from("src/a.rs")],
            &range,
            &head_export,
            &second_export,
            &dir,
            &executor,
        );
        assert_eq!(
            executor.calls.get(),
            2,
            "the second run is a pure cache hit — no side re-executes"
        );
        assert!(
            second.new.is_empty(),
            "a cache hit from another export root still dedupes as shared debt: {:?}",
            second.new
        );
        assert!(
            second
                .context
                .iter()
                .all(|finding| !finding.file.starts_with(dir.display().to_string().as_str())),
            "no cached finding leaks an absolute export path: {:?}",
            second.context
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn run_differential_fails_closed_when_either_side_cannot_run() -> Result<(), DtError> {
        let tool = diag_tool("eslint")?;
        let dir = scratch("fail-closed")?;
        let base = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.js", "one\ntwo\n")], "head")?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let executor = SequenceExecutor::new(vec![
            Ok(ExecOutput {
                text: "src/a.js:1: error: fresh\n".to_owned(),
                code: Some(0),
            }),
            Err(ToolRunError::Launch("cargo not found".to_owned())),
        ]);
        let outcome = run_differential(
            &tool,
            &[PathBuf::from("src/a.js")],
            &range,
            &dir,
            &dir,
            &dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            outcome.new.is_empty() && outcome.context.is_empty(),
            "an unrunnable base must not launder head findings as new"
        );
        assert_eq!(
            outcome.unavailable.len(),
            1,
            "the tool reports unavailable instead: {:?}",
            outcome.unavailable
        );
        Ok(())
    }

    #[test]
    fn a_crashing_base_side_launders_no_head_findings_as_new() -> Result<(), DtError> {
        let tool = diag_tool("eslint")?;
        let dir = scratch("crash-base")?;
        std::fs::write(dir.join("Cargo.lock"), "lock\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let base = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.js", "one\ntwo\n")], "head")?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let head_export = dir.join("head-side");
        let base_export = dir.join("base-side");
        std::fs::create_dir_all(&head_export).map_err(|err| DtError::Git(err.to_string()))?;
        std::fs::create_dir_all(&base_export).map_err(|err| DtError::Git(err.to_string()))?;
        let executor = SequenceExecutor::new(vec![
            Ok(ExecOutput {
                text: "src/a.js:1: error: debt\n".to_owned(),
                code: Some(0),
            }),
            Ok(ExecOutput {
                text: String::new(),
                code: Some(1),
            }),
        ]);
        let outcome = run_differential(
            &tool,
            &[PathBuf::from("src/a.js")],
            &range,
            &head_export,
            &base_export,
            &dir,
            &executor,
        );
        assert!(
            outcome.new.is_empty() && outcome.context.is_empty(),
            "a base that crashed with empty stdout never launders head findings as new: {:?}",
            outcome.new
        );
        assert_eq!(
            outcome.unavailable.len(),
            1,
            "the crash surfaces as unavailable: {:?}",
            outcome.unavailable
        );
        assert!(
            outcome
                .unavailable
                .first()
                .is_some_and(|reason| reason.contains("exited with 1")),
            "the reason names the crash exit: {:?}",
            outcome.unavailable
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_dirty_head_tree_fails_the_head_side_closed_without_running_or_caching()
    -> Result<(), DtError> {
        let tool = diag_tool("eslint")?;
        let dir = scratch("dirty")?;
        let base = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.js", "one\ntwo\n")], "head")?;
        std::fs::write(dir.join("src/a.js"), "uncommitted\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&cache_dir).map_err(|err| DtError::Git(err.to_string()))?;
        let executor = SequenceExecutor::new(Vec::new());
        let outcome = run_all(
            &[&tool],
            &[PathBuf::from("src/a.js")],
            &range,
            &dir,
            &cache_dir,
            &executor,
        )?;
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            outcome.new.is_empty() && outcome.context.is_empty(),
            "nothing is attributed from a tree the head sha cannot vouch for"
        );
        assert_eq!(
            outcome.unavailable.len(),
            1,
            "the dirty tree names itself per adapter: {:?}",
            outcome.unavailable
        );
        assert!(
            outcome
                .unavailable
                .first()
                .is_some_and(|reason| reason.contains("working tree")),
            "the reason tells the operator what to do: {:?}",
            outcome.unavailable
        );
        assert_eq!(
            executor.calls.get(),
            0,
            "a dirty head side refuses to run the tool at all"
        );
        assert!(
            std::fs::read_dir(&cache_dir).map_or(true, |entries| entries.count() == 0),
            "a refused run caches nothing"
        );
        Ok(())
    }

    #[test]
    fn run_differential_attributes_findings_across_both_sides() -> Result<(), DtError> {
        let tool = diag_tool("eslint")?;
        let dir = scratch("attribute")?;
        std::fs::write(dir.join("Cargo.lock"), "lock\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let base = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.js", "one\ntwo\n")], "head")?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let head_export = dir.join("head-side");
        let base_export = dir.join("base-side");
        std::fs::create_dir_all(&head_export).map_err(|err| DtError::Git(err.to_string()))?;
        std::fs::create_dir_all(&base_export).map_err(|err| DtError::Git(err.to_string()))?;
        std::fs::write(base_export.join("Cargo.lock"), "lock\n")
            .map_err(|err| DtError::Git(err.to_string()))?;
        let executor = SequenceExecutor::new(vec![
            Ok(ExecOutput {
                text: "src/a.js:1: error: debt\nsrc/a.js:2: error: fresh\n".to_owned(),
                code: Some(0),
            }),
            Ok(ExecOutput {
                text: "src/a.js:1: error: debt\n".to_owned(),
                code: Some(0),
            }),
        ]);
        let outcome = run_differential(
            &tool,
            &[PathBuf::from("src/a.js")],
            &range,
            &head_export,
            &base_export,
            &dir,
            &executor,
        );
        assert_eq!(outcome.unavailable.len(), 0);
        assert_eq!(
            outcome
                .new
                .iter()
                .map(|finding| (finding.file.as_str(), finding.line))
                .collect::<Vec<_>>(),
            vec![("src/a.js", 2)],
            "only the head-only finding is new"
        );
        assert_eq!(outcome.context.len(), 1, "the shared debt is context");
        let cwds = executor.cwds.borrow().clone();
        assert_eq!(
            cwds,
            vec![head_export, base_export],
            "the head runs at head's root, the base run at the export, in that order"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn run_all_exports_base_once_and_aggregates_every_adapter() -> Result<(), DtError> {
        let first = diag_tool("eslint")?;
        let second = diag_tool("shellcheck")?;
        let dir = scratch("run-all")?;
        let base = commit(&dir, &[("src/a.js", "one\n")], "base")?;
        let head = commit(&dir, &[("src/a.js", "one\ntwo\n")], "head")?;
        let range = PinnedRange {
            base_sha: base,
            head_sha: head,
        };
        let executor = SequenceExecutor::new(vec![
            Ok(ExecOutput {
                text: "src/a.js:2: error: fresh\n".to_owned(),
                code: Some(0),
            }),
            Ok(ExecOutput {
                text: String::new(),
                code: Some(0),
            }),
            Ok(ExecOutput {
                text: String::new(),
                code: Some(0),
            }),
            Err(ToolRunError::Timeout),
        ]);
        let outcome = run_all(
            &[&first, &second],
            &[PathBuf::from("src/a.js")],
            &range,
            &dir,
            &dir,
            &executor,
        )?;
        let _unused = std::fs::remove_dir_all(&dir);
        assert_eq!(outcome.new.len(), 1, "eslint's head finding survives");
        assert_eq!(
            outcome.unavailable.len(),
            1,
            "shellcheck times out at base and fails closed: {:?}",
            outcome.unavailable
        );
        assert_eq!(executor.calls.get(), 4, "each adapter ran both sides");
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn check_runs_without_the_hosted_gateway_errors_naming_it() -> Result<(), DtError> {
        let dir = scratch("check-runs")?;
        commit(&dir, &[("a.rs", "one\n")], "seed")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let sha = gateway.pinned_range().head_sha;
        let outcome = gateway.check_runs(&sha);
        let _unused = std::fs::remove_dir_all(&dir);
        let err = outcome
            .err()
            .unwrap_or_else(|| DtError::Gateway(String::new()));
        assert!(
            err.to_string().contains("hosted"),
            "the local gateway has no check-run mapping: {err}"
        );
        Ok(())
    }
}
