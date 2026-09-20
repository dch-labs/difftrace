//! The cross-round issue registry: every issue difftrace ever raised on a
//! pull request, kept as hidden JSON inside the marker comment and merged
//! each run. Issues match across rounds by file, nearby line, and shared
//! wording — not exact lines and titles — a retired issue the model
//! re-finds reopens instead of duplicating, and an unseen issue closes
//! only after two consecutive full rounds stop re-raising it, because
//! per-pass recall misses real issues. The visible lists render from the
//! state.

use crate::findings::Finding;
use crate::findings::Severity;
use crate::github::ReviewThread;
use crate::review::identity::same_spot;
use crate::review::identity::shares_any_token;

pub(crate) const REGISTRY_MARKER: &str = "<!-- difftrace:registry ";

const RETIRE_AFTER_CONSECUTIVE_MISSES: u32 = 2;

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
    #[serde(default)]
    pub misses: u32,
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
    pub retired: &'a [String],
    pub unreviewed_files: &'a [String],
    pub narrowed: bool,
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
        let original_len = self.issues.len();
        let mut seen: Vec<bool> = self.issues.iter().map(|_| false).collect();
        self.ingest_grounded(round, round_no, &mut seen);
        self.ingest_dropped(round, round_no, &mut seen);
        self.retire_unseen(round, round_no, original_len, &seen);
        self
    }

    fn ingest_grounded(&mut self, round: &RoundFindings<'_>, round_no: u32, seen: &mut Vec<bool>) {
        for (finding, thread_id) in grounded_with_threads(round.grounded, round.finding_threads) {
            let index = self.match_open_index(finding, thread_id, round.retired, round_no);
            if let Some(i) = index {
                tracing::debug!(
                    target: "difftrace::registry",
                    decision = "re-raised",
                    file = %finding.file,
                    line = finding.line,
                    title = %finding.title,
                    "registry merge matched an open issue"
                );
                let Some(issue) = self.issues.get_mut(i) else {
                    continue;
                };
                issue.last_round = round_no;
                issue.title.clone_from(&finding.title);
                issue.severity = finding.severity;
                issue.complexity = finding.complexity;
                issue.line = Some(finding.line as u64);
                issue.misses = 0;
                if let Some(id) = thread_id {
                    issue.thread_id = Some(id.to_owned());
                }
                if let Some(flag) = seen.get_mut(i) {
                    *flag = true;
                }
            } else if let Some(i) = self.match_retired_index(finding, round.retired) {
                tracing::debug!(
                    target: "difftrace::registry",
                    decision = "reopened",
                    file = %finding.file,
                    line = finding.line,
                    title = %finding.title,
                    "registry merge reopened a retired issue"
                );
                let Some(issue) = self.issues.get_mut(i) else {
                    continue;
                };
                issue.status = IssueStatus::Open;
                issue.title.clone_from(&finding.title);
                issue.severity = finding.severity;
                issue.complexity = finding.complexity;
                issue.line = Some(finding.line as u64);
                issue.misses = 0;
                issue.last_round = round_no;
                issue.resolved_round = None;
                issue.resolved_sha = None;
                if let Some(id) = thread_id {
                    issue.thread_id = Some(id.to_owned());
                }
                if let Some(flag) = seen.get_mut(i) {
                    *flag = true;
                }
            } else {
                tracing::debug!(
                    target: "difftrace::registry",
                    decision = "new",
                    file = %finding.file,
                    line = finding.line,
                    title = %finding.title,
                    "registry merge raised a new issue"
                );
                seen.push(true);
                self.issues
                    .push(raised_issue(finding, thread_id, round_no, round.head_sha));
            }
        }
    }

    fn ingest_dropped(&mut self, round: &RoundFindings<'_>, round_no: u32, seen: &mut [bool]) {
        for (finding, _reason) in round.dropped {
            let Some(i) = self.issues.iter().position(|issue| {
                issue.status == IssueStatus::Open
                    && issue.file == finding.file
                    && shares_any_token(&issue.title, &finding.title)
            }) else {
                // A dropped finding matching no open issue stays out of the
                // registry: it cites a line this round never touched, so it
                // is a pre-existing observation, not tracked round state.
                tracing::debug!(
                    target: "difftrace::registry",
                    decision = "unanchored ignored",
                    file = %finding.file,
                    title = %finding.title,
                    "registry merge left an unmatched unanchored finding out"
                );
                continue;
            };
            tracing::debug!(
                target: "difftrace::registry",
                decision = "unanchored re-raised",
                file = %finding.file,
                title = %finding.title,
                "registry merge matched an unanchored issue"
            );
            if let Some(issue) = self.issues.get_mut(i) {
                issue.last_round = round_no;
                issue.misses = 0;
            }
            if let Some(flag) = seen.get_mut(i) {
                *flag = true;
            }
        }
    }

    fn retire_unseen(
        &mut self,
        round: &RoundFindings<'_>,
        round_no: u32,
        original_len: usize,
        seen: &[bool],
    ) {
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
            if round
                .unreviewed_files
                .iter()
                .any(|file| file == &issue.file)
            {
                continue;
            }
            let manually = issue
                .thread_id
                .as_ref()
                .is_some_and(|id| resolved_threads.iter().any(|t| &t.id == id));
            if issue
                .thread_id
                .as_ref()
                .is_some_and(|id| round.retired.iter().any(|retired_id| retired_id == id))
            {
                // The thread was displaced by a different issue reported at
                // its anchor this round — replacement, not absence, so the
                // recorded issue retires immediately.
                issue.status = IssueStatus::Fixed;
                tracing::debug!(
                    target: "difftrace::registry",
                    decision = "displaced",
                    file = %issue.file,
                    title = %issue.title,
                    round = round_no,
                    "registry merge retired a displaced issue"
                );
                issue.resolved_round = Some(round_no);
                issue.resolved_sha = Some(round.head_sha.to_owned());
                continue;
            }
            if manually {
                issue.status = IssueStatus::ManuallyResolved;
                tracing::debug!(
                    target: "difftrace::registry",
                    decision = "manually resolved",
                    file = %issue.file,
                    title = %issue.title,
                    round = round_no,
                    "registry merge recorded a manual resolution"
                );
                issue.resolved_round = Some(round_no);
                issue.resolved_sha = Some(round.head_sha.to_owned());
                continue;
            }
            if round.narrowed {
                // A narrowed round (focus or severity floor set) cannot
                // re-raise out-of-scope issues; absence is not evidence
                // of a fix, so the issue stays open for a full round.
                continue;
            }
            // A full round that covered the file and stopped re-raising the
            // issue is one miss; per-pass recall misses real issues, so a
            // single absence retires nothing.
            issue.misses = issue.misses.saturating_add(1);
            if issue.misses < RETIRE_AFTER_CONSECUTIVE_MISSES {
                tracing::debug!(
                    target: "difftrace::registry",
                    decision = "missed",
                    file = %issue.file,
                    title = %issue.title,
                    round = round_no,
                    misses = issue.misses,
                    "registry merge counts one miss; the issue stays open"
                );
                continue;
            }
            issue.status = IssueStatus::Fixed;
            tracing::debug!(
                target: "difftrace::registry",
                decision = "fixed",
                file = %issue.file,
                title = %issue.title,
                round = round_no,
                "registry merge retired an unseen issue"
            );
            issue.resolved_round = Some(round_no);
            issue.resolved_sha = Some(round.head_sha.to_owned());
        }
    }

    fn match_open_index(
        &self,
        finding: &Finding,
        thread_id: Option<&str>,
        retired: &[String],
        round_no: u32,
    ) -> Option<usize> {
        if let Some(id) = thread_id
            && let Some(i) = self.issues.iter().position(|issue| {
                issue.status == IssueStatus::Open && issue.thread_id.as_deref() == Some(id)
            })
        {
            return Some(i);
        }
        self.issues.iter().position(|issue| {
            issue.status == IssueStatus::Open
                && spot_matches(issue, finding)
                // The fuzzy identity dedupes across rounds; two findings
                // raised in the same round are distinct locations of one
                // group, each tracked as its own issue, unless identical.
                && (issue.last_round < round_no || exact_spot(issue, finding))
                && issue
                    .thread_id
                    .as_deref()
                    .is_none_or(|id| !retired.iter().any(|retired_id| retired_id == id))
        })
    }

    fn match_retired_index(&self, finding: &Finding, retired: &[String]) -> Option<usize> {
        self.issues.iter().position(|issue| {
            issue.status != IssueStatus::Open
                && spot_matches(issue, finding)
                && issue
                    .thread_id
                    .as_deref()
                    .is_none_or(|id| !retired.iter().any(|retired_id| retired_id == id))
        })
    }
}

fn spot_matches(issue: &Issue, finding: &Finding) -> bool {
    issue.anchored
        && issue.file == finding.file
        && issue
            .line
            .is_some_and(|line| same_spot(line, &issue.title, finding.line as u64, &finding.title))
}

fn exact_spot(issue: &Issue, finding: &Finding) -> bool {
    issue.file == finding.file
        && issue.line == Some(finding.line as u64)
        && normalize_title(&issue.title) == normalize_title(&finding.title)
}

// The shared issue-identity key: trimmed, case-folded over Unicode.
pub(crate) fn normalize_title(title: &str) -> String {
    title.trim().to_lowercase()
}

pub(crate) fn same_issue_title(a: &str, b: &str) -> bool {
    crate::review::identity::titles_overlap_enough(a, b)
}

fn raised_issue(finding: &Finding, thread_id: Option<&str>, round_no: u32, sha: &str) -> Issue {
    Issue {
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
        misses: 0,
        resolved_round: None,
        resolved_sha: None,
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

#[must_use]
pub fn short_sha(sha: &str) -> String {
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
            kind: crate::config::IssueKind::Correctness,
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
            misses: 0,
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
    fn raised_issues_carry_their_round_and_anchor_shape() {
        let raised = raised_issue(
            &finding("src/a.rs", 3, Severity::Warning),
            Some("T9"),
            2,
            "abc1234",
        );
        assert_eq!(raised.thread_id.as_deref(), Some("T9"));
        assert!(raised.anchored);
        assert_eq!(raised.line, Some(3));
        assert_eq!(raised.status, IssueStatus::Open);
        assert_eq!(raised.raised_round, 2);
        assert_eq!(raised.raised_sha, "abc1234");
        assert_eq!(raised.last_round, 2);
        assert_eq!(raised.resolved_round, None);
    }

    #[test]
    fn an_unmatched_out_of_hunk_finding_never_becomes_a_registry_issue() {
        let registry = Registry {
            round: 1,
            issues: vec![],
        };
        let stray = Finding {
            file: "src/old.rs".to_owned(),
            line: 400,
            kind: crate::config::IssueKind::Correctness,
            severity: Severity::Warning,
            complexity: 2,
            title: "Ancient debt".to_owned(),
            body: "Pre-dates this branch.".to_owned(),
        };
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[(stray, "line outside the changed hunks")],
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        });
        assert!(
            merged.issues.is_empty(),
            "a pre-existing observation is round documentation, not tracked state"
        );
    }

    #[test]
    fn retire_unseen_resolves_through_the_threads_resolution_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry {
            round: 1,
            issues: vec![
                open_issue("src/alpha.rs", 2, Severity::Warning, "T_OPEN"),
                open_issue("src/beta.rs", 5, Severity::Warning, "T_GONE"),
            ],
        };
        let round = RoundFindings {
            head_sha: "round2sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[thread("T_GONE", true)],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        };
        registry.retire_unseen(&round, 2, 2, &[]);
        let first = registry
            .issues
            .first()
            .ok_or("the open issue stays in the registry")?;
        let second = registry
            .issues
            .get(1)
            .ok_or("the raised issue is appended")?;
        assert_eq!(
            first.status,
            IssueStatus::Open,
            "one covered absence is a miss, not a fix"
        );
        assert_eq!(first.misses, 1);
        assert_eq!(
            second.status,
            IssueStatus::ManuallyResolved,
            "a thread resolved by hand records the manual outcome"
        );
        let round_after = RoundFindings {
            head_sha: "round3sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        };
        registry.retire_unseen(&round_after, 3, 2, &[]);
        let first = registry
            .issues
            .first()
            .ok_or("the missed issue stays in the registry")?;
        assert_eq!(first.status, IssueStatus::Fixed);
        assert_eq!(first.resolved_sha.as_deref(), Some("round3sha"));
        assert_eq!(first.resolved_round, Some(3));
        Ok(())
    }

    #[test]
    fn retire_unseen_never_touches_freshly_raised_issues() {
        let mut registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_OLD")],
        };
        let raised = raised_issue(&finding("src/new.rs", 7, Severity::Nitpick), None, 2, "s2");
        registry.issues.push(raised);
        let round = RoundFindings {
            head_sha: "s2",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        };
        registry.retire_unseen(&round, 2, 1, &[false]);
        let statuses: Vec<IssueStatus> = registry
            .issues
            .iter()
            .map(|issue| issue.status.clone())
            .collect();
        assert_eq!(
            statuses,
            vec![IssueStatus::Open, IssueStatus::Open],
            "one miss retires nothing, and the freshly raised issue is never counted"
        );
        let round_after = RoundFindings {
            head_sha: "s3",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        };
        registry.retire_unseen(&round_after, 3, 2, &[]);
        let old = registry.issues.first();
        let fresh = registry.issues.get(1);
        assert_eq!(
            old.map(|issue| issue.status.clone()),
            Some(IssueStatus::Fixed),
            "the second consecutive miss retires the old issue"
        );
        assert_eq!(
            fresh.map(|issue| (issue.status.clone(), issue.misses)),
            Some((IssueStatus::Open, 1)),
            "the freshly raised issue is on its first miss, still open"
        );
    }

    #[test]
    fn merge_marks_unregraunded_issues_fixed_with_the_round_sha()
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
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        });
        assert_eq!(merged.round, 2);
        let held = merged.issues.first().ok_or("expected an issue")?;
        assert_eq!(
            (held.status.clone(), held.misses),
            (IssueStatus::Open, 1),
            "the first full round without a re-raise counts one miss"
        );
        assert_eq!(
            merged.issues.get(1).map(|i| i.status.clone()),
            Some(IssueStatus::Open)
        );
        let concluded = merged.merge(&RoundFindings {
            head_sha: "round3sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        });
        assert_eq!(concluded.round, 3);
        let fixed = concluded.issues.first().ok_or("expected an issue")?;
        assert_eq!(fixed.status, IssueStatus::Fixed);
        assert_eq!(fixed.resolved_sha.as_deref(), Some("round3sha"));
        assert_eq!(fixed.resolved_round, Some(3));
        Ok(())
    }

    #[test]
    fn an_open_issue_on_an_unreviewed_file_survives_a_partial_merge()
    -> Result<(), Box<dyn std::error::Error>> {
        // A partial round saw nothing on the file — "not re-raised"
        // cannot read as "fixed" there, or the registry would falsify
        // history and reset the issue's identity on the next full round.
        let registry = Registry {
            round: 1,
            issues: vec![
                open_issue("src/gone.rs", 3, Severity::Warning, "T_GONE"),
                open_issue("src/seen.rs", 4, Severity::Warning, "T_SEEN"),
            ],
        };
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[thread("T_GONE", false), thread("T_SEEN", false)],
            retired: &[],
            unreviewed_files: &["src/gone.rs".to_owned()],
            narrowed: false,
        });
        let gone = merged
            .issues
            .first()
            .ok_or("expected the unreviewed issue")?;
        assert_eq!(
            gone.status,
            IssueStatus::Open,
            "an unreviewed file cannot conclude its issues fixed"
        );
        assert_eq!(gone.resolved_sha, None, "no fixing commit may be recorded");
        assert_eq!(gone.last_round, 1, "the untouched issue keeps its history");
        let seen = merged.issues.get(1).ok_or("expected the reviewed issue")?;
        assert_eq!(
            (seen.status.clone(), seen.misses),
            (IssueStatus::Open, 1),
            "a reviewed file that re-raised nothing counts one miss, not a fix"
        );
        let concluded = merged.merge(&RoundFindings {
            head_sha: "round3sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[],
            retired: &[],
            unreviewed_files: &["src/gone.rs".to_owned()],
            narrowed: false,
        });
        let gone = concluded
            .issues
            .first()
            .ok_or("expected the unreviewed issue")?;
        assert_eq!(
            gone.status,
            IssueStatus::Open,
            "an unreviewed file still cannot conclude its issues fixed"
        );
        assert_eq!(gone.misses, 0, "an unreviewed file accrues no misses");
        let seen = concluded
            .issues
            .get(1)
            .ok_or("expected the reviewed issue")?;
        assert_eq!(
            seen.status,
            IssueStatus::Fixed,
            "a second full round without a re-raise concludes fixed"
        );
        Ok(())
    }

    #[test]
    fn a_narrowed_round_never_retires_issues_it_cannot_re_raise() {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Nitpick, "T_A")],
        };
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: true,
        });
        assert!(
            merged
                .issues
                .first()
                .is_some_and(|issue| issue.status == IssueStatus::Open),
            "a focused or floored round did not look for this issue; it stays open"
        );
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
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        });
        let issue = merged.issues.first().ok_or("expected an issue")?;
        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(issue.last_round, 2);
        assert!(issue.resolved_sha.is_none());
        Ok(())
    }

    #[test]
    fn merge_records_the_replaced_issue_fixed_when_its_thread_is_retired()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_A")],
        };
        let mut fresh = finding("src/alpha.rs", 2, Severity::Warning);
        fresh.title = "A different issue".to_owned();
        let grounded = vec![fresh];
        let finding_threads = vec![None];
        let dropped: Vec<(Finding, &str)> = Vec::new();
        let retired = vec!["T_A".to_owned()];
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &grounded,
            finding_threads: &finding_threads,
            dropped: &dropped,
            threads: &[thread("T_A", false)],
            retired: &retired,
            unreviewed_files: &[],
            narrowed: false,
        });
        let replaced = merged.issues.first().ok_or("expected the old issue")?;
        assert_eq!(
            replaced.status,
            IssueStatus::Fixed,
            "the retired thread's issue is fixed, not overwritten"
        );
        assert_eq!(replaced.title, "Lock dropped early");
        assert_eq!(replaced.resolved_sha.as_deref(), Some("round2sha"));
        assert_eq!(replaced.resolved_round, Some(2));
        let raised = merged.issues.get(1).ok_or("expected the fresh issue")?;
        assert_eq!(raised.status, IssueStatus::Open);
        assert_eq!(raised.title, "A different issue");
        assert_eq!(raised.thread_id, None);
        assert_eq!(raised.raised_round, 2);
        Ok(())
    }

    #[test]
    fn merge_does_not_glue_a_differently_titled_finding_through_the_positional_arm()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_A")],
        };
        let mut other = finding("src/alpha.rs", 2, Severity::Warning);
        other.title = "A different issue".to_owned();
        let grounded = vec![other];
        let finding_threads = vec![None];
        let dropped: Vec<(Finding, &str)> = Vec::new();
        let merged = registry.merge(&RoundFindings {
            head_sha: "round2sha",
            grounded: &grounded,
            finding_threads: &finding_threads,
            dropped: &dropped,
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        });
        let held = merged.issues.first().ok_or("expected the open issue")?;
        assert_eq!(
            (held.status.clone(), held.misses),
            (IssueStatus::Open, 1),
            "not re-raised, the issue takes a miss — never overwritten"
        );
        assert_eq!(held.title, "Lock dropped early");
        assert_eq!(held.resolved_sha, None);
        let raised = merged.issues.get(1).ok_or("expected the fresh issue")?;
        assert_eq!(raised.status, IssueStatus::Open);
        assert_eq!(raised.title, "A different issue");
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
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
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
                misses: 0,
                resolved_round: None,
                resolved_sha: None,
            }],
        };
        let dropped_finding = Finding {
            file: "src/gone.rs".to_owned(),
            line: 999,
            kind: crate::config::IssueKind::Correctness,
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
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
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
            kind: crate::config::IssueKind::Correctness,
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
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
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
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
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

    fn empty_round(sha: &'static str) -> RoundFindings<'static> {
        RoundFindings {
            head_sha: sha,
            grounded: &[],
            finding_threads: &[],
            dropped: &[],
            threads: &[],
            retired: &[],
            unreviewed_files: &[],
            narrowed: false,
        }
    }

    #[test]
    fn a_rephrased_refind_at_a_drifted_line_refreshes_the_standing_issue()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/a.rs", 40, Severity::Warning, "T_A")],
        };
        let mut drifted = finding("src/a.rs", 43, Severity::Warning);
        drifted.title = "The lock is dropped early".to_owned();
        let merged = registry.merge(&RoundFindings {
            finding_threads: &[None],
            grounded: &[drifted],
            ..empty_round("round2sha")
        });
        assert_eq!(
            merged.issues.len(),
            1,
            "a paraphrase at a nearby line matches its issue"
        );
        let issue = merged.issues.first().ok_or("issue")?;
        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(issue.title, "The lock is dropped early");
        assert_eq!(issue.line, Some(43));
        assert_eq!(issue.misses, 0);
        Ok(())
    }

    #[test]
    fn two_phrasings_at_the_same_line_are_one_issue() {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/a.rs", 12, Severity::Warning, "T_A")],
        };
        let mut reworded = finding("src/a.rs", 12, Severity::Warning);
        reworded.title = "The lock is dropped while a reader waits".to_owned();
        let merged = registry.merge(&RoundFindings {
            finding_threads: &[None],
            grounded: &[reworded],
            ..empty_round("round2sha")
        });
        assert_eq!(
            merged.issues.len(),
            1,
            "the same line reporting the same subject twice is one issue"
        );
    }

    #[test]
    fn a_far_drifted_refind_with_a_reworded_title_is_new() {
        let registry = Registry {
            round: 1,
            issues: vec![open_issue("src/a.rs", 40, Severity::Warning, "T_A")],
        };
        let mut far = finding("src/a.rs", 80, Severity::Warning);
        far.title = "The lock is dropped early".to_owned();
        let merged = registry.merge(&RoundFindings {
            finding_threads: &[None],
            grounded: &[far],
            ..empty_round("round2sha")
        });
        assert_eq!(
            merged.issues.len(),
            2,
            "forty lines away the identity does not stretch"
        );
    }

    #[test]
    fn a_fixed_issue_the_model_refinds_reopens_rather_than_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let start = Registry {
            round: 1,
            issues: vec![open_issue("src/a.rs", 10, Severity::Warning, "T_A")],
        };
        let once = start.merge(&empty_round("round2sha"));
        let twice = once.merge(&empty_round("round3sha"));
        let fixed = twice.issues.first().ok_or("issue")?;
        assert_eq!(fixed.status, IssueStatus::Fixed);
        let mut back = finding("src/a.rs", 12, Severity::Warning);
        back.title = "Lock dropped early again".to_owned();
        let merged = twice.merge(&RoundFindings {
            finding_threads: &[None],
            grounded: &[back],
            ..empty_round("round4sha")
        });
        assert_eq!(
            merged.issues.len(),
            1,
            "the known issue reopens instead of duplicating"
        );
        let issue = merged.issues.first().ok_or("issue")?;
        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(issue.resolved_round, None);
        assert_eq!(issue.resolved_sha, None);
        assert_eq!(issue.title, "Lock dropped early again");
        assert_eq!(issue.misses, 0);
        Ok(())
    }

    #[test]
    fn a_suppressed_rephrased_refind_resets_the_miss_count()
    -> Result<(), Box<dyn std::error::Error>> {
        let once = Registry {
            round: 1,
            issues: vec![open_issue("src/a.rs", 10, Severity::Warning, "T_A")],
        }
        .merge(&empty_round("round2sha"));
        assert_eq!(once.issues.first().ok_or("issue")?.misses, 1);
        let mut moved = finding("src/a.rs", 400, Severity::Warning);
        moved.title = "The lock is dropped early again".to_owned();
        let merged = once.merge(&RoundFindings {
            dropped: &[(moved, crate::tools::submit::OUT_OF_HUNK)],
            ..empty_round("round3sha")
        });
        let issue = merged.issues.first().ok_or("issue")?;
        assert_eq!(
            (issue.status.clone(), issue.misses),
            (IssueStatus::Open, 0),
            "a suppressed re-report is presence, not absence"
        );
        Ok(())
    }

    #[test]
    fn the_registry_parses_state_from_before_the_misses_field()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry {
            round: 2,
            issues: vec![open_issue("src/alpha.rs", 2, Severity::Warning, "T_A")],
        };
        let legacy = serde_json::to_string(&registry)?.replace(",\"misses\":0", "");
        assert!(
            !legacy.contains("misses"),
            "the legacy fixture must omit the field"
        );
        let parsed: Registry = serde_json::from_str(&legacy)?;
        assert_eq!(
            parsed.issues.first().ok_or("issue")?.misses,
            0,
            "absent misses reads as zero"
        );
        assert_eq!(parsed, registry);
        Ok(())
    }
}
