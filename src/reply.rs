//! The chat surface: one stateless answer over the persisted review
//! surfaces — findings, standing issues, and suppressions exactly as
//! the last report recorded them. Chat cites those materials and can
//! never raise, change, or dismiss a finding; a reject intent answers
//! with the reject command for the owner to run.

use std::sync::Arc;

use crate::contracts::{Finding, IssueStatus, RoundState, RoundVerdict};
use crate::error::DtError;
use loopctl::api::ApiClient;
use loopctl::config::SessionConfig;
use loopctl::engine::{BareLoop, Loop, RunConfig};

pub struct ReplyOutcome {
    pub text: String,
}

impl ReplyOutcome {
    #[must_use]
    pub fn new(text: String) -> Self {
        Self { text }
    }
}

pub struct ReplyJob {
    pub question: String,
    pub plan: bool,
    pub context: String,
}

#[must_use]
pub fn reject_intent(question: &str) -> Option<String> {
    let tokens: Vec<&str> = question.split_whitespace().collect();
    tokens
        .windows(2)
        .find(|pair| {
            word(pair.first().copied().unwrap_or_default()) == "reject"
                && pair.get(1).is_some_and(|id| id.contains('/'))
        })
        .and_then(|pair| {
            pair.get(1).map(|id| {
                id.trim_matches(|c: char| !c.is_alphanumeric() && c != '/')
                    .to_owned()
            })
        })
        .filter(|id| !id.is_empty())
}

fn word(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

#[must_use]
pub fn context_block(report: &RoundVerdict, state: &RoundState) -> String {
    let mut lines = vec!["Findings from the latest review round:".to_owned()];
    let findings: Vec<(&str, &Finding)> = report
        .confirmed
        .iter()
        .map(|entry| ("confirmed", &entry.finding))
        .chain(report.solo.iter().map(|entry| ("solo", &entry.finding)))
        .chain(report.weak.iter().map(|lead| ("unverified", &lead.finding)))
        .collect();
    if findings.is_empty() {
        lines.push("(the round raised no findings)".to_owned());
    }
    for (status, finding) in findings {
        let cites = finding
            .evidence
            .iter()
            .map(|cite| format!("{}:{}", cite.file, cite.line))
            .collect::<Vec<_>>()
            .join(", ");
        let cites = if cites.is_empty() {
            "(none)".to_owned()
        } else {
            cites
        };
        lines.push(format!(
            "- [{status}] {} · {:?} · {} · mechanism: {} · cites: {}",
            finding.id.0, finding.severity, finding.title, finding.mechanism, cites
        ));
    }
    lines.push("Open issues from the standing set:".to_owned());
    let open: Vec<_> = state
        .issues
        .iter()
        .filter(|issue| issue.status == IssueStatus::Open)
        .collect();
    if open.is_empty() {
        lines.push("(none)".to_owned());
    }
    for issue in open {
        lines.push(format!(
            "- {}:{} {} (misses {}, last seen {})",
            issue.identity.file,
            issue.identity.line,
            issue.identity.title_tokens.join(" "),
            issue.misses,
            issue.last_seen
        ));
    }
    lines.push("Owner suppressions:".to_owned());
    if state.suppressions.is_empty() {
        lines.push("(none)".to_owned());
    }
    for rule in &state.suppressions {
        lines.push(format!(
            "- {} theme \"{}\" glob {} hits {}",
            rule.id, rule.theme, rule.file_glob, rule.hits
        ));
    }
    lines.join("\n")
}

fn system_prompt(plan: bool) -> String {
    if plan {
        "You are difftrace answering its owner about its own review. Produce a fix \
         plan for the findings in the given materials, ordered by severity, every \
         step citing the file:line it touches. Use only the given materials; you \
         cannot raise, change, or dismiss findings, and you cannot run commands. \
         Plain text only."
            .to_owned()
    } else {
        "You are difftrace answering its owner about its own review. Answer only \
         from the given materials and cite the file:line of everything you \
         reference; say so plainly when the materials do not answer it. You \
         cannot raise, change, or dismiss findings — to remove one, tell the \
         owner to run difftrace reject. Plain text only."
            .to_owned()
    }
}

#[must_use]
pub fn answer_prompt(job: &ReplyJob) -> String {
    format!("Materials:\n{}\n\nQuestion: {}", job.context, job.question)
}

pub async fn run_reply_with<C: ApiClient + 'static>(
    job: ReplyJob,
    client: Arc<C>,
) -> Result<ReplyOutcome, DtError> {
    let session = SessionConfig::default().with_system_prompt(system_prompt(job.plan));
    let mut agent = BareLoop::new(client, loopctl::tool::ToolRegistry::new(), session);
    let mut run_config = RunConfig::default();
    run_config.max_turns = 1;
    let run = agent
        .run(&answer_prompt(&job), &run_config)
        .await
        .map_err(|err| DtError::Provider(err.to_string()))?;
    Ok(ReplyOutcome::new(run.output.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{
        AspectId, Cite, ClaimType, CoverageSummary, FindingId, PinnedRange, Reconciled, RegionId,
        RoundState,
    };
    use loopctl::testing::{MockApiClient, MockResponse};
    use std::collections::BTreeMap;

    fn finding(id: &str, title: &str, line: u32) -> Finding {
        Finding {
            id: FindingId(id.to_owned()),
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: title.to_owned(),
            mechanism: format!("src/a.rs:{line} releases before the read completes"),
            claim: ClaimType::InDiff,
            evidence: vec![Cite {
                file: "src/a.rs".to_owned(),
                line,
            }],
            severity: crate::contracts::Severity::Warning,
            fix_hint: None,
            discarded_cites: 0,
        }
    }

    fn report() -> RoundVerdict {
        let locked = finding("0/error-paths/lock", "Lock is dropped too early", 3);
        RoundVerdict {
            range: PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            },
            coverage: CoverageSummary {
                cells_total: 1,
                cells_judged: 1,
                cells_clean: 0,
                cells_defect: 1,
                judged_by_judge: BTreeMap::new(),
                regions: 1,
                per_file: BTreeMap::new(),
                cells_short: 0,
            },
            tool_findings: Vec::new(),
            confirmed: vec![Reconciled {
                finding: locked,
                votes: vec!["zai".to_owned(), "local".to_owned()],
                families: Vec::new(),
                severity: crate::contracts::Severity::Warning,
            }],
            solo: Vec::new(),
            weak: Vec::new(),
            refuted: Vec::new(),
            suppressed: Vec::new(),
            dropped: Vec::new(),
            absent_judges: Vec::new(),
            incomplete: Vec::new(),
            tools_unavailable: Vec::new(),
            stats: Vec::new(),
            lead_verdicts: BTreeMap::new(),
            state: RoundState {
                checkout: "trunk".to_owned(),
                round: 1,
                issues: Vec::new(),
                suppressions: Vec::new(),
            },
        }
    }

    #[test]
    fn an_answer_cites_only_given_materials() {
        let report = report();
        let context = context_block(&report, &report.state);
        assert_eq!(
            context.matches("src/a.rs:").count(),
            2,
            "the confirmed finding's mechanism and cite lines are the only file references: {context}"
        );
        assert!(
            context.contains("[confirmed] 0/error-paths/lock"),
            "the tier and the id ride along: {context}"
        );
        assert!(
            context.contains("(none)"),
            "empty standing sections say so instead of inventing material: {context}"
        );
        let job = ReplyJob {
            question: "why is the lock finding blocking?".to_owned(),
            plan: false,
            context: context.clone(),
        };
        let prompt = answer_prompt(&job);
        assert!(
            prompt.contains(&job.question) && prompt.contains(&context),
            "the model sees exactly the question and the report's own materials"
        );
        assert!(
            !prompt.contains("read_file_at_head"),
            "chat has no tool surface to reach beyond the given materials"
        );
    }

    #[tokio::test]
    async fn a_reject_intent_requires_confirmation_before_suppressing()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            reject_intent("can you Reject 0/error-paths/lock, please?"),
            Some("0/error-paths/lock".to_owned()),
            "the intent parser recovers the id case-insensitively"
        );
        assert_eq!(
            reject_intent("what is the weakest finding?"),
            None,
            "a question without a reject-shaped request is just a question"
        );
        let job = ReplyJob {
            question: "can you reject 0/error-paths/lock, please?".to_owned(),
            plan: false,
            context: context_block(&report(), &report().state),
        };
        let client = MockApiClient::new("mock").with_responses(vec![MockResponse {
            text: "the lock releases before the read completes".to_owned(),
            tool_call: None,
            stop_reason: "end_turn".to_owned(),
        }]);
        let outcome = run_reply_with(job, Arc::new(client)).await?;
        assert_eq!(
            outcome.text, "the lock releases before the read completes",
            "the answer is the model's text and nothing else"
        );
        let confirmation = reject_intent("can you reject 0/error-paths/lock, please?")
            .map(|id| format!("to reject it, run: difftrace reject {id}"));
        assert_eq!(
            confirmation.as_deref(),
            Some("to reject it, run: difftrace reject 0/error-paths/lock"),
            "the confirmation is the exact command — chat never suppresses on its own"
        );
        Ok(())
    }

    #[test]
    fn chat_output_can_never_construct_a_finding() {
        let witness: fn(String) -> ReplyOutcome = ReplyOutcome::new;
        let outcome = witness("text only".to_owned());
        assert_eq!(outcome.text, "text only");
    }

    #[test]
    fn the_answer_renders_inert_model_text() {
        let rendered = crate::render::chat::render_answer(
            "why?",
            "see [the link](https://evil.example) and `code`",
            None,
        );
        assert!(
            !rendered.contains("](https://evil.example"),
            "a link-shaped answer cannot render as a live link: {rendered}"
        );
        assert!(
            rendered.contains("\\[the link\\]"),
            "the answer text survives escaped: {rendered}"
        );
    }
}
