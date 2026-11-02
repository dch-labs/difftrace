//! The parity gate: the five release metrics for both runners, the
//! gate evaluation that names the owning task on every miss, and the
//! markdown comparison the owner signs off on before the cutover.

use std::fmt::Write as _;
use std::path::Path;

use crate::extract::Finding;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParityMetrics {
    pub m1_false_blockers: f32,
    pub m2_idempotent: Option<bool>,
    pub m3_rounds_to_zero: Option<u32>,
    pub m4_recall: Option<f32>,
    pub m5_wall_secs: u64,
    pub m5_tokens: Option<u64>,
}

#[derive(Debug)]
pub struct GateMiss {
    pub metric: &'static str,
    pub detail: String,
    pub owning_task: &'static str,
}

impl GateMiss {
    fn new(metric: &'static str, detail: String, owning_task: &'static str) -> Self {
        Self {
            metric,
            detail,
            owning_task,
        }
    }
}

#[must_use]
pub fn evaluate_gates(v1: &ParityMetrics, current: &ParityMetrics) -> Vec<GateMiss> {
    let mut misses = Vec::new();
    if current.m1_false_blockers >= 0.05 {
        misses.push(GateMiss::new(
            "M1",
            format!(
                "false blockers {:.1}% (target <5%) — seed the refuted findings; the seeds file ships empty",
                current.m1_false_blockers * 100.0
            ),
            "suppression seeds (owner) + judge pass",
        ));
    }
    match current.m2_idempotent {
        Some(true) => {}
        Some(false) => misses.push(GateMiss::new(
            "M2",
            "two runs on identical inputs produced non-equivalent verdicts".to_owned(),
            "reconcile purity",
        )),
        None => misses.push(GateMiss::new(
            "M2",
            "unmeasured — run the corpus twice (--twice); the harness reports it as a property"
                .to_owned(),
            "reconcile purity (owner-gated spend run)",
        )),
    }
    match current.m3_rounds_to_zero {
        Some(rounds) if rounds <= 3 => {}
        Some(rounds) => misses.push(GateMiss::new(
            "M3",
            format!("rounds to zero new findings: {rounds} (target ≤3)"),
            "state core convergence",
        )),
        None => misses.push(GateMiss::new(
            "M3",
            "unmeasured — needs repeated rounds over a standing branch".to_owned(),
            "state core convergence (owner-gated spend run)",
        )),
    }
    match current.m4_recall {
        Some(recall) if recall >= 0.60 => {}
        Some(recall) => misses.push(GateMiss::new(
            "M4",
            format!(
                "mutation recall {:.1}% (target ≥60% at N=3)",
                recall * 100.0
            ),
            "judge recall (aspects × panel × passes)",
        )),
        None => misses.push(GateMiss::new(
            "M4",
            "unmeasured — run the mutation corpus with the panel".to_owned(),
            "judge recall (owner-gated spend run)",
        )),
    }
    if v1.m5_wall_secs == 0 || current.m5_wall_secs == 0 {
        misses.push(GateMiss::new(
            "M5",
            "a wall-time side is absent (0s) — cost cannot be compared".to_owned(),
            "cost accounting (both runners must report wall time)",
        ));
    } else if current.m5_wall_secs.saturating_mul(2) > v1.m5_wall_secs {
        misses.push(GateMiss::new(
            "M5",
            format!(
                "wall time {}s vs v1 {}s (target ≤½ v1)",
                current.m5_wall_secs, v1.m5_wall_secs
            ),
            "cost budgets (lanes, region caps)",
        ));
    }
    misses
}

#[must_use]
pub fn render_parity(v1: &ParityMetrics, current: &ParityMetrics) -> String {
    let misses = evaluate_gates(v1, current);
    let mut out = String::new();
    let _unused = writeln!(
        out,
        "# current vs v1 parity report\n\n| metric | v1 baseline | current | target | gate |"
    );
    let _unused = writeln!(out, "|---|---|---|---|---|");
    let _unused = writeln!(
        out,
        "| M1 false blockers | {:.1}% | {:.1}% | <5% | {} |",
        v1.m1_false_blockers * 100.0,
        current.m1_false_blockers * 100.0,
        verdict(&misses, "M1")
    );
    let _unused = writeln!(
        out,
        "| M2 idempotency | fails (5 vs 12 on one SHA) | {} | structural equivalence | {} |",
        match current.m2_idempotent {
            Some(true) => "pass",
            Some(false) => "FAIL",
            None => "unmeasured",
        },
        verdict(&misses, "M2")
    );
    let _unused = writeln!(
        out,
        "| M3 convergence | never (26 rounds worst branch) | {} | ≤3 rounds | {} |",
        current
            .m3_rounds_to_zero
            .map_or_else(|| "unmeasured".to_owned(), |rounds| rounds.to_string()),
        verdict(&misses, "M3")
    );
    let _unused = writeln!(
        out,
        "| M4 mutation recall | not measured | {} | ≥60% at N=3 | {} |",
        current.m4_recall.map_or_else(
            || "unmeasured".to_owned(),
            |recall| format!("{:.1}%", recall * 100.0)
        ),
        verdict(&misses, "M4")
    );
    let _unused = writeln!(
        out,
        "| M5 wall time | {}s | {}s | ≤½ v1 | {} |",
        v1.m5_wall_secs,
        current.m5_wall_secs,
        verdict(&misses, "M5")
    );
    out.push_str("\n## gate misses\n\n");
    if misses.is_empty() {
        out.push_str("none — all five gates green.\n");
    } else {
        for miss in &misses {
            let _unused = writeln!(
                out,
                "- **{}**: {} — owning: {}",
                miss.metric, miss.detail, miss.owning_task
            );
        }
        out.push_str(
            "\nAny miss goes back to the owning task, not \"tuned\" here; the owner may waive a gate in writing.\n",
        );
    }
    out
}

fn verdict(misses: &[GateMiss], metric: &str) -> &'static str {
    if misses.iter().any(|miss| miss.metric == metric) {
        "MISS"
    } else {
        "pass"
    }
}

pub fn parse_current_report(text: &str) -> Result<Vec<Finding>, String> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|err| format!("the current report is not JSON: {err}"))?;
    let mut findings = Vec::new();
    for bucket in ["confirmed", "solo"] {
        let Some(entries) = value.get(bucket).and_then(|list| list.as_array()) else {
            continue;
        };
        for entry in entries {
            let Some(finding) = entry.get("finding") else {
                continue;
            };
            let Some(title) = finding.get("title").and_then(|t| t.as_str()) else {
                continue;
            };
            let evidence = finding
                .get("evidence")
                .and_then(|list| list.as_array())
                .map_or_else(Vec::new, std::clone::Clone::clone);
            let anchor = evidence.first();
            let file = anchor
                .and_then(|cite| cite.get("file"))
                .and_then(|f| f.as_str())
                .unwrap_or("(workspace)");
            let line = anchor
                .and_then(|cite| cite.get("line"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            findings.push(Finding {
                file: file.to_owned(),
                line,
                title: title.to_owned(),
                severity: severity_of(finding),
            });
        }
    }
    Ok(findings)
}

pub fn findings_from_new_reports(
    dir: &Path,
    before: &[String],
) -> Result<Vec<crate::extract::Finding>, crate::error::DtError> {
    let mut findings = Vec::new();
    let entries = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok());
    for name in entries {
        if before.contains(&name) || !name.ends_with(".report.json") {
            continue;
        }
        let text = std::fs::read_to_string(dir.join(&name)).map_err(crate::error::DtError::Io)?;
        match parse_current_report(&text) {
            Ok(parsed) => findings.extend(parsed),
            Err(err) => eprintln!("eval-run: skipping {name}: {err}"),
        }
    }
    Ok(findings)
}

fn severity_of(finding: &serde_json::Value) -> crate::extract::Severity {
    use crate::extract::Severity;
    match finding.get("severity").and_then(|s| s.as_str()) {
        Some("critical") => Severity::Critical,
        Some("warning") => Severity::Warning,
        Some("suggestion") => Severity::Suggestion,
        _ => Severity::Nitpick,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn green() -> ParityMetrics {
        ParityMetrics {
            m1_false_blockers: 0.01,
            m2_idempotent: Some(true),
            m3_rounds_to_zero: Some(2),
            m4_recall: Some(0.7),
            m5_wall_secs: 100,
            m5_tokens: None,
        }
    }

    fn v1() -> ParityMetrics {
        ParityMetrics {
            m1_false_blockers: 0.0,
            m2_idempotent: Some(false),
            m3_rounds_to_zero: None,
            m4_recall: None,
            m5_wall_secs: 300,
            m5_tokens: None,
        }
    }

    #[test]
    fn the_parity_report_renders_all_five_metrics_with_both_runners() {
        let text = render_parity(&v1(), &green());
        for row in [
            "M1 false blockers",
            "M2 idempotency",
            "M3 convergence",
            "M4 mutation recall",
            "M5 wall time",
        ] {
            assert!(text.contains(row), "the {row} row is present:\n{text}");
        }
        assert!(text.contains("70.0%"), "the current recall renders: {text}");
        assert!(
            text.contains("300s") && text.contains("100s"),
            "both walls render"
        );
        assert!(
            text.contains("all five gates green"),
            "a fully green current run says so: {text}"
        );
    }

    #[test]
    fn a_metric_miss_fails_the_report_with_the_owning_task_named() {
        let mut weak = green();
        weak.m4_recall = Some(0.4);
        weak.m2_idempotent = None;
        let misses = evaluate_gates(&v1(), &weak);
        assert_eq!(misses.len(), 2, "two gates miss: {:?}", misses.len());
        assert!(
            misses
                .iter()
                .any(|miss| miss.metric == "M4" && miss.owning_task.contains("judge recall")),
            "M4 names the owning task: {misses:?}"
        );
        assert!(
            misses
                .iter()
                .any(|miss| miss.metric == "M2" && miss.detail.contains("unmeasured")),
            "an unmeasured metric fails closed: {misses:?}"
        );
        let text = render_parity(&v1(), &weak);
        assert!(text.contains("MISS"));
        assert!(text.contains("owning: judge recall (aspects"));
    }

    #[test]
    fn unmeasured_metrics_fail_closed_instead_of_reading_green() {
        let unmeasured = ParityMetrics {
            m1_false_blockers: 0.0,
            m2_idempotent: None,
            m3_rounds_to_zero: None,
            m4_recall: None,
            m5_wall_secs: 0,
            m5_tokens: None,
        };
        let misses = evaluate_gates(&v1(), &unmeasured);
        assert_eq!(misses.len(), 4, "M2, M3, M4, and M5 all miss: {misses:?}");
        assert!(
            misses
                .iter()
                .filter(|miss| miss.detail.contains("unmeasured") || miss.detail.contains("absent"))
                .count()
                == 4,
            "every miss explains what measurement is missing"
        );
    }

    #[test]
    fn the_cost_gate_compares_against_half_the_v1_wall() {
        let mut costly = green();
        costly.m5_wall_secs = 151;
        assert!(
            evaluate_gates(&v1(), &costly)
                .iter()
                .any(|miss| miss.metric == "M5"),
            "151s against a 300s baseline misses the ≤½ target"
        );
        costly.m5_wall_secs = 150;
        assert!(
            !evaluate_gates(&v1(), &costly)
                .iter()
                .any(|miss| miss.metric == "M5"),
            "150s is exactly half and passes"
        );
    }

    #[test]
    fn the_current_harvest_takes_only_new_reports() -> Result<(), Box<dyn std::error::Error>> {
        let dir =
            std::env::temp_dir().join(format!("difftrace-eval-harvest-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("seen-1.report.json"),
            "{\"confirmed\":[],\"solo\":[]}",
        )?;
        std::fs::write(
            dir.join("w-1.log"),
            "a v1-era transcript the current runner ignores",
        )?;
        std::fs::write(
            dir.join("w-2.report.json"),
            "{\"confirmed\":[{\"finding\":{\"title\":\"Found it\",\"severity\":\"warning\",\"evidence\":[{\"file\":\"src/a.rs\",\"line\":3}]}}],\"solo\":[]}",
        )?;
        std::fs::write(dir.join("w-3.report.json"), "not json")?;
        let before = vec!["seen-1.report.json".to_owned()];
        let findings = findings_from_new_reports(&dir, &before)?;
        assert_eq!(
            findings.len(),
            1,
            "only the new parseable report yields findings: {findings:?}"
        );
        assert_eq!(
            findings.first().map(|f| f.title.clone()),
            Some("Found it".to_owned())
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn current_reports_ingest_confirmed_and_solo_by_their_first_cite() {
        let text = r#"{
            "confirmed": [
                { "finding": { "title": "Lock dropped early", "severity": "warning",
                  "evidence": [ { "file": "src/a.rs", "line": 42 } ] } }
            ],
            "solo": [
                { "finding": { "title": "Unconfirmed hunch", "severity": "suggestion",
                  "evidence": [ { "file": "src/b.rs", "line": 7 } ] } }
            ],
            "weak": [
                { "finding": { "title": "Evidence shape failed", "severity": "warning",
                  "evidence": [ { "file": "src/c.rs", "line": 3 } ] } }
            ]
        }"#;
        let findings = parse_current_report(text).unwrap_or_default();
        assert_eq!(
            findings.len(),
            2,
            "weak leads stay out of the recall signal: {findings:?}"
        );
        let fallback = Finding {
            file: String::new(),
            line: 0,
            title: String::new(),
            severity: crate::extract::Severity::Nitpick,
        };
        let first = findings.first().cloned().unwrap_or(fallback);
        assert_eq!(first.file, "src/a.rs");
        assert_eq!(first.line, 42);
        assert_eq!(first.title, "Lock dropped early");
        assert!(matches!(first.severity, crate::extract::Severity::Warning));
        assert!(
            parse_current_report("not json").is_err(),
            "a malformed report errors instead of reading clean"
        );
    }
}
