//! The deterministic tool layer beneath the judges.

pub mod cargo_json;
pub mod diag;

use std::path::PathBuf;

use crate::aspects::glob_matches;
use crate::config::{ExternalTool, ToolsConfig};
use crate::contracts::{Region, ToolFinding};
use crate::error::DtError;

const MAX_FINDINGS_PER_TOOL: usize = 200;
const FILES_PLACEHOLDER: &str = "{files}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Diag,
    CargoJson,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_lowercase().as_str() {
            "diag" => Ok(Self::Diag),
            "cargo-json" => Ok(Self::CargoJson),
            other => Err(format!(
                "unknown tool output format {other:?}; expected diag or cargo-json"
            )),
        }
    }
}

pub trait ToolAdapter {
    fn name(&self) -> &str;
    fn should_run(&self, changed: &[Region]) -> bool;
    fn command(&self, targets: &[PathBuf]) -> CommandSpec;
    fn parse(&self, output: &str) -> Vec<ToolFinding>;
}

pub struct ExternalToolAdapter {
    name: String,
    command: String,
    format: OutputFormat,
    run_on: String,
    budget_secs: u64,
}

impl ExternalToolAdapter {
    pub fn from_config(entry: &ExternalTool) -> Result<Self, DtError> {
        let format: OutputFormat = entry
            .format
            .parse()
            .map_err(|err| DtError::Config(format!("tool {}: {err}", entry.name)))?;
        if expand_braces(&entry.run_on)
            .any(|alternative| alternative.contains('{') || alternative.contains('}'))
        {
            return Err(DtError::Config(format!(
                "tool {}: run_on {:?} has nested or unmatched braces; one flat group only",
                entry.name, entry.run_on
            )));
        }
        Ok(Self {
            name: entry.name.clone(),
            command: entry.command.clone(),
            format,
            run_on: entry.run_on.clone(),
            budget_secs: entry.budget_secs,
        })
    }
}

impl ToolAdapter for ExternalToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_run(&self, changed: &[Region]) -> bool {
        changed
            .iter()
            .any(|region| matches_run_on(&self.run_on, &region.file))
    }

    fn command(&self, targets: &[PathBuf]) -> CommandSpec {
        let argv = split_quoted(&self.command)
            .into_iter()
            .flat_map(|token| expand_files(token, targets))
            .collect();
        CommandSpec {
            argv,
            cwd: None,
            timeout_secs: self.budget_secs,
        }
    }

    fn parse(&self, output: &str) -> Vec<ToolFinding> {
        let findings = match self.format {
            OutputFormat::Diag => diag::parse(output, &self.name),
            OutputFormat::CargoJson => cargo_json::parse(output, &self.name),
        };
        normalize_output(findings)
    }
}

pub fn external_adapters(config: &ToolsConfig) -> Result<Vec<ExternalToolAdapter>, DtError> {
    let mut adapters: Vec<ExternalToolAdapter> = Vec::new();
    for entry in &config.external {
        let adapter = ExternalToolAdapter::from_config(entry)?;
        if adapters
            .iter()
            .any(|existing| existing.name() == adapter.name())
        {
            return Err(DtError::Config(format!(
                "duplicate external tool name {:?}",
                adapter.name
            )));
        }
        adapters.push(adapter);
    }
    adapters.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(adapters)
}

fn normalize_output(mut findings: Vec<ToolFinding>) -> Vec<ToolFinding> {
    findings
        .sort_unstable_by(|a, b| (&a.file, a.line, &a.message).cmp(&(&b.file, b.line, &b.message)));
    findings.truncate(MAX_FINDINGS_PER_TOOL);
    findings
}

fn matches_run_on(pattern: &str, path: &str) -> bool {
    expand_braces(pattern).any(|alternative| glob_matches(&alternative, path))
}

fn expand_braces(pattern: &str) -> impl Iterator<Item = String> {
    let Some((prefix, rest)) = pattern.split_once('{') else {
        return vec![pattern.to_owned()].into_iter();
    };
    let Some((body, suffix)) = rest.split_once('}') else {
        return vec![pattern.to_owned()].into_iter();
    };
    body.split(',')
        .map(move |choice| format!("{prefix}{choice}{suffix}"))
        .collect::<Vec<_>>()
        .into_iter()
}

fn split_quoted(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for c in command.chars() {
        if quote.is_some() {
            if Some(c) == quote {
                quote = None;
            } else {
                current.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn expand_files(token: String, targets: &[PathBuf]) -> Vec<String> {
    let safe = targets
        .iter()
        .map(|path| path.display().to_string())
        .filter(|target| !target.starts_with('-'))
        .collect::<Vec<_>>();
    if token == FILES_PLACEHOLDER {
        return safe;
    }
    if token.contains(FILES_PLACEHOLDER) {
        let joined = safe.join(" ");
        return vec![token.replace(FILES_PLACEHOLDER, joined.as_str())];
    }
    vec![token]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExternalTool;
    use crate::contracts::RegionKind;

    fn entry(name: &str, format: &str, run_on: &str) -> ExternalTool {
        ExternalTool {
            name: name.to_owned(),
            command: "npx eslint --format unix \"{files}\"".to_owned(),
            format: format.to_owned(),
            run_on: run_on.to_owned(),
            budget_secs: 45,
        }
    }

    fn region(file: &str) -> Region {
        Region {
            file: file.to_owned(),
            start: 1,
            end: 10,
            kind: RegionKind::Added,
        }
    }

    #[test]
    fn should_run_respects_the_run_on_glob() -> Result<(), DtError> {
        let adapter =
            ExternalToolAdapter::from_config(&entry("eslint", "diag", "**/*.{js,ts,tsx}"))?;
        assert!(adapter.should_run(&[region("src/app.js")]));
        assert!(adapter.should_run(&[region("components/button.tsx")]));
        assert!(!adapter.should_run(&[region("src/lib.rs")]));
        assert!(!adapter.should_run(&[]));
        Ok(())
    }

    #[test]
    fn command_substitutes_files_into_the_argv() -> Result<(), DtError> {
        let adapter = ExternalToolAdapter::from_config(&entry("eslint", "diag", "**"))?;
        let spec = adapter.command(&[
            PathBuf::from("src/a.js"),
            PathBuf::from("src/space name.js"),
        ]);
        assert_eq!(
            spec.argv,
            vec![
                "npx".to_owned(),
                "eslint".to_owned(),
                "--format".to_owned(),
                "unix".to_owned(),
                "src/a.js".to_owned(),
                "src/space name.js".to_owned(),
            ],
            "quotes group the placeholder, the whole token expands per target"
        );
        assert_eq!(spec.timeout_secs, 45);
        assert!(spec.cwd.is_none());
        let prefixed = ExternalToolAdapter::from_config(&ExternalTool {
            command: "lint --glob={files}".to_owned(),
            ..entry("linter", "diag", "**")
        })?;
        let spec = prefixed.command(&[PathBuf::from("a.rs")]);
        assert_eq!(
            spec.argv,
            vec!["lint".to_owned(), "--glob=a.rs".to_owned()],
            "an embedded placeholder joins the targets in place"
        );
        Ok(())
    }

    #[test]
    fn unknown_output_formats_fail_closed() {
        let outcome = ExternalToolAdapter::from_config(&entry("mystery", "xml", "**"));
        assert!(
            outcome.is_err_and(|err| err.to_string().contains("xml")),
            "a format no parser reads must not silently produce nothing"
        );
    }

    #[test]
    fn run_on_patterns_the_expander_cannot_handle_reject() {
        let multi_group =
            ExternalToolAdapter::from_config(&entry("eslint", "diag", "**/{src,lib}/**/*.{js,ts}"));
        assert!(
            multi_group.is_err_and(|err| err.to_string().contains("nested or unmatched")),
            "a pattern that would silently never match must reject at construction"
        );
        let unclosed = ExternalToolAdapter::from_config(&entry("eslint", "diag", "**/*.{js"));
        assert!(unclosed.is_err_and(|err| err.to_string().contains("unmatched")));
        let stray = ExternalToolAdapter::from_config(&entry("eslint", "diag", "a}b"));
        assert!(stray.is_err());
    }

    #[test]
    fn flag_shaped_targets_never_reach_the_argv() -> Result<(), DtError> {
        let adapter = ExternalToolAdapter::from_config(&entry("eslint", "diag", "**"))?;
        let spec = adapter.command(&[
            PathBuf::from("src/a.js"),
            PathBuf::from("--format=json"),
            PathBuf::from("src/b.js"),
        ]);
        assert_eq!(
            spec.argv,
            vec![
                "npx".to_owned(),
                "eslint".to_owned(),
                "--format".to_owned(),
                "unix".to_owned(),
                "src/a.js".to_owned(),
                "src/b.js".to_owned(),
            ],
            "a file named like a flag is dropped rather than injected into argv"
        );
        Ok(())
    }

    #[test]
    fn duplicate_tool_names_reject() {
        let config = ToolsConfig {
            external: vec![
                entry("eslint", "diag", "**"),
                entry("eslint", "diag", "**/*.js"),
            ],
            ..ToolsConfig::default()
        };
        assert!(
            external_adapters(&config).is_err_and(|err| err.to_string().contains("duplicate")),
            "one tool label must mean one tool"
        );
    }

    #[test]
    fn registry_sorts_adapters_by_name() -> Result<(), DtError> {
        let config = ToolsConfig {
            external: vec![
                entry("zeta", "diag", "**"),
                entry("alpha", "cargo-json", "**"),
            ],
            ..ToolsConfig::default()
        };
        let adapters = external_adapters(&config)?;
        let names = adapters.iter().map(ToolAdapter::name).collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha", "zeta"]);
        Ok(())
    }

    #[test]
    fn tool_findings_sort_and_cap_deterministically() -> Result<(), DtError> {
        let adapter = ExternalToolAdapter::from_config(&entry("eslint", "diag", "**"))?;
        let unsorted =
            adapter.parse("b.rs:4: error: second\na.rs:9: error: later\na.rs:2: error: first\n");
        let messages = unsorted
            .iter()
            .map(|finding| finding.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec!["first", "later", "second"],
            "findings order by file, then line, then message"
        );
        let wall = (0..201usize)
            .map(|i| {
                let number = i.saturating_add(1);
                format!("a.rs:{number}: error: finding {number}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let capped = adapter.parse(&wall);
        assert_eq!(
            capped.len(),
            200,
            "one tool cannot flood the verdict section"
        );
        assert!(
            matches!(capped.last(), Some(finding) if finding.message == "finding 200"),
            "the cap keeps the lowest-sorted findings, dropping the tail"
        );
        Ok(())
    }
}
