//! Deterministic CI leg-coverage checking: a crate the diff touches whose
//! lints no CI leg enforces is a finding before any judge runs — the
//! guard rail missing from the pipeline needs no model to prove.

use std::collections::BTreeSet;

use crate::contracts::{Region, Severity, ToolFinding};
use crate::gateway::PrGateway;

const MAX_COVERAGE_FINDINGS: usize = 10;

#[must_use]
pub fn check(regions: &[Region], gateway: &dyn PrGateway) -> Vec<ToolFinding> {
    let Ok(tracked) = gateway.list_files_at_head() else {
        return Vec::new();
    };
    let workflow_text = read_workflows(gateway, &tracked);
    if workflow_text.is_empty() {
        return Vec::new();
    }
    let crates = crates_in_diff(regions, &tracked);
    let mut findings = Vec::new();
    for (krate, manifest) in crates {
        if crate_has_enforcing_leg(&krate, &manifest, &workflow_text) {
            continue;
        }
        findings.push(ToolFinding {
            tool: "ci-coverage".to_owned(),
            file: manifest,
            line: 1,
            severity: Severity::Warning,
            message: format!(
                "the diff touches crate {krate} but no CI leg compiles it with a lint gate — the lints are unenforced in CI"
            ),
        });
        if findings.len() >= MAX_COVERAGE_FINDINGS {
            break;
        }
    }
    findings
}

fn read_workflows(gateway: &dyn PrGateway, tracked: &[String]) -> String {
    let mut text = String::new();
    for path in tracked {
        if !path.starts_with(".github/workflows/") || !path.to_lowercase().ends_with(".yml") {
            continue;
        }
        if let Ok(snapshot) = gateway.read_file_at_head(path) {
            text.push_str(&snapshot.content);
            text.push('\n');
        }
    }
    text
}

fn crates_in_diff(regions: &[Region], tracked: &[String]) -> Vec<(String, String)> {
    let changed: BTreeSet<&str> = regions.iter().map(|region| region.file.as_str()).collect();
    tracked
        .iter()
        .filter(|path| path.ends_with("Cargo.toml"))
        .filter(|path| {
            path.strip_suffix("/Cargo.toml")
                .is_some_and(|dir| !dir.is_empty())
        })
        .filter_map(|path| {
            let dir = path.strip_suffix("/Cargo.toml").unwrap_or_default();
            let touches = changed.iter().any(|file| {
                file == &path.clone()
                    || file.starts_with(&format!("{dir}/"))
                    || *file == format!("{dir}/Cargo.toml")
            });
            touches.then(|| {
                (
                    dir.rsplit('/').next().unwrap_or(dir).to_owned(),
                    path.clone(),
                )
            })
        })
        .collect()
}

fn crate_has_enforcing_leg(krate: &str, manifest: &str, workflow_text: &str) -> bool {
    let root_workspace_gate = workflow_text
        .lines()
        .any(|line| is_clippy_invocation(line) && !line.contains(" -p "));
    if root_workspace_gate {
        return true;
    }
    workflow_text.lines().any(|line| {
        is_clippy_invocation(line) && line.contains(" -p ") && line.contains(krate)
            || is_clippy_invocation(line) && line.contains(" -p ") && manifest.contains(krate)
    }) || workflow_text.contains("cargo clippy")
        && workflow_text.contains(krate)
        && workflow_text.lines().any(|line| {
            line.contains("clippy") && line.contains(krate) && line.contains("-D warnings")
        })
}

fn is_clippy_invocation(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains("cargo clippy")
        && trimmed.contains("-- -D warnings")
        && !trimmed.starts_with('#')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{DiffSource, FileSnapshot, PinnedRange, RegionKind, SearchHit};

    struct CiGateway {
        tracked: Vec<String>,
        workflows: Vec<(String, String)>,
    }

    impl PrGateway for CiGateway {
        fn pinned_range(&self) -> PinnedRange {
            PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            }
        }
        fn diff(&self) -> DiffSource {
            DiffSource::Unified(String::new())
        }
        fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, crate::error::DtError> {
            self.workflows
                .iter()
                .find(|(file, _)| file == path)
                .map(|(file, body)| FileSnapshot {
                    path: file.clone(),
                    content: body.clone(),
                })
                .ok_or_else(|| crate::error::DtError::Gateway("no file".to_owned()))
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, crate::error::DtError> {
            Ok(self.tracked.clone())
        }
        fn search_at_head(
            &self,
            _pattern: &str,
            _cap: usize,
        ) -> Result<Vec<SearchHit>, crate::error::DtError> {
            Ok(Vec::new())
        }
    }

    fn region(file: &str) -> Region {
        Region {
            file: file.to_owned(),
            start: 1,
            end: 40,
            kind: RegionKind::File,
        }
    }

    const LOOPCTL_CI: &str = r#"
  clippy:
    runs-on: ubuntu-latest
    steps:
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo clippy --all-targets -p loopctl-derive -- -D warnings
"#;

    #[test]
    fn a_workspace_wide_clippy_leg_covers_every_member_crate() {
        let gateway = CiGateway {
            tracked: vec![
                ".github/workflows/ci.yml".to_owned(),
                "Cargo.toml".to_owned(),
                "crates/derive/Cargo.toml".to_owned(),
                "crates/derive/src/lib.rs".to_owned(),
                "crates/derive/src/attr.rs".to_owned(),
                "src/lib.rs".to_owned(),
            ],
            workflows: vec![(".github/workflows/ci.yml".to_owned(), LOOPCTL_CI.to_owned())],
        };
        let findings = check(&[region("crates/derive/src/attr.rs")], &gateway);
        assert!(
            findings.is_empty(),
            "the workspace-wide leg compiles loopctl-derive with -D warnings, so no crate is unenforced: {findings:?}"
        );
    }

    #[test]
    fn a_crate_no_leg_compiles_with_a_gate_is_the_finding() {
        let gateway = CiGateway {
            tracked: vec![
                ".github/workflows/ci.yml".to_owned(),
                "crates/derive/Cargo.toml".to_owned(),
                "crates/derive/src/attr.rs".to_owned(),
            ],
            workflows: vec![(
                ".github/workflows/ci.yml".to_owned(),
                "  test:\n    - run: cargo test -p loopctl-derive\n  fmt:\n    - run: cargo fmt --check\n"
                    .to_owned(),
            )],
        };
        let findings = check(&[region("crates/derive/src/attr.rs")], &gateway);
        let first = findings.first().cloned();
        assert!(
            matches!(&first, Some(finding) if finding.tool == "ci-coverage"
                && finding.file == "crates/derive/Cargo.toml"
                && finding.severity == Severity::Warning
                && finding.message.contains("no CI leg compiles it with a lint gate")),
            "a crate whose lints CI never enforces is the structural finding: {first:?}"
        );
    }

    #[test]
    fn an_untouched_crate_never_fires_even_without_any_gate() {
        let gateway = CiGateway {
            tracked: vec![
                ".github/workflows/ci.yml".to_owned(),
                "crates/derive/Cargo.toml".to_owned(),
                "crates/other/Cargo.toml".to_owned(),
                "crates/other/src/lib.rs".to_owned(),
                "src/lib.rs".to_owned(),
            ],
            workflows: vec![(
                ".github/workflows/ci.yml".to_owned(),
                "  test:\n    - run: cargo test\n".to_owned(),
            )],
        };
        let findings = check(&[region("src/lib.rs")], &gateway);
        assert!(
            findings.is_empty(),
            "the check is scoped to crates the diff touches: {findings:?}"
        );
    }

    #[test]
    fn a_root_package_crate_is_covered_by_the_workspace_gate() {
        let gateway = CiGateway {
            tracked: vec![
                ".github/workflows/ci.yml".to_owned(),
                "Cargo.toml".to_owned(),
                "src/lib.rs".to_owned(),
            ],
            workflows: vec![(
                ".github/workflows/ci.yml".to_owned(),
                "  - run: cargo clippy --all-targets -- -D warnings\n".to_owned(),
            )],
        };
        assert!(
            check(&[region("src/lib.rs")], &gateway).is_empty(),
            "the root package rides the workspace gate"
        );
    }
}
