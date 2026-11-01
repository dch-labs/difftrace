//! CLI entry: baseline metric computation and v1 reviewer runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use difftrace_eval::correlate::{GoldenFinding, Label};
use difftrace_eval::error::DtError;
use difftrace_eval::extract::{Finding, parse_transcript};
use difftrace_eval::metrics;
use difftrace_eval::mutations;

struct Args {
    golden: bool,
    parity: bool,
    v1_metrics: Option<PathBuf>,
    current_metrics: Option<PathBuf>,
    runner: Runner,
    index: PathBuf,
    transcripts: PathBuf,
    manifest: PathBuf,
    repos: Vec<(String, PathBuf)>,
    binary: Option<String>,
    only: Vec<usize>,
    twice: bool,
    out_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runner {
    V1,
    Current,
}

fn parse_args() -> Result<Args, DtError> {
    let mut golden = false;
    let mut mutations_mode = false;
    let mut parity = false;
    let mut v1_metrics = None;
    let mut current_metrics = None;
    let mut index = None;
    let mut transcripts = None;
    let mut manifest = None;
    let mut repos = Vec::new();
    let mut binary = None;
    let mut runner = Runner::V1;
    let mut only = Vec::new();
    let mut twice = false;
    let mut out_dir = None;
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let needs_value = matches!(
            flag.as_str(),
            "--index"
                | "--transcripts"
                | "--manifest"
                | "--repo"
                | "--binary"
                | "--only"
                | "--out-dir"
                | "--v1"
                | "--current"
                | "--runner"
        );
        let value = if needs_value {
            raw.next()
                .ok_or_else(|| DtError::Cli(format!("{flag} needs a value")))?
        } else {
            String::new()
        };
        match flag.as_str() {
            "--golden" => golden = true,
            "--parity" => parity = true,
            "--v1" => v1_metrics = Some(PathBuf::from(value)),
            "--current" => current_metrics = Some(PathBuf::from(value)),
            "--mutations" => mutations_mode = true,
            "--index" => index = Some(PathBuf::from(value)),
            "--transcripts" => transcripts = Some(PathBuf::from(value)),
            "--manifest" => manifest = Some(PathBuf::from(value)),
            "--repo" => {
                let (name, path) = value
                    .split_once('=')
                    .ok_or_else(|| DtError::Cli(format!("--repo needs name=path, got {value}")))?;
                repos.push((name.to_owned(), PathBuf::from(path)));
            }
            "--binary" => binary = Some(value),
            "--runner" => {
                runner = match value.as_str() {
                    "v1" => Runner::V1,
                    "current" => Runner::Current,
                    other => {
                        return Err(DtError::Cli(format!(
                            "--runner must be v1 or current, got {other:?}"
                        )));
                    }
                };
            }
            "--only" => {
                let one_based = value
                    .parse::<usize>()
                    .map_err(|_| DtError::Cli(format!("--only needs a case number: {value}")))?
                    .checked_sub(1)
                    .ok_or_else(|| DtError::Cli("--only is 1-based".to_owned()))?;
                only.push(one_based);
            }
            "--twice" => twice = true,
            "--out-dir" => out_dir = Some(PathBuf::from(value)),
            other => return Err(DtError::Cli(format!("unknown flag {other}"))),
        }
    }
    if parity {
        if golden || mutations_mode {
            return Err(DtError::Cli(
                "--parity takes no --golden/--mutations".to_owned(),
            ));
        }
    } else if golden == mutations_mode {
        return Err(DtError::Cli(
            "exactly one of --golden, --mutations, or --parity is required".to_owned(),
        ));
    }
    let home = std::env::var("HOME").map_err(|_| DtError::Cli("HOME is unset".to_owned()))?;
    let index = index.unwrap_or_else(|| PathBuf::from("eval").join("golden").join("index.json"));
    let transcripts =
        transcripts.unwrap_or_else(|| PathBuf::from(home).join(".difftrace").join("reviews"));
    let manifest = manifest.unwrap_or_else(|| {
        PathBuf::from("eval")
            .join("mutations")
            .join("manifest.toml")
    });
    if mutations_mode && repos.is_empty() {
        return Err(DtError::Cli(
            "--mutations needs at least one --repo name=path".to_owned(),
        ));
    }
    Ok(Args {
        golden,
        parity,
        v1_metrics,
        current_metrics,
        runner,
        index,
        transcripts,
        manifest,
        repos,
        binary,
        only,
        twice,
        out_dir: out_dir.unwrap_or_else(|| PathBuf::from("eval").join("reports")),
    })
}

fn main() {
    if let Err(err) = run() {
        eprintln!("eval-run: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), DtError> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out_dir).map_err(DtError::Io)?;
    if args.parity {
        return parity_report(&args);
    }
    if args.golden {
        golden_baseline(&args)
    } else {
        mutation_runs(&args)
    }
}

#[derive(serde::Serialize)]
struct GoldenReport {
    metrics: metrics::Metrics,
    branches: Vec<BranchConvergence>,
    runs: usize,
    posted_findings: usize,
    note: String,
}

#[derive(serde::Serialize)]
struct BranchConvergence {
    repo: String,
    branch: String,
    rounds: usize,
    m3_rounds_to_zero: Option<u32>,
}

fn parity_report(args: &Args) -> Result<(), DtError> {
    use difftrace_eval::report::{ParityMetrics, evaluate_gates, render_parity};
    let v1_path = args
        .v1_metrics
        .as_ref()
        .ok_or_else(|| DtError::Cli("--parity needs --v1 <metrics.json>".to_owned()))?;
    let current_path = args
        .current_metrics
        .as_ref()
        .ok_or_else(|| DtError::Cli("--parity needs --current <metrics.json>".to_owned()))?;
    let read = |path: &Path| {
        std::fs::read_to_string(path)
            .map_err(|err| DtError::Cli(format!("{}: {err}", path.display())))
    };
    let v1: ParityMetrics = serde_json::from_str(&read(v1_path)?)
        .map_err(|err| DtError::Cli(format!("{}: {err}", v1_path.display())))?;
    let current: ParityMetrics = serde_json::from_str(&read(current_path)?)
        .map_err(|err| DtError::Cli(format!("{}: {err}", current_path.display())))?;
    let text = render_parity(&v1, &current);
    let path = args.out_dir.join("current-vs-v1.md");
    std::fs::write(&path, &text).map_err(DtError::Io)?;
    eprintln!("eval-run: wrote {}", path.display());
    let misses = evaluate_gates(&v1, &current);
    for miss in &misses {
        eprintln!(
            "gate miss {}: {} (owning: {})",
            miss.metric, miss.detail, miss.owning_task
        );
    }
    if misses.is_empty() {
        Ok(())
    } else {
        Err(DtError::Cli(format!(
            "{} parity gate(s) missed — the cutover waits",
            misses.len()
        )))
    }
}

fn golden_baseline(args: &Args) -> Result<(), DtError> {
    let text = std::fs::read_to_string(&args.index).map_err(DtError::Io)?;
    let index: IndexFile =
        serde_json::from_str(&text).map_err(|err| DtError::Cli(err.to_string()))?;
    let index = index.transcripts;
    let mut golden: Vec<GoldenFinding> = Vec::new();
    for run in &index {
        for finding in &run.findings {
            golden.push(GoldenFinding {
                run: run.run.clone(),
                sha: run.sha.clone(),
                file: finding.file.clone(),
                line: finding.line,
                title: finding.title.clone(),
                severity: finding.severity,
                label: finding.label,
            });
        }
    }
    let mut groups: BTreeMap<(String, String), Vec<Vec<Finding>>> = BTreeMap::new();
    for run in &index {
        let branch = branch_of(&args.transcripts, &run.run).unwrap_or_else(|| "unknown".to_owned());
        groups
            .entry((run.repo.clone(), branch))
            .or_default()
            .push(run.findings.iter().map(to_finding).collect::<Vec<_>>());
    }
    let branches = groups
        .iter()
        .filter(|(_, rounds)| rounds.len() > 1)
        .map(|((repo, branch), rounds)| BranchConvergence {
            repo: repo.clone(),
            branch: branch.clone(),
            rounds: rounds.len(),
            m3_rounds_to_zero: metrics::m3_rounds_to_zero(rounds),
        })
        .collect::<Vec<_>>();
    let m1 = metrics::m1_false_blockers(&golden);
    let report = GoldenReport {
        metrics: metrics::Metrics {
            m1_false_blockers: m1,
            m2_idempotent: None,
            m3_rounds_to_zero: None,
            m4_recall: None,
            m5_wall_secs: 0,
            m5_tokens: None,
        },
        branches,
        runs: index.len(),
        posted_findings: golden.len(),
        note: "m1 is 0 by construction while the seed file is empty; m3 is per branch \
               below; m2/m4/m5 need live reviewer runs (owner-gated spend)."
            .to_owned(),
    };
    write_reports(&args.out_dir, "golden", &report)
}

#[derive(serde::Deserialize)]
struct IndexFile {
    transcripts: Vec<RunRow>,
}

#[derive(serde::Deserialize)]
struct RunRow {
    run: String,
    repo: String,
    sha: String,
    findings: Vec<FindingRow>,
}

#[derive(serde::Deserialize)]
struct FindingRow {
    file: String,
    line: u64,
    title: String,
    severity: difftrace_eval::extract::Severity,
    label: Label,
}

fn to_finding(row: &FindingRow) -> Finding {
    Finding {
        file: row.file.clone(),
        line: row.line,
        title: row.title.clone(),
        severity: row.severity,
    }
}

fn branch_of(transcripts: &Path, run: &str) -> Option<String> {
    let text = std::fs::read_to_string(transcripts.join(run)).ok()?;
    let header = text.lines().next()?;
    let (_, tail) = header.split_once(" — ")?;
    let (range, _) = tail.rsplit_once(" @ ")?;
    let (_, branch) = range.split_once("..")?;
    Some(branch.to_owned())
}

fn write_reports(out_dir: &Path, tag: &str, report: &GoldenReport) -> Result<(), DtError> {
    let stamp = timestamp();
    let json = serde_json::to_string_pretty(report).map_err(|err| DtError::Cli(err.to_string()))?;
    let json_path = out_dir.join(format!("report-{tag}-{stamp}.json"));
    std::fs::write(&json_path, &json).map_err(DtError::Io)?;
    eprintln!("eval-run: wrote {}", json_path.display());
    let mut lines = vec![
        "# eval report — golden baseline".to_owned(),
        String::new(),
        format!("- runs: {}", report.runs),
        format!("- posted findings: {}", report.posted_findings),
        format!(
            "- m1 false blockers: {:.3}",
            report.metrics.m1_false_blockers
        ),
        String::new(),
        "## convergence per branch".to_owned(),
        String::new(),
        "| repo | branch | rounds | m3 rounds to zero |".to_owned(),
        "|---|---|---|---|".to_owned(),
    ];
    for branch in &report.branches {
        let m3 = branch
            .m3_rounds_to_zero
            .map_or_else(|| "never".to_owned(), |rounds| rounds.to_string());
        lines.push(format!(
            "| {} | {} | {} | {} |",
            branch.repo, branch.branch, branch.rounds, m3
        ));
    }
    lines.push(String::new());
    lines.push(format!("_note: {}_", report.note));
    let mut md = lines.join("\n");
    md.push('\n');
    let md_path = out_dir.join(format!("report-{tag}-{stamp}.md"));
    std::fs::write(&md_path, md).map_err(DtError::Io)?;
    eprintln!("eval-run: wrote {}", md_path.display());
    Ok(())
}

fn timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|err| err.to_string(), |since| since.as_secs().to_string())
}

fn reviewer_binary(flag: Option<&String>) -> Result<String, DtError> {
    let binary = flag.map_or_else(|| "difftrace".to_owned(), Clone::clone);
    if !binary.contains(std::path::MAIN_SEPARATOR) {
        return Ok(binary);
    }
    std::fs::canonicalize(&binary)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|err| DtError::Cli(format!("{binary}: {err}")))
}

fn list_reviews(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

fn findings_from_new_transcripts(
    transcripts: &Path,
    before: &[String],
) -> Result<Vec<Finding>, DtError> {
    let mut findings = Vec::new();
    for name in list_reviews(transcripts) {
        if before.contains(&name) || name.ends_with(".report.json") {
            continue;
        }
        let text = std::fs::read_to_string(transcripts.join(&name)).map_err(DtError::Io)?;
        match parse_transcript(&name, &text) {
            Ok(parsed) => findings.extend(parsed.findings),
            Err(err) => eprintln!("eval-run: skipping {name}: {err}"),
        }
    }
    Ok(findings)
}

fn mutation_runs(args: &Args) -> Result<(), DtError> {
    let text = std::fs::read_to_string(&args.manifest).map_err(DtError::Io)?;
    let cases = mutations::parse_manifest(&text)?;
    let home = std::env::var("HOME").map_err(|_| DtError::Cli("HOME is unset".to_owned()))?;
    let work_root = PathBuf::from(&home).join(".difftrace").join("eval-work");
    let reviews = PathBuf::from(&home).join(".difftrace").join("reviews");
    let binary = reviewer_binary(args.binary.as_ref())?;
    let selected: Vec<usize> = if args.only.is_empty() {
        (0..cases.len()).collect()
    } else {
        args.only.clone()
    };
    let mut wall_total: u64 = 0;
    let mut results: Vec<CaseOutcome> = Vec::new();
    for idx in selected {
        let case = cases.get(idx).ok_or_else(|| {
            DtError::Cli(format!("case {} is out of range", idx.saturating_add(1)))
        })?;
        let Some((_, root)) = args.repos.iter().find(|(name, _)| *name == case.repo) else {
            return Err(DtError::Cli(format!("no --repo for {}", case.repo)));
        };
        let runs = if args.twice { 2 } else { 1 };
        let mut run_findings: Vec<Vec<Finding>> = Vec::new();
        for _ in 0..runs {
            let before = list_reviews(&reviews);
            let started = Instant::now();
            mutations::apply(case, root, &work_root, false)?;
            let work = work_root.join(format!(
                "{}-{}",
                case.repo,
                case.base.get(..7).unwrap_or(case.base.as_str())
            ));
            let status = std::process::Command::new(&binary)
                .current_dir(&work)
                .args(["review", "--base", &case.base])
                .status()
                .map_err(|err| DtError::Cli(format!("spawn {binary}: {err}")))?;
            let elapsed = started.elapsed().as_secs();
            wall_total = wall_total.saturating_add(elapsed);
            if !status.success() {
                eprintln!("eval-run: case {} exited {}", idx.saturating_add(1), status);
            }
            let harvested = match args.runner {
                Runner::V1 => findings_from_new_transcripts(&reviews, &before)?,
                Runner::Current => {
                    difftrace_eval::report::findings_from_new_reports(&reviews, &before)?
                }
            };
            run_findings.push(harvested);
        }

        let titles = run_findings
            .first()
            .map(|findings| {
                findings
                    .iter()
                    .map(|finding| finding.title.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let hit = metrics::mutation_hit(&titles, &case.expect_title_tokens);
        results.push(CaseOutcome { hit, run_findings });
        eprintln!(
            "eval-run: case {} [{}]: {} findings, hit={hit}",
            idx.saturating_add(1),
            case.recipe.as_str(),
            titles.len()
        );
    }
    let report = summarize_cases(&results, wall_total);
    eprintln!(
        "eval-run: recall {}/{} = {:?}",
        report.hits, report.cases_run, report.m4_recall
    );
    write_mutation_report(&args.out_dir, &report)
}

struct CaseOutcome {
    hit: bool,
    run_findings: Vec<Vec<Finding>>,
}

fn summarize_cases(results: &[CaseOutcome], wall_secs: u64) -> MutationReport {
    let hits = results.iter().filter(|outcome| outcome.hit).count();
    let mut idempotent: Option<bool> = None;
    for outcome in results {
        if outcome.run_findings.len() == 2 {
            let first = outcome.run_findings.first().cloned().unwrap_or_default();
            let second = outcome.run_findings.get(1).cloned().unwrap_or_default();
            idempotent =
                Some(idempotent.unwrap_or(true) && metrics::m2_idempotent(&first, &second));
        }
    }
    MutationReport {
        cases_run: results.len(),
        hits,
        m2_idempotent: idempotent,
        m4_recall: metrics::m4_recall(hits, results.len()),
        m5_wall_secs: wall_secs,
    }
}

#[derive(serde::Serialize)]
struct MutationReport {
    cases_run: usize,
    hits: usize,
    m2_idempotent: Option<bool>,
    m4_recall: Option<f64>,
    m5_wall_secs: u64,
}

fn write_mutation_report(out_dir: &Path, report: &MutationReport) -> Result<(), DtError> {
    let stamp = timestamp();
    let json = serde_json::to_string_pretty(report).map_err(|err| DtError::Cli(err.to_string()))?;
    let json_path = out_dir.join(format!("report-mutations-{stamp}.json"));
    std::fs::write(&json_path, json).map_err(DtError::Io)?;
    let mut lines = vec![
        "# eval report — mutation runs".to_owned(),
        String::new(),
        format!(
            "- cases run: {} · hits: {} · recall: {:.3}",
            report.cases_run,
            report.hits,
            report.m4_recall.unwrap_or(0.0)
        ),
    ];
    if let Some(idempotent) = report.m2_idempotent {
        lines.push(format!("- m2 idempotent (double run): {idempotent}"));
    }
    lines.push(format!(
        "- m5 wall secs: {} (token counts are not exposed by the v1 console)",
        report.m5_wall_secs
    ));
    let mut md = lines.join("\n");
    md.push('\n');
    let md_path = out_dir.join(format!("report-mutations-{stamp}.md"));
    std::fs::write(&md_path, md).map_err(DtError::Io)?;
    eprintln!(
        "eval-run: wrote {} and {}",
        json_path.display(),
        md_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_binary_default_resolves_from_path() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(reviewer_binary(None)?, "difftrace".to_owned());
        assert_eq!(
            reviewer_binary(Some(&"difftrace-nightly".to_owned()))?,
            "difftrace-nightly".to_owned(),
            "a bare name stays a PATH lookup"
        );
        let dir = std::env::temp_dir().join(format!("dt-eval-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let bin = dir.join("difftrace-fixture");
        std::fs::write(&bin, b"#!/bin/sh\n")?;
        let resolved = reviewer_binary(Some(&bin.to_string_lossy().into_owned()))?;
        let canonical = std::fs::canonicalize(&bin)?.to_string_lossy().into_owned();
        assert_eq!(
            resolved, canonical,
            "a path-bearing binary resolves absolute before any run changes directory"
        );
        assert!(
            reviewer_binary(Some(&"no/such/difftrace".to_owned()))
                .is_err_and(|err| err.to_string().contains("no/such/difftrace")),
            "a missing path-bearing binary fails here, not inside the worktree"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn twice_counts_cases_not_runs_for_recall() {
        let case = || {
            vec![Finding {
                file: "src/a.rs".to_owned(),
                line: 3,
                title: "the lock is dropped".to_owned(),
                severity: difftrace_eval::extract::Severity::Warning,
            }]
        };
        let one_case_two_runs = vec![CaseOutcome {
            hit: true,
            run_findings: vec![case(), case()],
        }];
        let report = summarize_cases(&one_case_two_runs, 0);
        assert_eq!(
            report.cases_run, 1,
            "a case counts once however many runs it took"
        );
        assert_eq!(report.hits, 1);
        assert!(
            (report.m4_recall.unwrap_or(0.0) - 1.0).abs() < 1e-9,
            "--twice keeps recall whole instead of halving it: {:?}",
            report.m4_recall
        );
        assert_eq!(report.m2_idempotent, Some(true));
        let two_cases = vec![
            CaseOutcome {
                hit: true,
                run_findings: vec![Vec::new()],
            },
            CaseOutcome {
                hit: false,
                run_findings: vec![Vec::new()],
            },
        ];
        let report = summarize_cases(&two_cases, 9);
        assert_eq!(report.cases_run, 2);
        assert_eq!(report.m5_wall_secs, 9);
        assert_eq!(
            report.m2_idempotent, None,
            "no double run leaves m2 unmeasured"
        );
    }
}
