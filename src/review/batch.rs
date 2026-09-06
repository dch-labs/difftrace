//! Batch orchestration: split the changed files into batches, review each
//! with one agent run, aggregate and ground the findings, then either post
//! the review or hand back the identical rendered content for a dry run —
//! the render is the single code path, only the terminal step differs.

use loopctl::api::ApiClient;

use crate::error::DifftraceError;
use crate::findings::Finding;
use crate::findings::Findings;
use crate::findings::ReviewSummary;
use crate::findings::Severity;
use crate::github::CommentPosition;
use crate::github::ReviewEvent;
use crate::github::ReviewSubmission;
use crate::github::ReviewThread;
use crate::prompts::fix_all_section;
use crate::prompts::re_raised_reply_body;
use crate::prompts::review_round_body;
use crate::review::ReviewRunner;
use crate::review::registry::Issue;
use crate::review::registry::IssueStatus;
use crate::review::registry::Registry;
use crate::review::registry::RoundFindings;
use crate::review::registry::embed_registry;
use crate::review::registry::extract_registry;
use crate::review::registry::parse_issue_header;
use crate::review::registry::same_issue_title;
use crate::review::registry::short_sha;
use crate::tools::submit::DroppedFinding;
use crate::tools::submit::ground_findings;

pub(crate) const VERDICT_MARKER: &str = "<!-- difftrace:verdict -->";
const VERDICT_WRITE_ATTEMPTS: u8 = 3;
const VERDICT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

pub fn plan_batches(files: &[String], batch_size: usize) -> Vec<Vec<String>> {
    let mut sorted = files.to_vec();
    sorted.sort();
    let size = batch_size.max(1);
    sorted
        .chunks(size)
        .map(<[String]>::to_vec)
        .collect::<Vec<_>>()
}

pub struct ReviewOutcome {
    pub summary: ReviewSummary,
    pub findings: Vec<Finding>,
    pub comments: Vec<CommentPosition>,
    pub dropped: Vec<DroppedFinding>,
    pub verified_out: Vec<(Finding, String)>,
    pub fixed_this_round: Vec<String>,
    pub unreviewed_batches: Vec<String>,
    pub pr: u64,
    pub head_sha: String,
    pub posted: bool,
    pub round_body: String,
    pub standing_body: String,
}

fn verification_note(verified: &[(Finding, String)]) -> String {
    if verified.is_empty() {
        return String::new();
    }
    let items = verified
        .iter()
        .map(|(finding, reason)| {
            format!(
                "- `{}:{}` — {} — {}",
                finding.file, finding.line, finding.title, reason
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("## Dropped after verification\n{items}")
}

fn unreviewed_note(batches: &[String]) -> String {
    if batches.is_empty() {
        return String::new();
    }
    let items = batches
        .iter()
        .map(|files| format!("- `{files}` — the reviewer run failed twice"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## ⚠️ Unreviewed files\n{items}\n\nThese files could not be reviewed this round; the rest of the review stands."
    )
}

fn drops_note(dropped: &[DroppedFinding]) -> String {
    if dropped.is_empty() {
        return String::new();
    }
    let drops = dropped
        .iter()
        .map(|entry| {
            format!(
                "<!-- {}:{} — {} -->",
                entry.finding.file, entry.finding.line, entry.reason
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("<!-- difftrace: dropped findings (never posted) -->\n{drops}")
}

fn issue_anchor(issue: &Issue) -> String {
    match issue.line {
        Some(line) => format!("`{}:{}`", issue.file, line),
        None => format!("`{}` (unanchored)", issue.file),
    }
}

fn registry_verdict_section(registry: &Registry, unreviewed: &[String]) -> String {
    let unresolved = registry.unresolved();
    let blockers = unresolved
        .iter()
        .filter(|issue| is_blocking(issue.severity))
        .count();
    let good = if unreviewed.is_empty() {
        "🎉 Good to go"
    } else {
        "⚠️ Approval withheld — some files went unreviewed (listed below)"
    };
    if blockers == 0 {
        if unresolved.is_empty() {
            return format!("## Verdict\n\n{good} — no unresolved findings.");
        }
        let list = unresolved
            .iter()
            .enumerate()
            .map(|(index, issue)| {
                format!(
                    "{}. {} — {} {} ({})",
                    index.saturating_add(1),
                    issue_anchor(issue),
                    issue.severity.glyph(),
                    issue.title,
                    crate::findings::complexity_glyph(issue.complexity),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return format!(
            "## Verdict\n\n{good} — no unresolved blocking findings. Nitpicks and suggestions don't block:\n\n{list}"
        );
    }
    let noun = if blockers == 1 { "finding" } else { "findings" };
    let list = unresolved
        .iter()
        .enumerate()
        .map(|(index, issue)| {
            format!(
                "{}. {} — {} {} ({})",
                index.saturating_add(1),
                issue_anchor(issue),
                issue.severity.glyph(),
                issue.title,
                crate::findings::complexity_glyph(issue.complexity),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Verdict\n\n🔴 Not good to go — {blockers} blocking {noun} (all unresolved, blockers first):\n\n{list}\n\nTo be good to go: fix the blockers — each inline comment carries a fix prompt, and each round's review body carries a copy-all prompt for its findings."
    )
}

fn history_section(registry: &Registry) -> String {
    let done = registry.fixed_history();
    if done.is_empty() {
        return String::new();
    }
    let list = done
        .iter()
        .map(|issue| {
            let resolution = if issue.status == IssueStatus::ManuallyResolved {
                "manually resolved".to_owned()
            } else {
                format!(
                    "fixed in round {} (`{}`)",
                    issue.resolved_round.unwrap_or(0),
                    short_sha(issue.resolved_sha.as_deref().unwrap_or(""))
                )
            };
            format!(
                "- ✅ ~~{}~~ {} — {}",
                issue.title,
                issue_anchor(issue),
                resolution
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("## ✅ Issue history\n{list}")
}

fn standing_render(outcome: &ReviewOutcome, registry: &Registry) -> String {
    let sections = [
        registry_verdict_section(registry, &outcome.unreviewed_batches),
        format!("## Summary\n{}", outcome.summary.summary),
        risks_section(&outcome.summary.risk_notes),
        format!("## Tests\n{}", outcome.summary.tests),
        drops_note(&outcome.dropped),
        verification_note(&outcome.verified_out),
        unreviewed_note(&outcome.unreviewed_batches),
        history_section(registry),
    ];
    sections
        .iter()
        .filter(|section| !section.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n"
}

pub(crate) fn is_blocking(severity: Severity) -> bool {
    matches!(severity, Severity::Warning | Severity::Critical)
}

pub(crate) fn review_event(findings: &[Finding]) -> ReviewEvent {
    let blocking = findings.iter().any(|finding| is_blocking(finding.severity));
    if blocking {
        ReviewEvent::ChangesRequested
    } else {
        ReviewEvent::Approved
    }
}

fn verdict_comment_body(outcome: &ReviewOutcome, registry: &Registry) -> Result<String, String> {
    let rendered = format!(
        "{VERDICT_MARKER}\n\n{}\n\n---\nReviewed commit: `{}`",
        standing_render(outcome, registry).trim_end(),
        outcome.head_sha
    );
    embed_registry(&rendered, registry)
}

struct ReplySplit {
    positions: Vec<CommentPosition>,
    replies: Vec<(u64, String)>,
    matched: Vec<String>,
    finding_threads: Vec<Option<String>>,
    retired: Vec<String>,
}

fn split_replies(
    threads: &[ReviewThread],
    comments: Vec<CommentPosition>,
    also_locations: &[Vec<(String, u64)>],
    head_sha: &str,
    registry: &Registry,
) -> ReplySplit {
    let mut split = ReplySplit {
        positions: Vec::new(),
        replies: Vec::new(),
        matched: Vec::new(),
        finding_threads: Vec::new(),
        retired: Vec::new(),
    };
    let at_anchor = |thread: &ReviewThread, comment: &CommentPosition| {
        thread.line.or(thread.original_line) == Some(comment.line) && thread.path == comment.path
    };
    let at_location = |thread: &ReviewThread, location: &(String, u64)| {
        thread.path == location.0 && thread.line.or(thread.original_line) == Some(location.1)
    };
    let recorded_title = |thread: &ReviewThread| {
        registry
            .issues
            .iter()
            .find(|issue| issue.thread_id.as_deref() == Some(thread.id.as_str()))
            .map(|issue| issue.title.as_str())
    };
    let titles: Vec<Option<String>> = comments
        .iter()
        .map(|comment| parse_issue_header(&comment.body).map(|(_, _, title)| title))
        .collect();
    let location_sets: Vec<Vec<(String, u64)>> = comments
        .iter()
        .enumerate()
        .map(|(comment_index, comment)| {
            let also = also_locations
                .get(comment_index)
                .cloned()
                .unwrap_or_default();
            std::iter::once((comment.path.clone(), comment.line))
                .chain(also)
                .collect()
        })
        .collect();
    // Pass one pairs comments with the threads that already carry the
    // same issue: the anchor thread takes the group's re-raise, and any
    // secondary location's matching thread keeps its own reply — a
    // grouped finding that reappeared must never resolve the thread it
    // reappeared in.
    let mut anchor_matches: Vec<Option<usize>> = Vec::new();
    let mut secondary_matches: Vec<(usize, usize)> = Vec::new();
    let mut claimed: Vec<String> = Vec::new();
    for (comment_index, comment) in comments.iter().enumerate() {
        let anchor = threads.iter().position(|thread| {
            !claimed.contains(&thread.id)
                && at_anchor(thread, comment)
                && same_issue(recorded_title(thread), &comment.body)
        });
        if let Some(thread) = anchor.and_then(|thread_index| threads.get(thread_index)) {
            claimed.push(thread.id.clone());
        }
        anchor_matches.push(anchor);
        let Some((_, rest)) = location_sets
            .get(comment_index)
            .and_then(|set| set.split_first())
        else {
            continue;
        };
        for location in rest {
            for (thread_index, thread) in threads.iter().enumerate() {
                if claimed.contains(&thread.id) {
                    continue;
                }
                if at_location(thread, location)
                    && same_issue(recorded_title(thread), &comment.body)
                {
                    claimed.push(thread.id.clone());
                    secondary_matches.push((thread_index, comment_index));
                }
            }
        }
    }
    // Pass two retires only the unclaimed threads whose recorded issue
    // differs from every comment covering their location: retiring
    // resolves the thread and lets the registry record the old issue as
    // fixed instead of letting a new issue overwrite it. A thread whose
    // issue matches any covering comment survives for that comment's
    // reply, whatever order the model emitted the findings in.
    for thread in threads {
        if claimed.contains(&thread.id) {
            continue;
        }
        let Some(recorded) = recorded_title(thread) else {
            continue;
        };
        let covering: Vec<Option<&String>> = comments
            .iter()
            .enumerate()
            .filter(|(comment_index, _)| {
                location_sets
                    .get(*comment_index)
                    .is_some_and(|set| set.iter().any(|location| at_location(thread, location)))
            })
            .map(|(comment_index, _)| titles.get(comment_index).and_then(Option::as_ref))
            .collect();
        let replaceable = !covering.is_empty()
            && covering
                .iter()
                .all(|title| title.is_none_or(|title| !same_issue_title(recorded, title)));
        if replaceable {
            split.retired.push(thread.id.clone());
        }
    }
    for (comment_index, comment) in comments.into_iter().enumerate() {
        if let Some(thread_index) = anchor_matches.get(comment_index).copied().flatten() {
            let Some(thread) = threads.get(thread_index) else {
                continue;
            };
            split.matched.push(thread.id.clone());
            split.replies.push((
                thread.comment_id,
                re_raised_reply_body(&comment.body, head_sha),
            ));
            split.finding_threads.push(Some(thread.id.clone()));
            for (secondary_index, owner) in &secondary_matches {
                if *owner != comment_index {
                    continue;
                }
                let Some(secondary) = threads.get(*secondary_index) else {
                    continue;
                };
                split.matched.push(secondary.id.clone());
                split.replies.push((
                    secondary.comment_id,
                    re_raised_reply_body(&comment.body, head_sha),
                ));
            }
        } else {
            split.positions.push(comment);
            split.finding_threads.push(None);
        }
    }
    split
}

fn same_issue(recorded_title: Option<&str>, comment_body: &str) -> bool {
    let Some((_, _, title)) = parse_issue_header(comment_body) else {
        return true;
    };
    let Some(recorded) = recorded_title else {
        return true;
    };
    same_issue_title(recorded, &title)
}

fn threads_to_resolve(threads: &[ReviewThread], matched: &[String]) -> Vec<String> {
    threads
        .iter()
        .filter(|thread| !matched.contains(&thread.id))
        .map(|thread| thread.id.clone())
        .collect()
}

fn risks_section(notes: &[String]) -> String {
    if notes.is_empty() {
        return "## Risks\n\n(none flagged)".to_owned();
    }
    let list = notes
        .iter()
        .map(|note| format!("- {note}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("## Risks\n{list}")
}

impl<C: ApiClient + 'static> ReviewRunner<C> {
    async fn hunt_misses(
        &self,
        batches: &[Vec<String>],
        history: &str,
        aggregated: &mut Findings,
    ) -> Vec<String> {
        if !self.settings().miss_hunt
            || aggregated
                .findings
                .iter()
                .any(|finding| is_blocking(finding.severity))
        {
            return Vec::new();
        }
        tracing::info!(
            target: "difftrace::review",
            "first pass recorded no blocking findings; hunting for misses"
        );
        let (hunted, unreviewed) = self.review_batches(batches, history, true).await;
        // The hunt is not told what the first pass found: drop its
        // findings that repeat an aggregate entry (same file, line, and
        // title) so a re-reported issue is not double-counted.
        for finding in hunted.findings {
            let duplicate = aggregated.findings.iter().any(|existing| {
                existing.file == finding.file
                    && existing.line == finding.line
                    && crate::review::registry::same_issue_title(&existing.title, &finding.title)
            });
            if !duplicate {
                aggregated.findings.push(finding);
            }
        }
        unreviewed
    }

    async fn evidence_for(&self, files: &[String]) -> String {
        crate::review::evidence::build_evidence(
            files,
            self.index(),
            &self.gateway(),
            self.head_sha(),
        )
        .await
    }

    async fn review_batches(
        &self,
        batches: &[Vec<String>],
        history: &str,
        hunt: bool,
    ) -> (Findings, Vec<String>) {
        let mut aggregated = Findings::default();
        let mut unreviewed: Vec<String> = Vec::new();
        for (index, batch) in batches.iter().enumerate() {
            let evidence = self.evidence_for(batch).await;
            let stage = if hunt {
                "hunt batch started"
            } else {
                "batch started"
            };
            tracing::info!(
                target: "difftrace::review",
                batch = index,
                files = batch.join(", "),
                stage
            );
            let mut attempted: Option<Findings> = None;
            for attempt in 0..2usize {
                match self.review_batch(batch, history, &evidence, hunt).await {
                    Ok(findings) => {
                        attempted = Some(findings);
                        break;
                    }
                    Err(err) if attempt == 0 => {
                        tracing::warn!(
                            target: "difftrace::review",
                            error = %crate::error::error_chain(&err),
                            batch = index,
                            "batch review failed; retrying once"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "difftrace::review",
                            error = %crate::error::error_chain(&err),
                            batch = index,
                            "batch review failed again; recording the batch as unreviewed"
                        );
                    }
                }
            }
            match attempted {
                Some(findings) => {
                    tracing::info!(
                        target: "difftrace::review",
                        batch = index,
                        findings = findings.findings.len(),
                        "batch finished"
                    );
                    aggregated.findings.extend(findings.findings);
                }
                None => unreviewed.push(batch.join(", ")),
            }
        }
        (aggregated, unreviewed)
    }

    async fn round_context(
        &self,
    ) -> Result<
        (
            Vec<ReviewThread>,
            Vec<ReviewThread>,
            Registry,
            String,
            String,
        ),
        DifftraceError,
    > {
        let all_threads = match self.own_threads().await {
            Ok(threads) => threads,
            Err(err) => {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "could not list previous review threads; none will be resolved"
                );
                Vec::new()
            }
        };
        let open_threads: Vec<ReviewThread> = all_threads
            .iter()
            .filter(|thread| !thread.resolved)
            .cloned()
            .collect();
        let registry = self.load_registry(&open_threads).await?;
        let history = crate::review::rubric::rubric_history(&registry);
        let verifier_history = crate::review::rubric::cross_round_section(&registry);
        Ok((
            all_threads,
            open_threads,
            registry,
            history,
            verifier_history,
        ))
    }

    pub async fn review_all(&self, dry_run: bool) -> Result<ReviewOutcome, DifftraceError> {
        let (all_threads, open_threads, registry, history, verifier_history) =
            self.round_context().await?;
        let files = self.file_names();
        let batches = plan_batches(&files, self.settings().batch_files);
        tracing::info!(
            target: "difftrace::review",
            files = files.len(),
            batches = batches.len(),
            "reviewing"
        );
        let (mut aggregated, first_pass_unreviewed) =
            self.review_batches(&batches, &history, false).await;
        let mut unreviewed = first_pass_unreviewed;
        // A batch that failed both passes appears in both lists; keep
        // one entry per batch.
        for batch in self.hunt_misses(&batches, &history, &mut aggregated).await {
            if !unreviewed.contains(&batch) {
                unreviewed.push(batch);
            }
        }
        let verified_out = self
            .apply_verification(&mut aggregated, &verifier_history)
            .await;
        let grounded = ground_findings(
            self.index(),
            aggregated.findings,
            self.settings().max_findings_per_file,
        );
        let comment_of_finding = grounded.comment_of_finding;
        let summary = self
            .summarize(&[Findings {
                findings: grounded.findings.clone(),
            }])
            .await?;
        let mut outcome = ReviewOutcome {
            summary,
            findings: grounded.findings,
            comments: grounded.comments,
            dropped: grounded.dropped,
            verified_out,
            unreviewed_batches: unreviewed,
            pr: self.pr(),
            head_sha: self.head_sha().to_owned(),
            posted: false,
            round_body: String::new(),
            standing_body: String::new(),
            fixed_this_round: Vec::new(),
        };
        let also_by_comment: Vec<Vec<(String, u64)>> =
            crate::prompts::grouped_by_title(&outcome.findings)
                .iter()
                .map(|(_, group)| {
                    let Some((_, rest)) = group.split_first() else {
                        return Vec::new();
                    };
                    rest.iter()
                        .map(|finding| (finding.file.clone(), finding.line as u64))
                        .collect()
                })
                .collect();
        let split = split_replies(
            &open_threads,
            outcome.comments.clone(),
            &also_by_comment,
            self.head_sha(),
            &registry,
        );
        let finding_threads: Vec<Option<String>> = comment_of_finding
            .iter()
            .map(|comment_index| {
                comment_index
                    .and_then(|index| split.finding_threads.get(index))
                    .cloned()
                    .flatten()
            })
            .collect();
        let dropped: Vec<(Finding, &str)> = outcome
            .dropped
            .iter()
            .map(|entry| (entry.finding.clone(), entry.reason))
            .collect();
        let registry = registry.merge(&RoundFindings {
            head_sha: self.head_sha(),
            grounded: &outcome.findings,
            finding_threads: &finding_threads,
            dropped: &dropped,
            threads: &all_threads,
            retired: &split.retired,
        });
        outcome.fixed_this_round = registry
            .issues
            .iter()
            .filter(|issue| issue.resolved_round == Some(registry.round))
            .map(|issue| issue.title.clone())
            .collect();
        let fix_all = fix_all_section(
            &outcome.findings,
            &outcome.dropped,
            outcome.pr,
            &outcome.head_sha,
        );
        let raised = outcome.findings.len().saturating_add(outcome.dropped.len());
        let clean = raised == 0 && outcome.unreviewed_batches.is_empty();
        outcome.round_body = review_round_body(&outcome.head_sha, raised, clean, &fix_all);
        let gap_note = unreviewed_note(&outcome.unreviewed_batches);
        if !gap_note.is_empty() {
            outcome.round_body.push('\n');
            outcome.round_body.push_str(gap_note.trim_end());
        }
        outcome.standing_body = standing_render(&outcome, &registry);
        let event = if registry.has_unresolved_blockers() {
            ReviewEvent::ChangesRequested
        } else if outcome.unreviewed_batches.is_empty() {
            ReviewEvent::Approved
        } else {
            // Files went unreviewed, so the round cannot vouch for the
            // change: a neutral review never satisfies branch protection.
            ReviewEvent::Commented
        };
        if dry_run {
            return Ok(outcome);
        }
        let submission = ReviewSubmission {
            head_sha: self.head_sha().to_owned(),
            event,
            summary: outcome.round_body.clone(),
            comments: split.positions,
        };
        self.submit(submission).await?;
        self.post_re_raised_replies(split.replies).await;
        let resolved = threads_to_resolve(&open_threads, &split.matched);
        for id in &resolved {
            if let Err(err) = self.resolve_thread(id.clone()).await {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "could not resolve a previous review thread"
                );
            }
        }
        tracing::info!(
            target: "difftrace::review",
            resolved = resolved.len(),
            "resolved previous threads"
        );
        let body = verdict_comment_body(&outcome, &registry)
            .map_err(|message| DifftraceError::Reply { message })?;
        self.upsert_verdict_comment(body).await?;
        outcome.posted = true;
        Ok(outcome)
    }

    async fn apply_verification(
        &self,
        aggregated: &mut Findings,
        history: &str,
    ) -> Vec<(Finding, String)> {
        if !self.settings().verify_findings || aggregated.findings.is_empty() {
            return Vec::new();
        }
        let verdicts = match self.verify(&aggregated.findings, history).await {
            Ok(verdicts) => verdicts,
            Err(err) => {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "verification pass failed; keeping every finding"
                );
                return Vec::new();
            }
        };
        let mut removed: Vec<(usize, (Finding, String))> = verdicts
            .into_iter()
            .filter(|verdict| !verdict.keep)
            .filter_map(|verdict| {
                aggregated
                    .findings
                    .get(verdict.index)
                    .cloned()
                    .map(|finding| (verdict.index, (finding, verdict.reason)))
            })
            .collect();
        // Remove highest index first so the lower indices stay valid;
        // duplicate drop-verdicts on one index collapse to one removal.
        removed.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
        removed.dedup_by(|a, b| a.0 == b.0);
        let mut verified_out = Vec::new();
        for (index, (finding, reason)) in removed {
            aggregated.findings.remove(index);
            verified_out.push((finding, reason));
        }
        verified_out.reverse();
        if !verified_out.is_empty() {
            tracing::info!(
                target: "difftrace::review",
                dropped = verified_out.len(),
                "findings dropped after verification"
            );
        }
        verified_out
    }

    async fn load_registry(
        &self,
        open_threads: &[ReviewThread],
    ) -> Result<Registry, DifftraceError> {
        let marker = self
            .gateway()
            .find_own_marker_comment(self.pr(), VERDICT_MARKER.to_owned())
            .await
            .inspect_err(|err| {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(err),
                    "could not look up the verdict comment; failing before any write"
                );
            })?;
        match marker {
            Some(comment_id) => {
                let comment = self.gateway().fetch_issue_comment(comment_id).await?;
                if let Some(registry) = extract_registry(&comment.body) {
                    return Ok(registry);
                }
                Ok(self.bootstrap_registry(open_threads).await)
            }
            None => Ok(self.bootstrap_registry(open_threads).await),
        }
    }

    async fn bootstrap_registry(&self, open_threads: &[ReviewThread]) -> Registry {
        let comments = match self.gateway().existing_review_comments(self.pr()).await {
            Ok(comments) => comments,
            Err(err) => {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "could not read previous review comments for the registry bootstrap"
                );
                Vec::new()
            }
        };
        let mut issues = Vec::new();
        for thread in open_threads {
            let Some(body) = comments
                .iter()
                .find(|comment| comment.id == thread.comment_id)
                .map(|comment| comment.body.clone())
            else {
                continue;
            };
            let Some((severity, complexity, title)) = parse_issue_header(&body) else {
                continue;
            };
            issues.push(Issue {
                title,
                file: thread.path.clone(),
                line: thread.line.or(thread.original_line),
                severity,
                complexity,
                anchored: true,
                status: IssueStatus::Open,
                thread_id: Some(thread.id.clone()),
                raised_round: 1,
                raised_sha: String::new(),
                last_round: 1,
                resolved_round: None,
                resolved_sha: None,
            });
        }
        if issues.is_empty() {
            Registry { round: 0, issues }
        } else {
            tracing::info!(
                target: "difftrace::review",
                issues = issues.len(),
                "registry bootstrapped from existing threads"
            );
            Registry { round: 1, issues }
        }
    }

    async fn post_re_raised_replies(&self, replies: Vec<(u64, String)>) {
        let mut posted = 0usize;
        for (comment_id, body) in replies {
            if let Err(err) = self
                .gateway()
                .reply_to_review_comment(self.pr(), comment_id, body)
                .await
            {
                tracing::warn!(
                    target: "difftrace::review",
                    error = %crate::error::error_chain(&err),
                    "could not reply into a previous review thread"
                );
                continue;
            }
            posted = posted.saturating_add(1);
        }
        tracing::info!(
            target: "difftrace::review",
            replied = posted,
            "replied into re-raised threads"
        );
    }

    async fn upsert_verdict_comment(&self, body: String) -> Result<(), DifftraceError> {
        let mut attempts_left = VERDICT_WRITE_ATTEMPTS;
        loop {
            let write = match self
                .gateway()
                .find_own_marker_comment(self.pr(), VERDICT_MARKER.to_owned())
                .await
            {
                Ok(Some(comment_id)) => {
                    self.gateway()
                        .update_issue_comment(comment_id, body.clone())
                        .await
                }
                Ok(None) => {
                    self.gateway()
                        .post_pr_comment(self.pr(), body.clone())
                        .await
                }
                Err(err) => Err(err),
            };
            match write {
                Ok(()) => {
                    tracing::info!(target: "difftrace::review", "verdict comment written");
                    return Ok(());
                }
                Err(err) if attempts_left <= 1 => {
                    tracing::error!(
                        target: "difftrace::review",
                        error = %crate::error::error_chain(&err),
                        "could not write the verdict comment; giving up after every retry"
                    );
                    return Err(err);
                }
                Err(err) => {
                    attempts_left = attempts_left.saturating_sub(1);
                    tracing::warn!(
                        target: "difftrace::review",
                        error = %crate::error::error_chain(&err),
                        "could not write the verdict comment; retrying"
                    );
                    tokio::time::sleep(VERDICT_RETRY_DELAY).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::PrOverview;
    use crate::github::Side;
    use crate::tools::fake_gateway::FakeGateway;
    use loopctl::testing::MockApiClient;
    use loopctl::testing::MockResponse;
    use loopctl::testing::MockToolCall;
    use serde_json::json;
    use std::sync::Arc;

    fn overview() -> PrOverview {
        PrOverview {
            number: 42,
            title: "Fix the worker".to_owned(),
            description: Some("Restarts consumers.".to_owned()),
            author: "dana".to_owned(),
            head_sha: "headsha".to_owned(),
            head_branch: "fix/worker".to_owned(),
            base_branch: "main".to_owned(),
            changed_files: 2,
            additions: 4,
            deletions: 2,
        }
    }

    fn diff_index() -> Result<crate::diff::DiffIndex, Box<dyn std::error::Error>> {
        let diff = "\
diff --git a/src/alpha.rs b/src/alpha.rs
--- a/src/alpha.rs
+++ b/src/alpha.rs
@@ -1,3 +1,3 @@
 ctx
-old
+new
 tail
diff --git a/src/beta.rs b/src/beta.rs
--- a/src/beta.rs
+++ b/src/beta.rs
@@ -10,3 +10,3 @@
 keep
-removed
+added
 done
";
        Ok(crate::diff::DiffIndex::parse(diff)?)
    }

    fn tool_call(id: &str, name: &str, input: serde_json::Value) -> MockResponse {
        MockResponse {
            text: String::new(),
            tool_call: Some(MockToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
            }),
            stop_reason: "tool_use".to_owned(),
        }
    }

    fn text_response(text: &str) -> MockResponse {
        MockResponse {
            text: text.to_owned(),
            tool_call: None,
            stop_reason: "end_turn".to_owned(),
        }
    }

    fn finding_json(file: &str, line: usize) -> serde_json::Value {
        finding_json_severity(file, line, "warning")
    }

    fn finding_json_severity(file: &str, line: usize, severity: &str) -> serde_json::Value {
        json!({
            "file": file,
            "line": line,
            "severity": severity,
            "complexity": 3,
            "title": format!("Title in {file}"),
            "body": "Body"
        })
    }

    fn scripted_client() -> MockApiClient {
        let summary = json!({
            "summary": "Two files reviewed.",
            "risk_notes": ["Retry can outlive shutdown."],
            "tests": "Covered by integration tests."
        });
        MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({ "findings": [finding_json("src/alpha.rs", 2)] }),
            ),
            text_response("Batch one done."),
            tool_call(
                "call_2",
                "record_findings",
                json!({
                    "findings": [
                        finding_json("src/beta.rs", 11),
                        json!({
                            "file": "src/beta.rs",
                            "line": 999,
                            "severity": "warning",
                            "complexity": 3,
                            "title": "Second issue",
                            "body": "Body"
                        })
                    ]
                }),
            ),
            text_response("Batch two done."),
            text_response(&summary.to_string()),
        ])
    }

    fn make_runner<C: ApiClient + 'static>(
        client: Arc<C>,
        gateway: Arc<FakeGateway>,
    ) -> Result<ReviewRunner<C>, Box<dyn std::error::Error>> {
        make_runner_with(client, gateway, overview())
    }

    fn make_runner_with<C: ApiClient + 'static>(
        client: Arc<C>,
        gateway: Arc<FakeGateway>,
        pr_overview: PrOverview,
    ) -> Result<ReviewRunner<C>, Box<dyn std::error::Error>> {
        Ok(ReviewRunner::new(
            client,
            gateway,
            Arc::new(diff_index()?),
            pr_overview,
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: false,
                miss_hunt: false,
                ..crate::config::ReviewSettings::default()
            },
            None,
        ))
    }

    fn overview_at_sha(head_sha: &str) -> PrOverview {
        PrOverview {
            head_sha: head_sha.to_owned(),
            ..overview()
        }
    }

    #[test]
    fn batches_are_sorted_and_sized() {
        let files: Vec<String> = [
            "c.rs", "a.rs", "b.rs", "d.rs", "e.rs", "f.rs", "g.rs", "h.rs", "i.rs",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let batches = plan_batches(&files, 4);
        assert_eq!(batches.len(), 3);
        let expected: Vec<Vec<String>> = vec![
            vec!["a.rs", "b.rs", "c.rs", "d.rs"],
            vec!["e.rs", "f.rs", "g.rs", "h.rs"],
            vec!["i.rs"],
        ]
        .into_iter()
        .map(|batch| batch.into_iter().map(String::from).collect())
        .collect();
        assert_eq!(batches, expected);
    }

    #[test]
    fn a_zero_batch_size_is_treated_as_one() {
        let files = vec!["a.rs".to_owned(), "b.rs".to_owned()];
        let batches = plan_batches(&files, 0);
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|batch| batch.len() == 1));
    }

    #[test]
    fn only_unmatched_threads_are_slated_for_resolution() {
        let threads = vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                comment_id: 501,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                comment_id: 502,
                resolved: false,
                path: "src/beta.rs".to_owned(),
                line: Some(13),
                original_line: Some(13),
            },
            ReviewThread {
                id: "T_MOVED".to_owned(),
                comment_id: 503,
                resolved: false,
                path: "src/gone.rs".to_owned(),
                line: None,
                original_line: Some(40),
            },
        ];
        let slated = threads_to_resolve(&threads, &["T_KEEP".to_owned()]);
        assert_eq!(slated, vec!["T_FIXED".to_owned(), "T_MOVED".to_owned()]);
    }

    #[test]
    fn reply_matching_prefers_the_current_line_then_the_original() {
        let threads = vec![
            ReviewThread {
                id: "T_CURRENT".to_owned(),
                comment_id: 601,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(9),
            },
            ReviewThread {
                id: "T_OUTDATED".to_owned(),
                comment_id: 602,
                resolved: false,
                path: "src/beta.rs".to_owned(),
                line: None,
                original_line: Some(11),
            },
        ];
        let comment = |path: &str, line: u64| CommentPosition {
            path: path.to_owned(),
            line,
            side: Side::Right,
            body: format!("{path}:{line}"),
        };
        let split = split_replies(
            &threads,
            vec![
                comment("src/alpha.rs", 2),
                comment("src/beta.rs", 11),
                comment("src/gamma.rs", 5),
            ],
            &[Vec::new(), Vec::new(), Vec::new()],
            "headsha",
            &Registry {
                round: 0,
                issues: Vec::new(),
            },
        );
        let (positions, replies, matched) = (&split.positions, &split.replies, &split.matched);
        assert_eq!(
            matched,
            &vec!["T_CURRENT".to_owned(), "T_OUTDATED".to_owned()],
            "the current line matches first, the original line as fallback"
        );
        assert_eq!(positions.len(), 1, "only the unmatched comment stays fresh");
        assert_eq!(
            positions.first().map(|c| c.path.as_str()),
            Some("src/gamma.rs")
        );
        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies.first().map(|(id, _)| *id),
            Some(601),
            "the current-line match replies first"
        );
        assert_eq!(replies.get(1).map(|(id, _)| *id), Some(602));
        assert!(
            replies.first().is_some_and(
                |(_, body)| body.contains("Re-raised in the review of commit `headsha`.")
            ),
            "each reply names the reviewed commit"
        );
        assert_eq!(
            split.finding_threads,
            vec![
                Some("T_CURRENT".to_owned()),
                Some("T_OUTDATED".to_owned()),
                None
            ],
            "each grounded finding carries its matched thread for the registry"
        );
        assert!(
            split.retired.is_empty(),
            "a re-raise never retires the thread it replies into"
        );
    }

    #[test]
    fn a_different_finding_at_a_threads_location_retires_it() {
        let threads = vec![ReviewThread {
            id: "T_OLD".to_owned(),
            comment_id: 601,
            resolved: false,
            path: "src/alpha.rs".to_owned(),
            line: Some(2),
            original_line: Some(2),
        }];
        let registry = Registry {
            round: 1,
            issues: vec![Issue {
                title: "Probe path is not portable".to_owned(),
                file: "src/alpha.rs".to_owned(),
                line: Some(2),
                severity: Severity::Warning,
                complexity: 3,
                anchored: true,
                status: IssueStatus::Open,
                thread_id: Some("T_OLD".to_owned()),
                raised_round: 1,
                raised_sha: String::new(),
                last_round: 1,
                resolved_round: None,
                resolved_sha: None,
            }],
        };
        let comment = CommentPosition {
            path: "src/alpha.rs".to_owned(),
            line: 2,
            side: Side::Right,
            body: "![warning](https://img.shields.io/badge/warning-orange) ![effort 1](https://img.shields.io/badge/effort_1-blue) **Manifest scraping is brittle**\n\nThe sed extraction breaks.".to_owned(),
        };
        let split = split_replies(&threads, vec![comment], &[], "headsha", &registry);
        assert!(
            split.replies.is_empty(),
            "a different issue must not be replied into the old thread"
        );
        assert_eq!(
            split.retired,
            vec!["T_OLD".to_owned()],
            "the replaced issue's thread is retired for resolution"
        );
        assert_eq!(split.matched, Vec::<String>::new());
        assert_eq!(
            split.finding_threads,
            vec![None],
            "the fresh issue opens a new thread"
        );
        assert_eq!(
            split.positions.len(),
            1,
            "the fresh issue is posted as a new comment"
        );
    }

    #[test]
    fn the_same_title_still_re_raises_into_its_thread() {
        let threads = vec![ReviewThread {
            id: "T_OLD".to_owned(),
            comment_id: 601,
            resolved: false,
            path: "src/alpha.rs".to_owned(),
            line: Some(2),
            original_line: Some(2),
        }];
        let registry = Registry {
            round: 1,
            issues: vec![Issue {
                title: "Manifest scraping is brittle".to_owned(),
                file: "src/alpha.rs".to_owned(),
                line: Some(2),
                severity: Severity::Warning,
                complexity: 3,
                anchored: true,
                status: IssueStatus::Open,
                thread_id: Some("T_OLD".to_owned()),
                raised_round: 1,
                raised_sha: String::new(),
                last_round: 1,
                resolved_round: None,
                resolved_sha: None,
            }],
        };
        let comment = CommentPosition {
            path: "src/alpha.rs".to_owned(),
            line: 2,
            side: Side::Right,
            body: "![warning](https://img.shields.io/badge/warning-orange) ![effort 1](https://img.shields.io/badge/effort_1-blue) **manifest scraping is brittle**\n\nStill present.".to_owned(),
        };
        let split = split_replies(&threads, vec![comment], &[], "headsha", &registry);
        assert_eq!(
            split.matched,
            vec!["T_OLD".to_owned()],
            "title comparison ignores case, so the re-raise lands in the thread"
        );
        assert!(split.retired.is_empty());
        assert_eq!(split.replies.len(), 1);
        assert!(split.positions.is_empty());
    }

    #[test]
    fn a_mismatched_thread_at_a_secondary_location_is_retired() {
        let threads = vec![
            ReviewThread {
                id: "T_ANCHOR".to_owned(),
                comment_id: 601,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_STALE".to_owned(),
                comment_id: 602,
                resolved: false,
                path: "src/beta.rs".to_owned(),
                line: Some(5),
                original_line: Some(5),
            },
        ];
        let registry = Registry {
            round: 1,
            issues: vec![
                Issue {
                    title: "Group issue".to_owned(),
                    file: "src/alpha.rs".to_owned(),
                    line: Some(2),
                    severity: Severity::Warning,
                    complexity: 3,
                    anchored: true,
                    status: IssueStatus::Open,
                    thread_id: Some("T_ANCHOR".to_owned()),
                    raised_round: 1,
                    raised_sha: String::new(),
                    last_round: 1,
                    resolved_round: None,
                    resolved_sha: None,
                },
                Issue {
                    title: "Old different issue".to_owned(),
                    file: "src/beta.rs".to_owned(),
                    line: Some(5),
                    severity: Severity::Warning,
                    complexity: 3,
                    anchored: true,
                    status: IssueStatus::Open,
                    thread_id: Some("T_STALE".to_owned()),
                    raised_round: 1,
                    raised_sha: String::new(),
                    last_round: 1,
                    resolved_round: None,
                    resolved_sha: None,
                },
            ],
        };
        let comment = CommentPosition {
            path: "src/alpha.rs".to_owned(),
            line: 2,
            side: Side::Right,
            body: "![warning](https://img.shields.io/badge/warning-orange) ![effort 1](https://img.shields.io/badge/effort_1-blue) **Group issue**\n\nStill present.".to_owned(),
        };
        let split = split_replies(
            &threads,
            vec![comment],
            &[vec![("src/beta.rs".to_owned(), 5)]],
            "headsha",
            &registry,
        );
        assert_eq!(
            split.matched,
            vec!["T_ANCHOR".to_owned()],
            "the anchor's thread still carries the group"
        );
        assert_eq!(
            split.retired,
            vec!["T_STALE".to_owned()],
            "a secondary location's different issue is retired, not overwritten"
        );
    }

    #[test]
    fn all_mismatched_threads_at_an_anchor_are_retired() {
        let threads = vec![
            ReviewThread {
                id: "T_ONE".to_owned(),
                comment_id: 601,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_TWO".to_owned(),
                comment_id: 602,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
        ];
        let registry = Registry {
            round: 1,
            issues: vec![
                Issue {
                    title: "First old issue".to_owned(),
                    file: "src/alpha.rs".to_owned(),
                    line: Some(2),
                    severity: Severity::Warning,
                    complexity: 3,
                    anchored: true,
                    status: IssueStatus::Open,
                    thread_id: Some("T_ONE".to_owned()),
                    raised_round: 1,
                    raised_sha: String::new(),
                    last_round: 1,
                    resolved_round: None,
                    resolved_sha: None,
                },
                Issue {
                    title: "Second old issue".to_owned(),
                    file: "src/alpha.rs".to_owned(),
                    line: Some(2),
                    severity: Severity::Warning,
                    complexity: 3,
                    anchored: true,
                    status: IssueStatus::Open,
                    thread_id: Some("T_TWO".to_owned()),
                    raised_round: 1,
                    raised_sha: String::new(),
                    last_round: 1,
                    resolved_round: None,
                    resolved_sha: None,
                },
            ],
        };
        let comment = CommentPosition {
            path: "src/alpha.rs".to_owned(),
            line: 2,
            side: Side::Right,
            body: "![warning](https://img.shields.io/badge/warning-orange) ![effort 1](https://img.shields.io/badge/effort_1-blue) **Brand new issue**\n\nFresh complaint.".to_owned(),
        };
        let split = split_replies(&threads, vec![comment], &[], "headsha", &registry);
        assert!(split.replies.is_empty());
        assert_eq!(
            split.retired,
            vec!["T_ONE".to_owned(), "T_TWO".to_owned()],
            "every same-anchor thread recording a different issue is retired"
        );
    }

    #[test]
    fn a_re_raised_issue_is_not_retired_by_a_different_issue_at_its_anchor() {
        let threads = vec![ReviewThread {
            id: "T_Y".to_owned(),
            comment_id: 601,
            resolved: false,
            path: "src/lib.rs".to_owned(),
            line: Some(2),
            original_line: Some(2),
        }];
        let registry = Registry {
            round: 1,
            issues: vec![Issue {
                title: "Off-by-one".to_owned(),
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                severity: Severity::Warning,
                complexity: 3,
                anchored: true,
                status: IssueStatus::Open,
                thread_id: Some("T_Y".to_owned()),
                raised_round: 1,
                raised_sha: String::new(),
                last_round: 1,
                resolved_round: None,
                resolved_sha: None,
            }],
        };
        let comment = |title: &str, body: &str| CommentPosition {
            path: "src/lib.rs".to_owned(),
            line: 2,
            side: Side::Right,
            body: format!(
                "![warning](https://img.shields.io/badge/warning-orange) ![effort 1](https://img.shields.io/badge/effort_1-blue) **{title}**\n\n{body}"
            ),
        };
        let comments = vec![
            comment("Unused import", "The import is unused."),
            comment("Off-by-one", "The loop runs one short."),
        ];
        let split = split_replies(&threads, comments, &[], "headsha", &registry);
        assert_eq!(
            split.matched,
            vec!["T_Y".to_owned()],
            "the re-raised issue keeps its thread whatever the emission order"
        );
        assert!(
            split.retired.is_empty(),
            "a same-title re-raise must never retire its own thread"
        );
        assert_eq!(split.replies.len(), 1);
        assert_eq!(
            split.positions.len(),
            1,
            "only the different issue opens a fresh thread"
        );
    }

    #[test]
    fn a_grouped_secondary_keeps_its_matching_thread_alive() {
        let threads = vec![
            ReviewThread {
                id: "T_ANCHOR".to_owned(),
                comment_id: 601,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_BETA".to_owned(),
                comment_id: 602,
                resolved: false,
                path: "src/beta.rs".to_owned(),
                line: Some(5),
                original_line: Some(5),
            },
        ];
        let mut registry = Registry {
            round: 1,
            issues: Vec::new(),
        };
        for (title, thread) in [("Shared issue", "T_ANCHOR"), ("Shared issue", "T_BETA")] {
            registry.issues.push(Issue {
                title: title.to_owned(),
                file: if thread == "T_ANCHOR" {
                    "src/alpha.rs".to_owned()
                } else {
                    "src/beta.rs".to_owned()
                },
                line: Some(if thread == "T_ANCHOR" { 2 } else { 5 }),
                severity: Severity::Warning,
                complexity: 3,
                anchored: true,
                status: IssueStatus::Open,
                thread_id: Some(thread.to_owned()),
                raised_round: 1,
                raised_sha: String::new(),
                last_round: 1,
                resolved_round: None,
                resolved_sha: None,
            });
        }
        let comment = CommentPosition {
            path: "src/alpha.rs".to_owned(),
            line: 2,
            side: Side::Right,
            body: "![warning](https://img.shields.io/badge/warning-orange) ![effort 1](https://img.shields.io/badge/effort_1-blue) **Shared issue**\n\nStill present.".to_owned(),
        };
        let split = split_replies(
            &threads,
            vec![comment],
            &[vec![("src/beta.rs".to_owned(), 5)]],
            "headsha",
            &registry,
        );
        assert_eq!(
            split.matched,
            vec!["T_ANCHOR".to_owned(), "T_BETA".to_owned()],
            "every location whose issue reappeared keeps its thread"
        );
        assert_eq!(split.replies.len(), 2, "both threads carry the re-raise");
        assert!(split.retired.is_empty());
        assert!(split.positions.is_empty());
    }

    #[test]
    fn verdict_comment_body_wraps_the_render_with_marker_and_footer()
    -> Result<(), Box<dyn std::error::Error>> {
        let outcome = ReviewOutcome {
            summary: crate::findings::ReviewSummary {
                summary: "One file.".to_owned(),
                risk_notes: Vec::new(),
                tests: "Covered.".to_owned(),
            },
            findings: Vec::new(),
            comments: Vec::new(),
            dropped: Vec::new(),
            pr: 42,
            head_sha: "9f3b2c1".to_owned(),
            posted: false,
            round_body: String::new(),
            standing_body: String::new(),
            verified_out: Vec::new(),
            fixed_this_round: Vec::new(),
            unreviewed_batches: Vec::new(),
        };
        let registry = Registry {
            round: 2,
            issues: vec![Issue {
                title: "Lock dropped early".to_owned(),
                file: "src/worker.rs".to_owned(),
                line: Some(9),
                severity: Severity::Warning,
                complexity: 3,
                anchored: true,
                status: IssueStatus::Open,
                thread_id: Some("T_1".to_owned()),
                raised_round: 1,
                raised_sha: "9f3b2c1".to_owned(),
                last_round: 2,
                resolved_round: None,
                resolved_sha: None,
            }],
        };
        let body = verdict_comment_body(&outcome, &registry)
            .map_err(Box::<dyn std::error::Error>::from)?;
        assert!(body.starts_with("<!-- difftrace:verdict -->\n"));
        assert!(body.contains("<!-- difftrace:registry "));
        assert!(body.contains("## Verdict"));
        assert!(body.contains("🔴 Not good to go — 1 blocking finding"));
        assert!(body.contains("`src/worker.rs:9`"));
        assert!(body.ends_with("Reviewed commit: `9f3b2c1`"));
        let round_tripped = extract_registry(&body).ok_or("registry must round-trip")?;
        assert_eq!(round_tripped, registry);
        assert!(
            !body.contains("Fix all findings"),
            "the standing comment carries no fix-all — that lives on each round's review"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_re_review_resolves_unreposted_threads_and_requests_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                comment_id: 501,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                comment_id: 502,
                resolved: false,
                path: "src/beta.rs".to_owned(),
                line: Some(13),
                original_line: Some(13),
            },
        ]));
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert!(outcome.posted);
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(submission.event, ReviewEvent::ChangesRequested);
        assert_eq!(
            gateway.resolved_threads(),
            vec!["T_FIXED".to_owned()],
            "only the thread whose line was not reposted resolves"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_re_raised_finding_replies_into_its_thread() -> Result<(), Box<dyn std::error::Error>>
    {
        let gateway = Arc::new(FakeGateway::with_threads(vec![
            ReviewThread {
                id: "T_KEEP".to_owned(),
                comment_id: 501,
                resolved: false,
                path: "src/alpha.rs".to_owned(),
                line: Some(2),
                original_line: Some(2),
            },
            ReviewThread {
                id: "T_FIXED".to_owned(),
                comment_id: 502,
                resolved: false,
                path: "src/beta.rs".to_owned(),
                line: Some(13),
                original_line: Some(13),
            },
        ]));
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            gateway.posted_replies().len(),
            1,
            "the re-raised anchor replies into its thread via the replies endpoint"
        );
        let (comment_id, reply_body) = gateway
            .posted_replies()
            .first()
            .cloned()
            .ok_or("expected a reply")?;
        assert_eq!(comment_id, 501);
        assert!(reply_body.contains("Re-raised in the review of commit `headsha`."));
        assert!(
            reply_body.contains("**Title in src/alpha.rs**"),
            "the reply keeps the finding body"
        );
        assert_eq!(
            submission.comments.len(),
            1,
            "only the fresh anchor posts a positioned comment"
        );
        assert_eq!(
            submission.comments.first().map(|c| c.line),
            Some(11),
            "the beta finding at its own anchor stays positioned"
        );
        assert_eq!(gateway.resolved_threads(), vec!["T_FIXED".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn a_shifted_finding_posts_a_new_comment_and_resolves_the_old()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![ReviewThread {
            id: "T_MOVED".to_owned(),
            comment_id: 503,
            resolved: false,
            path: "src/beta.rs".to_owned(),
            line: None,
            original_line: Some(13),
        }]));
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert!(
            gateway.posted_replies().is_empty(),
            "an anchor that moved does not reply into the outdated thread"
        );
        assert_eq!(
            submission
                .comments
                .iter()
                .filter(|c| c.path == "src/beta.rs")
                .count(),
            1,
            "the shifted finding posts a fresh positioned comment"
        );
        assert_eq!(gateway.resolved_threads(), vec!["T_MOVED".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn stage_and_observer_logs_reach_an_installed_subscriber()
    -> Result<(), Box<dyn std::error::Error>> {
        let (logs, _guard) = crate::review::logging::test_support::install();
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(scripted_client()), gateway)?;
        let outcome = runner.review_all(true).await?;
        assert_eq!(outcome.comments.len(), 2);
        let text = logs.text();
        assert!(text.contains("reviewing"), "stage logs must emit");
        assert!(text.contains("batch started"));
        assert!(text.contains("batch finished"));
        assert!(
            text.contains("run started"),
            "the logging observer must be registered"
        );
        assert!(text.contains("tool call"));
        Ok(())
    }

    #[tokio::test]
    async fn a_dry_run_aggregates_grounds_and_renders_without_posting()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let client = scripted_client();
        let runner = make_runner(Arc::new(client), Arc::clone(&gateway))?;
        let outcome = runner.review_all(true).await?;
        assert!(!outcome.posted);
        assert_eq!(gateway.submit_calls(), 0);
        assert_eq!(outcome.comments.len(), 2);
        assert_eq!(outcome.dropped.len(), 1);
        assert_eq!(
            outcome
                .dropped
                .first()
                .ok_or("expected a drop")?
                .finding
                .file,
            "src/beta.rs"
        );
        let standing = &outcome.standing_body;
        assert!(standing.starts_with("## Verdict"));
        assert!(standing.contains("🔴 Not good to go — 3 blocking findings"));
        assert!(standing.contains("`src/alpha.rs:2` — ⚠️ Title in src/alpha.rs (🟡)"));
        assert!(standing.contains("`src/beta.rs:11` — ⚠️ Title in src/beta.rs (🟡)"));
        assert!(
            standing.contains("`src/beta.rs` (unanchored) — ⚠️ Second issue"),
            "the registry lists unanchored issues too, marked as such"
        );
        assert!(standing.contains("## Summary\nTwo files reviewed."));
        assert!(standing.contains("- Retry can outlive shutdown."));
        assert!(standing.contains("## Tests\nCovered by integration tests."));
        assert!(!standing.contains("Fix all findings"));
        let round = &outcome.round_body;
        assert!(round.starts_with("🤖 difftrace reviewed `headsha` — 3 findings this round"));
        assert!(round.contains("## 🤖 Fix all findings"));
        assert!(round.contains("`src/alpha.rs:2` — ⚠️ Title in src/alpha.rs (🟡)"));
        assert!(round.contains("`src/beta.rs:11` — ⚠️ Title in src/beta.rs (🟡)"));
        assert!(round.contains(
            "- `src/beta.rs:999` — ⚠️ Second issue (🟡, line outside the changed hunks)"
        ));
        assert!(round.contains("PR #42"));
        assert!(round.contains("commit headsha"));
        assert!(round.contains("<summary>Copy the fix-all prompt for coding agents</summary>"));
        Ok(())
    }

    #[tokio::test]
    async fn a_review_without_findings_renders_no_fix_all_section()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = json!({
            "summary": "Nothing to flag.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call("call_1", "record_findings", json!({ "findings": [] })),
            text_response("Batch one done."),
            tool_call("call_2", "record_findings", json!({ "findings": [] })),
            text_response("Batch two done."),
            text_response(&summary.to_string()),
        ]);
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(client), gateway)?;
        let outcome = runner.review_all(true).await?;
        assert!(outcome.comments.is_empty());
        assert!(outcome.dropped.is_empty());
        assert!(outcome.standing_body.starts_with("## Verdict"));
        assert!(
            outcome
                .standing_body
                .contains("🎉 Good to go — no unresolved findings.")
        );
        assert!(outcome.standing_body.contains("## Risks\n\n(none flagged)"));
        assert!(
            outcome
                .standing_body
                .contains("## Summary\nNothing to flag.")
        );
        assert!(
            outcome
                .round_body
                .starts_with("🤖 difftrace reviewed `headsha` — clean round"),
            "a clean round's body carries no fix-all"
        );
        assert!(
            !outcome.round_body.contains("Fix all findings"),
            "a clean review must not render a fix-all section"
        );
        Ok(())
    }

    #[tokio::test]
    async fn nitpicks_and_suggestions_do_not_block_the_verdict()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = json!({
            "summary": "Polish only.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({ "findings": [finding_json_severity("src/alpha.rs", 2, "suggestion")] }),
            ),
            text_response("Batch one done."),
            tool_call(
                "call_2",
                "record_findings",
                json!({ "findings": [finding_json_severity("src/beta.rs", 11, "nitpick")] }),
            ),
            text_response("Batch two done."),
            text_response(&summary.to_string()),
        ]);
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(client), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert_eq!(outcome.comments.len(), 2);
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(submission.event, ReviewEvent::Approved);
        assert!(outcome.standing_body.contains(
            "🎉 Good to go — no unresolved blocking findings. Nitpicks and suggestions don't block:"
        ));
        assert!(
            outcome
                .standing_body
                .contains("`src/alpha.rs:2` — 💡 Title")
        );
        assert!(
            outcome
                .standing_body
                .contains("`src/beta.rs:11` — 💬 Title")
        );
        assert!(gateway.resolved_threads().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn the_summary_describes_only_the_grounded_findings()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = json!({
            "summary": "One finding: the beta anchor.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({
                    "findings": [
                        finding_json("src/alpha.rs", 2),
                        json!({
                            "file": "src/alpha.rs",
                            "line": 999,
                            "severity": "warning",
                            "complexity": 3,
                            "title": "Second issue",
                            "body": "Body"
                        })
                    ]
                }),
            ),
            text_response("Batch done."),
            text_response(&summary.to_string()),
        ]);
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(client), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(submission.comments.len(), 1);
        assert_eq!(outcome.dropped.len(), 1);
        let verdict = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the verdict comment must be posted")?;
        assert!(
            verdict.contains(
                "🔴 Not good to go — 2 blocking findings (all unresolved, blockers first):"
            )
        );
        assert!(verdict.contains("## Risks\n\n(none flagged)"));
        assert!(verdict.contains("One finding"));
        assert!(verdict.contains("alpha.rs:999"));
        assert!(
            verdict.contains("## Summary\nOne finding: the beta anchor."),
            "the model-written summary describes only the grounded findings"
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_posted_review_body_is_the_stat_line_with_the_rounds_fix_all()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert!(
            submission
                .summary
                .starts_with("🤖 difftrace reviewed `headsha`"),
            "the round body is the stat line"
        );
        assert!(
            submission.summary.contains("## 🤖 Fix all findings"),
            "the round body carries this round's fix-all"
        );
        assert!(
            !submission.summary.contains("## Verdict")
                && !submission.summary.contains("## Summary"),
            "the review body carries neither verdict nor summary — the standing comment is the only verdict surface"
        );
        let verdict = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the verdict comment must be posted")?;
        assert!(verdict.contains("## Verdict"));
        assert!(verdict.starts_with(VERDICT_MARKER));
        assert!(verdict.contains("<!-- difftrace:registry "));
        Ok(())
    }

    #[tokio::test]
    async fn an_unanchored_blocking_finding_still_requests_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let summary = json!({
            "summary": "One dropped finding.",
            "risk_notes": [],
            "tests": "Covered."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "call_1",
                "record_findings",
                json!({
                    "findings": [
                        json!({
                            "file": "src/lib.rs",
                            "line": 999,
                            "severity": "critical",
                            "complexity": 3,
                            "title": "Out of hunk",
                            "body": "Body"
                        })
                    ]
                }),
            ),
            text_response("Done."),
            text_response(&summary.to_string()),
        ]);
        let gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(client), Arc::clone(&gateway))?;
        runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            submission.event,
            ReviewEvent::ChangesRequested,
            "the event and the standing verdict share one blocking source: the merged registry"
        );
        let verdict = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the verdict comment must be posted")?;
        assert!(verdict.contains("🔴 Not good to go — 1 blocking finding"));
        Ok(())
    }

    #[tokio::test]
    async fn the_posted_review_matches_the_dry_run_content()
    -> Result<(), Box<dyn std::error::Error>> {
        let post_gateway = Arc::new(FakeGateway::empty());
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&post_gateway))?;
        let dry = {
            let dry_gateway = Arc::new(FakeGateway::empty());
            let dry_runner = make_runner(Arc::new(scripted_client()), dry_gateway)?;
            dry_runner.review_all(true).await?
        };
        let posted = runner.review_all(false).await?;
        assert!(posted.posted);
        let submission = post_gateway
            .submitted()
            .ok_or("expected a submission to reach the gateway")?;
        assert_eq!(submission.head_sha, "headsha");
        assert_eq!(submission.comments, dry.comments);
        assert!(
            submission
                .summary
                .starts_with("🤖 difftrace reviewed `headsha`"),
            "the round body is the stat line"
        );
        assert!(
            submission.summary.contains("## 🤖 Fix all findings"),
            "the round body carries this round's fix-all"
        );
        let verdict = post_gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the verdict comment must be posted")?;
        assert!(
            verdict.starts_with("<!-- difftrace:verdict -->"),
            "the posted comment carries the marker"
        );
        assert!(
            extract_registry(&verdict).is_some(),
            "the posted comment embeds the merged registry"
        );
        assert!(
            verdict.contains(dry.standing_body.trim_end()),
            "the posted comment's visible body matches the dry-run standing render"
        );
        assert_eq!(posted.dropped, dry.dropped);
        Ok(())
    }

    #[tokio::test]
    async fn the_verdict_comment_is_created_then_edited_not_duplicated()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let first = make_runner_with(
            Arc::new(scripted_client()),
            Arc::clone(&gateway),
            overview_at_sha("firstsha"),
        )?;
        first.review_all(false).await?;
        assert_eq!(
            gateway.posted_comments().len(),
            1,
            "the first run creates exactly one verdict comment"
        );
        let second = make_runner_with(
            Arc::new(scripted_client()),
            Arc::clone(&gateway),
            overview_at_sha("secondsha"),
        )?;
        second.review_all(false).await?;
        assert_eq!(
            gateway.posted_comments().len(),
            1,
            "the second run must not create another comment"
        );
        let updated = gateway.updated_comments();
        assert_eq!(
            updated.len(),
            1,
            "the second run edits the comment in place"
        );
        let (_, body) = updated.first().cloned().ok_or("expected an update")?;
        assert!(body.starts_with(VERDICT_MARKER));
        assert!(
            body.contains("Reviewed commit: `secondsha`"),
            "the edit carries the new round's commit"
        );
        assert_eq!(gateway.issue_comment_bodies().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_verdict_upsert_exhausting_the_retries_fails_the_run_after_posting()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![ReviewThread {
            id: "T_FIXED".to_owned(),
            comment_id: 502,
            resolved: false,
            path: "src/beta.rs".to_owned(),
            line: Some(13),
            original_line: Some(13),
        }]));
        gateway.fail_comment_writes(9);
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let err = runner
            .review_all(false)
            .await
            .err()
            .ok_or("the exhausted verdict retries must fail the run")?;
        assert!(
            !err.to_string().is_empty(),
            "the failure names the underlying error"
        );
        assert!(
            gateway.submitted().is_some(),
            "the review itself stands posted; only the verdict comment failed"
        );
        assert_eq!(
            gateway.resolved_threads(),
            vec!["T_FIXED".to_owned()],
            "resolution completes even when the verdict write fails"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_reply_degrades_without_failing_the_run_or_resolving()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![ReviewThread {
            id: "T_KEEP".to_owned(),
            comment_id: 501,
            resolved: false,
            path: "src/alpha.rs".to_owned(),
            line: Some(2),
            original_line: Some(2),
        }]));
        gateway.fail_reply_writes(1);
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert!(outcome.posted, "a failed reply never fails the run");
        assert!(
            gateway.posted_replies().is_empty(),
            "the reply did not land"
        );
        assert!(
            gateway.resolved_threads().is_empty(),
            "the still-present finding's thread must stay open for the next re-review"
        );
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            submission
                .comments
                .iter()
                .filter(|c| c.path == "src/alpha.rs")
                .count(),
            0,
            "the re-raised anchor was carved out for the reply, not reposted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_verdict_upsert_recovering_on_a_later_retry_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        gateway.fail_comment_writes(2);
        let runner = make_runner(Arc::new(scripted_client()), Arc::clone(&gateway))?;
        let outcome = runner.review_all(false).await?;
        assert!(outcome.posted);
        assert_eq!(gateway.posted_comments().len(), 1);
        Ok(())
    }
}

#[cfg(test)]
mod registry_flow_tests {
    use super::*;
    use crate::github::PrOverview;
    use crate::github::Side;
    use crate::tools::fake_gateway::FakeGateway;
    use futures::Stream;
    use loopctl::api::StreamRequest;
    use loopctl::api::error::ApiError;
    use loopctl::stream::StreamEvent;
    use loopctl::testing::MockApiClient;
    use loopctl::testing::MockResponse;
    use loopctl::testing::MockToolCall;
    use serde_json::json;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    fn overview() -> PrOverview {
        PrOverview {
            number: 42,
            title: "Fix the worker".to_owned(),
            description: Some("Restarts consumers.".to_owned()),
            author: "dana".to_owned(),
            head_sha: "headsha".to_owned(),
            head_branch: "fix/worker".to_owned(),
            base_branch: "main".to_owned(),
            changed_files: 1,
            additions: 3,
            deletions: 1,
        }
    }

    fn diff_index() -> Result<crate::diff::DiffIndex, Box<dyn std::error::Error>> {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 ctx
-old
+new
 tail
";
        crate::diff::DiffIndex::parse(diff).map_err(Into::into)
    }

    fn two_file_index() -> Result<crate::diff::DiffIndex, Box<dyn std::error::Error>> {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 ctx
-old
+new
 tail
diff --git a/src/force_quit.rs b/src/force_quit.rs
--- a/src/force_quit.rs
+++ b/src/force_quit.rs
@@ -1,3 +1,3 @@
 ctx
-old
+replaced
 tail
";
        crate::diff::DiffIndex::parse(diff).map_err(Into::into)
    }

    fn tool_call(id: &str, input: serde_json::Value) -> MockResponse {
        MockResponse {
            text: String::new(),
            tool_call: Some(MockToolCall {
                id: id.to_owned(),
                name: "record_findings".to_owned(),
                input,
            }),
            stop_reason: "tool_use".to_owned(),
        }
    }

    fn text_response(text: &str) -> MockResponse {
        MockResponse {
            text: text.to_owned(),
            tool_call: None,
            stop_reason: "end_turn".to_owned(),
        }
    }

    fn summary_json() -> String {
        json!({
            "summary": "One file reviewed.",
            "risk_notes": [],
            "tests": "Covered."
        })
        .to_string()
    }

    fn finding_json(line: usize) -> serde_json::Value {
        json!({
            "file": "src/lib.rs",
            "line": line,
            "severity": "warning",
            "complexity": 3,
            "title": "Lock dropped early",
            "body": "The guard is dropped before the read completes."
        })
    }

    fn runner<C: ApiClient + 'static>(
        client: Arc<C>,
        gateway: Arc<FakeGateway>,
    ) -> Result<ReviewRunner<C>, Box<dyn std::error::Error>> {
        Ok(ReviewRunner::new(
            client,
            gateway,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: false,
                miss_hunt: false,
                ..crate::config::ReviewSettings::default()
            },
            None,
        ))
    }

    fn thread_at(line: Option<u64>, original: u64) -> ReviewThread {
        ReviewThread {
            id: format!("T_{line:?}_{original}"),
            comment_id: original.saturating_add(900),
            resolved: false,
            path: "src/lib.rs".to_owned(),
            line,
            original_line: Some(original),
        }
    }

    #[tokio::test]
    async fn two_rounds_leave_a_registry_with_open_and_fixed_issues()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let round_one = runner(
            Arc::new(MockApiClient::new("review-model").with_responses(vec![
                tool_call("c1", json!({ "findings": [finding_json(2)] })),
                text_response("Done."),
                text_response(&summary_json()),
            ])),
            Arc::clone(&gateway),
        )?;
        round_one.review_all(false).await?;
        let first_comment = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("round one posts the standing comment")?;
        assert!(
            first_comment.contains("<!-- difftrace:registry "),
            "the posted comment embeds the registry JSON"
        );
        assert!(first_comment.contains("1 blocking finding"));

        let round_two = runner(
            Arc::new(MockApiClient::new("review-model").with_responses(vec![
                tool_call("c1", json!({ "findings": [] })),
                text_response("Done."),
                text_response(&summary_json()),
            ])),
            Arc::clone(&gateway),
        )?;
        let outcome = round_two.review_all(false).await?;
        assert!(
            outcome
                .round_body
                .starts_with("🤖 difftrace reviewed `headsha` — clean round"),
            "the fixing round's body is the clean stat line"
        );
        let updated = gateway
            .updated_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("round two edits the standing comment")?;
        assert!(updated.contains("🎉 Good to go — no unresolved findings."));
        assert!(
            updated.contains("✅ Issue history"),
            "the fixed issue lands in the history section"
        );
        assert!(
            updated.contains(
                "- ✅ ~~Lock dropped early~~ `src/lib.rs:2` — fixed in round 2 (`headsha`)"
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_registry_bootstraps_from_existing_threads_without_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let header = "![warning](https://img.shields.io/badge/warning-orange) ![effort 3](https://img.shields.io/badge/effort_3-yellow) **Lock dropped early**";
        let gateway = Arc::new(
            FakeGateway::with_threads(vec![thread_at(Some(2), 2)]).and_comments(vec![
                crate::github::ExistingComment {
                    id: 902,
                    path: "src/lib.rs".to_owned(),
                    line: Some(2),
                    side: Some(Side::Right),
                    body: format!("{header}\n\nThe guard is dropped."),
                    author: "difftrace[bot]".to_owned(),
                    in_reply_to: None,
                },
            ]),
        );
        let round = runner(
            Arc::new(MockApiClient::new("review-model").with_responses(vec![
                tool_call("c1", json!({ "findings": [finding_json(2)] })),
                text_response("Done."),
                text_response(&summary_json()),
            ])),
            Arc::clone(&gateway),
        )?;
        round.review_all(false).await?;
        let comment = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the standing comment must be posted")?;
        let registry = extract_registry(&comment).ok_or("registry must embed")?;
        assert_eq!(
            registry.issues.len(),
            1,
            "the existing thread bootstraps one issue"
        );
        assert_eq!(
            registry.issues.first().ok_or("issue")?.title,
            "Lock dropped early"
        );
        assert_eq!(
            registry.round, 2,
            "bootstrap counts as round one, this run is two"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_grouped_finding_replies_once_and_registers_every_location()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::with_threads(vec![thread_at(Some(2), 2)]));
        let round = runner(
            Arc::new(MockApiClient::new("review-model").with_responses(vec![
                tool_call(
                    "c1",
                    json!({ "findings": [finding_json(2), finding_json(3)] }),
                ),
                text_response("Done."),
                text_response(&summary_json()),
            ])),
            Arc::clone(&gateway),
        )?;
        let outcome = round.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert!(
            submission.comments.is_empty(),
            "the grouped comment replies into the existing thread instead of posting fresh"
        );
        assert_eq!(
            gateway.posted_replies().len(),
            1,
            "one reply carries the whole group"
        );
        assert!(
            gateway
                .posted_replies()
                .first()
                .is_some_and(|(_, body)| body.contains("Also occurs at: `src/lib.rs:3`")),
            "the reply lists the secondary location"
        );
        let comment = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the standing comment must be posted")?;
        let registry = extract_registry(&comment).ok_or("registry must embed")?;
        assert_eq!(
            registry.issues.len(),
            2,
            "each location of the group is a registry issue"
        );
        let thread_id = thread_at(Some(2), 2).id;
        assert_eq!(
            registry
                .issues
                .first()
                .ok_or("primary")?
                .thread_id
                .as_deref(),
            Some(thread_id.as_str()),
            "the primary location keeps the thread"
        );
        assert_eq!(
            registry.issues.get(1).ok_or("secondary")?.thread_id,
            None,
            "the secondary location posted no thread of its own"
        );
        assert!(outcome.standing_body.contains("`src/lib.rs:3`"));
        Ok(())
    }

    #[tokio::test]
    async fn findings_failing_verification_are_dropped_with_a_reason()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "c1",
                json!({ "findings": [
                    {
                        "file": "src/lib.rs",
                        "line": 2,
                        "severity": "warning",
                        "complexity": 3,
                        "title": "Real issue",
                        "body": "The guard is dropped."
                    },
                    {
                        "file": "src/lib.rs",
                        "line": 3,
                        "severity": "suggestion",
                        "complexity": 2,
                        "title": "Conceded issue",
                        "body": "This is deliberate; no change required."
                    }
                ] }),
            ),
            text_response("Done."),
            text_response(
                json!({ "verdicts": [
                    { "index": 0, "keep": true, "reason": "solid" },
                    {
                        "index": 1,
                        "keep": false,
                        "reason": "the finding's own text concedes no change is needed"
                    }
                ] })
                .to_string()
                .as_str(),
            ),
            text_response(&summary_json()),
        ]);
        let gateway_handle = std::sync::Arc::clone(&gateway);
        let gateway_for_runner: Arc<dyn crate::github::PrGateway> = gateway_handle;
        let runner = ReviewRunner::new(
            Arc::new(client),
            gateway_for_runner,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: true,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let outcome = runner.review_all(false).await?;
        assert_eq!(
            outcome.comments.len(),
            1,
            "the conceded finding posts no comment"
        );
        assert!(
            outcome
                .findings
                .iter()
                .all(|finding| finding.title != "Conceded issue"),
            "the conceded finding is not actionable"
        );
        assert_eq!(outcome.verified_out.len(), 1);
        let (_, reason) = outcome.verified_out.first().cloned().ok_or("a verdict")?;
        assert!(reason.contains("concedes no change"));
        assert!(
            outcome
                .standing_body
                .contains("## Dropped after verification")
        );
        assert!(outcome.standing_body.contains("Conceded issue"));
        assert!(
            !outcome.round_body.contains("Conceded issue"),
            "the fix-all prompt carries only kept findings"
        );
        let comment = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the standing comment must be posted")?;
        let registry = extract_registry(&comment).ok_or("registry must embed")?;
        assert_eq!(
            registry.issues.len(),
            1,
            "a verification failure never becomes a registry issue"
        );
        Ok(())
    }

    #[tokio::test]
    async fn multiple_verification_drops_remove_every_dropped_index_without_panicking()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let finding = |line: usize, title: &str| {
            json!({
                "file": "src/lib.rs",
                "line": line,
                "severity": "warning",
                "complexity": 2,
                "title": title,
                "body": "The detail."
            })
        };
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "c1",
                json!({ "findings": [
                    finding(1, "Issue one"),
                    finding(2, "Issue two"),
                    finding(3, "Issue three")
                ] }),
            ),
            text_response("Done."),
            text_response(
                json!({ "verdicts": [
                    { "index": 0, "keep": false, "reason": "concedes no change" },
                    { "index": 1, "keep": false, "reason": "premise unverifiable" },
                    { "index": 1, "keep": false, "reason": "premise unverifiable" }
                ] })
                .to_string()
                .as_str(),
            ),
            text_response(&summary_json()),
        ]);
        let runner = ReviewRunner::new(
            Arc::new(client),
            gateway,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: true,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let outcome = runner.review_all(false).await?;
        assert_eq!(
            outcome.comments.len(),
            1,
            "only the surviving finding posts a comment"
        );
        assert_eq!(
            outcome.findings.first().map(|f| f.title.as_str()),
            Some("Issue three"),
            "the only kept finding survives"
        );
        assert_eq!(
            outcome.verified_out.len(),
            2,
            "a duplicate drop-verdict collapses to one removal"
        );
        assert_eq!(
            outcome
                .verified_out
                .iter()
                .map(|(finding, _)| finding.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Issue one", "Issue two"],
            "drops are listed in finding order"
        );
        assert!(
            outcome
                .standing_body
                .contains("## Dropped after verification")
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_persistently_failing_verification_keeps_every_finding()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call(
                "c1",
                json!({ "findings": [
                    {
                        "file": "src/lib.rs",
                        "line": 2,
                        "severity": "warning",
                        "complexity": 3,
                        "title": "First kept issue",
                        "body": "The guard is dropped."
                    },
                    {
                        "file": "src/lib.rs",
                        "line": 3,
                        "severity": "warning",
                        "complexity": 2,
                        "title": "Second kept issue",
                        "body": "The anchor moved."
                    }
                ] }),
            ),
            text_response("Done."),
            text_response("that is not JSON"),
            text_response("still not JSON"),
            text_response(&summary_json()),
        ]);
        let runner = ReviewRunner::new(
            Arc::new(client),
            gateway,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: true,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let outcome = runner.review_all(false).await?;
        assert!(
            outcome.verified_out.is_empty(),
            "an unavailable verifier never vetoes findings"
        );
        assert_eq!(outcome.comments.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_clean_first_pass_triggers_the_miss_hunt() -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call("c1", json!({ "findings": [] })),
            text_response("First pass is clean."),
            tool_call("h1", json!({ "findings": [finding_json(2)] })),
            text_response("The hunt found what the first pass missed."),
            text_response(&summary_json()),
        ]);
        let gateway_handle = std::sync::Arc::clone(&gateway);
        let gateway_for_runner: Arc<dyn crate::github::PrGateway> = gateway_handle;
        let runner = ReviewRunner::new(
            Arc::new(client),
            gateway_for_runner,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: false,
                miss_hunt: true,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let outcome = runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(submission.comments.len(), 1);
        assert_eq!(
            submission.event,
            ReviewEvent::ChangesRequested,
            "a hunt finding blocks the verdict"
        );
        assert!(
            outcome
                .round_body
                .contains("1 finding this round; fix prompts below."),
            "the hunt's finding rides the normal round body"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_blocking_first_pass_skips_the_miss_hunt() -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call("c1", json!({ "findings": [finding_json(2)] })),
            text_response("Found one."),
            tool_call(
                "h1",
                json!({ "findings": [{
                    "file": "src/lib.rs",
                    "line": 3,
                    "severity": "nitpick",
                    "complexity": 1,
                    "title": "Hunt nitpick",
                    "body": "Would only appear if the hunt ran."
                }] }),
            ),
            text_response(&summary_json()),
        ]);
        let gateway_handle = std::sync::Arc::clone(&gateway);
        let gateway_for_runner: Arc<dyn crate::github::PrGateway> = gateway_handle;
        let runner = ReviewRunner::new(
            Arc::new(client),
            gateway_for_runner,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: false,
                miss_hunt: true,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let outcome = runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            submission.comments.len(),
            1,
            "the hunt never ran: a blocking first pass skips it"
        );
        assert!(
            outcome
                .findings
                .iter()
                .all(|finding| finding.severity != crate::findings::Severity::Nitpick),
            "the would-be hunt finding never entered the aggregate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_hunt_finding_repeating_the_first_pass_is_dropped()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let nitpick = json!({
            "file": "src/lib.rs",
            "line": 2,
            "severity": "nitpick",
            "complexity": 1,
            "title": "Rename x to y",
            "body": "Style only."
        });
        let client = MockApiClient::new("review-model").with_responses(vec![
            tool_call("c1", json!({ "findings": [nitpick.clone()] })),
            text_response("First pass: one nitpick."),
            tool_call("h1", json!({ "findings": [nitpick] })),
            text_response("Hunt re-reported it."),
            text_response(&summary_json()),
        ]);
        let gateway_handle = std::sync::Arc::clone(&gateway);
        let gateway_for_runner: Arc<dyn crate::github::PrGateway> = gateway_handle;
        let runner = ReviewRunner::new(
            Arc::new(client),
            gateway_for_runner,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: false,
                miss_hunt: true,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let outcome = runner.review_all(false).await?;
        assert_eq!(
            outcome.findings.len(),
            1,
            "the repeated hunt finding never enters the aggregate"
        );
        assert!(
            !outcome.round_body.contains("Also occurs at"),
            "the surviving finding is not its own secondary location"
        );
        assert!(
            !outcome.round_body.contains("2 findings"),
            "the round counts one issue"
        );
        Ok(())
    }

    struct StreamFailingClient;

    impl loopctl::api::ApiClient for StreamFailingClient {
        fn model(&self) -> String {
            "review-model".to_owned()
        }

        fn stream_messages(
            &self,
            _request: &StreamRequest,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ApiError>> + Send + 'static>> {
            Box::pin(futures::stream::once(async {
                Err(loopctl::api::error::ApiError::api(
                    "stream reset mid-flight",
                ))
            }))
        }

        fn create_message(
            &self,
            _request: &StreamRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<loopctl::api::NonStreamingResponse, ApiError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async {
                Err(loopctl::api::error::ApiError::api(
                    "stream reset mid-flight",
                ))
            })
        }
    }

    // Fails every streaming call whose request mentions the marker
    // (the persistent-failure batch's file) and delegates everything
    // else, so one batch fails the way a context-window overflow does
    // while the rest of the round proceeds.
    struct SelectiveFailureClient {
        inner: MockApiClient,
        marker: &'static str,
    }

    impl loopctl::api::ApiClient for SelectiveFailureClient {
        fn model(&self) -> String {
            self.inner.model()
        }

        fn stream_messages(
            &self,
            request: &StreamRequest,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ApiError>> + Send + 'static>> {
            if format!("{request:?}").contains(self.marker) {
                return Box::pin(futures::stream::once(async {
                    Err(ApiError::api("stream reset mid-flight"))
                }));
            }
            self.inner.stream_messages(request)
        }

        fn stream_messages_with_options(
            &self,
            request: &StreamRequest,
            _options: loopctl::structured::RequestOptions,
        ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ApiError>> + Send + 'static>> {
            self.stream_messages(request)
        }

        fn create_message(
            &self,
            request: &StreamRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<loopctl::api::NonStreamingResponse, ApiError>>
                    + Send
                    + '_,
            >,
        > {
            if format!("{request:?}").contains(self.marker) {
                return Box::pin(async { Err(ApiError::api("stream reset mid-flight")) });
            }
            self.inner.create_message(request)
        }

        fn create_message_with_options(
            &self,
            request: &StreamRequest,
            options: loopctl::structured::RequestOptions,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<loopctl::api::NonStreamingResponse, ApiError>>
                    + Send
                    + '_,
            >,
        > {
            if format!("{request:?}").contains(self.marker) {
                return Box::pin(async { Err(ApiError::api("stream reset mid-flight")) });
            }
            self.inner.create_message_with_options(request, options)
        }
    }

    #[tokio::test]
    async fn a_round_with_unreviewed_files_does_not_post_approval()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let gateway_handle = std::sync::Arc::clone(&gateway);
        let gateway_for_runner: Arc<dyn crate::github::PrGateway> = gateway_handle;
        // Two batches of one file: the force_quit batch fails every run
        // (first pass and hunt); the lib.rs batch reviews clean twice,
        // and the summary call sees no findings and no marker.
        let inner = MockApiClient::new("review-model").with_responses(vec![
            tool_call("c1", json!({ "findings": [] })),
            text_response("No findings in this batch."),
            tool_call("h1", json!({ "findings": [] })),
            text_response("The hunt found nothing."),
            text_response(&summary_json()),
        ]);
        let client = Arc::new(SelectiveFailureClient {
            inner,
            marker: "force_quit.rs",
        });
        let runner = ReviewRunner::new(
            client,
            gateway_for_runner,
            Arc::new(two_file_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: false,
                miss_hunt: true,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let outcome = runner.review_all(false).await?;
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            submission.event,
            ReviewEvent::Commented,
            "unreviewed files downgrade the verdict to a neutral comment"
        );
        assert_eq!(
            outcome.unreviewed_batches,
            vec!["src/force_quit.rs".to_owned()]
        );
        assert!(outcome.standing_body.contains("Approval withheld"));
        assert!(
            !outcome.standing_body.contains("Good to go"),
            "an incomplete round never claims good-to-go"
        );
        assert!(outcome.round_body.contains("the round is incomplete"));
        assert_eq!(
            outcome.round_body.matches("src/force_quit.rs").count(),
            1,
            "a batch failing both passes is listed once, not once per pass"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_batch_whose_runs_fail_is_recorded_unreviewed_and_the_review_continues()
    -> Result<(), Box<dyn std::error::Error>> {
        let gateway = Arc::new(FakeGateway::empty());
        let gateway_handle = std::sync::Arc::clone(&gateway);
        let gateway_for_runner: Arc<dyn crate::github::PrGateway> = gateway_handle;
        let runner = ReviewRunner::new(
            Arc::new(StreamFailingClient),
            gateway_for_runner,
            Arc::new(diff_index()?),
            overview(),
            crate::config::ReviewSettings {
                batch_files: 1,
                verify_findings: false,
                miss_hunt: false,
                ..crate::config::ReviewSettings::default()
            },
            None,
        );
        let (findings, unreviewed) = runner
            .review_batches(&[vec!["src/lib.rs".to_owned()]], "", false)
            .await;
        assert!(findings.findings.is_empty());
        assert_eq!(unreviewed, vec!["src/lib.rs".to_owned()]);
        let section = unreviewed_note(&unreviewed);
        assert!(section.contains("⚠️ Unreviewed files"));
        assert!(section.contains("`src/lib.rs`"));
        assert!(section.contains("the reviewer run failed twice"));
        Ok(())
    }

    #[tokio::test]
    async fn a_different_finding_at_the_same_line_resolves_the_old_thread_and_opens_a_new_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let header = "![warning](https://img.shields.io/badge/warning-orange) ![effort 3](https://img.shields.io/badge/effort_3-yellow) **Lock dropped early**";
        let old_thread = ReviewThread {
            id: "T_OLD".to_owned(),
            comment_id: 902,
            resolved: false,
            path: "src/lib.rs".to_owned(),
            line: Some(2),
            original_line: Some(2),
        };
        let gateway = Arc::new(
            FakeGateway::with_threads(vec![old_thread]).and_comments(vec![
                crate::github::ExistingComment {
                    id: 902,
                    path: "src/lib.rs".to_owned(),
                    line: Some(2),
                    side: Some(Side::Right),
                    body: format!("{header}\n\nThe guard is dropped."),
                    author: "difftrace[bot]".to_owned(),
                    in_reply_to: None,
                },
            ]),
        );
        let round = runner(
            Arc::new(MockApiClient::new("review-model").with_responses(vec![
                tool_call(
                    "c1",
                    json!({
                        "findings": [{
                            "file": "src/lib.rs",
                            "line": 2,
                            "severity": "warning",
                            "complexity": 2,
                            "title": "Anchor drifted",
                            "body": "The anchor moved."
                        }]
                    }),
                ),
                text_response("Done."),
                text_response(&summary_json()),
            ])),
            Arc::clone(&gateway),
        )?;
        let outcome = round.review_all(false).await?;
        assert_eq!(
            gateway.resolved_threads(),
            vec!["T_OLD".to_owned()],
            "the replaced issue's thread is resolved"
        );
        assert!(
            gateway.posted_replies().is_empty(),
            "nothing is replied into the replaced issue's thread"
        );
        let submission = gateway.submitted().ok_or("expected a submission")?;
        assert_eq!(
            submission.comments.len(),
            1,
            "the fresh issue is posted as a new thread"
        );
        let comment = gateway
            .posted_comments()
            .first()
            .map(|(_, body)| body.clone())
            .ok_or("the standing comment must be posted")?;
        let registry = extract_registry(&comment).ok_or("registry must embed")?;
        assert_eq!(registry.issues.len(), 2);
        let replaced = registry.issues.first().ok_or("old issue")?;
        assert_eq!(replaced.title, "Lock dropped early");
        assert_eq!(replaced.status, IssueStatus::Fixed);
        assert_eq!(replaced.resolved_round, Some(2));
        assert_eq!(replaced.resolved_sha.as_deref(), Some("headsha"));
        let raised = registry.issues.get(1).ok_or("fresh issue")?;
        assert_eq!(raised.title, "Anchor drifted");
        assert_eq!(raised.status, IssueStatus::Open);
        assert_eq!(raised.raised_round, 2);
        assert_eq!(raised.thread_id, None);
        assert!(
            outcome.standing_body.contains("✅ ~~Lock dropped early~~"),
            "the replaced issue appears in the history section"
        );
        Ok(())
    }
}
