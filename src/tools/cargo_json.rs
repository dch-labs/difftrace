//! Parser for cargo's `--message-format=json` diagnostics.

use crate::contracts::{Severity, ToolFinding};

const SPANLESS_TARGET: &str = "(workspace)";

#[must_use]
pub fn parse(output: &str, tool: &str) -> Vec<ToolFinding> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| map_message(&value, tool))
        .collect()
}

fn map_message(value: &serde_json::Value, tool: &str) -> Option<ToolFinding> {
    if value.get("reason").and_then(|reason| reason.as_str()) != Some("compiler-message") {
        return None;
    }
    let message = value.get("message")?;
    let severity = match message.get("level").and_then(|level| level.as_str())? {
        "error" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => return None,
    };
    let text = message.get("message").and_then(|text| text.as_str())?;
    let (file, line) = anchor(message);
    Some(ToolFinding {
        tool: tool.to_owned(),
        file,
        line,
        severity,
        message: text.to_owned(),
    })
}

fn anchor(message: &serde_json::Value) -> (String, u32) {
    let Some(spans) = message.get("spans").and_then(|spans| spans.as_array()) else {
        return (SPANLESS_TARGET.to_owned(), 0);
    };
    let Some(span) = spans
        .iter()
        .find(|span| {
            span.get("is_primary")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .or_else(|| spans.first())
    else {
        return (SPANLESS_TARGET.to_owned(), 0);
    };
    let file = span
        .get("file_name")
        .and_then(|file| file.as_str())
        .unwrap_or(SPANLESS_TARGET);
    let line = span
        .get("line_start")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    (file.to_owned(), u32::try_from(line).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_json_maps_compiler_diagnostics() {
        let output = concat!(
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",",
            "\"message\":\"mismatched types\",\"spans\":[",
            "{\"file_name\":\"src/expansion.rs\",\"line_start\":3,\"is_primary\":false},",
            "{\"file_name\":\"src/lib.rs\",\"line_start\":42,\"is_primary\":true}]}}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",",
            "\"message\":\"unused variable: x\",\"spans\":[",
            "{\"file_name\":\"src/main.rs\",\"line_start\":3}]}}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"note\",",
            "\"message\":\"an attachment\",\"spans\":[{\"file_name\":\"a.rs\",\"line_start\":1}]}}\n",
            "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"difftrace\"}}\n",
            "not json at all\n",
        );
        let findings = parse(output, "cargo-clippy");
        assert_eq!(findings.len(), 2, "notes and non-messages skip");
        let error = findings
            .iter()
            .find(|finding| finding.severity == Severity::Critical);
        assert!(
            matches!(error, Some(finding) if finding.file == "src/lib.rs"
                && finding.line == 42
                && finding.message == "mismatched types"),
            "the primary span anchors, not the macro expansion site"
        );
        let warning = findings
            .iter()
            .find(|finding| finding.severity == Severity::Warning);
        assert!(
            matches!(warning, Some(finding) if finding.file == "src/main.rs"
                && finding.line == 3
                && finding.message == "unused variable: x"
                && finding.tool == "cargo-clippy"),
            "a span without a primary flag falls back to the first"
        );
    }

    #[test]
    fn spanless_compile_errors_surface_at_a_workspace_pseudo_target() {
        let output = concat!(
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",",
            "\"message\":\"aborting due to 2 previous errors\",\"spans\":[]}}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",",
            "\"message\":\"could not compile `difftrace`\",\"spans\":null}}\n",
        );
        let findings = parse(output, "cargo-check");
        assert_eq!(
            findings.len(),
            2,
            "a hard compile error with no span is signal, not noise"
        );
        assert!(findings.iter().all(|finding| finding.file == "(workspace)"
            && finding.line == 0
            && finding.severity == Severity::Critical));
        assert!(
            findings
                .iter()
                .any(|finding| finding.message == "aborting due to 2 previous errors")
        );
    }
}
