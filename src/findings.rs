//! The model-output contracts: inline [`Finding`]s per batch and a
//! whole-review [`ReviewSummary`], each a loopctl `StructuredOutput`
//! with a strict JSON Schema.

use loopctl::structured::StructuredOutput;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Nitpick,
    Suggestion,
    Warning,
    Critical,
}

impl Severity {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nitpick => "nitpick",
            Self::Suggestion => "suggestion",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub severity: Severity,
    pub title: String,
    pub body: String,
}

pub(crate) fn reject_zero_lines(findings: &[Finding]) -> Result<(), String> {
    for finding in findings {
        if finding.line == 0 {
            return Err(format!(
                "finding line must be at least 1: {}:{}",
                finding.file, finding.line
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Findings {
    pub findings: Vec<Finding>,
}

#[must_use]
pub fn findings_array_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "file": { "type": "string" },
                "line": { "type": "integer", "minimum": 1 },
                "severity": {
                    "type": "string",
                    "enum": ["nitpick", "suggestion", "warning", "critical"]
                },
                "title": { "type": "string" },
                "body": { "type": "string" }
            },
            "required": ["file", "line", "severity", "title", "body"],
            "additionalProperties": false
        }
    })
}

impl StructuredOutput for Findings {
    fn name() -> &'static str {
        "difftrace_findings"
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "findings": findings_array_schema()
            },
            "required": ["findings"],
            "additionalProperties": false
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub summary: String,
    pub risk_notes: Vec<String>,
    pub tests: String,
}

impl StructuredOutput for ReviewSummary {
    fn name() -> &'static str {
        "difftrace_review_summary"
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" },
                "risk_notes": { "type": "array", "items": { "type": "string" } },
                "tests": { "type": "string" }
            },
            "required": ["summary", "risk_notes", "tests"],
            "additionalProperties": false
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_requires_every_finding_field() -> Result<(), Box<dyn std::error::Error>> {
        let schema = Findings::schema();
        let required = schema
            .pointer("/properties/findings/items/required")
            .and_then(serde_json::Value::as_array)
            .ok_or("schema must carry the findings required list")?;
        for field in ["file", "line", "severity", "title", "body"] {
            assert!(
                required.iter().any(|v| v.as_str() == Some(field)),
                "schema must require {field}"
            );
        }
        Ok(())
    }

    #[test]
    fn model_values_round_trip_into_findings() -> Result<(), Box<dyn std::error::Error>> {
        let value = serde_json::json!({
            "findings": [{
                "file": "src/main.rs",
                "line": 12,
                "severity": "warning",
                "title": "Lock dropped early",
                "body": "The guard is dropped before the read completes."
            }]
        });
        let findings: Findings = StructuredOutput::from_value(value)?;
        let finding = findings.findings.first().ok_or("expected a value")?;
        assert_eq!(finding.file, "src/main.rs");
        assert_eq!(finding.line, 12);
        assert_eq!(finding.severity, Severity::Warning);
        Ok(())
    }

    #[test]
    fn an_unknown_severity_is_rejected() {
        let value = serde_json::json!({
            "findings": [{
                "file": "src/main.rs",
                "line": 12,
                "severity": "catastrophic",
                "title": "t",
                "body": "b"
            }]
        });
        assert!(<Findings as StructuredOutput>::from_value(value).is_err());
    }

    #[test]
    fn empty_findings_are_allowed() -> Result<(), Box<dyn std::error::Error>> {
        let findings: Findings =
            StructuredOutput::from_value(serde_json::json!({ "findings": [] }))?;
        assert!(findings.findings.is_empty());
        Ok(())
    }

    #[test]
    fn the_schema_requires_every_summary_field() -> Result<(), Box<dyn std::error::Error>> {
        let schema = ReviewSummary::schema();
        let required = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .ok_or("schema must carry the required list")?;
        for field in ["summary", "risk_notes", "tests"] {
            assert!(
                required.iter().any(|v| v.as_str() == Some(field)),
                "schema must require {field}"
            );
        }
        Ok(())
    }

    #[test]
    fn model_values_round_trip_into_a_summary() -> Result<(), Box<dyn std::error::Error>> {
        let value = serde_json::json!({
            "summary": "Adds retry with backoff to the worker loop.",
            "risk_notes": ["Retry can now outlive the shutdown signal."],
            "tests": "Covered by the new integration test."
        });
        let summary: ReviewSummary = StructuredOutput::from_value(value)?;
        assert_eq!(summary.risk_notes.len(), 1);
        Ok(())
    }

    #[test]
    fn structured_output_names_are_identifier_safe() {
        for name in [Findings::name(), ReviewSummary::name()] {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "name must be identifier-safe: {name}"
            );
        }
    }
}
