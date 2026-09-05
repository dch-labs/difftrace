//! The cross-round issue registry: every issue difftrace ever raised on a
//! pull request, kept as hidden JSON inside the marker comment and merged
//! each run — open issues carry across rounds, fixed ones close with the
//! round that fixed them, and the visible lists render from the state.

use crate::findings::Finding;
use crate::findings::Severity;
use crate::github::ReviewThread;

pub(crate) const REGISTRY_MARKER: &str = "<!-- difftrace:registry ";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IssueStatus {
    Open,
    Fixed,
    ManuallyResolved,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Issue {
    pub title: String,
    pub file: String,
    pub line: Option<u64>,
    pub severity: Severity,
    pub complexity: u8,
    pub anchored: bool,
    pub status: IssueStatus,
    pub thread_id: Option<String>,
    pub raised_round: u32,
    pub raised_sha: String,
    pub last_round: u32,
    pub resolved_round: Option<u32>,
    pub resolved_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Registry {
    pub round: u32,
    pub issues: Vec<Issue>,
}

pub(crate) struct RoundFindings<'a> {
    pub head_sha: &'a str,
    pub grounded: &'a [Finding],
    pub finding_threads: &'a [Option<String>],
    pub dropped: &'a [(Finding, &'a str)],
    pub threads: &'a [ReviewThread],
}

impl Registry {
    pub(crate) fn unresolved(&self) -> Vec<&Issue> {
        let mut open: Vec<&Issue> = self
            .issues
            .iter()
            .filter(|issue| issue.status == IssueStatus::Open)
            .collect();
        open.sort_by_key(|issue| !crate::review::batch::is_blocking(issue.severity));
        open
    }

    pub(crate) fn has_unresolved_blockers(&self) -> bool {
        self.issues.iter().any(|issue| {
            issue.status == IssueStatus::Open && crate::review::batch::is_blocking(issue.severity)
        })
    }

    pub(crate) fn fixed_history(&self) -> Vec<&Issue> {
        let mut done: Vec<&Issue> = self
            .issues
            .iter()
            .filter(|issue| issue.status != IssueStatus::Open)
            .collect();
        done.sort_by_key(|issue| issue.resolved_round.unwrap_or(0));
        done
    }

    #[must_use]
    pub(crate) fn merge(mut self, round: &RoundFindings<'_>) -> Self {
        self.round = self.round.saturating_add(1);
        let round_no = self.round;
        let sha = round.head_sha;

        let original_len = self.issues.len();
        let mut seen: Vec<bool> = self.issues.iter().map(|_| false).collect();

        for (finding, thread_id) in grounded_with_threads(round.grounded, round.finding_threads) {
            let index = self.match_open_index(finding, thread_id);
            if let Some(i) = index {
                let Some(issue) = self.issues.get_mut(i) else {
                    continue;
                };
                issue.last_round = round_no;
                issue.title.clone_from(&finding.title);
                issue.severity = finding.severity;
                issue.complexity = finding.complexity;
                if let Some(id) = thread_id {
                    issue.thread_id = Some(id.to_owned());
                }
                if let Some(flag) = seen.get_mut(i) {
                    *flag = true;
                }
            } else {
                seen.push(true);
                self.issues.push(Issue {
                    title: finding.title.clone(),
                    file: finding.file.clone(),
                    line: Some(finding.line as u64),
                    severity: finding.severity,
                    complexity: finding.complexity,
                    anchored: true,
                    status: IssueStatus::Open,
                    thread_id: thread_id.map(String::from),
                    raised_round: round_no,
                    raised_sha: sha.to_owned(),
                    last_round: round_no,
                    resolved_round: None,
                    resolved_sha: None,
                });
            }
        }

        for (finding, _reason) in round.dropped {
            if let Some(i) = self.issues.iter().position(|issue| {
                issue.status == IssueStatus::Open
                    && issue.file == finding.file
                    && issue.title == finding.title
            }) {
                if let Some(issue) = self.issues.get_mut(i) {
                    issue.last_round = round_no;
                }
                if let Some(flag) = seen.get_mut(i) {
                    *flag = true;
                }
            } else {
                seen.push(true);
                self.issues.push(Issue {
                    title: finding.title.clone(),
                    file: finding.file.clone(),
                    line: None,
                    severity: finding.severity,
                    complexity: finding.complexity,
                    anchored: false,
                    status: IssueStatus::Open,
                    thread_id: None,
                    raised_round: round_no,
                    raised_sha: sha.to_owned(),
                    last_round: round_no,
                    resolved_round: None,
                    resolved_sha: None,
                });
            }
        }

        let resolved_threads: Vec<&ReviewThread> = round
            .threads
            .iter()
            .filter(|thread| thread.resolved)
            .collect();

        for (i, issue) in self.issues.iter_mut().enumerate() {
            let marked = i >= original_len || seen.get(i).is_some_and(|flag| *flag);
            if issue.status != IssueStatus::Open || marked {
                continue;
            }
            let manually = issue
                .thread_id
                .as_ref()
                .is_some_and(|id| resolved_threads.iter().any(|t| &t.id == id));
            issue.status = if manually {
                IssueStatus::ManuallyResolved
            } else {
                IssueStatus::Fixed
            };
            issue.resolved_round = Some(round_no);
            issue.resolved_sha = Some(sha.to_owned());
        }

        self
    }

    fn match_open_index(&self, finding: &Finding, thread_id: Option<&str>) -> Option<usize> {
        if let Some(id) = thread_id
            && let Some(i) = self.issues.iter().position(|issue| {
                issue.status == IssueStatus::Open && issue.thread_id.as_deref() == Some(id)
            })
        {
            return Some(i);
        }
        self.issues.iter().position(|issue| {
            issue.status == IssueStatus::Open
                && issue.anchored
                && issue.file == finding.file
                && issue.line == Some(finding.line as u64)
        })
    }
}

fn grounded_with_threads<'a>(
    grounded: &'a [Finding],
    finding_threads: &'a [Option<String>],
) -> Vec<(&'a Finding, Option<&'a str>)> {
    grounded
        .iter()
        .zip(finding_threads.iter())
        .map(|(finding, id)| (finding, id.as_deref()))
        .collect()
}

pub(crate) fn embed_registry(body: &str, registry: &Registry) -> Result<String, String> {
    let json = serde_json::to_string(registry)
        .map_err(|err| err.to_string())?
        .replace('>', "\\u003e");
    let replacement = format!("{REGISTRY_MARKER}{json} -->");
    if let Some(start) = body.find(REGISTRY_MARKER) {
        let tail = body.get(start..).ok_or("registry start out of range")?;
        let end_rel = tail.find(" -->").ok_or("registry block never closes")?;
        let end = start.saturating_add(end_rel).saturating_add(4);
        let head = body.get(..start).ok_or("registry head out of range")?;
        let rest = body.get(end..).ok_or("registry tail out of range")?;
        Ok(format!("{head}{replacement}{rest}"))
    } else {
        let newline = body.find('\n').unwrap_or(0);
        let (first, rest) = body.split_at(newline);
        Ok(format!("{first}\n{replacement}\n{rest}"))
    }
}

pub(crate) fn extract_registry(body: &str) -> Option<Registry> {
    let start = body.find(REGISTRY_MARKER)?;
    let rest = &body[start.saturating_add(REGISTRY_MARKER.len())..];
    let end = rest.find(" -->")?;
    serde_json::from_str(rest.get(0..end)?).ok()
}

pub(crate) fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

pub(crate) fn parse_issue_header(body: &str) -> Option<(crate::findings::Severity, u8, String)> {
    let first = body.lines().next()?;
    let severity = if first.contains("/nitpick-") {
        crate::findings::Severity::Nitpick
    } else if first.contains("/suggestion-") {
        crate::findings::Severity::Suggestion
    } else if first.contains("/warning-") {
        crate::findings::Severity::Warning
    } else if first.contains("/critical-") {
        crate::findings::Severity::Critical
    } else {
        return None;
    };
    let complexity = first
        .split("/effort_")
        .nth(1)?
        .split('-')
        .next()?
        .parse()
        .ok()?;
    let title_start = first.find("**")?.saturating_add(2);
    let tail = first.get(title_start..)?;
    let title_end = tail.find("**").map(|rel| title_start.saturating_add(rel))?;
    Some((
        severity,
        complexity,
        first[title_start..title_end].to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::Severity;

    fn finding(file: &str, line: usize, severity: Severity) -> Finding {
        Finding {
            file: file.to_owned(),
            line,
            severity,
            complexity: 3,
            title: "Lock dropped early".to_owned(),
            body: "The guard is dropped before the read completes.".to_owned(),
        }
    }

    fn open_issue(file: &str, line: u64, severity: Severity, thread: &str) -> Issue {
        Issue {
            title: "Lock dropped early".to_owned(),
            file: file.to_owned(),
            line: Some(line),
            severity,
            complexity: 3,
            anchored: true,
            status: IssueStatus::Open,
            thread_id: Some(thread.to_owned()),
            raised_round: 1,
            raised_sha: "round1sha".to_owned(),
            last_round: 1,
            resolved_round: None,
            resolved_sha: None,
        }
    }

    fn thread(id: &str, resolved: bool) -> ReviewThread {
        ReviewThread {
            id: id.to_owned(),
            comment_id: 700,
            resolved,
            path: "src/alpha.rs".to_owned(),
            line: Some(2),
            original_line: Some(2),
        }
    }

    #[test]
    fn merge_marks_unregraounded_issues_fixed_with_the_round_sha()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_A")],
        };
        let grounded = vec![finding("src/beta.rs", 11, Severity::Nitpick)];
        let finding_threads = vec![None];
        let dropped: Vec<(Finding, &str)> = Vec::new();
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &grounded,
            finding_threads: &finding_threads,
            dropped: &dropped,
            threads: &[thread("T_A", false)],
        });
        assert_eq!(merged.round, 2);
        let fixed = merged.issues.first().ok_or("expected an issue")?;
        assert_eq!(fixed.status, IssueStatus::Fixed);
        assert_eq!(fixed.resolved_sha.as_deref(), Some("round2sha"));
        assert_eq!(
            merged.issues.get(1).map(|i| i.status.clone()),
            Some(IssueStatus::Open)
        );
        Ok(())
    }

    #[test]
    fn merge_keeps_regrounded_issues_open_and_refreshes_them()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_A")],
        };
        let grounded = vec![finding("src/alpha.rs", 2, Severity::Warning)];
        let finding_threads = vec![Some("T_A".to_owned())];
        let dropped: Vec<(Finding, &str)> = Vec::new();
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &grounded,
            finding_threads: &finding_threads,
            dropped: &dropped,
            threads: &[thread("T_A", false)],
        });
        let issue = merged.issues.first().ok_or("expected an issue")?;
        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(issue.last_round, 2);
        assert!(issue.resolved_sha.is_none());
        Ok(())
    }

    #[test]
    fn merge_flags_pre_resolved_threads_as_manually_resolved()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Nitpick, "T_A")],
        };
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[thread("T_A", true)],
        });
        let issue = merged.issues.first().ok_or("expected an issue")?;
        assert_eq!(issue.status, IssueStatus::ManuallyResolved);
        Ok(())
    }

    #[test]
    fn merge_matches_dropped_findings_to_open_unanchored_issues()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![Issue {
                title: "Wide net".to_owned(),
                file: "src/gone.rs".to_owned(),
                line: None,
                severity: Severity::Suggestion,
                complexity: 2,
                anchored: false,
                status: IssueStatus::Open,
                thread_id: None,
                raised_round: 1,
                raised_sha: "round1sha".to_owned(),
                last_round: 1,
                resolved_round: None,
                resolved_sha: None,
            }],
        };
        let dropped_finding = Finding {
            file: "src/gone.rs".to_owned(),
            line: 999,
            severity: Severity::Suggestion,
            complexity: 2,
            title: "Wide net".to_owned(),
            body: "…".to_owned(),
        };
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[(dropped_finding, "line outside the changed hunks")],
            threads: &[],
        });
        let issue = merged.issues.first().ok_or("expected an issue")?;
        assert_eq!(issue.status, IssueStatus::Open);
        assert!(!issue.anchored);
        assert_eq!(issue.last_round, 2);
        Ok(())
    }

    #[test]
    fn unresolved_lists_blockers_first_and_history_orders_by_fix_round() {
        let mut registry = Registry {
            round: 3,
            issues: vec![
                open_issue("src/a.rs", 1, Severity::Nitpick, "T_1"),
                open_issue("src/b.rs", 2, Severity::Warning, "T_2"),
                Issue {
                    resolved_round: Some(2),
                    resolved_sha: Some("fixedsha2".to_owned()),
                    status: IssueStatus::Fixed,
                    ..open_issue("src/c.rs", 3, Severity::Suggestion, "T_3")
                },
                Issue {
                    resolved_round: Some(1),
                    resolved_sha: Some("fixedsha1".to_owned()),
                    status: IssueStatus::ManuallyResolved,
                    ..open_issue("src/d.rs", 4, Severity::Nitpick, "T_4")
                },
            ],
        };
        if let Some(issue) = registry.issues.get_mut(3) {
            issue.raised_round = 1;
        }
        let unresolved = registry.unresolved();
        assert_eq!(
            unresolved
                .iter()
                .map(|issue| issue.severity)
                .collect::<Vec<_>>(),
            vec![Severity::Warning, Severity::Nitpick],
            "blockers sort before non-blockers"
        );
        let history = registry.fixed_history();
        assert_eq!(
            history
                .iter()
                .map(|issue| issue.resolved_round)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2)],
            "history orders by fix time"
        );
    }

    #[test]
    fn parse_issue_header_reads_the_comment_header_line() -> Result<(), Box<dyn std::error::Error>>
    {
        let body = "![warning](https://img.shields.io/badge/warning-orange) ![effort 3](https://img.shields.io/badge/effort_3-yellow) **Lock dropped early**\\n\\nThe guard is dropped.";
        let (severity, complexity, title) = parse_issue_header(body).ok_or("unparseable header")?;
        assert_eq!(severity, Severity::Warning);
        assert_eq!(complexity, 3);
        assert_eq!(title, "Lock dropped early");
        assert!(parse_issue_header("plain text without badges").is_none());
        Ok(())
    }

    #[test]
    fn the_embedded_registry_survives_arrows_in_titles_and_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry {
            round: 1,
            issues: Vec::new(),
        };
        registry.issues.push(Issue {
            title: "map `-->` to `->`".to_owned(),
            file: "stop --> drop.rs".to_owned(),
            ..open_issue("src/x.rs", 1, Severity::Nitpick, "T_X")
        });
        let body = "<!-- difftrace:verdict -->\n\n## Verdict\n…".to_owned();
        let embedded = embed_registry(&body, &registry)?;
        let start = embedded
            .find(REGISTRY_MARKER)
            .ok_or("registry block missing")?
            .saturating_add(REGISTRY_MARKER.len());
        let line_end = embedded
            .get(start..)
            .and_then(|tail| tail.find('\n'))
            .map(|rel| start.saturating_add(rel))
            .ok_or("registry block never ends its line")?;
        let block = embedded.get(start..line_end).ok_or("block out of range")?;
        let payload = block
            .strip_suffix(" -->")
            .ok_or("block must end with the terminator")?;
        assert!(
            !payload.contains("-->"),
            "no '-->' may appear inside the JSON payload: {payload}"
        );
        assert_eq!(
            extract_registry(&embedded).ok_or("registry must survive arrows")?,
            registry
        );
        Ok(())
    }

    #[test]
    fn a_dropped_reflag_keeps_the_anchored_issue_open() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_A")],
        };
        let dropped_finding = Finding {
            file: "src/alpha.rs".to_owned(),
            line: 999,
            severity: Severity::Warning,
            complexity: 3,
            title: "Lock dropped early".to_owned(),
            body: "…".to_owned(),
        };
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[(dropped_finding, "line outside the changed hunks")],
            threads: &[thread("T_A", false)],
        });
        assert_eq!(
            merged.issues.len(),
            1,
            "an out-of-hunk re-flag must not spawn a second issue or flip the anchored one fixed"
        );
        let issue = merged.issues.first().ok_or("issue")?;
        assert_eq!(issue.status, IssueStatus::Open);
        assert!(issue.anchored);
        assert_eq!(issue.last_round, 2);
        Ok(())
    }

    #[test]
    fn a_thread_match_refreshes_the_stale_thread_id() -> Result<(), Box<dyn std::error::Error>> {
        let mut seed = open_issue("src/alpha.rs", 2, Severity::Warning, "T_OLD_RESOLVED");
        seed.thread_id = Some("T_OLD_RESOLVED".to_owned());
        let registry = Registry {
            round: 1,
            issues: vec![seed],
        };
        let grounded = vec![finding("src/alpha.rs", 2, Severity::Warning)];
        let finding_threads = vec![Some("T_NEW".to_owned())];
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &grounded,
            finding_threads: &finding_threads,
            dropped: &[],
            threads: &[],
        });
        assert_eq!(
            merged.issues.first().ok_or("issue")?.thread_id.as_deref(),
            Some("T_NEW"),
            "the issue adopts the live thread so a later fix records as fixed, not manually resolved"
        );
        Ok(())
    }

    #[test]
    fn embed_and_extract_registry_round_trip_in_the_comment_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 2,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_A")],
        };
        let body = "<!-- difftrace:verdict -->\\n\\n## Verdict\\n…".to_owned();
        let embedded = embed_registry(&body, &registry)?;
        assert!(embedded.contains(REGISTRY_MARKER));
        assert!(embedded.contains("## Verdict"));
        assert_eq!(
            extract_registry(&embedded).ok_or("registry must round-trip")?,
            registry
        );
        let rewritten = embed_registry(
            &embedded,
            &Registry {
                round: 3,
                issues: Vec::new(),
            },
        )?;
        assert_eq!(
            extract_registry(&rewritten)
                .ok_or("rewritten registry must round-trip")?
                .round,
            3
        );
        assert_eq!(
            rewritten.find(REGISTRY_MARKER),
            rewritten.rfind(REGISTRY_MARKER)
        );
        Ok(())
    }
}
