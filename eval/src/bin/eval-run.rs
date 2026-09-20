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
    index: PathBuf,
    transcripts: PathBuf,
    manifest: PathBuf,
    repos: Vec<(String, PathBuf)>,
    binary: Option<String>,
    only: Vec<usize>,
    twice: bool,
    out_dir: PathBuf,
}

fn parse_args() -> Result<Args, DtError> {
    let mut golden = false;
    let mut mutations_mode = false;
    let mut index = None;
    let mut transcripts = None;
    let mut manifest = None;
    let mut repos = Vec::new();
    let mut binary = None;
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
        );
        let value = if needs_value {
            raw.next()
                .ok_or_else(|| DtError::Cli(format!("{flag} needs a value")))?
        } else {
            String::new()
        };
        match flag.as_str() {
            "--golden" => golden = true,
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
    if golden == mutations_mode {
        return Err(DtError::Cli(
            "exactly one of --golden or --mutations is required".to_owned(),
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

fn v1_binary(flag: Option<&String>) -> String {
    flag.map_or_else(
        || {
            let local = PathBuf::from("target").join("release").join("difftrace");
            if local.exists() {
                local.to_string_lossy().into_owned()
            } else {
                "difftrace".to_owned()
            }
        },
        Clone::clone,
    )
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
        if before.contains(&name) {
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
    let binary = v1_binary(args.binary.as_ref());
    let selected: Vec<usize> = if args.only.is_empty() {
        (0..cases.len()).collect()
    } else {
        args.only.clone()
    };
    let mut hits: usize = 0;
    let mut ran: usize = 0;
    let mut wall_total: u64 = 0;
    let mut idempotent: Option<bool> = None;
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
            run_findings.push(findings_from_new_transcripts(&reviews, &before)?);
            ran = ran.saturating_add(1);
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
        if hit {
            hits = hits.saturating_add(1);
        }
        if runs == 2 {
            let first = run_findings.first().cloned().unwrap_or_default();
            let second = run_findings.get(1).cloned().unwrap_or_default();
            idempotent =
                Some(idempotent.unwrap_or(true) && metrics::m2_idempotent(&first, &second));
        }
        eprintln!(
            "eval-run: case {} [{}]: {} findings, hit={hit}",
            idx.saturating_add(1),
            case.recipe.as_str(),
            titles.len()
        );
    }
    let recall = metrics::m4_recall(hits, ran);
    eprintln!("eval-run: recall {hits}/{ran} = {recall:?}");
    let report = MutationReport {
        cases_run: ran,
        hits,
        m2_idempotent: idempotent,
        m4_recall: recall,
        m5_wall_secs: wall_total,
    };
    write_mutation_report(&args.out_dir, &report)
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
