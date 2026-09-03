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

    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Nitpick => "💬",
            Self::Suggestion => "💡",
            Self::Warning => "⚠️",
            Self::Critical => "🔴",
        }
    }

    #[must_use]
    pub fn badge(self) -> String {
        let color = match self {
            Self::Nitpick => "lightgrey",
            Self::Suggestion => "blue",
            Self::Warning => "orange",
            Self::Critical => "red",
        };
        format!(
            "![{}](https://img.shields.io/badge/{}-{color})",
            self.as_str(),
            self.as_str()
        )
    }
}

#[must_use]
pub fn complexity_badge(level: u8) -> String {
    let color = match level {
        1 => "blue",
        2 => "green",
        3 => "yellow",
        4 => "orange",
        5 => "purple",
        _ => "lightgrey",
    };
    format!("![effort {level}](https://img.shields.io/badge/effort_{level}-{color})")
}

#[must_use]
pub fn complexity_glyph(level: u8) -> &'static str {
    match level {
        1 => "🔵",
        2 => "🟢",
        3 => "🟡",
        4 => "🟠",
        5 => "🟣",
        _ => "⚪",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub severity: Severity,
    pub complexity: u8,
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

pub(crate) fn reject_out_of_range_complexity(findings: &[Finding]) -> Result<(), String> {
    for finding in findings {
        if !(1..=5).contains(&finding.complexity) {
            return Err(format!(
                "finding complexity must be 1-5: {}:{} rated {}",
                finding.file, finding.line, finding.complexity
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
                "complexity": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 5,
                    "description": "Fix complexity from 1 (a one-liner) to 5 (needs restructuring)."
                },
                "title": { "type": "string" },
                "body": { "type": "string" }
            },
            "required": ["file", "line", "severity", "complexity", "title", "body"],
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
        for field in ["file", "line", "severity", "complexity", "title", "body"] {
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
                "complexity": 3,
                "title": "Lock dropped early",
                "body": "The guard is dropped before the read completes."
            }]
        });
        let findings: Findings = StructuredOutput::from_value(value)?;
        let finding = findings.findings.first().ok_or("expected a value")?;
        assert_eq!(finding.file, "src/main.rs");
        assert_eq!(finding.line, 12);
        assert_eq!(finding.severity, Severity::Warning);
        assert_eq!(finding.complexity, 3);
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

    #[test]
    fn severity_glyphs_and_badges_are_stable() {
        let pairs = [
            (Severity::Nitpick, "💬", "lightgrey"),
            (Severity::Suggestion, "💡", "blue"),
            (Severity::Warning, "⚠️", "orange"),
            (Severity::Critical, "🔴", "red"),
        ];
        for (severity, glyph, color) in pairs {
            assert_eq!(severity.glyph(), glyph);
            assert!(severity.badge().contains("img.shields.io/badge/"));
            assert!(severity.badge().contains(color));
            assert!(severity.badge().contains(severity.as_str()));
        }
    }

    #[test]
    fn complexity_ladder_badges_and_glyphs_follow_the_color_ramp() {
        let pairs = [
            (1u8, "🔵", "blue"),
            (2, "🟢", "green"),
            (3, "🟡", "yellow"),
            (4, "🟠", "orange"),
            (5, "🟣", "purple"),
        ];
        for (level, glyph, color) in pairs {
            assert_eq!(complexity_glyph(level), glyph);
            assert!(complexity_badge(level).contains(color));
        }
    }

    #[test]
    fn complexity_outside_the_ladder_is_rejected_at_entry() -> Result<(), Box<dyn std::error::Error>>
    {
        let finding = |complexity: u8| Finding {
            file: "src/lib.rs".to_owned(),
            line: 2,
            severity: Severity::Warning,
            complexity,
            title: "T".to_owned(),
            body: "B".to_owned(),
        };
        let err = reject_out_of_range_complexity(&[finding(0)])
            .err()
            .ok_or("expected zero complexity to fail")?;
        assert!(err.contains("1-5"));
        assert!(reject_out_of_range_complexity(&[finding(6)]).is_err());
        assert!(reject_out_of_range_complexity(&[finding(1), finding(5)]).is_ok());
        Ok(())
    }
}
