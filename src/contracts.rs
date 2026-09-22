//! The data spine: every pipeline stage communicates only through
//! these types — coverage plans, judge reports, findings, verdicts,
//! and the cross-round state they update.

use std::collections::BTreeMap;

use crate::error::JudgeId;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PinnedRange {
    pub base_sha: String,
    pub head_sha: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct RegionId(pub usize);

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct AspectId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegionKind {
    Added,
    Hunk,
    File,
    Repo,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Region {
    pub file: String,
    pub start: u32,
    pub end: u32,
    pub kind: RegionKind,
}

pub type Cell = (RegionId, AspectId);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoveragePlan {
    pub range: PinnedRange,
    pub regions: Vec<Region>,
    pub cells: Vec<Cell>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Nitpick,
    Suggestion,
    Warning,
    Critical,
}

impl std::str::FromStr for Severity {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_lowercase().as_str() {
            "nitpick" => Ok(Self::Nitpick),
            "suggestion" => Ok(Self::Suggestion),
            "warning" => Ok(Self::Warning),
            "critical" => Ok(Self::Critical),
            other => Err(format!(
                "unknown severity {other:?}; expected nitpick, suggestion, warning, or critical"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimType {
    InDiff,
    NeedsExecution,
    ExternalAssumption,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cite {
    pub file: String,
    pub line: u32,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct FindingId(pub String);

#[must_use]
pub fn normalize_title(title: &str) -> String {
    let folded = title.to_lowercase();
    let mut out = String::new();
    for word in folded.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(word);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FindingV2 {
    pub id: FindingId,
    pub region: RegionId,
    pub aspect: AspectId,
    pub title: String,
    pub mechanism: String,
    pub claim: ClaimType,
    pub evidence: Vec<Cite>,
    pub severity: Severity,
    pub fix_hint: Option<String>,
}

fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub struct FindingDraft {
    pub region: RegionId,
    pub aspect: AspectId,
    pub title: String,
    pub mechanism: String,
    pub claim: ClaimType,
    pub evidence: Vec<Cite>,
    pub severity: Severity,
    pub fix_hint: Option<String>,
}

impl FindingV2 {
    #[must_use]
    pub fn new(draft: FindingDraft) -> Self {
        let mut slug = normalize_title(&draft.title);
        if slug.is_empty() {
            slug = format!("raw:{:016x}", fnv1a64(&draft.title));
        }
        let id = FindingId(format!("{}/{}/{}", draft.region.0, draft.aspect.0, slug));
        Self {
            id,
            region: draft.region,
            aspect: draft.aspect,
            title: draft.title,
            mechanism: draft.mechanism,
            claim: draft.claim,
            evidence: draft.evidence,
            severity: draft.severity,
            fix_hint: draft.fix_hint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum CellVerdict {
    Clean,
    Defect(FindingV2),
    OutOfScope,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegionReport {
    pub region: RegionId,
    pub note: String,
    pub cells: Vec<(AspectId, CellVerdict)>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JudgeReport {
    pub judge: JudgeId,
    pub regions: Vec<RegionReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolFinding {
    pub tool: String,
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reconciled {
    pub finding: FindingV2,
    pub votes: Vec<JudgeId>,
    pub severity: Severity,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverageSummary {
    pub cells_total: usize,
    pub cells_judged: usize,
    pub cells_clean: usize,
    pub cells_defect: usize,
    pub judged_by_judge: BTreeMap<JudgeId, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssueIdentity {
    pub file: String,
    pub line: u32,
    pub title_tokens: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum IssueStatus {
    Open,
    Retired { round: u32, sha: String },
    Rejected { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Issue {
    pub identity: IssueIdentity,
    pub first_seen: String,
    pub last_seen: String,
    pub status: IssueStatus,
    pub misses: u32,
    pub votes_seen: Vec<(u32, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Suppression {
    pub id: String,
    pub theme: String,
    pub file_glob: String,
    pub reason: String,
    pub created: String,
    pub hits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoundState {
    pub checkout: String,
    pub round: u32,
    pub issues: Vec<Issue>,
    pub suppressions: Vec<Suppression>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RoundVerdict {
    pub range: PinnedRange,
    pub coverage: CoverageSummary,
    pub tool_findings: Vec<ToolFinding>,
    pub confirmed: Vec<Reconciled>,
    pub solo: Vec<Reconciled>,
    pub refuted: Vec<Reconciled>,
    pub suppressed: Vec<Reconciled>,
    pub absent_judges: Vec<JudgeId>,
    pub state: RoundState,
}

#[must_use]
pub fn equivalent(a: &RoundVerdict, b: &RoundVerdict) -> bool {
    if a.range != b.range
        || a.coverage != b.coverage
        || a.tool_findings != b.tool_findings
        || a.state != b.state
    {
        return false;
    }
    lists_equivalent(&a.confirmed, &b.confirmed)
        && lists_equivalent(&a.solo, &b.solo)
        && lists_equivalent(&a.refuted, &b.refuted)
        && lists_equivalent(&a.suppressed, &b.suppressed)
        && sorted(&a.absent_judges) == sorted(&b.absent_judges)
}

fn sorted(judges: &[JudgeId]) -> Vec<&JudgeId> {
    let mut copy: Vec<&JudgeId> = judges.iter().collect();
    copy.sort();
    copy
}

fn lists_equivalent(a: &[Reconciled], b: &[Reconciled]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut left: Vec<&Reconciled> = a.iter().collect();
    let mut right: Vec<&Reconciled> = b.iter().collect();
    left.sort_by_key(|entry| entry.finding.id.clone());
    right.sort_by_key(|entry| entry.finding.id.clone());
    left.iter().zip(right.iter()).all(|(x, y)| {
        x.finding == y.finding && sorted(&x.votes) == sorted(&y.votes) && x.severity == y.severity
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_finding(title: &str, region: usize) -> FindingV2 {
        FindingV2::new(FindingDraft {
            region: RegionId(region),
            aspect: AspectId("concurrency".to_owned()),
            title: title.to_owned(),
            mechanism: "path → line → failure".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 10,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        })
    }

    fn sample_verdict(vote_order: [&str; 2]) -> RoundVerdict {
        let finding = sample_finding("Lock is dropped too early", 0);
        let reconciled = |votes: Vec<JudgeId>| Reconciled {
            finding: finding.clone(),
            votes,
            severity: Severity::Warning,
        };
        RoundVerdict {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            coverage: CoverageSummary {
                cells_total: 4,
                cells_judged: 4,
                cells_clean: 3,
                cells_defect: 1,
                judged_by_judge: BTreeMap::from([
                    (vote_order[0].to_owned(), 4),
                    (vote_order[1].to_owned(), 4),
                ]),
            },
            tool_findings: Vec::new(),
            confirmed: vec![reconciled(vec![
                vote_order[0].to_owned(),
                vote_order[1].to_owned(),
            ])],
            solo: Vec::new(),
            refuted: Vec::new(),
            suppressed: Vec::new(),
            absent_judges: Vec::new(),
            state: RoundState {
                checkout: "trunk".to_owned(),
                round: 1,
                issues: Vec::new(),
                suppressions: Vec::new(),
            },
        }
    }

    #[test]
    fn finding_id_is_stable_across_construction_order() {
        let first = sample_finding("Lock is dropped too early", 0);
        let second = sample_finding("Lock is dropped too early", 0);
        assert_eq!(first.id, second.id);
        assert_ne!(
            sample_finding("Lock is dropped too early", 1).id,
            first.id,
            "a different region is a different issue"
        );
        assert_ne!(
            sample_finding("Buffer grows unbounded", 0).id,
            first.id,
            "a different title is a different issue"
        );
    }

    #[test]
    fn punctuation_only_titles_still_get_distinct_ids() {
        let bangs = sample_finding("!!!", 0);
        let marks = sample_finding("???", 0);
        assert_ne!(bangs.id, marks.id, "the raw digest keeps them distinct");
        assert!(bangs.id.0.contains("raw:"), "the digest slug is present");
    }

    #[test]
    fn a_finding_id_normalizes_titles() {
        assert_eq!(
            sample_finding("Lock IS Dropped  too---early!", 0).id,
            sample_finding("lock is dropped too early", 0).id,
            "case, spacing, and punctuation fold away"
        );
        assert_eq!(normalize_title("— (…)"), "");
    }

    #[test]
    fn round_verdict_serializes_for_json_output() {
        let verdict = sample_verdict(["zai:glm-5.3", "local"]);
        let json = serde_json::to_string(&verdict).unwrap_or_default();
        let back: RoundVerdict =
            serde_json::from_str(&json).unwrap_or_else(|_| sample_verdict(["x", "y"]));
        assert_eq!(back, verdict, "the verdict round-trips");
        let reserialized = serde_json::to_string(&back).unwrap_or_default();
        assert_eq!(
            json, reserialized,
            "re-serializing the parsed verdict reproduces the same bytes"
        );
        assert!(
            json.contains("\"severity\":\"warning\"") && json.contains("\"round\":1"),
            "the wire spelling is pinned: {json}"
        );
    }

    #[test]
    fn cell_verdicts_round_trip_over_the_wire() -> Result<(), Box<dyn std::error::Error>> {
        let finding = sample_finding("Lock is dropped too early", 0);
        for (verdict, tag) in [
            (CellVerdict::Clean, "clean"),
            (CellVerdict::Defect(finding.clone()), "defect"),
            (CellVerdict::OutOfScope, "out-of-scope"),
        ] {
            let report = RegionReport {
                region: RegionId(0),
                note: "traced the guard through both drops".to_owned(),
                cells: vec![(AspectId("concurrency".to_owned()), verdict)],
            };
            let wire = serde_json::to_string(&report).unwrap_or_default();
            assert!(
                wire.contains(&format!("\"verdict\":\"{tag}\"")),
                "the verdict tag spelling is pinned: {wire}"
            );
            let back: RegionReport =
                serde_json::from_str(&wire).map_err(|err| format!("wire {wire}: {err}"))?;
            assert_eq!(
                serde_json::to_string(&back).unwrap_or_default(),
                wire,
                "the report re-serializes identically"
            );
        }
        let defect_wire = serde_json::to_string(&CellVerdict::Defect(finding)).unwrap_or_default();
        assert!(
            defect_wire.contains("\"claim\":\"in-diff\""),
            "claim types spell kebab-case on the wire: {defect_wire}"
        );
        Ok(())
    }

    #[test]
    fn equivalent_normalizes_judge_order_and_nothing_else() {
        let a = sample_verdict(["zai:glm-5.3", "local"]);
        let b = sample_verdict(["local", "zai:glm-5.3"]);
        assert!(
            equivalent(&a, &b),
            "reordered judges and votes are the same run"
        );
        let mut different = sample_verdict(["zai:glm-5.3", "local"]);
        different.coverage.cells_clean = 2;
        different.coverage.cells_defect = 2;
        assert!(
            !equivalent(&a, &different),
            "different coverage is different"
        );
        let mut extra_absent = sample_verdict(["zai:glm-5.3", "local"]);
        extra_absent.absent_judges = vec!["zai:glm-5.3-flash".to_owned()];
        assert!(!equivalent(&a, &extra_absent));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    CiteOutsideMaterials,
    MechanismCitesNoRegionLine,
    AssumptionNotCapped,
    SeverityAboveAspectCeiling,
    MalformedTitle,
}

pub const MAX_TITLE_CHARS: usize = 80;

pub fn validate_finding(
    finding: &FindingV2,
    region: &Region,
    index: &crate::diff::DiffIndex,
    aspects: &crate::aspects::AspectRegistry,
) -> Result<(), DropReason> {
    for cite in &finding.evidence {
        if !index.resolve(cite) {
            return Err(DropReason::CiteOutsideMaterials);
        }
    }
    if !mechanism_cites_region(&finding.mechanism, region, index) {
        return Err(DropReason::MechanismCitesNoRegionLine);
    }
    if finding.claim == ClaimType::ExternalAssumption && finding.severity > Severity::Suggestion {
        return Err(DropReason::AssumptionNotCapped);
    }
    if finding.severity > aspects.ceiling(&finding.aspect) {
        return Err(DropReason::SeverityAboveAspectCeiling);
    }
    if finding.title.trim().is_empty() || finding.title.chars().count() > MAX_TITLE_CHARS {
        return Err(DropReason::MalformedTitle);
    }
    Ok(())
}

fn mechanism_cites_region(
    mechanism: &str,
    region: &Region,
    index: &crate::diff::DiffIndex,
) -> bool {
    mechanism.split_whitespace().any(|word| {
        let cleaned = word
            .trim_matches(|c: char| {
                !c.is_alphanumeric() && c != ':' && c != '/' && c != '.' && c != '_'
            })
            .trim_matches('`');
        let Some((file, line)) = cleaned.rsplit_once(':') else {
            return false;
        };
        let Ok(line) = line.parse::<u32>() else {
            return false;
        };
        file == region.file
            && line >= region.start
            && line <= region.end
            && index.resolve(&Cite {
                file: file.to_owned(),
                line,
            })
    })
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use crate::aspects::AspectRegistry;
    use crate::diff::DiffIndex;

    const DIFF: &str =
        "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,5 +1,5 @@\n ctx\n+new\n ctx\n ctx\n ctx\n";

    fn setup() -> (DiffIndex, Region, AspectRegistry) {
        let index = DiffIndex::parse(DIFF).unwrap_or_default();
        let region = Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 5,
            kind: RegionKind::Added,
        };
        (index, region, crate::aspects::AspectRegistry::new())
    }

    fn draft() -> FindingDraft {
        FindingDraft {
            region: RegionId(0),
            aspect: AspectId("concurrency".to_owned()),
            title: "Lock is dropped too early".to_owned(),
            mechanism: "src/a.rs:2 releases before the read at src/a.rs:4 completes".to_owned(),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 2,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        }
    }

    fn with(mut d: FindingDraft, f: impl FnOnce(&mut FindingDraft)) -> FindingV2 {
        f(&mut d);
        FindingV2::new(d)
    }

    #[test]
    fn a_finding_whose_cite_lies_outside_the_materials_is_dropped() {
        let (index, region, aspects) = setup();
        let finding = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 99,
            }];
        });
        assert_eq!(
            validate_finding(&finding, &region, &index, &aspects),
            Err(DropReason::CiteOutsideMaterials)
        );
    }

    #[test]
    fn a_finding_whose_mechanism_cites_no_region_line_is_dropped() {
        let (index, region, aspects) = setup();
        let finding = with(draft(), |d| {
            d.mechanism = "the code is generally racy".to_owned();
        });
        assert_eq!(
            validate_finding(&finding, &region, &index, &aspects),
            Err(DropReason::MechanismCitesNoRegionLine)
        );
        let outside = with(draft(), |d| {
            d.mechanism = "src/other.rs:3 shows the bug".to_owned();
        });
        assert_eq!(
            validate_finding(&outside, &region, &index, &aspects),
            Err(DropReason::MechanismCitesNoRegionLine)
        );
    }

    #[test]
    fn an_assumption_above_suggestion_is_dropped() {
        let (index, region, aspects) = setup();
        let finding = with(draft(), |d| {
            d.claim = ClaimType::ExternalAssumption;
            d.severity = Severity::Warning;
        });
        assert_eq!(
            validate_finding(&finding, &region, &index, &aspects),
            Err(DropReason::AssumptionNotCapped)
        );
        let capped = with(draft(), |d| {
            d.claim = ClaimType::ExternalAssumption;
            d.severity = Severity::Suggestion;
        });
        assert_eq!(validate_finding(&capped, &region, &index, &aspects), Ok(()));
    }

    #[test]
    fn a_finding_above_the_aspect_ceiling_is_dropped() {
        let (index, region, aspects) = setup();
        let finding = with(draft(), |d| {
            d.aspect = AspectId("docs".to_owned());
            d.mechanism = "src/a.rs:2 contradicts the doc at src/a.rs:3".to_owned();
        });
        assert_eq!(
            validate_finding(&finding, &region, &index, &aspects),
            Err(DropReason::SeverityAboveAspectCeiling)
        );
        let tests_aspect = with(draft(), |d| {
            d.aspect = AspectId("tests".to_owned());
        });
        assert_eq!(
            validate_finding(&tests_aspect, &region, &index, &aspects),
            Ok(()),
            "warning sits at the tests ceiling"
        );
    }

    #[test]
    fn a_malformed_title_is_dropped() {
        let (index, region, aspects) = setup();
        let empty = with(draft(), |d| d.title = "   ".to_owned());
        assert_eq!(
            validate_finding(&empty, &region, &index, &aspects),
            Err(DropReason::MalformedTitle)
        );
        let long = with(draft(), |d| {
            d.title = "x".repeat(81);
        });
        assert_eq!(
            validate_finding(&long, &region, &index, &aspects),
            Err(DropReason::MalformedTitle)
        );
    }

    #[test]
    fn a_valid_finding_passes_all_five() {
        let (index, region, aspects) = setup();
        let finding = FindingV2::new(draft());
        assert_eq!(
            validate_finding(&finding, &region, &index, &aspects),
            Ok(())
        );
    }

    #[test]
    fn an_unknown_aspect_fails_closed() {
        let (index, region, aspects) = setup();
        let finding = with(draft(), |d| {
            d.aspect = AspectId("vibes".to_owned());
        });
        assert_eq!(
            validate_finding(&finding, &region, &index, &aspects),
            Err(DropReason::SeverityAboveAspectCeiling),
            "an unregistered aspect has a nitpick ceiling"
        );
    }
}
