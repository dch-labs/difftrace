//! Cheap-claim execution: findings whose own mechanism names a runnable
//! command (a git check, a packaging step) are settled by running that
//! command once through the process executor — no judge calls, a hard
//! per-run cap, and the command plus its exit status ride the finding's
//! lead verdict.

use crate::contracts::{Finding, OracleVerdict};
use crate::tools::CommandSpec;
use crate::tools::rust::Executor;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

#[must_use]
pub fn derive_command(mechanism: &str) -> Option<Vec<String>> {
    let text = mechanism.to_lowercase();
    if text.contains("untracked and unignored") || text.contains("not ignored") {
        let path = quoted_path(mechanism, "AGENTS.md").to_owned();
        return Some(vec![
            "git".to_owned(),
            "check-ignore".to_owned(),
            "-q".to_owned(),
            "--".to_owned(),
            path,
        ]);
    }
    if text.contains("cargo package --list") {
        return Some(vec![
            "cargo".to_owned(),
            "package".to_owned(),
            "--list".to_owned(),
            "--allow-dirty".to_owned(),
            "--no-verify".to_owned(),
        ]);
    }
    None
}

fn quoted_path<'a>(mechanism: &'a str, fallback: &'a str) -> &'a str {
    mechanism
        .split(|c: char| c.is_whitespace() || c == '`' || c == '"' || c == '(' || c == ')')
        .find(|token| {
            Path::new(token).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("md")
                    || ext.eq_ignore_ascii_case("rs")
                    || ext.eq_ignore_ascii_case("toml")
            })
        })
        .unwrap_or(fallback)
}

#[must_use]
pub fn execute_claims(
    findings: &[&Finding],
    repo: &Path,
    budget: Duration,
    max_claims: usize,
    executor: &dyn Executor,
) -> BTreeMap<String, OracleVerdict> {
    let mut results = BTreeMap::new();
    let started = std::time::Instant::now();
    for finding in findings.iter().take(max_claims) {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining < Duration::from_secs(1) {
            break;
        }
        let argv = derive_command(&finding.mechanism);
        let Some(argv) = argv else {
            continue;
        };
        let spec = CommandSpec {
            argv: argv.clone(),
            cwd: Some(repo.to_path_buf()),
            timeout_secs: remaining.as_secs(),
        };
        let output = executor.run(&spec);
        let Ok(output) = output else {
            continue;
        };
        match output.code {
            Some(code) if code != 0 => {
                results.insert(finding.id.0.clone(), OracleVerdict::Confirmed);
            }
            Some(0) => {
                results.insert(
                    finding.id.0.clone(),
                    OracleVerdict::Refuted {
                        command: spec.argv.join(" "),
                    },
                );
            }
            _ => {}
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        AspectId, Cite, ClaimType, FindingDraft, FindingId, RegionId, Severity,
    };
    use crate::tools::rust::{ExecOutput, ToolRunError};

    fn lead(mechanism: &str, id: &str) -> Finding {
        let mut finding = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("canon".to_owned()),
            title: "AGENTS.md is untracked and unignored".to_owned(),
            mechanism: mechanism.to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "AGENTS.md".to_owned(),
                line: 1,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        });
        finding.id = FindingId(id.to_owned());
        finding
    }

    struct ScriptedExecutor {
        code: i32,
        ran: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl Executor for ScriptedExecutor {
        fn run(&self, spec: &CommandSpec) -> Result<ExecOutput, ToolRunError> {
            self.ran
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(spec.argv.clone());
            Ok(ExecOutput {
                text: String::new(),
                code: Some(self.code),
            })
        }
    }

    #[test]
    fn an_unignored_claim_is_refuted_when_check_ignore_matches() {
        let finding = lead(
            "AGENTS.md is untracked and unignored so package-check is red",
            "0/canon/unignored",
        );
        let executor = ScriptedExecutor {
            code: 0,
            ran: std::sync::Mutex::new(Vec::new()),
        };
        let results = execute_claims(
            &[&finding],
            Path::new("/repo"),
            Duration::from_secs(30),
            5,
            &executor,
        );
        let verdict = results.get("0/canon/unignored").cloned();
        assert!(
            matches!(&verdict, Some(OracleVerdict::Refuted { command })
                if command.starts_with("git check-ignore")),
            "check-ignore exit 0 (ignored) refutes the unignored claim: {verdict:?}"
        );
        let ran = executor
            .ran
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            ran.first()
                .is_some_and(|argv| argv.first().is_some_and(|a| a == "git")),
            "the derived command ran once: {ran:?}"
        );
    }

    #[test]
    fn a_package_list_failure_confirms_the_gate_defect() {
        let finding = lead(
            "package-check discards cargo package --list failures and prints success",
            "2/error-paths/package-check",
        );
        let executor = ScriptedExecutor {
            code: 1,
            ran: std::sync::Mutex::new(Vec::new()),
        };
        let results = execute_claims(
            &[&finding],
            Path::new("/repo"),
            Duration::from_secs(30),
            5,
            &executor,
        );
        assert!(
            matches!(
                results.get("2/error-paths/package-check"),
                Some(OracleVerdict::Confirmed)
            ),
            "a failing package listing mechanically confirms the gate defect"
        );
    }

    #[test]
    fn a_mechanism_without_a_command_stays_unverifiable_by_execution() {
        let finding = lead("the doc overclaims the ceiling", "0/canon/doc");
        let executor = ScriptedExecutor {
            code: 0,
            ran: std::sync::Mutex::new(Vec::new()),
        };
        let results = execute_claims(
            &[&finding],
            Path::new("/repo"),
            Duration::from_secs(30),
            5,
            &executor,
        );
        assert!(
            results.is_empty(),
            "prose claims record nothing rather than guessing: {results:?}"
        );
    }

    #[test]
    fn the_per_run_cap_bounds_claims() {
        let findings: Vec<Finding> = (0..4)
            .map(|index| {
                lead(
                    "AGENTS.md is untracked and unignored",
                    &format!("0/canon/lead-{index}"),
                )
            })
            .collect();
        let refs: Vec<&Finding> = findings.iter().collect();
        let executor = ScriptedExecutor {
            code: 0,
            ran: std::sync::Mutex::new(Vec::new()),
        };
        let results = execute_claims(
            &refs,
            Path::new("/repo"),
            Duration::from_secs(30),
            2,
            &executor,
        );
        assert_eq!(
            results.len(),
            2,
            "the hard cap bounds the pass: {results:?}"
        );
    }
}
