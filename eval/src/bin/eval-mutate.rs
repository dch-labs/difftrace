//! CLI entry: manifest validation and per-case mutation application.

use std::path::PathBuf;

use difftrace_eval::error::DtError;
use difftrace_eval::mutations::{self, Case};

struct Args {
    manifest: PathBuf,
    repos: Vec<(String, PathBuf)>,
    work: PathBuf,
    apply: Option<usize>,
    check: bool,
}

fn parse_args() -> Result<Args, DtError> {
    let mut manifest = None;
    let mut repos = Vec::new();
    let mut work = None;
    let mut apply = None;
    let mut check = false;
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let value = raw
            .next()
            .ok_or_else(|| DtError::Cli(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value)),
            "--work" => work = Some(PathBuf::from(value)),
            "--apply" => {
                apply = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| DtError::Cli(format!("--apply needs a case number: {value}")))?
                        .checked_sub(1)
                        .ok_or_else(|| DtError::Cli("--apply is 1-based".to_owned()))?,
                );
            }
            "--check" => {
                check = value == "1";
            }
            "--repo" => {
                let (name, path) = value
                    .split_once('=')
                    .ok_or_else(|| DtError::Cli(format!("--repo needs name=path, got {value}")))?;
                repos.push((name.to_owned(), PathBuf::from(path)));
            }
            other => return Err(DtError::Cli(format!("unknown flag {other}"))),
        }
    }
    let manifest = manifest.unwrap_or_else(|| {
        PathBuf::from("eval")
            .join("mutations")
            .join("manifest.toml")
    });
    let work = match work {
        Some(path) => path,
        None => PathBuf::from(
            std::env::var("HOME")
                .map_err(|_| DtError::Cli("HOME is unset and --work was not given".to_owned()))?,
        )
        .join(".difftrace")
        .join("eval-work"),
    };
    if repos.is_empty() {
        return Err(DtError::Cli(
            "at least one --repo name=path is required".to_owned(),
        ));
    }
    Ok(Args {
        manifest,
        repos,
        work,
        apply,
        check,
    })
}

fn main() {
    if let Err(err) = run() {
        eprintln!("eval-mutate: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), DtError> {
    let args = parse_args()?;
    let text = std::fs::read_to_string(&args.manifest).map_err(DtError::Io)?;
    let cases = mutations::parse_manifest(&text)?;
    eprintln!(
        "eval-mutate: {} cases from {}",
        cases.len(),
        args.manifest.display()
    );
    match args.apply {
        None => validate_all(&cases, &args.repos),
        Some(_) => apply_one(&cases, &args),
    }
}

fn validate_all(cases: &[Case], repos: &[(String, PathBuf)]) -> Result<(), DtError> {
    let mut failures: usize = 0;
    for case in cases {
        let Some((_, root)) = repos.iter().find(|(name, _)| *name == case.repo) else {
            eprintln!("eval-mutate: {}: no --repo for {}", label(case), case.repo);
            failures = failures.saturating_add(1);
            continue;
        };
        match mutations::validate(case, |sha, file| mutations::show_file_at(root, sha, file)) {
            Ok(()) => eprintln!("eval-mutate: {}: ok", label(case)),
            Err(err) => {
                eprintln!("eval-mutate: {err}");
                failures = failures.saturating_add(1);
            }
        }
    }
    if failures > 0 {
        return Err(DtError::Mutation {
            case: "validation".to_owned(),
            message: format!("{failures} of {} cases failed", cases.len()),
        });
    }
    eprintln!("eval-mutate: all {} cases validate", cases.len());
    Ok(())
}

fn apply_one(cases: &[Case], args: &Args) -> Result<(), DtError> {
    let index = args
        .apply
        .ok_or_else(|| DtError::Cli("no case".to_owned()))?;
    let case = cases.get(index).ok_or_else(|| {
        DtError::Cli(format!(
            "case {} is out of range (1..={})",
            index.saturating_add(1),
            cases.len()
        ))
    })?;
    let Some((_, root)) = args.repos.iter().find(|(name, _)| *name == case.repo) else {
        return Err(DtError::Cli(format!("no --repo for {}", case.repo)));
    };
    let range = mutations::apply(case, root, &args.work, args.check)?;
    println!(
        "{}\t{}\t{}\t{}",
        label(case),
        range.base,
        range.head,
        mutations::token_set(case)
            .into_iter()
            .collect::<Vec<_>>()
            .join(" ")
    );
    eprintln!("eval-mutate: work tree at {}", args.work.display());
    Ok(())
}

fn label(case: &Case) -> String {
    format!(
        "{}@{}[{}]",
        case.repo,
        case.base.get(..7).unwrap_or(case.base.as_str()),
        case.recipe.as_str()
    )
}
