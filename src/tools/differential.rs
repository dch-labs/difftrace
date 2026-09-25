//! Differential tool attribution: findings measured at both ends of
//! the pinned range, so repo debt renders as context and only what
//! the branch changed can block.

use std::collections::BTreeSet;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::contracts::{PinnedRange, ToolFinding};
use crate::error::DtError;
use crate::gateway::local::hermetic_git;

use super::ToolAdapter;
use super::rust::{CacheKey, Executor, ToolOutcome, run_cached};

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
    let base_ids: BTreeSet<(&str, &str, u32, &str)> = base
        .iter()
        .map(|finding| {
            (
                finding.tool.as_str(),
                finding.file.as_str(),
                finding.line,
                finding.message.as_str(),
            )
        })
        .collect();
    let mut new: Vec<ToolFinding> = Vec::new();
    let mut context: Vec<ToolFinding> = Vec::new();
    for finding in head {
        let identity = (
            finding.tool.as_str(),
            finding.file.as_str(),
            finding.line,
            finding.message.as_str(),
        );
        if base_ids.contains(&identity) {
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
    let head = run_side(
        adapter,
        targets,
        head_root,
        &range.head_sha,
        cache_dir,
        executor,
    );
    let base = run_side(
        adapter,
        targets,
        base_export,
        &range.base_sha,
        cache_dir,
        executor,
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
    export_tree(repo_root, &range.base_sha, &base_export)?;
    let mut combined = DifferentialOutcome {
        new: Vec::new(),
        context: Vec::new(),
        unavailable: Vec::new(),
    };
    for adapter in adapters {
        let outcome = run_differential(
            *adapter,
            targets,
            range,
            repo_root,
            &base_export,
            cache_dir,
            executor,
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
) -> ToolOutcome {
    let mut spec = adapter.command(targets);
    spec.cwd = Some(root.to_owned());
    let key = CacheKey {
        sha: sha.to_owned(),
        lock_hash: lock_digest(root),
    };
    run_cached(adapter, &spec, &key, cache_dir, executor)
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
        .args(["status", "--porcelain"])
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
    use crate::tools::rust::{ExecOutput, ToolRunError};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

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
