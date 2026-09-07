//! Agent fix prompts embedded in the posted review: a collapsible,
//! copyable prompt under every inline finding and one fix-all prompt
//! in the summary body, both rendered from this single wording source.

use crate::findings::Finding;
use crate::findings::complexity_badge;
use crate::findings::complexity_glyph;
use crate::tools::submit::DroppedFinding;

const PROMPT_LABEL: &str = "Prompt for coding agents:";
const VERIFY_FIRST: &str = "First check whether this issue still exists at the location below. If it is already fixed or no longer applies, say so and change nothing.";
const VERIFY_ALL: &str = "Check each item still exists at its location before fixing it; skip anything already fixed or no longer applicable.";
const FIX_DIRECTIVES: &str = "If the issue still exists: read the surrounding code, apply a minimal focused fix, and cover the behavior change with a test.";
const ALL_DIRECTIVES: &str = "If an issue still exists: read the surrounding code before each fix, keep each change minimal and focused, and cover behavior changes with tests.";

const WRAP_WIDTH: usize = 80;
const WRAP_INDENT: &str = "   ";

pub(crate) fn review_round_body(
    head_sha: &str,
    findings: usize,
    clean: bool,
    fix_all: &str,
) -> String {
    let short = crate::review::registry::short_sha(head_sha);
    let noun = if findings == 1 { "finding" } else { "findings" };
    let head = if clean {
        format!("🤖 difftrace reviewed `{short}` — clean round, nothing to fix.")
    } else if findings == 0 {
        format!(
            "🤖 difftrace reviewed `{short}` — no findings in the files it reviewed; the round is incomplete."
        )
    } else {
        format!(
            "🤖 difftrace reviewed `{short}` — {findings} {noun} this round; fix prompts below."
        )
    };
    if clean {
        return head;
    }
    let pointer = "Verdict, summary, and risks: the difftrace comment on this pull request.";
    if fix_all.is_empty() {
        format!("{head}\n\n{pointer}")
    } else {
        format!("{head}\n\n{pointer}\n\n{fix_all}")
    }
}

pub(crate) fn re_raised_reply_body(comment_body: &str, head_sha: &str) -> String {
    format!("{comment_body}\n\n*Re-raised in the review of commit `{head_sha}`.*")
}

pub(crate) fn grouped_by_title(findings: &[Finding]) -> Vec<(usize, Vec<&Finding>)> {
    let mut groups: Vec<(String, usize, Vec<&Finding>)> = Vec::new();
    for (index, finding) in findings.iter().enumerate() {
        let key = crate::review::registry::normalize_title(&finding.title);
        match groups.iter_mut().find(|(existing, _, _)| *existing == key) {
            Some((_, _, group)) => group.push(finding),
            None => groups.push((key, index, vec![finding])),
        }
    }
    groups
        .into_iter()
        .map(|(_, primary, group)| (primary, group))
        .collect()
}

fn locations_line(findings: &[&Finding]) -> String {
    findings
        .iter()
        .map(|finding| format!("`{}:{}`", finding.file, finding.line))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn fence_for(payload: &str) -> String {
    let longest = payload
        .lines()
        .map(|line| {
            line.trim_start()
                .chars()
                .take_while(|ch| *ch == '`')
                .count()
        })
        .max()
        .unwrap_or(0);
    "`".repeat(longest.saturating_add(1).max(4))
}

const PLAN_DIRECTIVES: &str = "Implement the plan below step by step. First check each step still applies — skip anything already done or no longer relevant and say so — and keep each change minimal.";

pub(crate) fn plan_post_body(plan: &str) -> String {
    let prompt = format!(
        "{PROMPT_LABEL}\n\n{PLAN_DIRECTIVES}\n\n{}",
        wrap_prompt(plan)
    );
    let fence = fence_for(&prompt);
    format!(
        "{plan}\n\n<details>\n<summary>🤖 Plan prompt for coding agents</summary>\n\n{fence}text\n{prompt}\n{fence}\n</details>"
    )
}

pub(crate) fn wrap_prompt(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.chars().count() <= WRAP_WIDTH {
                return line.to_owned();
            }
            let mut lines: Vec<String> = Vec::new();
            let mut current = String::new();
            for word in line.split_whitespace() {
                let limit = if lines.is_empty() {
                    WRAP_WIDTH
                } else {
                    WRAP_WIDTH.saturating_sub(WRAP_INDENT.len())
                };
                let projected = if current.is_empty() {
                    word.chars().count()
                } else {
                    current
                        .chars()
                        .count()
                        .saturating_add(1)
                        .saturating_add(word.chars().count())
                };
                if projected > limit && !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
            if !current.is_empty() {
                lines.push(current);
            }
            let mut wrapped = String::new();
            for (index, line) in lines.iter().enumerate() {
                if index > 0 {
                    wrapped.push('\n');
                    wrapped.push_str(WRAP_INDENT);
                }
                wrapped.push_str(line);
            }
            wrapped
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn comment_body(finding: &Finding, line: u64, also: &[&Finding]) -> String {
    let mut also_in_prompt = String::new();
    for location in also {
        also_in_prompt.push_str("\nAlso at: ");
        also_in_prompt.push_str(&location.file);
        also_in_prompt.push_str(", line ");
        also_in_prompt.push_str(&location.line.to_string());
    }
    let prompt = format!(
        "{PROMPT_LABEL}\n\n{VERIFY_FIRST}\n\nFile: {}, line {}\nSeverity: {}\nComplexity: {}/5\nTitle: {}\nDetail: {}{also_in_prompt}\n\n{FIX_DIRECTIVES}",
        finding.file,
        line,
        finding.severity.as_str(),
        finding.complexity,
        finding.title,
        finding.body,
    );
    let also_in_body = if also.is_empty() {
        String::new()
    } else {
        format!("\n\nAlso occurs at: {}", locations_line(also))
    };
    let wrapped = wrap_prompt(&prompt);
    let fence = fence_for(&wrapped);
    format!(
        "{} {} **{}**\n\n{}{also_in_body}\n\n<details>\n<summary>🤖 Fix prompt for coding agents</summary>\n\n{fence}text\n{wrapped}\n{fence}\n</details>",
        finding.severity.badge(),
        complexity_badge(finding.complexity),
        finding.title,
        finding.body,
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
    let grounded = grounded_list(findings);
    let unanchored = dropped_list(dropped);
    let wrapped = wrap_prompt(&fix_all_prompt(findings, dropped, pr, head_sha));
    let fence = fence_for(&wrapped);
    format!(
        "## 🤖 Fix all findings\n{grounded}{unanchored}\n\n<details>\n<summary>Copy the fix-all prompt for coding agents</summary>\n\n{fence}text\n{wrapped}\n{fence}\n</details>"
    )
}

fn grounded_list(findings: &[Finding]) -> String {
    grouped_by_title(findings)
        .iter()
        .enumerate()
        .map(|(index, (_, group))| {
            let Some((primary, also)) = group.split_first() else {
                return String::new();
            };
            let also_suffix = if also.is_empty() {
                String::new()
            } else {
                format!(" — also at {}", locations_line(also))
            };
            format!(
                "{}. `{}:{}` — {} {} ({}){also_suffix}",
                index.saturating_add(1),
                primary.file,
                primary.line,
                primary.severity.glyph(),
                primary.title,
                complexity_glyph(primary.complexity),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn dropped_list(dropped: &[DroppedFinding]) -> String {
    if dropped.is_empty() {
        return String::new();
    }
    let entries = dropped
        .iter()
        .map(|entry| {
            format!(
                "- `{}:{}` — {} {} ({}, {})",
                entry.finding.file,
                entry.finding.line,
                entry.finding.severity.glyph(),
                entry.finding.title,
                complexity_glyph(entry.finding.complexity),
                entry.reason,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n\nUnanchored (no inline comment posted):\n{entries}")
}

fn fix_all_prompt(
    findings: &[Finding],
    dropped: &[DroppedFinding],
    pr: u64,
    head_sha: &str,
) -> String {
    let mut items: Vec<String> = Vec::new();
    for (index, (_, group)) in grouped_by_title(findings).into_iter().enumerate() {
        let Some((finding, also)) = group.split_first() else {
            continue;
        };
        let head = format!(
            "{}. {}:{} [{}] {} (effort {}/5)\n   Detail: {}",
            index.saturating_add(1),
            finding.file,
            finding.line,
            finding.severity.as_str(),
            finding.title,
            finding.complexity,
            finding.body,
        );
        let item = if also.is_empty() {
            head
        } else {
            format!("{head}\n   Also occurs at: {}", locations_line(also))
        };
        items.push(item);
    }
    for entry in dropped {
        let finding = &entry.finding;
        items.push(format!(
            "{}. {}:{} [{}] {} (effort {}/5) — no inline comment ({}).\n   Detail: {}",
            items.len().saturating_add(1),
            finding.file,
            finding.line,
            finding.severity.as_str(),
            finding.title,
            finding.complexity,
            entry.reason,
            finding.body,
        ));
    }
    format!(
        "{PROMPT_LABEL}\n\nFix every finding from the code review of PR #{pr} (commit {head_sha}).\n{VERIFY_ALL}\nWork through the list in order:\n\n{}\n\n{ALL_DIRECTIVES}",
        items.join("\n\n"),
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
            complexity: 3,
            title: "Lock dropped early".to_owned(),
            body: "The guard is dropped before the read completes.".to_owned(),
        }
    }

    fn titled(file: &str, line: usize, title: &str) -> Finding {
        Finding {
            title: title.to_owned(),
            ..finding(file, line)
        }
    }

    #[test]
    fn a_comment_body_carries_the_finding_and_a_collapsed_fix_prompt() {
        let finding = finding("src/worker.rs", 9);
        let body = comment_body(&finding, 123, &[]);
        assert!(body.contains("![warning](https://img.shields.io/badge/warning-orange)"));
        assert!(body.contains("![effort 3](https://img.shields.io/badge/effort_3-yellow)"));
        assert!(body.contains("**Lock dropped early**"));
        assert!(body.contains("The guard is dropped before the read completes."));
        assert!(
            body.contains("<details>\n<summary>🤖 Fix prompt for coding agents</summary>"),
            "the collapsed section names its audience"
        );
        assert!(body.contains("````text\nPrompt for coding agents:"));
        let prompt_text = body.replace("\n   ", " ");
        assert!(prompt_text.contains(
            "First check whether this issue still exists at the location below. If it is already fixed or no longer applies, say so and change nothing."
        ));
        assert!(body.contains("\n````\n</details>"));
        assert!(body.contains("File: src/worker.rs, line 123"));
        assert!(body.contains("Severity: warning"));
        assert!(body.contains("Complexity: 3/5"));
        assert!(body.contains("Title: Lock dropped early"));
        assert!(prompt_text.contains(
            "If the issue still exists: read the surrounding code, apply a minimal focused fix, and cover the behavior change with a test."
        ));
        assert!(
            !body.contains("Also occurs at"),
            "a single-location finding lists no other locations"
        );
        assert!(
            !body.contains("line 9"),
            "the prompt must cite the anchored comment line, not the raw finding line"
        );
    }

    #[test]
    fn a_grouped_comment_lists_the_other_locations() {
        let finding = finding("src/worker.rs", 9);
        let other = titled("src/queue.rs", 14, "Lock dropped early");
        let also = vec![&other];
        let body = comment_body(&finding, 9, &also);
        assert!(body.contains("Also occurs at: `src/queue.rs:14`"));
        assert!(body.contains("Also at: src/queue.rs, line 14"));
    }

    #[test]
    fn the_fix_all_section_reports_and_prompts_every_raised_finding() {
        let grounded = vec![
            titled("src/alpha.rs", 2, "Lock dropped early"),
            titled("src/beta.rs", 11, "Retry can outlive shutdown"),
        ];
        let dropped = vec![DroppedFinding {
            finding: titled("src/beta.rs", 999, "Lock dropped early"),
            reason: "line outside the changed hunks",
        }];
        let section = fix_all_section(&grounded, &dropped, 42, "9f3b2c1");
        assert!(section.contains("## 🤖 Fix all findings"));
        assert!(section.contains("1. `src/alpha.rs:2` — ⚠️ Lock dropped early (🟡)"));
        assert!(section.contains("2. `src/beta.rs:11` — ⚠️ Retry can outlive shutdown (🟡)"));
        assert!(section.contains(
            "- `src/beta.rs:999` — ⚠️ Lock dropped early (🟡, line outside the changed hunks)"
        ));
        assert!(section.contains("<summary>Copy the fix-all prompt for coding agents</summary>"));
        assert!(section.contains("PR #42"));
        assert!(section.contains("commit 9f3b2c1"));
        let prompt_text = section.replace("\n   ", " ");
        assert!(
            prompt_text.contains("1. src/alpha.rs:2 [warning] Lock dropped early (effort 3/5)")
        );
        assert!(section.contains("\n   Detail: The guard is dropped"));
        assert!(prompt_text.contains(
            "3. src/beta.rs:999 [warning] Lock dropped early (effort 3/5) — no inline comment (line outside the changed hunks)."
        ));
        assert!(prompt_text.contains(
            "If an issue still exists: read the surrounding code before each fix, keep each change minimal and focused, and cover behavior changes with tests."
        ));
        assert!(section.contains("\n````\n</details>"));
    }

    #[test]
    fn same_titled_findings_share_one_fix_all_item() {
        let grounded = vec![
            titled("src/alpha.rs", 2, "Lock dropped early"),
            titled("src/beta.rs", 11, "Lock dropped early"),
        ];
        let section = fix_all_section(&grounded, &[], 42, "sha");
        assert!(section.contains(
            "1. `src/alpha.rs:2` — ⚠️ Lock dropped early (🟡) — also at `src/beta.rs:11`"
        ));
        assert!(
            !section.contains("2. "),
            "a grouped pair renders one item, not two"
        );
        assert!(section.contains("\n   Also occurs at: `src/beta.rs:11`"));
    }

    #[test]
    fn a_payload_with_longer_backtick_runs_gets_a_taller_fence()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(fence_for("plain text"), "````");
        assert_eq!(fence_for("````\nscanned"), "`````");
        let mut body_finding = finding("src/worker.rs", 9);
        body_finding.body = "code:\n````\nraw stays inside".to_owned();
        let rendered = comment_body(&body_finding, 9, &[]);
        assert!(
            rendered.contains("`````text"),
            "the fence grows past the payload's longest backtick run"
        );
        let (_, after_open) = rendered
            .split_once("`````text\n")
            .ok_or("expected the taller fence")?;
        let (inside, _) = after_open
            .split_once("\n`````\n</details>")
            .ok_or("expected the taller close")?;
        assert!(inside.contains("````"), "the payload line survives inside");
        Ok(())
    }

    #[test]
    fn prompt_lines_wrap_at_eighty_columns_with_a_hanging_indent() {
        let long = "Detail: ".to_owned() + &"word ".repeat(40);
        let wrapped = wrap_prompt(&long);
        for line in wrapped.split('\n') {
            assert!(
                line.chars().count() <= 80,
                "every wrapped line stays within the width: {line}"
            );
        }
        let mut lines = wrapped.split('\n');
        let first = lines.next().unwrap_or("");
        assert!(!first.starts_with(' '), "the first line carries no indent");
        for continuation in lines {
            assert!(
                continuation.starts_with("   "),
                "continuation lines carry the hanging indent"
            );
        }
        assert!(wrapped.contains("word word"), "words stay space-separated");
    }

    #[test]
    fn the_fix_all_section_is_omitted_when_nothing_was_raised() {
        let section = fix_all_section(&[], &[], 42, "sha");
        assert!(section.is_empty());
    }

    #[test]
    fn the_round_body_is_a_stat_line_with_the_rounds_fix_all() {
        let body = review_round_body("9f3b2c1full", 4, false, "## 🤖 Fix all findings\n…");
        assert!(body.starts_with(
            "🤖 difftrace reviewed `9f3b2c1` — 4 findings this round; fix prompts below."
        ));
        assert!(
            body.contains(
                "Verdict, summary, and risks: the difftrace comment on this pull request."
            )
        );
        assert!(body.contains("## 🤖 Fix all findings"));
        let clean = review_round_body("9f3b2c1full", 0, true, "");
        assert!(
            clean.starts_with("🤖 difftrace reviewed `9f3b2c1` — clean round, nothing to fix.")
        );
        assert!(!clean.contains("Fix all findings"));
        assert!(
            !clean.contains("Verdict, summary, and risks"),
            "a clean round has no fix prompts to point at"
        );
        let single = review_round_body("9f3b2c1full", 1, false, "x");
        assert!(single.contains("— 1 finding this round"));
    }

    #[test]
    fn a_clean_round_body_is_a_single_stat_line_without_the_pointer() {
        let body = review_round_body("9f3b2c1full", 0, true, "");
        assert_eq!(
            body, "🤖 difftrace reviewed `9f3b2c1` — clean round, nothing to fix.",
            "a clean round carries nothing but the stat line"
        );
        let with_findings = review_round_body("9f3b2c1full", 4, false, "## 🤖 Fix all findings");
        assert!(
            with_findings.contains("Verdict, summary, and risks"),
            "the pointer stays for rounds with findings"
        );
    }

    #[test]
    fn a_re_raised_reply_carries_the_round_commit() {
        let body = re_raised_reply_body("the finding body", "9f3b2c1");
        assert!(body.starts_with("the finding body"));
        assert!(body.contains("Re-raised in the review of commit `9f3b2c1`."));
    }

    #[test]
    fn grouping_is_case_insensitive_and_keeps_first_occurrence_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let findings = vec![
            titled("src/a.rs", 1, "Same Issue"),
            titled("src/b.rs", 2, "Other issue"),
            titled("src/c.rs", 3, "same issue "),
        ];
        let groups = grouped_by_title(&findings);
        assert_eq!(groups.len(), 2);
        let (primary_index, group) = groups.first().cloned().ok_or("no primary group")?;
        assert_eq!(
            findings.get(primary_index).map(|f| f.file.as_str()),
            Some("src/a.rs")
        );
        let (primary, also) = group.split_first().ok_or("empty group")?;
        assert_eq!(primary.file.as_str(), "src/a.rs");
        assert_eq!(also.len(), 1);
        assert_eq!(also.first().map(|f| f.file.as_str()), Some("src/c.rs"));
        let (secondary_index, secondary_group) =
            groups.get(1).cloned().ok_or("no secondary group")?;
        assert_eq!(
            findings.get(secondary_index).map(|f| f.file.as_str()),
            Some("src/b.rs")
        );
        let (_, none) = secondary_group.split_first().ok_or("empty group")?;
        assert!(none.is_empty());
        Ok(())
    }
}
