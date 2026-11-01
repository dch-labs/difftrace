//! Deterministic doc-reference checking: a markdown region whose LINK
//! TARGETS name files the branch neither tracks nor deliberately ignores
//! is a finding before any judge runs — backticked prose and quoted
//! command output are not references, and a gitignored path is unpacked
//! on purpose, not dangling.

use std::collections::BTreeSet;

use crate::contracts::{Region, Severity, ToolFinding};
use crate::gateway::PrGateway;

const MAX_DOC_FINDINGS: usize = 20;

#[must_use]
pub fn check(regions: &[Region], gateway: &dyn PrGateway) -> Vec<ToolFinding> {
    let Ok(tracked) = gateway.list_files_at_head() else {
        return Vec::new();
    };
    let tracked: BTreeSet<&str> = tracked.iter().map(String::as_str).collect();
    let untracked = match gateway.untracked_files() {
        Ok(files) => files.into_iter().collect::<BTreeSet<String>>(),
        Err(err) => {
            tracing::warn!(
                target: "difftrace::docrefs",
                cause = %err,
                "doc-refs cannot tell a dangling reference from an untracked file without the status listing; staying silent"
            );
            return Vec::new();
        }
    };
    let mut findings = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for region in regions {
        if !region.file.to_lowercase().ends_with(".md") {
            continue;
        }
        let Ok(snapshot) = gateway.read_file_at_head(&region.file) else {
            continue;
        };
        for (offset, line) in snapshot.content.lines().enumerate() {
            let number = u32::try_from(offset.saturating_add(1)).unwrap_or(u32::MAX);
            if number < region.start || number > region.end {
                continue;
            }
            for candidate in link_targets(line) {
                if !looks_like_repo_path(&candidate)
                    || tracked.contains(candidate.as_str())
                    || !seen.insert(format!("{}:{number}:{candidate}", region.file))
                {
                    continue;
                }
                if gateway.check_ignore(&candidate).unwrap_or(false) {
                    continue;
                }
                let message = if untracked.contains(&candidate) {
                    format!(
                        "references {candidate}, which exists only as an untracked working-tree file — the branch does not carry it; commit it with this change"
                    )
                } else {
                    format!(
                        "references {candidate}, which the branch does not track at head — the reference dangles once merged"
                    )
                };
                findings.push(ToolFinding {
                    tool: "doc-refs".to_owned(),
                    file: region.file.clone(),
                    line: number,
                    severity: Severity::Warning,
                    message,
                });
                if findings.len() >= MAX_DOC_FINDINGS {
                    return findings;
                }
            }
        }
    }
    findings
}

fn link_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = line;
    while let Some((_, after_label)) = rest.split_once("](") {
        let Some((target, tail)) = after_label.split_once(')') else {
            break;
        };
        let cleaned = target.trim().trim_start_matches('<').trim_end_matches('>');
        if !cleaned.starts_with('#') {
            targets.push(cleaned.to_owned());
        }
        rest = tail;
    }
    targets
}

fn looks_like_repo_path(candidate: &str) -> bool {
    if candidate.is_empty()
        || candidate.contains(char::is_whitespace)
        || candidate.contains("://")
        || candidate.contains("mailto:")
        || candidate.starts_with('/')
        || candidate.starts_with('-')
        || candidate.starts_with('.')
        || candidate.contains('*')
    {
        return false;
    }
    let Some((_, ext)) = candidate.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext,
        "rs" | "toml" | "md" | "json" | "yaml" | "yml" | "sh" | "py" | "ts" | "js" | "lock"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{DiffSource, FileSnapshot, PinnedRange, RegionKind};

    struct DocsGateway {
        files: Vec<String>,
        contents: Vec<(String, String)>,
        untracked: Vec<String>,
        ignored: Vec<String>,
        untracked_fails: bool,
    }

    impl DocsGateway {
        fn readme(body: &str) -> Self {
            Self {
                files: vec!["README.md".to_owned(), "src/lib.rs".to_owned()],
                contents: vec![("README.md".to_owned(), body.to_owned())],
                untracked: vec!["example.config.toml".to_owned()],
                ignored: vec!["target/coverage.md".to_owned()],
                untracked_fails: false,
            }
        }
    }

    impl PrGateway for DocsGateway {
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
            self.contents
                .iter()
                .find(|(file, _)| file == path)
                .map(|(file, body)| FileSnapshot {
                    path: file.clone(),
                    content: body.clone(),
                })
                .ok_or_else(|| crate::error::DtError::Gateway("no file".to_owned()))
        }

        fn list_files_at_head(&self) -> Result<Vec<String>, crate::error::DtError> {
            Ok(self.files.clone())
        }

        fn search_at_head(
            &self,
            _pattern: &str,
            _cap: usize,
        ) -> Result<Vec<crate::contracts::SearchHit>, crate::error::DtError> {
            Ok(Vec::new())
        }

        fn untracked_files(&self) -> Result<Vec<String>, crate::error::DtError> {
            if self.untracked_fails {
                return Err(crate::error::DtError::Git("status failed".to_owned()));
            }
            Ok(self.untracked.clone())
        }

        fn check_ignore(&self, path: &str) -> Result<bool, crate::error::DtError> {
            Ok(self.ignored.iter().any(|ignored| ignored == path))
        }
    }

    fn md_region() -> Region {
        Region {
            file: "README.md".to_owned(),
            start: 1,
            end: u32::MAX,
            kind: RegionKind::Added,
        }
    }

    #[test]
    fn a_dangling_link_target_is_a_finding() {
        let gateway = DocsGateway::readme("See [the setup](missing.settings.toml) today.\n");
        let findings = check(&[md_region()], &gateway);
        assert_eq!(findings.len(), 1);
        assert!(
            findings
                .first()
                .is_some_and(|finding| finding.message.contains("does not track")),
            "a genuinely dangling link still fires: {findings:?}"
        );
    }

    #[test]
    fn an_untracked_link_target_names_the_merge_fix() {
        let gateway =
            DocsGateway::readme("A complete example lives in [the config](example.config.toml).\n");
        let findings = check(&[md_region()], &gateway);
        assert_eq!(findings.len(), 1);
        assert!(
            findings.first().is_some_and(|finding| finding
                .message
                .contains("untracked working-tree file")
                && finding.message.contains("example.config.toml")
                && finding.line == 1),
            "the finding names the file, line, and the merge fix: {findings:?}"
        );
    }

    #[test]
    fn backticked_prose_is_never_a_reference() {
        let gateway = DocsGateway::readme(
            "The v1 `loop.yaml` manifest model; vendored `curated_prices.json`; the matrix lands in `target/provider-conformance/matrix.md`.\n",
        );
        let findings = check(&[md_region()], &gateway);
        assert!(
            findings.is_empty(),
            "conceptual filenames and prose mentions are not references: {findings:?}"
        );
    }

    #[test]
    fn an_ignored_link_target_is_deliberately_unpacked() {
        let gateway = DocsGateway::readme("Coverage lands in [the report](target/coverage.md).\n");
        let findings = check(&[md_region()], &gateway);
        assert!(
            findings.is_empty(),
            "a gitignored path is unpacked on purpose, not dangling: {findings:?}"
        );
    }

    #[test]
    fn absolute_paths_and_anchors_are_not_repo_paths() {
        let gateway = DocsGateway::readme(
            "Error text: `python3 /tmp/probe.py` failed — see [details](#probe) and [docs](https://x.dev/guide.md).\n",
        );
        let findings = check(&[md_region()], &gateway);
        assert!(
            findings.is_empty(),
            "absolute paths, anchors, and URLs are not repo references: {findings:?}"
        );
    }

    #[test]
    fn a_tracked_link_target_stays_quiet() {
        let gateway = DocsGateway::readme("The seam lives in [lib](src/lib.rs).\n");
        assert!(
            check(&[md_region()], &gateway).is_empty(),
            "tracked targets never fire"
        );
    }

    #[test]
    fn a_failed_status_listing_stays_silent_instead_of_mislabeling() {
        let mut gateway = DocsGateway::readme("See [setup](missing.settings.toml).\n");
        gateway.untracked_fails = true;
        assert!(
            check(&[md_region()], &gateway).is_empty(),
            "without the status listing doc-refs cannot know a dangle from an untracked file, so it claims nothing"
        );
    }

    #[test]
    fn non_markdown_regions_and_region_line_bounds_are_respected() {
        let gateway = DocsGateway::readme("line one\nline two links [gone](gone.file.md)\n");
        let mut region = md_region();
        region.start = 1;
        region.end = 1;
        assert!(
            check(&[region], &gateway).is_empty(),
            "a reference outside the region span stays quiet"
        );
        let code = Region {
            file: "src/lib.rs".to_owned(),
            start: 1,
            end: u32::MAX,
            kind: RegionKind::Added,
        };
        assert!(
            check(&[code], &gateway).is_empty(),
            "only markdown regions are scanned"
        );
    }
}
