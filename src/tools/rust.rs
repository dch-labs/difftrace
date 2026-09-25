//! Rust built-in adapters and the run/budget/cache seam beneath them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::contracts::{Region, Severity, ToolFinding};
use crate::error::DtError;

use super::cargo_json;
use super::crates::WorkspaceMap;
use super::{CommandSpec, ToolAdapter, normalize_output};

const WORKSPACE_TARGET: &str = "(workspace)";

pub struct BuiltinAdapter {
    kind: BuiltinKind,
    map: WorkspaceMap,
    budget_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltinKind {
    Fmt,
    Clippy,
    Check,
    Audit,
}

impl BuiltinKind {
    fn name(self) -> &'static str {
        match self {
            Self::Fmt => "cargo-fmt",
            Self::Clippy => "cargo-clippy",
            Self::Check => "cargo-check",
            Self::Audit => "cargo-audit",
        }
    }
}

impl ToolAdapter for BuiltinAdapter {
    fn name(&self) -> &str {
        self.kind.name()
    }

    fn should_run(&self, changed: &[Region]) -> bool {
        match self.kind {
            BuiltinKind::Audit => true,
            BuiltinKind::Fmt => region_files(changed).iter().any(|path| {
                Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
            }),
            BuiltinKind::Clippy | BuiltinKind::Check => {
                !self.map.crates_for_paths(&region_files(changed)).is_empty()
            }
        }
    }

    fn command(&self, targets: &[PathBuf]) -> CommandSpec {
        let mut argv: Vec<String> = match self.kind {
            BuiltinKind::Fmt => vec!["cargo", "fmt", "--check"],
            BuiltinKind::Clippy => vec!["cargo", "clippy", "--all-targets"],
            BuiltinKind::Check => vec!["cargo", "check"],
            BuiltinKind::Audit => vec!["cargo", "audit", "--json"],
        }
        .iter()
        .map(|word| (*word).to_owned())
        .collect();
        if matches!(self.kind, BuiltinKind::Clippy | BuiltinKind::Check) {
            let paths = targets
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            for name in self.map.crates_for_paths(&paths) {
                argv.push("-p".to_owned());
                argv.push(name);
            }
            argv.push("--message-format=json".to_owned());
        }
        if self.kind == BuiltinKind::Clippy {
            argv.push("--".to_owned());
            argv.push("-D".to_owned());
            argv.push("warnings".to_owned());
        }
        CommandSpec {
            argv,
            cwd: None,
            timeout_secs: self.budget_secs,
        }
    }

    fn parse(&self, output: &str) -> Vec<ToolFinding> {
        let findings = match self.kind {
            BuiltinKind::Fmt => fmt_findings(output, self.kind.name()),
            BuiltinKind::Clippy | BuiltinKind::Check => cargo_json::parse(output, self.kind.name()),
            BuiltinKind::Audit => audit_findings(output, self.kind.name()),
        };
        normalize_output(findings)
    }
}

pub fn builtin_adapters(
    map: &WorkspaceMap,
    enabled: &[String],
    budget_secs: u64,
) -> Result<Vec<BuiltinAdapter>, DtError> {
    let mut adapters: Vec<BuiltinAdapter> = Vec::new();
    for name in enabled {
        let kind = match name.trim().to_lowercase().as_str() {
            "fmt" => BuiltinKind::Fmt,
            "clippy" => BuiltinKind::Clippy,
            "check" => BuiltinKind::Check,
            "audit" => BuiltinKind::Audit,
            other => {
                return Err(DtError::Config(format!(
                    "unknown built-in tool {other:?}; expected fmt, clippy, check, or audit"
                )));
            }
        };
        if adapters.iter().any(|existing| existing.kind == kind) {
            return Err(DtError::Config(format!(
                "duplicate built-in tool {name:?}; one tool label must mean one tool"
            )));
        }
        adapters.push(BuiltinAdapter {
            kind,
            map: map.clone(),
            budget_secs,
        });
    }
    Ok(adapters)
}

fn region_files(changed: &[Region]) -> Vec<String> {
    changed.iter().map(|region| region.file.clone()).collect()
}

fn fmt_findings(output: &str, tool: &str) -> Vec<ToolFinding> {
    let mut findings: Vec<ToolFinding> = Vec::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("Diff in ") else {
            continue;
        };
        let Some((path, tail)) = rest.split_once(':') else {
            continue;
        };
        let number = tail.split_once(':').map_or(tail, |(number, _)| number);
        let Ok(number) = number.trim().parse::<u32>() else {
            continue;
        };
        if number == 0 {
            continue;
        }
        if !findings.iter().any(|finding| finding.file == path) {
            findings.push(ToolFinding {
                tool: tool.to_owned(),
                file: path.to_owned(),
                line: number,
                severity: Severity::Suggestion,
                message: "formatting differs from rustfmt".to_owned(),
            });
        }
    }
    findings
}

fn audit_findings(output: &str, tool: &str) -> Vec<ToolFinding> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let Some(list) = value
        .get("vulnerabilities")
        .and_then(|found| found.get("list"))
        .and_then(|list| list.as_array())
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|entry| {
            let advisory = entry.get("advisory")?;
            let id = advisory.get("id").and_then(|id| id.as_str())?;
            let title = advisory
                .get("title")
                .and_then(|title| title.as_str())
                .unwrap_or("");
            let package = entry
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(|name| name.as_str())
                .unwrap_or("an unnamed package");
            let patched = entry
                .get("versions")
                .and_then(|versions| versions.get("patched"))
                .and_then(|patched| patched.as_array())
                .map_or_else(String::new, |items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                });
            Some(ToolFinding {
                tool: tool.to_owned(),
                file: WORKSPACE_TARGET.to_owned(),
                line: 0,
                severity: Severity::Suggestion,
                message: format!("{package}: {id} {title} (patched: {patched})"),
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub enum ToolRunError {
    Timeout,
    Launch(String),
    Read(String),
}

#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub text: String,
    pub code: Option<i32>,
}

impl ExecOutput {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

pub trait Executor {
    fn run(&self, spec: &CommandSpec) -> Result<ExecOutput, ToolRunError>;
}

pub struct ProcessExecutor;

impl Executor for ProcessExecutor {
    fn run(&self, spec: &CommandSpec) -> Result<ExecOutput, ToolRunError> {
        let Some(program) = spec.argv.first() else {
            return Err(ToolRunError::Launch("empty command".to_owned()));
        };
        let mut command = std::process::Command::new(program);
        command.args(spec.argv.iter().skip(1));
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|err| ToolRunError::Launch(err.to_string()))?;
        let stdout = child.stdout.take();
        let (sender, receiver) = std::sync::mpsc::channel::<Result<String, String>>();
        std::thread::spawn(move || {
            let outcome = match stdout {
                Some(mut stream) => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut stream, &mut text)
                        .map(|_bytes| text)
                        .map_err(|err| err.to_string())
                }
                None => Ok(String::new()),
            };
            let _unused = sender.send(outcome);
        });
        let deadline = Instant::now().checked_add(Duration::from_secs(spec.timeout_secs));
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let budget = deadline.map_or(Duration::ZERO, |end| {
                        end.saturating_duration_since(Instant::now())
                    });
                    return match receiver.recv_timeout(budget) {
                        Ok(Ok(text)) => Ok(ExecOutput {
                            text,
                            code: status.code(),
                        }),
                        Ok(Err(cause)) => Err(ToolRunError::Read(cause)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            Err(ToolRunError::Timeout)
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(
                            ToolRunError::Read("the output reader did not finish".to_owned()),
                        ),
                    };
                }
                Ok(None) => {
                    let expired = deadline.is_none_or(|end| Instant::now() >= end);
                    if expired {
                        let _unused = child.kill();
                        return Err(ToolRunError::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    let _unused = child.kill();
                    return Err(ToolRunError::Launch(err.to_string()));
                }
            }
        }
    }
}

pub struct CacheKey {
    pub sha: String,
    pub lock_hash: String,
}

#[derive(Debug)]
pub enum ToolOutcome {
    Ran(Vec<ToolFinding>),
    Unavailable(String),
}

pub fn run_cached(
    adapter: &dyn ToolAdapter,
    spec: &CommandSpec,
    key: &CacheKey,
    cache_dir: &Path,
    executor: &dyn Executor,
) -> ToolOutcome {
    let path = cache_dir.join(format!(
        "{}.{}.{}.json",
        safe_label(adapter.name()),
        safe_label(&key.sha),
        safe_label(&key.lock_hash)
    ));
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(findings) = serde_json::from_str::<Vec<ToolFinding>>(&text)
    {
        return ToolOutcome::Ran(findings);
    }
    let output = match executor.run(spec) {
        Ok(output) => output,
        Err(ToolRunError::Timeout) => {
            return ToolOutcome::Unavailable(format!(
                "{} exceeded its {}s budget",
                adapter.name(),
                spec.timeout_secs
            ));
        }
        Err(ToolRunError::Launch(cause)) => {
            return ToolOutcome::Unavailable(format!("{} could not run: {cause}", adapter.name()));
        }
        Err(ToolRunError::Read(cause)) => {
            return ToolOutcome::Unavailable(format!(
                "{} output could not be read: {cause}",
                adapter.name()
            ));
        }
    };
    let findings = adapter.parse(&output.text);
    if !output.succeeded() && findings.is_empty() {
        let exit = output
            .code
            .map_or_else(|| "a signal".to_owned(), |code| code.to_string());
        return ToolOutcome::Unavailable(format!(
            "{} exited with {exit} and no findings on stdout",
            adapter.name()
        ));
    }
    write_best_effort(&path, &findings);
    ToolOutcome::Ran(findings)
}

fn write_best_effort(path: &Path, findings: &[ToolFinding]) {
    let Some(json) = serde_json::to_string(findings).ok() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _unused = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _unused = std::fs::rename(&tmp, path);
    }
}

fn safe_label(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExternalTool;
    use crate::contracts::RegionKind;
    use crate::tools::ExternalToolAdapter;
    use std::cell::Cell;

    fn adapter(kind: BuiltinKind, map: WorkspaceMap) -> BuiltinAdapter {
        BuiltinAdapter {
            kind,
            map,
            budget_secs: 90,
        }
    }

    fn virtual_map() -> WorkspaceMap {
        WorkspaceMap::new(None, vec![("crates/one".to_owned(), "one".to_owned())])
    }

    fn region(file: &str) -> Region {
        Region {
            file: file.to_owned(),
            start: 1,
            end: 4,
            kind: RegionKind::Added,
        }
    }

    #[test]
    fn the_fmt_adapter_reads_real_rustfmt_diff_headers() {
        let fmt = adapter(BuiltinKind::Fmt, WorkspaceMap::default());
        let output = concat!(
            "Diff in /tmp/fmttest/src/main.rs:1:\n",
            "\u{1b}[31m-fn main(){let x=1;println!(\"{}\",x);}\n",
            "\u{1b}[32m+fn main() {\n",
            "Diff in /tmp/fmttest/src/main.rs:7:\n",
            "\u{1b}[31m-let x=1 ;\n",
        );
        let findings = fmt.parse(output);
        assert_eq!(
            findings.len(),
            1,
            "one finding per unformatted file at its first reported line; colored body lines are not diagnostics"
        );
        assert!(
            matches!(findings.first(), Some(finding) if finding.file == "/tmp/fmttest/src/main.rs"
                && finding.line == 1
                && finding.severity == Severity::Suggestion
                && finding.tool == "cargo-fmt")
        );
        assert!(
            fmt.parse("--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-a\n+b\n")
                .is_empty(),
            "unified-diff text is not rustfmt's format and must not read as findings"
        );
        assert!(fmt.parse("").is_empty(), "a formatted tree yields nothing");
    }

    #[test]
    fn built_in_output_hits_the_same_sort_and_cap() {
        let fmt = adapter(BuiltinKind::Fmt, WorkspaceMap::default());
        let raw = (0..205usize)
            .rev()
            .map(|i| format!("Diff in f/bug{i:03}.rs:1:"))
            .collect::<Vec<_>>()
            .join("\n");
        let findings = fmt.parse(&raw);
        assert_eq!(
            findings.len(),
            200,
            "a flood from a built-in tool cannot exceed the shared cap"
        );
        assert_eq!(
            findings.first().map(|finding| finding.file.as_str()),
            Some("f/bug000.rs")
        );
        assert_eq!(
            findings.last().map(|finding| finding.file.as_str()),
            Some("f/bug199.rs"),
            "the cap keeps the lowest-sorted findings"
        );
        assert!(
            findings.is_sorted_by(|a, b| a.file <= b.file),
            "parallel-tool interleaving cannot leak through unsorted"
        );
        assert!(
            findings
                .iter()
                .all(|finding| !finding.file.contains("bug2")),
            "the five highest-sorted files drop off the tail"
        );
    }

    #[test]
    fn audit_advisories_parse_suggestion_grade_at_the_workspace() {
        let audit = adapter(BuiltinKind::Audit, WorkspaceMap::default());
        let output = concat!(
            "{\"vulnerabilities\":{\"count\":1,\"list\":[",
            "{\"advisory\":{\"id\":\"RUSTSEC-2026-0001\",\"title\":\"Soundness bug in x\"},",
            "\"package\":{\"name\":\"serde\",\"version\":\"1.0.100\"},",
            "\"versions\":{\"patched\":[\">=2.1.0\"]}}]}}\n",
        );
        let findings = audit.parse(output);
        assert_eq!(findings.len(), 1);
        assert!(
            matches!(findings.first(), Some(finding) if finding.file == "(workspace)"
                && finding.line == 0
                && finding.severity == Severity::Suggestion
                && finding.message.starts_with("serde: RUSTSEC-2026-0001")
                && finding.message.contains(">=2.1.0")),
            "the affected package leads the message"
        );
        assert!(audit.parse("cargo-audit is not installed").is_empty());
    }

    #[test]
    fn clippy_and_check_commands_target_affected_crates_with_json_output() -> Result<(), DtError> {
        let map = WorkspaceMap::new(
            Some("app".to_owned()),
            vec![("crates/one".to_owned(), "one".to_owned())],
        );
        let adapters = builtin_adapters(&map, &["clippy".to_owned(), "check".to_owned()], 90)?;
        let clippy = adapters
            .iter()
            .find(|adapter| adapter.name() == "cargo-clippy")
            .ok_or_else(|| DtError::Config("clippy adapter missing".to_owned()))?;
        let spec = clippy.command(&[PathBuf::from("crates/one/src/lib.rs")]);
        assert_eq!(
            spec.argv,
            vec![
                "cargo",
                "clippy",
                "--all-targets",
                "-p",
                "one",
                "--message-format=json",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(spec.timeout_secs, 90);
        let check = adapters
            .iter()
            .find(|adapter| adapter.name() == "cargo-check")
            .ok_or_else(|| DtError::Config("check adapter missing".to_owned()))?;
        let spec = check.command(&[PathBuf::from("crates/one/src/lib.rs")]);
        assert_eq!(
            spec.argv,
            vec!["cargo", "check", "-p", "one", "--message-format=json"]
        );
        Ok(())
    }

    #[test]
    fn the_rust_adapters_gate_on_rust_paths() {
        let map = virtual_map();
        let fmt = adapter(BuiltinKind::Fmt, map.clone());
        let clippy = adapter(BuiltinKind::Clippy, map.clone());
        let audit = adapter(BuiltinKind::Audit, map);
        assert!(!fmt.should_run(&[]));
        assert!(!fmt.should_run(&[region("docs/guide.md")]));
        assert!(fmt.should_run(&[region("crates/one/src/lib.rs")]));
        assert!(!clippy.should_run(&[region("docs/guide.md")]));
        assert!(clippy.should_run(&[region("crates/one/src/deep/x.rs")]));
        assert!(
            audit.should_run(&[]),
            "the advisory scan is the one non-diff-scoped exception"
        );
    }

    #[test]
    fn builtin_adapters_respect_the_enabled_list() -> Result<(), DtError> {
        let map = virtual_map();
        let fmt_only = builtin_adapters(&map, &["fmt".to_owned()], 60)?;
        assert_eq!(fmt_only.len(), 1);
        assert_eq!(
            fmt_only.iter().map(ToolAdapter::name).collect::<Vec<_>>(),
            vec!["cargo-fmt"]
        );
        assert!(builtin_adapters(&map, &[], 60)?.is_empty());
        let unknown = builtin_adapters(&map, &["vibes".to_owned()], 60);
        assert!(unknown.is_err_and(|err| err.to_string().contains("vibes")));
        let duplicate = builtin_adapters(&map, &["clippy".to_owned(), "clippy".to_owned()], 60);
        assert!(
            duplicate.is_err_and(|err| err.to_string().contains("duplicate")),
            "a repeated enabled name must not render its findings twice"
        );
        Ok(())
    }

    struct FakeExecutor {
        calls: Cell<usize>,
        result: Result<ExecOutput, ToolRunError>,
    }

    impl Executor for FakeExecutor {
        fn run(&self, _spec: &CommandSpec) -> Result<ExecOutput, ToolRunError> {
            self.calls.set(self.calls.get().saturating_add(1));
            self.result.clone()
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("difftrace-tools-{}-{name}", std::process::id()))
    }

    fn external_diag_tool() -> Result<ExternalToolAdapter, DtError> {
        ExternalToolAdapter::from_config(&ExternalTool {
            name: "eslint".to_owned(),
            command: "eslint \"{files}\"".to_owned(),
            format: "diag".to_owned(),
            run_on: "**".to_owned(),
            budget_secs: 60,
        })
    }

    #[test]
    fn a_timeout_marks_the_tool_unavailable_without_failing_the_round() -> Result<(), DtError> {
        let tool = external_diag_tool()?;
        let executor = FakeExecutor {
            calls: Cell::new(0),
            result: Err(ToolRunError::Timeout),
        };
        let dir = scratch("timeout");
        let outcome = run_cached(
            &tool,
            &tool.command(&[PathBuf::from("src/a.js")]),
            &CacheKey {
                sha: "abc123".to_owned(),
                lock_hash: "feed".to_owned(),
            },
            &dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(&outcome, ToolOutcome::Unavailable(reason)
                if reason.contains("budget")),
            "the round continues with the section marked unavailable: {outcome:?}"
        );
        assert!(
            std::fs::read_dir(&dir).map_or(true, |entries| entries.count() == 0),
            "an unavailable tool caches nothing"
        );
        Ok(())
    }

    #[test]
    fn a_launch_failure_marks_the_tool_unavailable() -> Result<(), DtError> {
        let tool = external_diag_tool()?;
        let executor = FakeExecutor {
            calls: Cell::new(0),
            result: Err(ToolRunError::Launch("cargo not found".to_owned())),
        };
        let dir = scratch("launch");
        let outcome = run_cached(
            &tool,
            &tool.command(&[PathBuf::from("src/a.js")]),
            &CacheKey {
                sha: "abc123".to_owned(),
                lock_hash: "feed".to_owned(),
            },
            &dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(&outcome, ToolOutcome::Unavailable(reason)
                if reason.contains("could not run")),
            "{outcome:?}"
        );
        Ok(())
    }

    #[test]
    fn cache_hits_skip_the_command() -> Result<(), DtError> {
        let tool = external_diag_tool()?;
        let executor = FakeExecutor {
            calls: Cell::new(0),
            result: Ok(ExecOutput {
                text: "src/a.js:2: error: real\n".to_owned(),
                code: Some(0),
            }),
        };
        let dir = scratch("cache");
        let spec = tool.command(&[PathBuf::from("src/a.js")]);
        let key = CacheKey {
            sha: "abc123".to_owned(),
            lock_hash: "feed".to_owned(),
        };
        let first = run_cached(&tool, &spec, &key, &dir, &executor);
        assert!(matches!(&first, ToolOutcome::Ran(findings) if findings.len() == 1));
        assert_eq!(executor.calls.get(), 1);
        let second = run_cached(&tool, &spec, &key, &dir, &executor);
        assert_eq!(
            executor.calls.get(),
            1,
            "the cached run never invokes the tool"
        );
        assert_eq!(
            format!("{second:?}"),
            format!("{first:?}"),
            "the cached findings are byte-identical"
        );
        assert!(dir.join("eslint.abc123.feed.json").is_file());
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_corrupt_cache_reruns_and_is_replaced() -> Result<(), Box<dyn std::error::Error>> {
        let tool = external_diag_tool()?;
        let executor = FakeExecutor {
            calls: Cell::new(0),
            result: Ok(ExecOutput {
                text: "src/a.js:3: error: fresh\n".to_owned(),
                code: Some(0),
            }),
        };
        let dir = scratch("corrupt");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("eslint.abc123.feed.json");
        std::fs::write(&path, "{ not findings json")?;
        let outcome = run_cached(
            &tool,
            &tool.command(&[PathBuf::from("src/a.js")]),
            &CacheKey {
                sha: "abc123".to_owned(),
                lock_hash: "feed".to_owned(),
            },
            &dir,
            &executor,
        );
        assert_eq!(
            executor.calls.get(),
            1,
            "a corrupt cache is a miss, not a wall"
        );
        assert!(
            matches!(&outcome, ToolOutcome::Ran(findings) if findings.len() == 1),
            "{outcome:?}"
        );
        let repaired = std::fs::read_to_string(&path)?;
        assert!(
            serde_json::from_str::<Vec<ToolFinding>>(&repaired).is_ok(),
            "the run replaces the corrupt entry with a valid one"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn a_read_failure_marks_the_tool_unavailable_and_caches_nothing() -> Result<(), DtError> {
        let tool = external_diag_tool()?;
        let executor = FakeExecutor {
            calls: Cell::new(0),
            result: Err(ToolRunError::Read("stream error".to_owned())),
        };
        let dir = scratch("read-failure");
        let outcome = run_cached(
            &tool,
            &tool.command(&[PathBuf::from("src/a.js")]),
            &CacheKey {
                sha: "abc123".to_owned(),
                lock_hash: "feed".to_owned(),
            },
            &dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(&outcome, ToolOutcome::Unavailable(reason)
                if reason.contains("could not be read")),
            "partial output is not findings: {outcome:?}"
        );
        assert!(
            std::fs::read_dir(&dir).map_or(true, |entries| entries.count() == 0),
            "a failed read caches nothing — truncated findings must not poison later runs"
        );
        Ok(())
    }

    #[test]
    fn the_process_executor_collects_stdout() -> Result<(), ToolRunError> {
        let spec = CommandSpec {
            argv: vec!["/bin/echo".to_owned(), "hello".to_owned()],
            cwd: None,
            timeout_secs: 5,
        };
        let output = ProcessExecutor.run(&spec)?;
        assert_eq!(output.text, "hello\n");
        assert_eq!(
            output.code,
            Some(0),
            "the exit status travels with the drained output"
        );
        Ok(())
    }

    #[test]
    fn a_crashed_tool_with_no_findings_is_unavailable_and_never_cached() -> Result<(), DtError> {
        let tool = ExternalToolAdapter::from_config(&ExternalTool {
            name: "false-tool".to_owned(),
            command: "/usr/bin/false".to_owned(),
            format: "diag".to_owned(),
            run_on: "**".to_owned(),
            budget_secs: 5,
        })?;
        let dir = scratch("crash");
        let outcome = run_cached(
            &tool,
            &tool.command(&[PathBuf::from("src/a.js")]),
            &CacheKey {
                sha: "abc123".to_owned(),
                lock_hash: "feed".to_owned(),
            },
            &dir,
            &ProcessExecutor,
        );
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(&outcome, ToolOutcome::Unavailable(reason) if reason.contains("exited with 1")),
            "a crashing tool with empty stdout is unavailable, never a clean run: {outcome:?}"
        );
        assert!(
            std::fs::read_dir(&dir).map_or(true, |entries| entries.count() == 0),
            "a crashed run caches nothing — a false clean must not outlive the invocation"
        );
        Ok(())
    }

    #[test]
    fn a_failing_exit_with_findings_on_stdout_still_rans() -> Result<(), DtError> {
        let tool = external_diag_tool()?;
        let executor = FakeExecutor {
            calls: Cell::new(0),
            result: Ok(ExecOutput {
                text: "src/a.js:2: error: found\n".to_owned(),
                code: Some(1),
            }),
        };
        let dir = scratch("exit-with-findings");
        let outcome = run_cached(
            &tool,
            &tool.command(&[PathBuf::from("src/a.js")]),
            &CacheKey {
                sha: "abc123".to_owned(),
                lock_hash: "feed".to_owned(),
            },
            &dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(&outcome, ToolOutcome::Ran(findings) if findings.len() == 1),
            "diag tools exit non-zero while reporting findings; the findings win over the status: {outcome:?}"
        );
        Ok(())
    }

    #[test]
    fn a_grandchild_holding_the_pipe_cannot_hang_the_run_past_its_budget() {
        let spec = CommandSpec {
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "sleep 5 & exit 0".to_owned(),
            ],
            cwd: None,
            timeout_secs: 1,
        };
        let started = Instant::now();
        let outcome = ProcessExecutor.run(&spec);
        assert!(
            matches!(outcome, Err(ToolRunError::Timeout)),
            "the output drain is bounded by the same budget that bounds the child"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "a background holder of stdout cannot out-wait the budget: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn unreadable_child_output_fails_the_read_instead_of_returning_partial_text() {
        let spec = CommandSpec {
            argv: vec!["/usr/bin/printf".to_owned(), "\\375\\376\\n".to_owned()],
            cwd: None,
            timeout_secs: 5,
        };
        let outcome = ProcessExecutor.run(&spec);
        assert!(
            matches!(&outcome, Err(ToolRunError::Read(cause)) if cause.contains("valid UTF-8")),
            "a read that errors mid-drain surfaces the failure, never the partial text: {outcome:?}"
        );
    }
}
