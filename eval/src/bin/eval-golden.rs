//! CLI entry: transcripts in, labeled golden index out.

use std::collections::BTreeMap;
use std::path::PathBuf;

use difftrace_eval::correlate;
use difftrace_eval::correlate::{Correlator, Label, Repo};
use difftrace_eval::error::DtError;
use difftrace_eval::extract::{is_transcript_name, parse_transcript};

#[derive(Debug, serde::Serialize)]
struct RunSummary {
    run: String,
    repo: String,
    sha: String,
    findings: Vec<correlate::GoldenFinding>,
    dropped: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct Index {
    transcripts: Vec<RunSummary>,
}

struct Args {
    transcripts: PathBuf,
    repos: Vec<Repo>,
    seeds: PathBuf,
    out: PathBuf,
}

fn parse_args() -> Result<Args, DtError> {
    let mut transcripts = None;
    let mut repos = Vec::new();
    let mut seeds = None;
    let mut out = None;
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let value = raw
            .next()
            .ok_or_else(|| DtError::Cli(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--transcripts" => transcripts = Some(PathBuf::from(value)),
            "--seeds" => seeds = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--repo" => {
                let (name, path) = value
                    .split_once('=')
                    .ok_or_else(|| DtError::Cli(format!("--repo needs name=path, got {value}")))?;
                repos.push(Repo {
                    name: name.to_owned(),
                    path: PathBuf::from(path),
                });
            }
            other => return Err(DtError::Cli(format!("unknown flag {other}"))),
        }
    }
    let home = std::env::var("HOME")
        .map_err(|_| DtError::Cli("HOME is unset and --transcripts was not given".to_owned()))?;
    let transcripts =
        transcripts.unwrap_or_else(|| PathBuf::from(home).join(".difftrace").join("reviews"));
    let seeds = seeds.unwrap_or_else(|| PathBuf::from("eval").join("seed-false-positives.md"));
    let out = out.unwrap_or_else(|| PathBuf::from("eval").join("golden").join("index.json"));
    if repos.is_empty() {
        return Err(DtError::Cli(
            "at least one --repo name=path is required".to_owned(),
        ));
    }
    Ok(Args {
        transcripts,
        repos,
        seeds,
        out,
    })
}

fn main() {
    if let Err(err) = run() {
        eprintln!("eval-golden: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), DtError> {
    let args = parse_args()?;
    let seeds = load_seeds(&args.seeds)?;
    let mut correlator = Correlator::new(args.repos, seeds);
    let mut summaries = Vec::new();
    let mut skipped = Vec::new();
    let mut names = std::fs::read_dir(&args.transcripts)
        .map_err(DtError::Io)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().into_string().ok())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| DtError::Cli("non-utf8 transcript name".to_owned()))?;
    names.retain(|name| is_transcript_name(name));
    names.sort();
    for name in names {
        let text = std::fs::read_to_string(args.transcripts.join(&name)).map_err(DtError::Io)?;
        let parsed = match parse_transcript(&name, &text) {
            Ok(parsed) => parsed,
            Err(err) => {
                skipped.push(name.clone());
                eprintln!("eval-golden: skipping {name}: {err}");
                continue;
            }
        };
        let repo_idx = correlator.resolve(&parsed.sha)?;
        let repo_name = repo_idx
            .and_then(|idx| correlator.repo(idx))
            .map_or_else(|| "unresolved".to_owned(), |repo| repo.name.clone());
        let findings = correlator.label(&name, &parsed.sha, repo_idx, &parsed.findings)?;
        summaries.push(RunSummary {
            run: name,
            repo: repo_name,
            sha: parsed.sha,
            findings,
            dropped: parsed.dropped,
        });
    }
    print_distribution(&summaries, &skipped);
    let index = Index {
        transcripts: summaries,
    };
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).map_err(DtError::Io)?;
    }
    let mut json =
        serde_json::to_string_pretty(&index).map_err(|err| DtError::Cli(err.to_string()))?;
    json.push('\n');
    std::fs::write(&args.out, json).map_err(DtError::Io)?;
    eprintln!("eval-golden: wrote {}", args.out.display());
    Ok(())
}

fn load_seeds(path: &std::path::Path) -> Result<Vec<correlate::SeedEntry>, DtError> {
    match std::fs::read_to_string(path) {
        Ok(text) => correlate::parse_seeds(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "eval-golden: no seed file at {} — false positives come from fix commits only",
                path.display()
            );
            Ok(Vec::new())
        }
        Err(err) => Err(DtError::Io(err)),
    }
}

fn print_distribution(summaries: &[RunSummary], skipped: &[String]) {
    let mut per_repo: BTreeMap<&str, (usize, usize, usize, usize, usize)> = BTreeMap::new();
    let mut dropped_total: usize = 0;
    for summary in summaries {
        let entry = per_repo
            .entry(summary.repo.as_str())
            .or_insert((0, 0, 0, 0, 0));
        entry.0 = entry.0.saturating_add(1);
        dropped_total = dropped_total.saturating_add(summary.dropped.len());
        for finding in &summary.findings {
            entry.1 = entry.1.saturating_add(1);
            match finding.label {
                Label::TruePositive => entry.2 = entry.2.saturating_add(1),
                Label::FalsePositive => entry.3 = entry.3.saturating_add(1),
                Label::Unknown => entry.4 = entry.4.saturating_add(1),
            }
        }
    }
    for (repo, (runs, findings, true_positives, false_positives, unknown)) in &per_repo {
        eprintln!(
            "eval-golden: {repo}: {runs} transcripts · {findings} posted findings · \
             true-positive {true_positives} · false-positive {false_positives} · \
             unknown {unknown} ({pct}%)",
            pct = percentage(*unknown, *findings),
        );
    }
    let runs: usize = summaries.len();
    let findings: usize = summaries.iter().map(|summary| summary.findings.len()).sum();
    let true_positives: usize = summaries
        .iter()
        .flat_map(|summary| summary.findings.iter())
        .filter(|finding| finding.label == Label::TruePositive)
        .count();
    let false_positives: usize = summaries
        .iter()
        .flat_map(|summary| summary.findings.iter())
        .filter(|finding| finding.label == Label::FalsePositive)
        .count();
    let unknown = findings
        .checked_sub(true_positives)
        .and_then(|rest| rest.checked_sub(false_positives))
        .unwrap_or(0);
    eprintln!(
        "eval-golden: total: {runs} transcripts · {findings} posted findings · \
         true-positive {true_positives} ({tp}%) · false-positive {false_positives} · \
         unknown {unknown} ({up}%) · dropped {dropped_total} · skipped files {}",
        skipped.len(),
        tp = percentage(true_positives, findings),
        up = percentage(unknown, findings),
    );
}

fn percentage(part: usize, total: usize) -> u64 {
    if total == 0 {
        return 0;
    }
    let part = u64::try_from(part).unwrap_or(0);
    let total = u64::try_from(total).unwrap_or(1);
    part.saturating_mul(100).checked_div(total).unwrap_or(0)
}
