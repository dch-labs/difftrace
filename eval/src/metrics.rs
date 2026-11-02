//! M1–M5 metric definitions over golden findings and mutation hits.

use std::collections::BTreeSet;

use crate::correlate::{GoldenFinding, Label, title_tokens};
use crate::extract::{Finding, Severity};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Metrics {
    pub m1_false_blockers: f64,
    pub m2_idempotent: Option<bool>,
    pub m3_rounds_to_zero: Option<u32>,
    pub m4_recall: Option<f64>,
    pub m5_wall_secs: u64,
    pub m5_tokens: Option<u64>,
}

const LINE_DRIFT_TOLERANCE: u64 = 5;

#[must_use]
pub fn is_blocking(severity: Severity) -> bool {
    matches!(severity, Severity::Warning | Severity::Critical)
}

#[must_use]
pub fn m1_false_blockers(golden: &[GoldenFinding]) -> f64 {
    let blocking: Vec<&GoldenFinding> = golden
        .iter()
        .filter(|finding| is_blocking(finding.severity))
        .collect();
    let Some(total) = u32::try_from(blocking.len()).ok() else {
        return 0.0;
    };
    if total == 0 {
        return 0.0;
    }
    let false_positives = blocking
        .iter()
        .filter(|finding| finding.label == Label::FalsePositive)
        .count();
    f64::from(u32::try_from(false_positives).unwrap_or(0)) / f64::from(total)
}

fn structural_key(finding: &Finding) -> (String, u64, Severity, BTreeSet<String>) {
    (
        finding.file.clone(),
        finding.line,
        finding.severity,
        title_tokens(&finding.title),
    )
}

#[must_use]
pub fn m2_idempotent(first: &[Finding], second: &[Finding]) -> bool {
    let mut left: Vec<_> = first.iter().map(structural_key).collect();
    let mut right: Vec<_> = second.iter().map(structural_key).collect();
    left.sort();
    right.sort();
    left == right
}

fn lines_within(a: u64, b: u64, span: u64) -> bool {
    a.max(b).saturating_sub(a.min(b)) <= span
}

fn same_spot_tokens(
    line_a: u64,
    tokens_a: &BTreeSet<String>,
    line_b: u64,
    tokens_b: &BTreeSet<String>,
) -> bool {
    if line_a == line_b {
        return tokens_a.intersection(tokens_b).next().is_some();
    }
    lines_within(line_a, line_b, LINE_DRIFT_TOLERANCE)
        && tokens_a.union(tokens_b).count() > 0
        && tokens_a.intersection(tokens_b).count().saturating_mul(2)
            >= tokens_a.union(tokens_b).count()
}

const RETIRE_AFTER_CONSECUTIVE_MISSES: u32 = 2;

struct Standing {
    file: String,
    line: u64,
    tokens: BTreeSet<String>,
    misses: u32,
}

#[must_use]
pub fn m3_rounds_to_zero(rounds: &[Vec<Finding>]) -> Option<u32> {
    let mut standing: Vec<Standing> = Vec::new();
    let mut standing_counts: Vec<usize> = Vec::new();
    for round in rounds {
        let refound = |issue: &Standing| {
            round.iter().any(|finding| {
                finding.file == issue.file
                    && same_spot_tokens(
                        issue.line,
                        &issue.tokens,
                        finding.line,
                        &title_tokens(&finding.title),
                    )
            })
        };
        for issue in &mut standing {
            if refound(issue) {
                issue.misses = 0;
            } else {
                issue.misses = issue.misses.saturating_add(1);
            }
        }
        standing.retain(|issue| issue.misses < RETIRE_AFTER_CONSECUTIVE_MISSES);
        for finding in round {
            let tokens = title_tokens(&finding.title);
            let known = standing.iter().any(|issue| {
                finding.file == issue.file
                    && same_spot_tokens(issue.line, &issue.tokens, finding.line, &tokens)
            });
            if !known {
                standing.push(Standing {
                    file: finding.file.clone(),
                    line: finding.line,
                    tokens,
                    misses: 0,
                });
            }
        }
        standing_counts.push(standing.len());
    }
    for (idx, pair) in standing_counts.windows(2).enumerate() {
        let (Some(&earlier), Some(&later)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        if earlier == 0 && later == 0 {
            return u32::try_from(idx.saturating_add(1)).ok();
        }
    }
    None
}

#[must_use]
fn hit_tokens(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split([' ', '-', '\t'])
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect()
}

#[must_use]
pub fn mutation_hit(titles: &[String], expect: &[String]) -> bool {
    let expect_set = hit_tokens(&expect.join(" "));
    if expect_set.is_empty() {
        return false;
    }
    let needed = expect_set.len().div_ceil(2);
    titles.iter().any(|title| {
        let tokens = hit_tokens(title);
        expect_set.intersection(&tokens).count() >= needed
    })
}

#[must_use]
pub fn m4_recall(hits: usize, cases: usize) -> Option<f64> {
    if cases == 0 {
        return None;
    }
    let hits = u32::try_from(hits).unwrap_or(0);
    let cases = u32::try_from(cases).unwrap_or(1);
    Some(f64::from(hits) / f64::from(cases))
}

#[must_use]
pub fn same_spot_finding(a: &Finding, b: &Finding) -> bool {
    a.file == b.file
        && same_spot_tokens(
            a.line,
            &title_tokens(&a.title),
            b.line,
            &title_tokens(&b.title),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn finding(file: &str, line: u64, title: &str, severity: Severity) -> Finding {
        Finding {
            file: file.to_owned(),
            line,
            title: title.to_owned(),
            severity,
        }
    }

    fn golden(line: u64, title: &str, severity: Severity, label: Label) -> GoldenFinding {
        GoldenFinding {
            run: "run.md".to_owned(),
            sha: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            file: "src/a.rs".to_owned(),
            line,
            title: title.to_owned(),
            severity,
            label,
        }
    }

    #[test]
    fn m1_computes_the_false_blocker_fraction_from_fixtures() {
        let set = vec![
            golden(1, "real blocker", Severity::Warning, Label::TruePositive),
            golden(
                2,
                "refuted blocker",
                Severity::Critical,
                Label::FalsePositive,
            ),
            golden(
                3,
                "refuted nitpick",
                Severity::Nitpick,
                Label::FalsePositive,
            ),
            golden(4, "unknown blocker", Severity::Warning, Label::Unknown),
            golden(5, "suggestion", Severity::Suggestion, Label::Unknown),
        ];
        let m1 = m1_false_blockers(&set);
        assert!(
            (m1 - 1.0 / 3.0).abs() < 1e-9,
            "one of three blocking findings is false — the refuted nitpick is not blocking: {m1}"
        );
        assert!(
            (m1_false_blockers(&[]) - 0.0).abs() < 1e-9,
            "no findings is zero"
        );
        let non_blocking = vec![golden(
            9,
            "only nitpicks",
            Severity::Nitpick,
            Label::Unknown,
        )];
        assert!((m1_false_blockers(&non_blocking) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn m2_is_true_only_for_structurally_equivalent_verdicts() {
        let a = vec![
            finding(
                "src/a.rs",
                1,
                "Lock is dropped too early",
                Severity::Warning,
            ),
            finding("src/b.rs", 9, "unwrap on a Result", Severity::Suggestion),
        ];
        let reworded = vec![
            finding(
                "src/a.rs",
                1,
                "dropped too early is lock",
                Severity::Warning,
            ),
            finding("src/b.rs", 9, "unwrap on a Result", Severity::Suggestion),
        ];
        let shifted = vec![
            finding(
                "src/a.rs",
                2,
                "Lock is dropped too early",
                Severity::Warning,
            ),
            finding("src/b.rs", 9, "unwrap on a Result", Severity::Suggestion),
        ];
        assert!(m2_idempotent(&a, &a.clone()));
        assert!(
            m2_idempotent(&a, &reworded),
            "same tokens in a different order are the same verdict"
        );
        assert!(!m2_idempotent(&a, &shifted), "a moved anchor is different");
        let head = vec![finding(
            "src/a.rs",
            1,
            "Lock is dropped too early",
            Severity::Warning,
        )];
        assert!(!m2_idempotent(&a, &head), "count matters");
    }

    #[test]
    fn m4_counts_a_hit_only_on_token_overlap() {
        let expect = vec![
            "boundary".to_owned(),
            "comparison".to_owned(),
            "off-by-one".to_owned(),
        ];
        assert!(
            mutation_hit(
                &["the loop bound comparison is off by one".to_owned()],
                &expect
            ),
            "hyphenated expect tokens match spaced finding words"
        );
        assert!(
            mutation_hit(&["off-by-one flips the boundary".to_owned()], &expect),
            "two of three tokens are enough"
        );
        assert!(
            !mutation_hit(&["unrelated wording entirely".to_owned()], &expect),
            "one shared token of three is not"
        );
        assert!(!mutation_hit(&["anything".to_owned()], &[]));
        assert!((m4_recall(3, 4).unwrap_or(0.0) - 0.75).abs() < 1e-9);
        assert_eq!(m4_recall(0, 0), None);
    }

    #[test]
    fn m3_returns_the_first_zero_standing_pair_and_none_when_never() {
        let rounds = vec![
            vec![finding("src/a.rs", 1, "first issue", Severity::Warning)],
            vec![finding(
                "src/a.rs",
                1,
                "first issue stands",
                Severity::Warning,
            )],
            vec![finding(
                "src/a.rs",
                1,
                "first issue still",
                Severity::Warning,
            )],
        ];
        assert_eq!(
            m3_rounds_to_zero(&rounds),
            None,
            "a perpetually re-found issue keeps standing forever"
        );
        let resolved = vec![
            vec![finding("src/a.rs", 1, "first issue", Severity::Warning)],
            vec![finding(
                "src/a.rs",
                1,
                "first issue stands",
                Severity::Warning,
            )],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];
        assert_eq!(
            m3_rounds_to_zero(&resolved),
            Some(4),
            "two consecutive misses retire the issue; rounds four and five stand at zero"
        );
        let diverging = vec![
            vec![finding("src/a.rs", 1, "first issue", Severity::Warning)],
            vec![finding("src/b.rs", 5, "second issue", Severity::Warning)],
            vec![finding("src/c.rs", 9, "third issue", Severity::Warning)],
        ];
        assert_eq!(m3_rounds_to_zero(&diverging), None, "never converges");
        let rephrased_new = vec![
            vec![finding(
                "src/a.rs",
                10,
                "the clamp breaks the walk",
                Severity::Warning,
            )],
            vec![finding(
                "src/a.rs",
                12,
                "clamp breaks the walk differently",
                Severity::Warning,
            )],
        ];
        assert_eq!(
            m3_rounds_to_zero(&rephrased_new),
            None,
            "a drifted rephrase with overlap is the same issue, so round two adds nothing but there is no pair yet"
        );
    }

    #[test]
    fn findings_in_different_files_never_share_a_standing_issue() {
        let same_line = vec![
            vec![
                finding("src/a.rs", 4, "the same wording", Severity::Warning),
                finding("src/b.rs", 4, "the same wording", Severity::Warning),
            ],
            Vec::new(),
            Vec::new(),
        ];
        assert_eq!(
            m3_rounds_to_zero(&same_line),
            None,
            "two same-line same-worded findings in different files are two issues, never one"
        );
        assert!(
            !same_spot_finding(
                &finding("src/a.rs", 4, "the lock is dropped", Severity::Warning),
                &finding("src/b.rs", 4, "the lock is dropped", Severity::Warning),
            ),
            "identity is file-scoped: a different file is a different finding"
        );
        assert!(
            same_spot_finding(
                &finding("src/a.rs", 4, "the lock is dropped", Severity::Warning),
                &finding("src/a.rs", 6, "dropped is the lock", Severity::Warning),
            ),
            "same file, drifted line, same tokens stays one identity"
        );
    }
}
