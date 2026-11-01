//! The data spine: every pipeline stage communicates only through
//! these types — coverage plans, judge reports, findings, verdicts,
//! and the cross-round state they update.

use std::collections::BTreeMap;

use crate::error::{DtError, JudgeId};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PinnedRange {
    pub base_sha: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiffSource {
    Unified(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub file: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub conclusion: String,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ToolFinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Side {
    Right,
    Left,
}

impl Side {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Right => "RIGHT",
            Self::Left => "LEFT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReviewEvent {
    #[serde(rename = "APPROVE")]
    Approved,
    #[serde(rename = "REQUEST_CHANGES")]
    ChangesRequested,
    #[serde(rename = "COMMENT")]
    Comment,
}

impl ReviewEvent {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "APPROVE",
            Self::ChangesRequested => "REQUEST_CHANGES",
            Self::Comment => "COMMENT",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InlineComment {
    pub path: String,
    pub line: u32,
    pub side: Side,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewSubmission {
    pub head_sha: String,
    pub event: ReviewEvent,
    pub body: String,
    pub comments: Vec<InlineComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Thread {
    pub id: String,
    pub path: String,
    pub line: u32,
    pub original_line: Option<u32>,
    pub comment_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FlowMap {
    pub defines: Vec<String>,
    pub callers: Vec<SearchHit>,
    pub callees: Vec<String>,
    pub triggers: Vec<SearchHit>,
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
    pub narrowed: bool,
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
    AdjacentPreexisting,
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
pub struct Finding {
    pub id: FindingId,
    pub region: RegionId,
    pub aspect: AspectId,
    pub title: String,
    pub mechanism: String,
    pub claim: ClaimType,
    pub evidence: Vec<Cite>,
    pub severity: Severity,
    pub fix_hint: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub discarded_cites: usize,
}

fn is_default<T: PartialEq + Default>(value: &T) -> bool {
    *value == T::default()
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

impl Finding {
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
            discarded_cites: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum CellVerdict {
    Clean,
    Defect(Finding),
    OutOfScope,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegionReport {
    pub region: RegionId,
    pub note: String,
    pub cells: Vec<(AspectId, CellVerdict)>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegionStats {
    pub judge: JudgeId,
    pub region: RegionId,
    pub file: String,
    pub attempts: u8,
    pub judged: bool,
    pub secs: u64,
    pub turns: usize,
    pub tool_calls: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_secs: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JudgeReport {
    pub judge: JudgeId,
    pub regions: Vec<RegionReport>,
    #[serde(default)]
    pub failed: Vec<RegionId>,
    #[serde(default)]
    pub stats: Vec<RegionStats>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolFinding {
    pub tool: String,
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolEvidence {
    pub findings: Vec<ToolFinding>,
    pub unavailable: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reconciled {
    pub finding: Finding,
    pub votes: Vec<JudgeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub families: Vec<String>,
    pub severity: Severity,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OracleVerdict {
    Confirmed,
    Refuted { command: String },
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct OracleResults {
    pub verdicts: BTreeMap<FindingId, OracleVerdict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FileCoverage {
    pub cells: usize,
    pub judged: usize,
    pub clean: usize,
    pub defect: usize,
    #[serde(default, skip_serializing_if = "is_default")]
    pub short: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverageSummary {
    pub cells_total: usize,
    pub cells_judged: usize,
    pub cells_clean: usize,
    pub cells_defect: usize,
    pub judged_by_judge: BTreeMap<JudgeId, usize>,
    #[serde(default)]
    pub regions: usize,
    #[serde(default)]
    pub per_file: BTreeMap<String, FileCoverage>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub cells_short: usize,
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
    #[serde(default)]
    pub misses: u32,
    #[serde(default)]
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
    #[serde(default)]
    pub weak: Vec<WeakLead>,
    pub refuted: Vec<Reconciled>,
    pub suppressed: Vec<Reconciled>,
    #[serde(default)]
    pub dropped: Vec<Dropped>,
    pub absent_judges: Vec<JudgeId>,
    #[serde(default)]
    pub incomplete: Vec<JudgeId>,
    #[serde(default)]
    pub tools_unavailable: Vec<String>,
    #[serde(default)]
    pub stats: Vec<RegionStats>,
    #[serde(default)]
    pub lead_verdicts: std::collections::BTreeMap<String, crate::contracts::OracleVerdict>,
    pub state: RoundState,
}

impl RoundVerdict {
    #[must_use]
    pub fn blocking_count(&self) -> usize {
        let confirmed = self
            .confirmed
            .iter()
            .filter(|entry| entry.severity >= Severity::Warning)
            .count();
        let tools = self
            .tool_findings
            .iter()
            .filter(|finding| finding.severity >= Severity::Warning)
            .count();
        confirmed.saturating_add(tools)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Clean,
    BlockingFindings,
    InfrastructureFailure,
    PartialRound,
}

impl RunOutcome {
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::BlockingFindings => 1,
            Self::InfrastructureFailure => 2,
            Self::PartialRound => 3,
        }
    }
}

impl From<DtError> for RunOutcome {
    fn from(err: DtError) -> Self {
        match err {
            DtError::PartialRound { .. } => Self::PartialRound,
            _ => Self::InfrastructureFailure,
        }
    }
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
        && weak_equivalent(&a.weak, &b.weak)
        && dropped_equivalent(&a.dropped, &b.dropped)
        && sorted(&a.absent_judges) == sorted(&b.absent_judges)
        && sorted(&a.incomplete) == sorted(&b.incomplete)
        && sorted(&a.tools_unavailable) == sorted(&b.tools_unavailable)
        && a.lead_verdicts == b.lead_verdicts
}

fn dropped_equivalent(a: &[Dropped], b: &[Dropped]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut left: Vec<&Dropped> = a.iter().collect();
    let mut right: Vec<&Dropped> = b.iter().collect();
    left.sort_by_key(|entry| entry.finding.id.clone());
    right.sort_by_key(|entry| entry.finding.id.clone());
    left.iter()
        .zip(right.iter())
        .all(|(x, y)| x.finding == y.finding && x.reason == y.reason)
}

fn weak_equivalent(a: &[WeakLead], b: &[WeakLead]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut left: Vec<&WeakLead> = a.iter().collect();
    let mut right: Vec<&WeakLead> = b.iter().collect();
    left.sort_by_key(|entry| (entry.judge.clone(), entry.finding.id.clone(), entry.reason));
    right.sort_by_key(|entry| (entry.judge.clone(), entry.finding.id.clone(), entry.reason));
    left.iter()
        .zip(right.iter())
        .all(|(x, y)| x.judge == y.judge && x.finding == y.finding && x.reason == y.reason)
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DropReason {
    CiteOutsideMaterials,
    UnparseableEvidence,
    MechanismCitesNoRegionLine,
    AssumptionNotCapped,
    SeverityAboveAspectCeiling,
    MalformedTitle,
    NeedsRepair,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WeakLead {
    pub judge: JudgeId,
    pub finding: Finding,
    pub reason: DropReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repaired: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_at: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dropped {
    #[serde(default)]
    pub judge: JudgeId,
    pub finding: Finding,
    pub reason: DropReason,
}

pub const MAX_TITLE_CHARS: usize = 80;

pub type ServedReads = BTreeMap<String, Vec<(u32, u32)>>;

pub struct CiteAuthority<'a> {
    pub index: &'a crate::diff::DiffIndex,
    pub snapshots: &'a BTreeMap<String, crate::judges::prompt::SnapshotWindow>,
    pub served: &'a ServedReads,
}

impl CiteAuthority<'_> {
    #[must_use]
    pub fn admits(&self, region: &Region, cite: &Cite) -> bool {
        if self.index.resolve(cite) {
            return true;
        }
        if self.served_admits(&cite.file, cite.line) {
            return true;
        }
        matches!(region.kind, RegionKind::File | RegionKind::Repo)
            && cite.file == region.file
            && self
                .snapshots
                .get(&cite.file)
                .is_some_and(|window| crate::judges::prompt::snapshot_admits(*window, cite.line))
    }

    #[must_use]
    pub fn served_admits(&self, file: &str, line: u32) -> bool {
        self.served
            .get(file)
            .is_some_and(|windows| windows.iter().any(|(lo, hi)| line >= *lo && line <= *hi))
    }
}

pub fn repaired_title(finding: &Finding) -> Option<String> {
    let clause = finding
        .mechanism
        .split([';', '\n'])
        .flat_map(|segment| segment.split(". "))
        .map(str::trim)
        .find(|clause| clause.chars().any(char::is_alphanumeric))?;
    let mut kept = String::new();
    for ch in clause.chars() {
        if kept.chars().count() >= MAX_TITLE_CHARS.saturating_sub(1) {
            break;
        }
        kept.push(ch);
    }
    let trimmed = kept.trim_end();
    let mut title = trimmed.to_owned();
    if title.chars().count() >= MAX_TITLE_CHARS.saturating_sub(1) {
        title.push('…');
    }
    (!title.is_empty()).then_some(title)
}

pub fn validate_finding(
    finding: &Finding,
    region: &Region,
    authority: &CiteAuthority<'_>,
    aspects: &crate::aspects::AspectRegistry,
) -> Result<(), FindingFailure> {
    if finding.title.trim().is_empty() || finding.title.chars().count() > MAX_TITLE_CHARS {
        return Err(FindingFailure::Drop(DropReason::MalformedTitle));
    }
    if finding.claim == ClaimType::ExternalAssumption && finding.severity > Severity::Suggestion {
        return Err(FindingFailure::Drop(DropReason::AssumptionNotCapped));
    }
    if finding.severity > aspects.ceiling(&finding.aspect) {
        return Err(FindingFailure::Drop(DropReason::SeverityAboveAspectCeiling));
    }
    if finding.evidence.is_empty() {
        return Err(FindingFailure::Weaken(if finding.discarded_cites > 0 {
            DropReason::UnparseableEvidence
        } else {
            DropReason::CiteOutsideMaterials
        }));
    }
    for cite in &finding.evidence {
        if !authority.admits(region, cite) {
            return Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials));
        }
    }
    if !mechanism_cites_region(&finding.mechanism, region, authority) {
        return Err(FindingFailure::Weaken(
            DropReason::MechanismCitesNoRegionLine,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingFailure {
    Weaken(DropReason),
    Drop(DropReason),
}

fn mechanism_cites_region(mechanism: &str, region: &Region, authority: &CiteAuthority<'_>) -> bool {
    mechanism.split_whitespace().any(|word| {
        let Some(cite) = mechanism_cite(word, mechanism) else {
            return false;
        };
        let inside_span = cite.file == region.file
            && cite.line >= region.start
            && cite.line <= region.end
            && authority.admits(region, &cite);
        inside_span || (cite.file == region.file && authority.served_admits(&cite.file, cite.line))
    })
}

#[must_use]
pub fn mine_cites(mechanism: &str) -> Vec<Cite> {
    mechanism
        .split_whitespace()
        .filter_map(|word| mechanism_cite(word, mechanism))
        .collect()
}

fn mechanism_cite(word: &str, _mechanism: &str) -> Option<Cite> {
    let cleaned = word
        .trim_matches(|c: char| {
            !c.is_alphanumeric() && c != ':' && c != '/' && c != '.' && c != '_'
        })
        .trim_matches('`');
    let (file, line) = cleaned.rsplit_once(':')?;
    let line = line.parse::<u32>().ok()?;
    let path_like = file.contains('.') || file.contains('/');
    (!file.is_empty() && path_like).then(|| Cite {
        file: file.to_owned(),
        line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_check_run_without_findings_parses_empty() -> Result<(), Box<dyn std::error::Error>>
    {
        let run: CheckRun =
            serde_json::from_str(r#"{"name":"ci","conclusion":"success","summary":""}"#)?;
        assert!(run.findings.is_empty());
        Ok(())
    }

    #[test]
    fn a_review_submission_round_trips_with_its_inline_comments()
    -> Result<(), Box<dyn std::error::Error>> {
        let submission = ReviewSubmission {
            head_sha: "abc123".to_owned(),
            event: ReviewEvent::ChangesRequested,
            body: "difftrace reviewed abc123.".to_owned(),
            comments: vec![InlineComment {
                path: "src/a.rs".to_owned(),
                line: 10,
                side: Side::Right,
                body: "finding body".to_owned(),
            }],
        };
        let text = serde_json::to_string(&submission)?;
        assert!(
            text.contains(r#""event":"REQUEST_CHANGES""#),
            "the serde spelling matches the wire event: {text}"
        );
        let back: ReviewSubmission = serde_json::from_str(&text)?;
        assert_eq!(back, submission);
        assert_eq!(ReviewEvent::Approved.as_str(), "APPROVE");
        Ok(())
    }

    fn sample_finding(title: &str, region: usize) -> Finding {
        Finding::new(FindingDraft {
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

    #[test]
    fn a_legacy_judge_report_without_failed_or_incomplete_parses() {
        let report: JudgeReport =
            serde_json::from_str(r#"{"judge":"a","regions":[{"region":0,"note":"n","cells":[]}]}"#)
                .unwrap_or(JudgeReport {
                    judge: "parse failed".to_owned(),
                    regions: Vec::new(),
                    failed: Vec::new(),
                    stats: Vec::new(),
                });
        assert_eq!(report.judge, "a");
        assert!(report.failed.is_empty(), "absent keys default to empty");
        assert!(report.stats.is_empty(), "absent stats default to empty");
    }

    fn sample_verdict(vote_order: [&str; 2]) -> RoundVerdict {
        let finding = sample_finding("Lock is dropped too early", 0);
        let reconciled = |votes: Vec<JudgeId>| Reconciled {
            finding: finding.clone(),
            votes,
            families: Vec::new(),
            severity: Severity::Warning,
        };
        RoundVerdict {
            weak: Vec::new(),
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
                regions: 2,
                per_file: BTreeMap::from([(
                    "src/a.rs".to_owned(),
                    FileCoverage {
                        cells: 4,
                        judged: 4,
                        clean: 3,
                        defect: 1,
                        short: 0,
                    },
                )]),
                cells_short: 0,
            },
            tool_findings: Vec::new(),
            confirmed: vec![reconciled(vec![
                vote_order[0].to_owned(),
                vote_order[1].to_owned(),
            ])],
            solo: Vec::new(),
            refuted: Vec::new(),
            suppressed: Vec::new(),
            dropped: Vec::new(),
            absent_judges: Vec::new(),
            incomplete: Vec::new(),
            tools_unavailable: Vec::new(),
            stats: Vec::new(),
            lead_verdicts: std::collections::BTreeMap::new(),
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
        let mut more_incomplete = sample_verdict(["zai:glm-5.3", "local"]);
        more_incomplete.incomplete = vec!["zai:glm-5.3-flash".to_owned()];
        assert!(
            !equivalent(&a, &more_incomplete),
            "a run with an incomplete judge is not equivalent to a complete one"
        );
        let mut skipped_tools = sample_verdict(["zai:glm-5.3", "local"]);
        skipped_tools.tools_unavailable = vec!["clippy head side: tree differs".to_owned()];
        assert!(
            !equivalent(&a, &skipped_tools),
            "a run whose deterministic layer skipped is not equivalent to one that ran"
        );
        let mut reordered = sample_verdict(["zai:glm-5.3", "local"]);
        reordered.tools_unavailable = vec!["b skipped".to_owned(), "a skipped".to_owned()];
        let mut reordered_other = sample_verdict(["zai:glm-5.3", "local"]);
        reordered_other.tools_unavailable = vec!["a skipped".to_owned(), "b skipped".to_owned()];
        assert!(
            equivalent(&reordered, &reordered_other),
            "skip lists compare order-insensitively"
        );
        let mut confirmed_lead = sample_verdict(["zai:glm-5.3", "local"]);
        confirmed_lead
            .lead_verdicts
            .insert("0/error-paths/lead".to_owned(), OracleVerdict::Confirmed);
        assert!(
            !equivalent(&a, &confirmed_lead),
            "a lead the oracle confirmed distinguishes runs"
        );
    }

    #[test]
    fn equivalent_ignores_dropped_order_and_respects_content() {
        let entry = |title: &str, reason: DropReason| Dropped {
            judge: "zai:glm-5.3".to_owned(),
            finding: sample_finding(title, 0),
            reason,
        };
        let mut first = sample_verdict(["zai:glm-5.3", "local"]);
        first.dropped = vec![
            entry(
                "Lock is dropped too early",
                DropReason::CiteOutsideMaterials,
            ),
            entry("Buffer grows unbounded", DropReason::MalformedTitle),
        ];
        let mut second = sample_verdict(["zai:glm-5.3", "local"]);
        second.dropped = vec![
            entry("Buffer grows unbounded", DropReason::MalformedTitle),
            entry(
                "Lock is dropped too early",
                DropReason::CiteOutsideMaterials,
            ),
        ];
        assert!(
            equivalent(&first, &second),
            "a pure reordering of dropped entries is the same run"
        );
        second.dropped = vec![
            entry("Buffer grows unbounded", DropReason::MalformedTitle),
            entry("Lock is dropped too early", DropReason::MalformedTitle),
        ];
        assert!(
            !equivalent(&first, &second),
            "a different drop reason is a different run"
        );
        second.dropped.pop();
        assert!(
            !equivalent(&first, &second),
            "a missing dropped entry is a different run"
        );
    }

    #[test]
    fn equivalent_respects_weak_leads_and_ignores_their_order() {
        let lead = |title: &str, reason: DropReason| WeakLead {
            judge: "zai:glm-5.3".to_owned(),
            finding: sample_finding(title, 0),
            reason,
            repaired: None,
            also_at: Vec::new(),
        };
        let mut first = sample_verdict(["zai:glm-5.3", "local"]);
        first.weak = vec![
            lead(
                "Lock is dropped too early",
                DropReason::CiteOutsideMaterials,
            ),
            lead("Buffer grows unbounded", DropReason::UnparseableEvidence),
        ];
        let mut reordered = sample_verdict(["zai:glm-5.3", "local"]);
        reordered.weak = vec![
            lead("Buffer grows unbounded", DropReason::UnparseableEvidence),
            lead(
                "Lock is dropped too early",
                DropReason::CiteOutsideMaterials,
            ),
        ];
        assert!(
            equivalent(&first, &reordered),
            "a pure reordering of weak leads is the same run"
        );
        let mut drained = sample_verdict(["zai:glm-5.3", "local"]);
        drained.weak = vec![lead(
            "Buffer grows unbounded",
            DropReason::UnparseableEvidence,
        )];
        assert!(
            !equivalent(&first, &drained),
            "a weak lead one run raised and the other lost is drift, not equivalence"
        );
        let mut changed_reason = sample_verdict(["zai:glm-5.3", "local"]);
        changed_reason.weak = vec![
            lead(
                "Lock is dropped too early",
                DropReason::CiteOutsideMaterials,
            ),
            lead("Buffer grows unbounded", DropReason::CiteOutsideMaterials),
        ];
        assert!(
            !equivalent(&first, &changed_reason),
            "a different weaken reason is a different run"
        );
    }

    #[test]
    fn oracle_and_dropped_surfaces_round_trip_with_pinned_spellings() {
        let oracle = OracleResults {
            verdicts: BTreeMap::from([
                (
                    FindingId("0/error-paths/lock".to_owned()),
                    OracleVerdict::Confirmed,
                ),
                (
                    FindingId("0/docs/buffer".to_owned()),
                    OracleVerdict::Refuted {
                        command: "cargo test -p dt".to_owned(),
                    },
                ),
            ]),
        };
        let oracle_json = serde_json::to_string(&oracle).unwrap_or_default();
        let oracle_back: OracleResults = serde_json::from_str(&oracle_json).unwrap_or_default();
        assert_eq!(oracle_back, oracle, "the oracle results round-trip");
        assert!(
            oracle_json.contains("\"confirmed\"")
                && oracle_json.contains("\"refuted\"")
                && oracle_json.contains("\"command\""),
            "the oracle wire spellings are pinned: {oracle_json}"
        );
        let mut verdict = sample_verdict(["zai:glm-5.3", "local"]);
        verdict.dropped = vec![
            Dropped {
                judge: "zai:glm-5.3".to_owned(),
                finding: sample_finding("Cite hangs outside the materials", 0),
                reason: DropReason::CiteOutsideMaterials,
            },
            Dropped {
                judge: "local".to_owned(),
                finding: sample_finding("Evidence never parsed", 0),
                reason: DropReason::UnparseableEvidence,
            },
        ];
        let json = serde_json::to_string(&verdict).unwrap_or_default();
        let back: RoundVerdict =
            serde_json::from_str(&json).unwrap_or_else(|_| sample_verdict(["x", "y"]));
        assert_eq!(back, verdict, "a verdict with dropped entries round-trips");
        assert!(
            json.contains("\"cite-outside-materials\"")
                && json.contains("\"unparseable-evidence\""),
            "the drop reason spellings are pinned: {json}"
        );
    }

    #[test]
    fn a_verdict_without_regions_and_per_file_parses_with_defaults() -> Result<(), serde_json::Error>
    {
        let verdict = sample_verdict(["zai:glm-5.3", "local"]);
        let mut value = serde_json::to_value(&verdict)?;
        if let Some(serde_json::Value::Object(coverage)) = value.get_mut("coverage") {
            coverage.remove("regions");
            coverage.remove("per_file");
        }
        if let serde_json::Value::Object(root) = &mut value {
            root.remove("tools_unavailable");
        }
        let legacy = serde_json::to_string(&value)?;
        let back: RoundVerdict = serde_json::from_str(&legacy)?;
        assert_eq!(
            back.coverage.regions, 0,
            "a legacy verdict without the new fields parses to the default"
        );
        assert!(back.coverage.per_file.is_empty());
        assert!(
            back.tools_unavailable.is_empty(),
            "a legacy verdict without the skip list parses to no skips"
        );
        assert_eq!(back.coverage.cells_total, verdict.coverage.cells_total);
        Ok(())
    }

    #[test]
    fn a_finding_without_discarded_cites_omits_the_field_and_legacy_parses_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        let finding = sample_finding("Lock is dropped too early", 0);
        let json = serde_json::to_string(&finding)?;
        assert!(
            !json.contains("discarded_cites"),
            "the clean path serializes no skip counter: {json}"
        );
        let legacy = r#"{"id":"0/concurrency/lock","region":0,"aspect":"concurrency","title":"Lock is dropped too early","mechanism":"m","claim":"in-diff","evidence":[],"severity":"warning","fix_hint":null}"#;
        let back: Finding = serde_json::from_str(legacy)?;
        assert_eq!(back.discarded_cites, 0, "legacy findings default to zero");
        Ok(())
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use crate::aspects::AspectRegistry;
    use crate::diff::DiffIndex;

    const DIFF: &str =
        "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,4 +1,5 @@\n ctx\n+new\n ctx\n ctx\n ctx\n";

    static SERVED: std::sync::LazyLock<ServedReads> = std::sync::LazyLock::new(ServedReads::new);

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

    fn no_head_lines() -> BTreeMap<String, crate::judges::prompt::SnapshotWindow> {
        BTreeMap::new()
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

    fn with(mut d: FindingDraft, f: impl FnOnce(&mut FindingDraft)) -> Finding {
        f(&mut d);
        Finding::new(d)
    }

    #[test]
    fn a_finding_whose_cite_lies_outside_the_materials_is_dropped() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 99,
            }];
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials))
        );
        let bare = with(draft(), |d| {
            d.evidence = Vec::new();
        });
        assert_eq!(
            validate_finding(&bare, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials)),
            "a finding with no evidence grounds nothing"
        );
    }

    #[test]
    fn a_finding_whose_mechanism_cites_no_region_line_is_dropped() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = with(draft(), |d| {
            d.mechanism = "the code is generally racy".to_owned();
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(
                DropReason::MechanismCitesNoRegionLine,
            ))
        );
        let outside = with(draft(), |d| {
            d.mechanism = "src/other.rs:3 shows the bug".to_owned();
        });
        assert_eq!(
            validate_finding(&outside, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(
                DropReason::MechanismCitesNoRegionLine,
            ))
        );
    }

    #[test]
    fn an_assumption_above_suggestion_is_dropped() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = with(draft(), |d| {
            d.claim = ClaimType::ExternalAssumption;
            d.severity = Severity::Warning;
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Err(FindingFailure::Drop(DropReason::AssumptionNotCapped))
        );
        let capped = with(draft(), |d| {
            d.claim = ClaimType::ExternalAssumption;
            d.severity = Severity::Suggestion;
        });
        assert_eq!(
            validate_finding(&capped, &region, &authority, &aspects),
            Ok(())
        );
    }

    #[test]
    fn a_finding_above_the_aspect_ceiling_is_dropped() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = with(draft(), |d| {
            d.aspect = AspectId("docs".to_owned());
            d.mechanism = "src/a.rs:2 contradicts the doc at src/a.rs:3".to_owned();
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Err(FindingFailure::Drop(DropReason::SeverityAboveAspectCeiling))
        );
        let tests_aspect = with(draft(), |d| {
            d.aspect = AspectId("tests".to_owned());
        });
        assert_eq!(
            validate_finding(&tests_aspect, &region, &authority, &aspects),
            Ok(()),
            "warning sits at the tests ceiling"
        );
    }

    #[test]
    fn a_malformed_title_is_dropped() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let empty = with(draft(), |d| d.title = "   ".to_owned());
        assert_eq!(
            validate_finding(&empty, &region, &authority, &aspects),
            Err(FindingFailure::Drop(DropReason::MalformedTitle))
        );
        let long = with(draft(), |d| {
            d.title = "x".repeat(81);
        });
        assert_eq!(
            validate_finding(&long, &region, &authority, &aspects),
            Err(FindingFailure::Drop(DropReason::MalformedTitle))
        );
    }

    #[test]
    fn a_valid_finding_passes_all_five() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = Finding::new(draft());
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Ok(())
        );
    }

    #[test]
    fn an_unknown_aspect_fails_closed() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = with(draft(), |d| {
            d.aspect = AspectId("vibes".to_owned());
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Err(FindingFailure::Drop(DropReason::SeverityAboveAspectCeiling)),
            "an unregistered aspect has a nitpick ceiling"
        );
    }

    fn file_region() -> Region {
        Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 40,
            kind: RegionKind::File,
        }
    }

    #[test]
    fn a_file_region_admits_a_cite_outside_the_hunks_inside_the_file() {
        let (index, _, aspects) = setup();
        let snapshots = BTreeMap::from([(
            "src/a.rs".to_owned(),
            crate::judges::prompt::SnapshotWindow::line_window(40),
        )]);
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 12,
            }];
            d.mechanism =
                "src/a.rs:12 releases before the read at src/a.rs:14 completes".to_owned();
        });
        assert_eq!(
            validate_finding(&finding, &file_region(), &authority, &aspects),
            Ok(()),
            "deep and full scope regions were given the whole head snapshot, so its lines admit"
        );
    }

    #[test]
    fn an_added_region_never_admits_a_line_outside_the_hunks() {
        let (index, region, aspects) = setup();
        let snapshots = BTreeMap::from([(
            "src/a.rs".to_owned(),
            crate::judges::prompt::SnapshotWindow::line_window(40),
        )]);
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let finding = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 12,
            }];
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials)),
            "the diff scope's hunk-only authority is unchanged by head snapshots"
        );
    }

    #[test]
    fn a_file_region_still_drops_cites_beyond_the_file_or_without_a_snapshot() {
        let (index, _, aspects) = setup();
        let snapshots = BTreeMap::from([(
            "src/a.rs".to_owned(),
            crate::judges::prompt::SnapshotWindow::line_window(40),
        )]);
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let beyond = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 41,
            }];
            d.mechanism = "src/a.rs:41 ends past the snapshot".to_owned();
        });
        assert_eq!(
            validate_finding(&beyond, &file_region(), &authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials)),
            "a cite past the head file's last line grounds nothing"
        );
        let snapshots = no_head_lines();
        let bare_authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let in_file = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 12,
            }];
            d.mechanism = "src/a.rs:12 releases before the read".to_owned();
        });
        assert_eq!(
            validate_finding(&in_file, &file_region(), &bare_authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials)),
            "without a recorded snapshot length the file falls back to diff-only authority"
        );
    }

    #[test]
    fn a_tool_served_line_admits_a_cite_any_region_kind() {
        let (index, region, aspects) = setup();
        let mut served = ServedReads::new();
        served.insert("src/a.rs".to_owned(), vec![(800, 1200)]);
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &served,
        };
        let finding = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 950,
            }];
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Ok(()),
            "a cite into a window the judge's own tool served is grounded in any scope"
        );
    }

    #[test]
    fn a_mechanism_citing_a_served_line_of_the_region_file_passes() {
        let (index, region, aspects) = setup();
        let mut served = ServedReads::new();
        served.insert("src/a.rs".to_owned(), vec![(40, 60)]);
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &served,
        };
        let finding = with(draft(), |d| {
            d.mechanism = "src/a.rs:52 breaks the contract the region changes".to_owned();
        });
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Ok(()),
            "a mechanism walking to a served line of the region's file is a walk the judge saw"
        );
    }

    #[test]
    fn a_file_region_admits_only_the_snapshot_window_for_long_files() {
        let (index, _, aspects) = setup();
        let snapshots = BTreeMap::from([(
            "src/a.rs".to_owned(),
            crate::judges::prompt::SnapshotWindow::line_window(1000),
        )]);
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let region = Region {
            file: "src/a.rs".to_owned(),
            start: 1,
            end: 1000,
            kind: RegionKind::File,
        };
        let tail = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 950,
            }];
            d.mechanism = "src/a.rs:950 sits in the tail the judge did see".to_owned();
        });
        assert_eq!(
            validate_finding(&tail, &region, &authority, &aspects),
            Ok(()),
            "a cite inside the truncated snapshot's tail window grounds"
        );
        let middle = with(draft(), |d| {
            d.evidence = vec![Cite {
                file: "src/a.rs".to_owned(),
                line: 500,
            }];
            d.mechanism = "src/a.rs:500 hides in the truncated middle".to_owned();
        });
        assert_eq!(
            validate_finding(&middle, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials)),
            "a cite into lines the judge's materials never contained weakens, not validates"
        );
    }

    #[test]
    fn evidence_that_never_parses_drops_as_unparseable_not_outside_materials() {
        let (index, region, aspects) = setup();
        let snapshots = no_head_lines();
        let authority = CiteAuthority {
            index: &index,
            snapshots: &snapshots,
            served: &SERVED,
        };
        let mut finding = Finding::new(draft());
        finding.evidence = Vec::new();
        finding.discarded_cites = 2;
        assert_eq!(
            validate_finding(&finding, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::UnparseableEvidence)),
            "the model misused the file:line contract, which is not the same as citing nothing"
        );
        let mut untouched = Finding::new(draft());
        untouched.evidence = Vec::new();
        assert_eq!(
            validate_finding(&untouched, &region, &authority, &aspects),
            Err(FindingFailure::Weaken(DropReason::CiteOutsideMaterials)),
            "a finding that sent no evidence at all keeps its old bucket"
        );
    }
}
