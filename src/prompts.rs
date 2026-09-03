//! Agent fix prompts embedded in the posted review: a collapsible,
//! copyable prompt under every inline finding and one fix-all prompt
//! in the summary body, both rendered from this single wording source.

use crate::findings::Finding;
use crate::tools::submit::DroppedFinding;

const FIX_DIRECTIVES: &str = "Read the surrounding code first, apply a minimal focused fix, and cover the behavior change with a test.";
const ALL_DIRECTIVES: &str = "Read the surrounding code before each fix, keep each change minimal and focused, and cover behavior changes with tests.";

pub(crate) fn comment_body(finding: &Finding, line: u64) -> String {
    let prompt = format!(
        "Fix this code-review finding.\n\nFile: {}, line {}\nSeverity: {}\nTitle: {}\nDetail: {}\n\n{}",
        finding.file,
        line,
        finding.severity.as_str(),
        finding.title,
        finding.body,
        FIX_DIRECTIVES,
    );
    format!(
        "**[{}] {}**\n\n{}\n\n<details>\n<summary>🤖 Fix prompt</summary>\n\n````text\n{}\n````\n</details>",
        finding.severity.as_str(),
        finding.title,
        finding.body,
        prompt,
    )
}

pub(crate) fn fix_all_section(
    findings: &[Finding],
    dropped: &[DroppedFinding],
    pr: u64,
    head_sha: &str,
) -> String {
    if findings.is_empty() && dropped.is_empty() {
        return String::new();
    }
    let grounded = findings
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            format!(
                "{}. `{}:{}` — [{}] {}",
                index.saturating_add(1),
                finding.file,
                finding.line,
                finding.severity.as_str(),
                finding.title,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let unanchored = if dropped.is_empty() {
        String::new()
    } else {
        let entries = dropped
            .iter()
            .map(|entry| {
                format!(
                    "- `{}:{}` — [{}] {} ({})",
                    entry.finding.file,
                    entry.finding.line,
                    entry.finding.severity.as_str(),
                    entry.finding.title,
                    entry.reason,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\nUnanchored (no inline comment posted):\n{entries}")
    };
    format!(
        "## 🤖 Fix all findings\n{grounded}{unanchored}\n\n<details>\n<summary>Copy the fix-all prompt</summary>\n\n````text\n{}\n````\n</details>",
        fix_all_prompt(findings, dropped, pr, head_sha),
    )
}

fn fix_all_prompt(
    findings: &[Finding],
    dropped: &[DroppedFinding],
    pr: u64,
    head_sha: &str,
) -> String {
    let grounded = findings
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            format!(
                "{}. {}:{} [{}] {} — {}",
                index.saturating_add(1),
                finding.file,
                finding.line,
                finding.severity.as_str(),
                finding.title,
                finding.body,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let unanchored = if dropped.is_empty() {
        String::new()
    } else {
        let entries = dropped
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                format!(
                    "{}. {}:{} [{}] {} — {} (no inline comment — {})",
                    findings.len().saturating_add(index.saturating_add(1)),
                    entry.finding.file,
                    entry.finding.line,
                    entry.finding.severity.as_str(),
                    entry.finding.title,
                    entry.finding.body,
                    entry.reason,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\nUnanchored findings (no inline comment was posted):\n{entries}")
    };
    format!(
        "Fix every finding from the code review of PR #{pr} (commit {head_sha}).\nWork through the list in order:\n\n{grounded}{unanchored}\n\n{ALL_DIRECTIVES}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::Severity;

    fn finding(file: &str, line: usize) -> Finding {
        Finding {
            file: file.to_owned(),
            line,
            severity: Severity::Warning,
            title: "Lock dropped early".to_owned(),
            body: "The guard is dropped before the read completes.".to_owned(),
        }
    }

    #[test]
    fn a_comment_body_carries_the_finding_and_a_collapsed_fix_prompt() {
        let finding = finding("src/worker.rs", 9);
        let body = comment_body(&finding, 123);
        assert!(body.contains("**[warning] Lock dropped early**"));
        assert!(body.contains("The guard is dropped before the read completes."));
        assert!(body.contains("<details>\n<summary>🤖 Fix prompt</summary>"));
        assert!(body.contains("````text\nFix this code-review finding."));
        assert!(body.contains("\n````\n</details>"));
        assert!(body.contains("File: src/worker.rs, line 123"));
        assert!(body.contains("Severity: warning"));
        assert!(body.contains("Title: Lock dropped early"));
        assert!(body.contains("cover the behavior change with a test."));
        assert!(
            !body.contains("line 9"),
            "the prompt must cite the anchored comment line, not the raw finding line"
        );
    }

    #[test]
    fn the_fix_all_section_reports_and_prompts_every_raised_finding() {
        let grounded = vec![finding("src/alpha.rs", 2), finding("src/beta.rs", 11)];
        let dropped = vec![DroppedFinding {
            finding: finding("src/beta.rs", 999),
            reason: "line outside the changed hunks",
        }];
        let section = fix_all_section(&grounded, &dropped, 42, "9f3b2c1");
        assert!(section.contains("## 🤖 Fix all findings"));
        assert!(section.contains("1. `src/alpha.rs:2` — [warning] Lock dropped early"));
        assert!(section.contains("2. `src/beta.rs:11` — [warning] Lock dropped early"));
        assert!(section.contains(
            "- `src/beta.rs:999` — [warning] Lock dropped early (line outside the changed hunks)"
        ));
        assert!(section.contains("<summary>Copy the fix-all prompt</summary>"));
        assert!(section.contains("PR #42"));
        assert!(section.contains("commit 9f3b2c1"));
        assert!(section.contains(
            "1. src/alpha.rs:2 [warning] Lock dropped early — The guard is dropped before the read completes."
        ));
        assert!(section.contains(
            "3. src/beta.rs:999 [warning] Lock dropped early — The guard is dropped before the read completes. (no inline comment — line outside the changed hunks)"
        ));
        assert!(section.contains("cover behavior changes with tests."));
        assert!(section.contains("\n````\n</details>"));
    }

    #[test]
    fn the_fix_all_section_is_omitted_when_nothing_was_raised() {
        let section = fix_all_section(&[], &[], 42, "sha");
        assert!(section.is_empty());
    }
}
