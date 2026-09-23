//! Parser for the `file:line[:col]: level: message` diagnostic convention
//! shared by most linters.

use crate::contracts::{Severity, ToolFinding};

#[must_use]
pub fn parse(output: &str, tool: &str) -> Vec<ToolFinding> {
    output
        .lines()
        .filter_map(|line| parse_line(line, tool))
        .collect()
}

fn parse_line(line: &str, tool: &str) -> Option<ToolFinding> {
    let (file, rest) = line.split_once(':')?;
    let (number, rest) = rest.split_once(':')?;
    if file.trim().is_empty() {
        return None;
    }
    let number: u32 = number.trim().parse().ok()?;
    if number == 0 {
        return None;
    }
    let (line_number, rest) = match rest.split_once(':') {
        Some((column, tail))
            if !column.trim().is_empty() && column.trim().chars().all(|c| c.is_ascii_digit()) =>
        {
            column.trim().parse::<u32>().ok()?;
            (number, tail)
        }
        _ => (number, rest),
    };
    let (severity, message) = split_level(rest.trim_start());
    Some(ToolFinding {
        tool: tool.to_owned(),
        file: file.trim().to_owned(),
        line: line_number,
        severity,
        message,
    })
}

fn split_level(rest: &str) -> (Severity, String) {
    let Some((word, tail)) = rest.split_once(' ') else {
        return (Severity::Warning, rest.to_owned());
    };
    let severity = match word.trim_matches([':', '-']).to_lowercase().as_str() {
        "error" | "err" | "fatal" => Severity::Critical,
        "warning" | "warn" => Severity::Warning,
        "info" | "note" | "style" | "performance" | "hint" => Severity::Suggestion,
        _ => return (Severity::Warning, rest.to_owned()),
    };
    let mut message = tail.trim_start();
    if let Some(stripped) = message.strip_prefix('-') {
        message = stripped.trim_start();
    }
    (severity, message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diag_parses_each_dialect_fixture() {
        let output = "src/a.js:2:1: Error - 'x' is not defined (no-undef)\n\
                      main.go:13:2: unused variable x (unused)\n\
                      src/a.cpp:7:3: warning: unused variable 'x' [misc]\n\
                      build.sh:4:8: error: Double quote to prevent globbing [SC2086]\n\
                      Dockerfile:1:1: warning: Do not use latest tag [DL3006]\n\
                      main.py:1:1: E501 Line too long (100 > 88)\n\
                      plain.txt:12: error: no columns in this dialect\n\
                      5 problems (5 errors, 0 warnings)\n\
                      npm WARN deprecated foo@1.0.0: async versions\n";
        let findings = parse(output, "linter");
        assert_eq!(
            findings.len(),
            7,
            "progress noise and plain prose are not diagnostics"
        );
        let eslint = findings.iter().find(|finding| finding.file == "src/a.js");
        assert!(
            matches!(eslint, Some(finding) if finding.severity == Severity::Critical
            && finding.line == 2
            && finding.message == "'x' is not defined (no-undef)")
        );
        let golangci = findings.iter().find(|finding| finding.file == "main.go");
        assert!(
            matches!(golangci, Some(finding) if finding.severity == Severity::Warning
            && finding.line == 13
            && finding.message == "unused variable x (unused)")
        );
        let clang = findings.iter().find(|finding| finding.file == "src/a.cpp");
        assert!(
            matches!(clang, Some(finding) if finding.severity == Severity::Warning
            && finding.line == 7
            && finding.message == "unused variable 'x' [misc]")
        );
        let shellcheck = findings.iter().find(|finding| finding.file == "build.sh");
        assert!(
            matches!(shellcheck, Some(finding) if finding.severity == Severity::Critical
            && finding.line == 4
            && finding.message.starts_with("Double quote"))
        );
        let hadolint = findings.iter().find(|finding| finding.file == "Dockerfile");
        assert!(
            matches!(hadolint, Some(finding) if finding.severity == Severity::Warning
            && finding.line == 1)
        );
        let ruff = findings.iter().find(|finding| finding.file == "main.py");
        assert!(
            matches!(ruff, Some(finding) if finding.severity == Severity::Warning
            && finding.message == "E501 Line too long (100 > 88)")
        );
        let columnless = findings.iter().find(|finding| finding.file == "plain.txt");
        assert!(
            matches!(columnless, Some(finding) if finding.severity == Severity::Critical
            && finding.line == 12
            && finding.message == "no columns in this dialect")
        );
        assert!(findings.iter().all(|finding| finding.tool == "linter"));
    }

    #[test]
    fn timestamp_shaped_noise_is_not_a_finding() {
        let output = "10:00:01 build started\n\
                      2026-09-23 10:00:00 running eslint\n\
                      a.rs:2: error: real\n";
        let findings = parse(output, "linter");
        assert_eq!(
            findings.len(),
            1,
            "real diagnostics are 1-based; clock shapes parse as line zero and die"
        );
        assert!(
            matches!(findings.first(), Some(finding) if finding.file == "a.rs"
                && finding.message == "real")
        );
    }
}
