//! Transcript parsing: on-disk review logs into finding rows.

use std::collections::BTreeSet;

use crate::error::DtError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Nitpick,
    Suggestion,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: u64,
    pub title: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub run: String,
    pub sha: String,
    pub findings: Vec<Finding>,
    pub dropped: Vec<String>,
}

const DROPPED_MARKER: &str = "<!-- difftrace: dropped findings (never posted) -->";

const SEVERITY_GLYPHS: [(&str, Severity); 5] = [
    ("\u{1f4ac}", Severity::Nitpick),
    ("\u{1f4a1}", Severity::Suggestion),
    ("\u{26a0}\u{fe0f}", Severity::Warning),
    ("\u{26a0}", Severity::Warning),
    ("\u{1f534}", Severity::Critical),
];

const COMPLEXITY_CIRCLES: [&str; 5] = [
    "\u{1f535}",
    "\u{1f7e2}",
    "\u{1f7e1}",
    "\u{1f7e0}",
    "\u{1f7e3}",
];

pub fn parse_transcript(run: &str, text: &str) -> Result<Transcript, DtError> {
    let sha = header_sha(text).ok_or_else(|| DtError::Parse {
        run: run.to_owned(),
        message: "no `# difftrace … @ <sha>` header line".to_owned(),
    })?;
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    let mut dropped = Vec::new();
    let mut in_dropped = false;
    for line in text.lines() {
        if in_dropped {
            if let Some(entry) = parse_dropped(line) {
                dropped.push(entry);
                continue;
            }
            in_dropped = false;
        }
        if line.trim() == DROPPED_MARKER {
            in_dropped = true;
            continue;
        }
        if let Some(finding) = parse_finding_line(line) {
            let key = (finding.file.clone(), finding.line, finding.title.clone());
            if seen.insert(key) {
                findings.push(finding);
            }
        }
    }
    Ok(Transcript {
        run: run.to_owned(),
        sha,
        findings,
        dropped,
    })
}

#[must_use]
pub fn is_transcript_name(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext == "md" || ext == "log")
}

fn header_sha(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    if !first.starts_with("# difftrace") {
        return None;
    }
    let (_, sha) = first.rsplit_once(" @ ")?;
    (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha.to_owned())
}

fn parse_finding_line(line: &str) -> Option<Finding> {
    let rest = numbered_rest(line)?;
    let rest = rest.strip_prefix('`')?;
    let (anchor, rest) = rest.split_once('`')?;
    let rest = rest.strip_prefix(" — ")?;
    let (severity, title) = split_glyph(rest)?;
    let title = strip_complexity(cut_group_suffix(title));
    let (file, number) = anchor.rsplit_once(':')?;
    if file.is_empty() || title.is_empty() {
        return None;
    }
    let line_number = number.parse::<u64>().ok()?;
    Some(Finding {
        file: file.to_owned(),
        line: line_number,
        title,
        severity,
    })
}

fn numbered_rest(line: &str) -> Option<&str> {
    let (number, rest) = line.split_once(". ")?;
    (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then_some(rest)
}

fn split_glyph(rest: &str) -> Option<(Severity, &str)> {
    for (glyph, severity) in SEVERITY_GLYPHS {
        if let Some(tail) = rest.strip_prefix(glyph) {
            let tail = tail.trim_start_matches('\u{fe0f}');
            let tail = tail.strip_prefix(' ')?;
            return Some((severity, tail));
        }
    }
    None
}

fn cut_group_suffix(title: &str) -> &str {
    title
        .split_once(" — also at ")
        .map_or(title, |(head, _)| head)
}

fn strip_complexity(title: &str) -> String {
    for circle in COMPLEXITY_CIRCLES {
        let suffix = format!(" ({circle})");
        if let Some(stripped) = title.strip_suffix(&suffix) {
            return stripped.to_owned();
        }
    }
    title.to_owned()
}

fn parse_dropped(line: &str) -> Option<String> {
    let inner = line.strip_prefix("<!-- ")?.strip_suffix(" -->")?;
    let (anchor, _) = inner.split_once(" — ")?;
    let (_, number) = anchor.rsplit_once(':')?;
    number.parse::<u64>().ok()?;
    Some(anchor.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Result<String, Box<dyn std::error::Error>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name);
        Ok(std::fs::read_to_string(path)?)
    }

    #[test]
    fn a_transcript_verdict_parses_into_findings() -> Result<(), Box<dyn std::error::Error>> {
        let text = fixture("log-era.md")?;
        let parsed = parse_transcript("loopctl-example.log", &text)?;
        assert_eq!(
            parsed.sha, "023b1b20b78d8f8eb53c6cc34ea94a48183b0089",
            "the sha comes from the header"
        );
        assert_eq!(parsed.findings.len(), 3, "verdict duplicate collapses");
        let first = parsed.findings.first().ok_or("first finding")?;
        assert_eq!(first.file, "derive/src/attr.rs");
        assert_eq!(first.line, 553);
        assert_eq!(first.severity, Severity::Warning);
        assert_eq!(
            first.title,
            "rename_all mirroring diverges from serde's field arm"
        );
        let second = parsed.findings.get(1).ok_or("second finding")?;
        assert_eq!(second.severity, Severity::Suggestion);
        let third = parsed.findings.get(2).ok_or("third finding")?;
        assert_eq!(third.severity, Severity::Nitpick);
        assert_eq!(third.file, "src/numeric.rs");
        assert_eq!(
            parsed.dropped,
            vec![
                "src/detection/convergence.rs:564".to_owned(),
                "src/engine/bare/dispatch.rs:1018".to_owned(),
            ],
            "dropped anchors come from the footer"
        );
        Ok(())
    }

    #[test]
    fn an_old_era_transcript_parses_the_same_way() -> Result<(), Box<dyn std::error::Error>> {
        let text = fixture("local-era.md")?;
        let parsed = parse_transcript("local-1788828878-09485eb.md", &text)?;
        assert_eq!(
            parsed.sha, "09485ebc733aa1d34f02a20ab0da613905f7b1b2",
            "the pre-version header carries the same @ sha shape"
        );
        assert_eq!(parsed.findings.len(), 2);
        let first = parsed.findings.first().ok_or("first finding")?;
        assert_eq!(first.file, "crates/dch-config/src/lib.rs");
        assert_eq!(first.line, 1232);
        assert_eq!(first.severity, Severity::Suggestion);
        assert!(parsed.dropped.is_empty());
        Ok(())
    }

    #[test]
    fn prose_lines_are_not_mistaken_for_findings() -> Result<(), Box<dyn std::error::Error>> {
        let text = fixture("log-era.md")?;
        let parsed = parse_transcript("loopctl-example.log", &text)?;
        let files: Vec<&str> = parsed.findings.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(
            files,
            vec![
                "derive/src/attr.rs",
                "src/hooks/builtin/confirmation_hook.rs",
                "src/numeric.rs",
            ],
            "summary backticks and risks bullets do not yield rows"
        );
        Ok(())
    }

    #[test]
    fn a_headerless_text_is_rejected() {
        let parsed = parse_transcript("stray.md", "just some prose\n\n1. `a.rs:2` — 💡 t (🟢)\n");
        assert!(parsed.is_err());
    }
}
