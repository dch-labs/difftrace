//! The local findings ledger: cumulative review state per repository, so
//! repeated rounds converge instead of re-sampling — new findings append,
//! findings a clean round stops re-finding retire as fixed, and the round
//! reports `new / fixed / standing` against the whole history. A partial
//! round never retires anything: absence is not evidence when files went
//! unreviewed.

use std::path::Path;

use crate::findings::Finding;
use crate::findings::Severity;
use crate::review::identity::same_spot;
use crate::review::identity::shares_any_token;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LedgerIssue {
    pub file: String,
    pub line: u64,
    pub title: String,
    pub severity: Severity,
    pub first_sha: String,
    pub last_sha: String,
    #[serde(default)]
    pub misses: u32,
    pub fixed: Option<String>,
}

pub struct LocalMergeInputs {
    pub rereported: Vec<Finding>,
    pub may_retire: bool,
}

// The same narrowed-round rule the hosted registry applies: a round with
// a focus or a severity floor cannot re-raise out-of-scope issues, and a
// partial round cannot re-raise unreviewed ones, so absence is not
// evidence of a fix; every dropped re-report — whatever its reason — is
// presence evidence.
#[must_use]
pub fn local_merge_inputs(
    outcome: &crate::review::ReviewOutcome,
    narrowed_round: bool,
) -> LocalMergeInputs {
    LocalMergeInputs {
        rereported: outcome
            .dropped
            .iter()
            .map(|entry| entry.finding.clone())
            .collect(),
        may_retire: outcome.unreviewed_batches.is_empty() && !narrowed_round,
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ledger {
    pub issues: Vec<LedgerIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerReport {
    pub new: usize,
    pub fixed: usize,
    pub standing: usize,
    pub fixed_all_time: usize,
}

impl LedgerReport {
    #[must_use]
    pub fn line(self) -> String {
        format!(
            "📊 ledger: +{} new · {} fixed this round · {} standing ({} fixed all-time)",
            self.new, self.fixed, self.standing, self.fixed_all_time
        )
    }
}

const RETIRE_AFTER_CONSECUTIVE_MISSES: u32 = 2;

fn same_issue(issue: &LedgerIssue, finding: &Finding) -> bool {
    issue.file == finding.file
        && same_spot(
            issue.line,
            &issue.title,
            finding.line as u64,
            &finding.title,
        )
}

impl Ledger {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(
                    target: "difftrace::ledger",
                    error = %err,
                    "ledger unreadable; starting an empty one"
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    #[must_use]
    pub fn merge(
        &mut self,
        findings: &[Finding],
        rereported: &[Finding],
        sha: &str,
        may_retire: bool,
    ) -> LedgerReport {
        let sha = sha.to_owned();
        let original_len = self.issues.len();
        let mut seen = vec![false; original_len];
        for finding in findings {
            for (index, issue) in self.issues.iter_mut().enumerate() {
                if same_issue(issue, finding)
                    && let Some(slot) = seen.get_mut(index)
                {
                    issue.last_sha.clone_from(&sha);
                    issue.line = finding.line as u64;
                    issue.severity = finding.severity;
                    issue.fixed = None;
                    issue.misses = 0;
                    *slot = true;
                    break;
                }
            }
        }
        let mut new = 0usize;
        for finding in findings {
            if !self
                .issues
                .iter()
                .any(|issue| issue.fixed.is_none() && same_issue(issue, finding))
            {
                new = new.saturating_add(1);
                self.issues.push(LedgerIssue {
                    file: finding.file.clone(),
                    line: finding.line as u64,
                    title: finding.title.clone(),
                    severity: finding.severity,
                    first_sha: sha.clone(),
                    last_sha: sha.clone(),
                    misses: 0,
                    fixed: None,
                });
            }
        }
        let mut fixed = 0usize;
        if may_retire {
            for (index, issue) in self.issues.iter_mut().enumerate() {
                let still_reported = rereported.iter().any(|finding| {
                    issue.file == finding.file && shares_any_token(&issue.title, &finding.title)
                });
                if index < original_len
                    && issue.fixed.is_none()
                    && !seen.get(index).copied().unwrap_or(false)
                    && !still_reported
                {
                    issue.misses = issue.misses.saturating_add(1);
                    if issue.misses >= RETIRE_AFTER_CONSECUTIVE_MISSES {
                        issue.fixed = Some(sha.clone());
                        fixed = fixed.saturating_add(1);
                    }
                }
            }
        }
        let standing = self
            .issues
            .iter()
            .filter(|issue| issue.fixed.is_none())
            .count();
        LedgerReport {
            new,
            fixed,
            standing,
            fixed_all_time: self
                .issues
                .iter()
                .filter(|issue| issue.fixed.is_some())
                .count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IssueKind;
    use crate::findings::ReviewSummary;
    use crate::review::ReviewOutcome;
    use crate::tools::submit::DroppedFinding;

    fn bare_outcome(
        dropped: Vec<DroppedFinding>,
        unreviewed_batches: Vec<String>,
    ) -> ReviewOutcome {
        ReviewOutcome {
            summary: ReviewSummary {
                summary: String::new(),
                risk_notes: Vec::new(),
                tests: String::new(),
            },
            findings: Vec::new(),
            comments: Vec::new(),
            dropped,
            below_floor: 0,
            out_of_focus: 0,
            verified_out: Vec::new(),
            fixed_this_round: Vec::new(),
            unreviewed_batches,
            pr: 0,
            head_sha: String::new(),
            posted: false,
            round_body: String::new(),
            standing_body: String::new(),
        }
    }

    fn finding(file: &str, line: usize, title: &str) -> Finding {
        Finding {
            file: file.to_owned(),
            line,
            severity: Severity::Warning,
            kind: IssueKind::Correctness,
            complexity: 3,
            title: title.to_owned(),
            body: "Detail.".to_owned(),
        }
    }

    #[test]
    fn the_local_merge_inputs_count_every_drop_as_presence_and_gate_retirement() {
        let dropped = vec![
            DroppedFinding {
                finding: finding("src/a.rs", 10, "Cap-ranked re-report of a standing issue"),
                reason: "per-file finding cap reached",
            },
            DroppedFinding {
                finding: finding("src/b.rs", 3, "Out-of-hunk re-report of a standing issue"),
                reason: crate::tools::submit::OUT_OF_HUNK,
            },
        ];
        let full = bare_outcome(dropped, Vec::new());
        let inputs = local_merge_inputs(&full, false);
        assert!(inputs.may_retire, "a full round may retire");
        assert_eq!(
            inputs.rereported.len(),
            2,
            "cap-dropped re-reports count as presence like every other drop"
        );
        assert!(
            !local_merge_inputs(&full, true).may_retire,
            "a narrowed round never retires"
        );
        let partial = bare_outcome(Vec::new(), vec!["src/c.rs, src/d.rs".to_owned()]);
        assert!(
            !local_merge_inputs(&partial, false).may_retire,
            "a partial round never retires"
        );
    }

    #[test]
    fn a_rephrased_refind_at_a_shifted_line_is_not_new() {
        let mut ledger = Ledger::default();
        let first = ledger.merge(
            &[finding("src/a.rs", 40, "Lock is dropped too early")],
            &[],
            "s1",
            true,
        );
        assert_eq!(first.new, 1);
        let second = ledger.merge(
            &[finding("src/a.rs", 43, "The lock is dropped early")],
            &[],
            "s2",
            true,
        );
        assert_eq!(
            second.new, 0,
            "a paraphrase at a nearby line matches its issue"
        );
        assert_eq!(second.fixed, 0);
        assert_eq!(second.standing, 1);
    }

    #[test]
    fn a_single_missed_round_does_not_retire_but_two_consecutive_do() {
        let mut ledger = Ledger::default();
        let _unused = ledger.merge(&[finding("src/a.rs", 10, "Off by one")], &[], "s1", true);
        let once = ledger.merge(&[], &[], "s2", true);
        assert_eq!(
            (once.fixed, once.standing),
            (0, 1),
            "one absence is not evidence: per-pass recall misses real issues"
        );
        let twice = ledger.merge(&[], &[], "s3", true);
        assert_eq!(
            (twice.fixed, twice.standing),
            (1, 0),
            "two consecutive covered absences retire the issue"
        );
        assert_eq!(twice.fixed_all_time, 1);
    }

    #[test]
    fn a_partial_round_never_retires() {
        let mut ledger = Ledger::default();
        let _unused = ledger.merge(&[finding("src/a.rs", 10, "Off by one")], &[], "s1", true);
        let report = ledger.merge(&[], &[], "s2", false);
        assert_eq!(report.fixed, 0, "absence without coverage is not a fix");
        assert_eq!(report.standing, 1);
    }

    #[test]
    fn a_fixed_issue_reappearing_reopens_rather_than_duplicates() {
        let mut ledger = Ledger::default();
        let _unused = ledger.merge(&[finding("src/a.rs", 10, "Off by one")], &[], "s1", true);
        let _unused = ledger.merge(&[], &[], "s2", true);
        let report = ledger.merge(
            &[finding("src/a.rs", 12, "Off by one again")],
            &[],
            "s3",
            true,
        );
        assert_eq!(
            report.new, 0,
            "the known issue reopens instead of duplicating"
        );
        assert_eq!(report.standing, 1);
    }

    #[test]
    fn two_phrasings_at_the_same_line_are_one_issue() {
        let mut ledger = Ledger::default();
        let _unused = ledger.merge(
            &[finding(
                "Cargo.lock",
                712,
                "windows-sys downgraded across seven transitive crates",
            )],
            &[],
            "s1",
            true,
        );
        let report = ledger.merge(
            &[finding(
                "Cargo.lock",
                712,
                "Cargo.lock windows-sys pins are inconsistent with their manifests",
            )],
            &[],
            "s2",
            true,
        );
        assert_eq!(
            (report.new, report.standing),
            (0, 1),
            "the same line reporting the same subject twice is one issue"
        );
    }

    #[test]
    fn a_suppressed_refind_on_an_unchanged_line_prevents_retirement() {
        let mut ledger = Ledger::default();
        let _unused = ledger.merge(
            &[finding(
                "src/provider.rs",
                394,
                "api.max_tokens is ignored for OpenAI profiles",
            )],
            &[],
            "s1",
            true,
        );
        let moved = finding(
            "src/provider.rs",
            563,
            "api.max_tokens is silently ignored for the OpenAI profile family",
        );
        let once = ledger.merge(&[], &[moved], "s2", true);
        assert_eq!(
            (once.fixed, once.standing),
            (0, 1),
            "a suppressed re-report is presence, not absence"
        );
    }

    #[test]
    fn the_ledger_round_trips_through_disk() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("difftrace-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("ledger.json");
        let mut ledger = Ledger::default();
        let _unused = ledger.merge(
            &[finding("src/a.rs", 3, "Races with shutdown")],
            &[],
            "s1",
            true,
        );
        ledger.save(&path)?;
        let back = Ledger::load(&path);
        let _unused = std::fs::remove_file(&path);
        let _unused = std::fs::remove_dir(&dir);
        assert_eq!(back, ledger);
        Ok(())
    }
}
