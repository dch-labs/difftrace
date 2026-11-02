//! Test-module context for lint findings: a deny-level lint inside
//! `#[cfg(test)]` code is demoted — the run's own house style is that
//! test code unwraps freely, and the structural fact (a crate whose
//! lints no CI leg enforces) is the ci-coverage finding, not twenty
//! `unwrap()` lines.

use crate::contracts::ToolFinding;

#[must_use]
pub fn demote_test_module_hits(
    findings: Vec<ToolFinding>,
    contents: impl Fn(&str) -> Option<String>,
) -> Vec<ToolFinding> {
    findings
        .into_iter()
        .map(|finding| {
            let Some(source) = contents(&finding.file) else {
                return finding;
            };
            if !line_sits_in_test_module(&source, finding.line) {
                return finding;
            }
            let demoted = match finding.severity {
                crate::contracts::Severity::Critical => crate::contracts::Severity::Warning,
                other => other,
            };
            ToolFinding {
                severity: demoted,
                message: format!(
                    "{} [test-module code — severity demoted; the crate's own test-lint policy governs here]",
                    finding.message
                ),
                ..finding
            }
        })
        .collect()
}

fn line_sits_in_test_module(source: &str, line: u32) -> bool {
    let mut depth_before_marker: i32 = 0;
    let mut current_line: u32 = 0;
    let mut test_depths: Vec<(i32, i32)> = Vec::new();
    for stripped in crate::judges::flow::stripped_lines(source) {
        current_line = current_line.saturating_add(1);
        let trimmed = stripped.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            test_depths.push((depth_before_marker, i32::MAX));
            continue;
        }
        for ch in stripped.chars() {
            match ch {
                '{' => depth_before_marker = depth_before_marker.saturating_add(1),
                '}' => {
                    depth_before_marker = depth_before_marker.saturating_sub(1);
                    test_depths.retain(|(open, _)| *open < depth_before_marker);
                }
                _ => {}
            }
        }
        if current_line >= line {
            return test_depths
                .iter()
                .any(|(open, _)| depth_before_marker > *open);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Severity;

    fn finding(line: u32) -> ToolFinding {
        ToolFinding {
            tool: "cargo-clippy".to_owned(),
            file: "crates/derive/src/attr.rs".to_owned(),
            line,
            severity: Severity::Critical,
            message: "used `unwrap()` on an `Option` value".to_owned(),
        }
    }

    const ATTR_RS_SHAPE: &str = r#"
fn pascal() -> String {
    let kept = "x".to_owned();
    kept
}

#[cfg(test)]
mod tests {
    #[test]
    fn shape() {
        let value = option_env!("X").unwrap();
        assert!(value.is_some());
    }
}
"#;

    #[test]
    fn a_lint_inside_the_cfg_test_module_is_demoted() {
        let line = 10;
        let demoted = demote_test_module_hits(vec![finding(line)], |path| {
            (path == "crates/derive/src/attr.rs").then(|| ATTR_RS_SHAPE.to_owned())
        });
        let first = demoted.first().cloned();
        assert!(
            matches!(&first, Some(f) if f.severity == Severity::Warning
                && f.message.contains("test-module code")),
            "the attr.rs test-module unwrap demotes from critical: {first:?}"
        );
    }

    #[test]
    fn a_lint_in_production_code_keeps_its_severity() {
        let line = 2;
        let kept = demote_test_module_hits(vec![finding(line)], |path| {
            (path == "crates/derive/src/attr.rs").then(|| ATTR_RS_SHAPE.to_owned())
        });
        assert!(
            kept.first()
                .is_some_and(|f| f.severity == Severity::Critical),
            "production lines keep the deny-level severity"
        );
    }

    #[test]
    fn the_test_module_closes_and_later_production_code_recovers() {
        let source =
            "#[cfg(test)]\nmod tests {\n    fn a() {}\n}\n\nfn prod() {\n    let x = 1;\n}\n";
        let line = 7;
        assert!(
            !line_sits_in_test_module(source, line),
            "code after the test module closes is production again"
        );
        assert!(
            line_sits_in_test_module(source, 3),
            "code inside the module is test code"
        );
    }

    #[test]
    fn braces_inside_string_literals_do_not_drift_the_module_span() {
        let source = "#[cfg(test)]\nmod tests {\n    let s = \"{\";\n    fn helper() {}\n}\n\nfn prod() {\n    let x = 1;\n}\n";
        assert!(
            line_sits_in_test_module(source, 4),
            "the helper inside the module is test code beside an unbalanced literal"
        );
        assert!(
            !line_sits_in_test_module(source, 8),
            "an unbalanced brace inside a string literal cannot hold the module open past its close"
        );
    }

    #[test]
    fn an_unreadable_file_keeps_the_original_severity() {
        let kept = demote_test_module_hits(vec![finding(10)], |_| None);
        assert!(
            kept.first()
                .is_some_and(|f| f.severity == Severity::Critical),
            "without the file the demotion never guesses"
        );
    }
}
