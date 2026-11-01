//! Lazy mechanical verification of claims that need execution to prove.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::OraclePolicy;
use crate::contracts::{ClaimType, Finding, OracleResults, OracleVerdict};
use crate::tools::CommandSpec;
use crate::tools::crates::WorkspaceMap;
use crate::tools::rust::{Executor, safe_label};

const FAILURE_MARKERS: [&str; 7] = [
    "fail",
    "panic",
    "does not compile",
    "won't compile",
    "crash",
    "hang",
    "deadlock",
];

#[must_use]
pub fn verify(
    claims: &[&Finding],
    policy: OraclePolicy,
    repo: &Path,
    head_sha: &str,
    budget: Duration,
    max_claims: usize,
    executor: &dyn Executor,
    cache_dir: &Path,
) -> OracleResults {
    let mut results = OracleResults::default();
    if policy == OraclePolicy::Off {
        return results;
    }
    let map = WorkspaceMap::load(repo).ok();
    let started = Instant::now();
    for finding in claims
        .iter()
        .copied()
        .filter(|finding| finding.claim == ClaimType::NeedsExecution)
        .take(max_claims)
    {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining < Duration::from_secs(1) {
            break;
        }
        let verdict = cached(
            finding,
            policy,
            repo,
            head_sha,
            remaining,
            map.as_ref(),
            executor,
            cache_dir,
        );
        results.verdicts.insert(finding.id.clone(), verdict);
    }
    results
}

fn cached(
    finding: &Finding,
    policy: OraclePolicy,
    repo: &Path,
    head_sha: &str,
    remaining: Duration,
    map: Option<&WorkspaceMap>,
    executor: &dyn Executor,
    cache_dir: &Path,
) -> OracleVerdict {
    let path = cache_path(cache_dir, head_sha, policy, &finding.id.0);
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(verdict) = serde_json::from_str::<OracleVerdict>(&text)
    {
        return verdict;
    }
    let verdict = decide(finding, policy, repo, remaining, map, executor);
    store(&path, &verdict);
    verdict
}

fn cache_path(cache_dir: &Path, head_sha: &str, policy: OraclePolicy, finding_id: &str) -> PathBuf {
    cache_dir.join(format!(
        "oracle.{}.{}.{}.json",
        safe_label(head_sha),
        policy_label(policy),
        safe_label(finding_id)
    ))
}

fn policy_label(policy: OraclePolicy) -> &'static str {
    match policy {
        OraclePolicy::Off => "off",
        OraclePolicy::Check => "check",
        OraclePolicy::Test => "test",
    }
}

fn store(path: &Path, verdict: &OracleVerdict) {
    if let Some(parent) = path.parent() {
        let _unused = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(verdict) {
        let _unused = std::fs::write(path, text);
    }
}

fn decide(
    finding: &Finding,
    policy: OraclePolicy,
    repo: &Path,
    remaining: Duration,
    map: Option<&WorkspaceMap>,
    executor: &dyn Executor,
) -> OracleVerdict {
    if !asserts_failure(&finding.title, &finding.mechanism) {
        return OracleVerdict::Inconclusive;
    }
    let Some(map) = map else {
        return OracleVerdict::Inconclusive;
    };
    let Some(cite) = finding.evidence.first() else {
        return OracleVerdict::Inconclusive;
    };
    let Some(krate) = map
        .crates_for_paths(std::slice::from_ref(&cite.file))
        .first()
        .cloned()
    else {
        return OracleVerdict::Inconclusive;
    };
    let spec = CommandSpec {
        argv: command_for(policy, &krate, &finding.title, &finding.mechanism),
        cwd: Some(repo.to_path_buf()),
        timeout_secs: remaining.as_secs(),
    };
    let Ok(output) = executor.run(&spec) else {
        return OracleVerdict::Inconclusive;
    };
    let refuted =
        output.code == Some(0) && (policy == OraclePolicy::Check || ran_a_test(&output.text));
    if refuted {
        return OracleVerdict::Refuted {
            command: spec.argv.join(" "),
        };
    }
    match (policy, output.code) {
        (OraclePolicy::Test | OraclePolicy::Check, Some(101)) => OracleVerdict::Confirmed,
        _ => OracleVerdict::Inconclusive,
    }
}

fn ran_a_test(text: &str) -> bool {
    text.lines().any(|line| {
        line.contains("test result:") && !(line.contains("0 passed") && line.contains("0 failed"))
    })
}

fn asserts_failure(title: &str, mechanism: &str) -> bool {
    let haystack = format!("{title} {mechanism}").to_lowercase();
    FAILURE_MARKERS
        .iter()
        .any(|marker| haystack.contains(marker))
}

fn command_for(policy: OraclePolicy, krate: &str, title: &str, mechanism: &str) -> Vec<String> {
    match policy {
        OraclePolicy::Off => Vec::new(),
        OraclePolicy::Check => vec![
            "cargo".to_owned(),
            "check".to_owned(),
            "-p".to_owned(),
            krate.to_owned(),
        ],
        OraclePolicy::Test => {
            let mut argv = vec![
                "cargo".to_owned(),
                "test".to_owned(),
                "-p".to_owned(),
                krate.to_owned(),
            ];
            if let Some(target) = test_target(title, mechanism) {
                argv.push(target);
            }
            argv
        }
    }
}

fn test_target(title: &str, mechanism: &str) -> Option<String> {
    let words = format!("{title} {mechanism}");
    let mut file_stem: Option<String> = None;
    for word in words.split_whitespace() {
        let cleaned = word
            .trim_matches(|c: char| !c.is_alphanumeric() && !matches!(c, '_' | ':' | '/' | '.'));
        if cleaned.starts_with('-') {
            continue;
        }
        if cleaned.contains("test_") || cleaned.ends_with("_test") || cleaned.contains("::test") {
            return Some(cleaned.to_owned());
        }
        if file_stem.is_none()
            && Path::new(cleaned)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        {
            let stem = match cleaned.rfind('.') {
                Some(at) => cleaned.get(..at).unwrap_or(cleaned),
                None => cleaned,
            };
            let stem = stem.rsplit('/').next().unwrap_or(stem);
            if !stem.starts_with('-') {
                file_stem = Some(stem.to_owned());
            }
        }
    }
    file_stem
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{AspectId, Cite, FindingDraft, RegionId, Severity};
    use crate::tools::rust::{ExecOutput, ToolRunError};
    use std::cell::Cell;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("difftrace-oracle-{}-{name}", std::process::id()))
    }

    fn workspace(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let root = scratch(name);
        let _unused = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/one/src"))?;
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )?;
        std::fs::write(
            root.join("crates/one/Cargo.toml"),
            "[package]\nname = \"one\"\nversion = \"0.1.0\"\n",
        )?;
        Ok(root)
    }

    fn cache(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let dir = scratch(name);
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn claim(title: &str, line: u32, claim_type: ClaimType) -> Finding {
        Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: title.to_owned(),
            mechanism: format!("the walk reaches crates/one/src/lib.rs:{line}"),
            claim: claim_type,
            evidence: vec![Cite {
                file: "crates/one/src/lib.rs".to_owned(),
                line,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        })
    }

    fn unowned_claim(title: &str) -> Finding {
        Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: title.to_owned(),
            mechanism: "the walk reaches docs/guide.md:2".to_owned(),
            claim: ClaimType::NeedsExecution,
            evidence: vec![Cite {
                file: "docs/guide.md".to_owned(),
                line: 2,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        })
    }

    struct FakeExecutor {
        calls: Cell<usize>,
        succeed: bool,
        killed: bool,
        timeout: bool,
        sleep: Option<Duration>,
        force_code: Option<i32>,
        output_text: Option<String>,
    }

    impl FakeExecutor {
        fn passing() -> Self {
            Self {
                calls: Cell::new(0),
                succeed: true,
                killed: false,
                timeout: false,
                sleep: None,
                force_code: None,
                output_text: None,
            }
        }
    }

    impl Executor for FakeExecutor {
        fn run(&self, _spec: &CommandSpec) -> Result<ExecOutput, ToolRunError> {
            self.calls.set(self.calls.get().saturating_add(1));
            if let Some(sleep) = self.sleep {
                std::thread::sleep(sleep);
            }
            if self.timeout {
                return Err(ToolRunError::Timeout);
            }
            if self.killed {
                return Ok(ExecOutput {
                    text: String::new(),
                    code: None,
                });
            }
            let code = self.force_code.unwrap_or_else(|| i32::from(!self.succeed));
            Ok(ExecOutput {
                text: self.output_text.clone().unwrap_or_default(),
                code: Some(code),
            })
        }
    }

    fn drive(
        findings: &[&Finding],
        policy: OraclePolicy,
        root: &Path,
        cache_dir: &Path,
        executor: &FakeExecutor,
    ) -> OracleResults {
        verify(
            findings,
            policy,
            root,
            "abc123",
            Duration::from_secs(30),
            5,
            executor,
            cache_dir,
        )
    }

    #[test]
    fn a_refuted_claim_carries_the_command_that_refuted_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("refuted")?;
        let cache_dir = cache("refuted-cache")?;
        let executor = FakeExecutor::passing();
        let finding = claim(
            "The crate does not compile with the new bound",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Check,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Refuted {
                command: "cargo check -p one".to_owned()
            })
        );
        Ok(())
    }

    #[test]
    fn an_inconclusive_run_keeps_the_finding_solo() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("inconclusive")?;
        let cache_dir = cache("inconclusive-cache")?;
        let timed_out = FakeExecutor {
            timeout: true,
            ..FakeExecutor::passing()
        };
        let failing_path = claim(
            "The build does not compile on this toolchain",
            3,
            ClaimType::NeedsExecution,
        );
        let first = drive(
            &[&failing_path],
            OraclePolicy::Check,
            &root,
            &cache_dir,
            &timed_out,
        );
        assert_eq!(
            first.verdicts.get(&failing_path.id),
            Some(&OracleVerdict::Inconclusive)
        );
        assert_eq!(timed_out.calls.get(), 1);
        let indifferent = FakeExecutor {
            succeed: false,
            ..FakeExecutor::passing()
        };
        let stale = claim(
            "The cache returns the stale entry",
            4,
            ClaimType::NeedsExecution,
        );
        let second = drive(
            &[&stale],
            OraclePolicy::Check,
            &root,
            &cache_dir,
            &indifferent,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            second.verdicts.get(&stale.id),
            Some(&OracleVerdict::Inconclusive),
            "a claim that asserts no failure is inconclusive without a run"
        );
        assert_eq!(indifferent.calls.get(), 0);
        let broken = scratch("inconclusive-broken");
        std::fs::create_dir_all(&broken)?;
        std::fs::write(broken.join("Cargo.toml"), "not a manifest at all")?;
        let unmapped = FakeExecutor::passing();
        let lost = claim(
            "The build does not compile anywhere",
            3,
            ClaimType::NeedsExecution,
        );
        let third = drive(
            &[&lost],
            OraclePolicy::Check,
            &broken,
            &cache_dir,
            &unmapped,
        );
        let _unused = std::fs::remove_dir_all(&broken);
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            third.verdicts.get(&lost.id),
            Some(&OracleVerdict::Inconclusive),
            "an unreadable workspace map inconclusives every claim without a run"
        );
        assert_eq!(unmapped.calls.get(), 0);
        Ok(())
    }

    #[test]
    fn budget_exhaustion_leaves_remaining_claims_unverified()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("budget")?;
        let cache_dir = cache("budget-cache")?;
        let executor = FakeExecutor {
            sleep: Some(Duration::from_millis(400)),
            ..FakeExecutor::passing()
        };
        let first = claim(
            "The build does not compile with the feature set",
            3,
            ClaimType::NeedsExecution,
        );
        let second = claim(
            "The walk hangs on the empty iterator",
            4,
            ClaimType::NeedsExecution,
        );
        let results = verify(
            &[&first, &second],
            OraclePolicy::Check,
            &root,
            "abc123",
            Duration::from_millis(1200),
            5,
            &executor,
            &cache_dir,
        );
        assert!(
            matches!(
                results.verdicts.get(&first.id),
                Some(OracleVerdict::Refuted { .. })
            ),
            "the claim inside the budget is decided"
        );
        assert!(
            !results.verdicts.contains_key(&second.id),
            "a sub-second remainder is a spent budget — the next claim is unverified, not inconclusive"
        );
        assert_eq!(executor.calls.get(), 1);
        let starved = FakeExecutor::passing();
        let third = claim(
            "The build hangs under the release profile",
            5,
            ClaimType::NeedsExecution,
        );
        let empty = verify(
            &[&third],
            OraclePolicy::Check,
            &root,
            "abc123",
            Duration::from_millis(500),
            5,
            &starved,
            &cache_dir,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert!(
            !empty.verdicts.contains_key(&third.id),
            "a sub-second budget from the start attempts nothing"
        );
        assert_eq!(starved.calls.get(), 0);
        Ok(())
    }

    #[test]
    fn the_claim_cap_bounds_verifications_per_round() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("cap")?;
        let cache_dir = cache("cap-cache")?;
        let executor = FakeExecutor::passing();
        let first = claim(
            "The build does not compile with rustc 1.98",
            3,
            ClaimType::NeedsExecution,
        );
        let second = claim(
            "The login flow panics on the empty password",
            4,
            ClaimType::NeedsExecution,
        );
        let third = claim(
            "The batch runner hangs on the poisoned lock",
            5,
            ClaimType::NeedsExecution,
        );
        let results = verify(
            &[&first, &second, &third],
            OraclePolicy::Check,
            &root,
            "abc123",
            Duration::from_secs(30),
            2,
            &executor,
            &cache_dir,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert!(
            results.verdicts.contains_key(&first.id) && results.verdicts.contains_key(&second.id),
            "the first two claims verify in input order"
        );
        assert!(
            !results.verdicts.contains_key(&third.id),
            "the third claim sits past the per-round cap and is unattempted"
        );
        assert_eq!(executor.calls.get(), 2);
        Ok(())
    }

    #[test]
    fn the_oracle_never_demotes_a_confirmed_finding() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("selection")?;
        let cache_dir = cache("selection-cache")?;
        let executor = FakeExecutor::passing();
        let in_diff = claim("The build does not compile here", 3, ClaimType::InDiff);
        let assumption = claim(
            "The walk crashes on the untested branch",
            4,
            ClaimType::ExternalAssumption,
        );
        let results = drive(
            &[&in_diff, &assumption],
            OraclePolicy::Check,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert!(
            results.verdicts.is_empty(),
            "only execution claims are ever verified"
        );
        assert_eq!(executor.calls.get(), 0);
        Ok(())
    }

    #[test]
    fn off_policy_runs_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("off")?;
        let cache_dir = cache("off-cache")?;
        let executor = FakeExecutor::passing();
        let finding = claim(
            "The build does not compile at all",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(&[&finding], OraclePolicy::Off, &root, &cache_dir, &executor);
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert!(results.verdicts.is_empty());
        assert_eq!(executor.calls.get(), 0);
        Ok(())
    }

    #[test]
    fn check_and_test_commands_target_the_owning_crate() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("targeting")?;
        let cache_dir = cache("targeting-cache")?;
        let check = claim(
            "The crate does not compile with the feature set",
            3,
            ClaimType::NeedsExecution,
        );
        let named = claim(
            "The login test_login_flow fails on the retry",
            3,
            ClaimType::NeedsExecution,
        );
        let from_file = claim(
            "The integration run of tests/parser.rs fails",
            4,
            ClaimType::NeedsExecution,
        );
        let package_wide = claim(
            "The integration run fails on the batch",
            5,
            ClaimType::NeedsExecution,
        );
        let unowned = unowned_claim("The documented example fails");
        let findings = [&check, &named, &from_file, &package_wide, &unowned];
        let executor = FakeExecutor {
            output_text: Some(
                "running 1 tests\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured"
                    .to_owned(),
            ),
            ..FakeExecutor::passing()
        };
        let commands = drive(&findings, OraclePolicy::Test, &root, &cache_dir, &executor);
        let check_only = drive(&[&check], OraclePolicy::Check, &root, &cache_dir, &executor);
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        let command_of = |finding: &Finding| match commands.verdicts.get(&finding.id) {
            Some(OracleVerdict::Refuted { command }) => Some(command.as_str()),
            _ => None,
        };
        assert_eq!(
            command_of(&named),
            Some("cargo test -p one test_login_flow"),
            "a test-shaped token rides as the name filter"
        );
        assert_eq!(
            command_of(&from_file),
            Some("cargo test -p one parser"),
            "a .rs path token contributes its stem"
        );
        assert_eq!(
            command_of(&package_wide),
            Some("cargo test -p one"),
            "no extractable target means the whole package"
        );
        assert_eq!(
            commands.verdicts.get(&unowned.id),
            Some(&OracleVerdict::Inconclusive),
            "a claim no crate owns never runs"
        );
        assert_eq!(
            executor.calls.get(),
            5,
            "four owned claims under test plus one re-verification under check — the unowned claim spends no call"
        );
        assert_eq!(
            check_only.verdicts.get(&check.id),
            Some(&OracleVerdict::Refuted {
                command: "cargo check -p one".to_owned()
            })
        );
        Ok(())
    }

    #[test]
    fn verdicts_cache_by_head_sha_and_claim() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("cache")?;
        let cache_dir = cache("cache-cache")?;
        let finding = claim(
            "The build does not compile under release",
            3,
            ClaimType::NeedsExecution,
        );
        let first_run = FakeExecutor::passing();
        let first = verify(
            &[&finding],
            OraclePolicy::Check,
            &root,
            "sha-one",
            Duration::from_secs(30),
            5,
            &first_run,
            &cache_dir,
        );
        assert_eq!(first_run.calls.get(), 1);
        let second_run = FakeExecutor::passing();
        let second = verify(
            &[&finding],
            OraclePolicy::Check,
            &root,
            "sha-one",
            Duration::from_secs(30),
            5,
            &second_run,
            &cache_dir,
        );
        assert_eq!(second_run.calls.get(), 0, "the same sha and claim is a hit");
        assert_eq!(first, second);
        let timed_out = FakeExecutor {
            timeout: true,
            ..FakeExecutor::passing()
        };
        let hung = claim("The build hangs under load", 4, ClaimType::NeedsExecution);
        let inconclusive = verify(
            &[&hung],
            OraclePolicy::Check,
            &root,
            "sha-one",
            Duration::from_secs(30),
            5,
            &timed_out,
            &cache_dir,
        );
        assert_eq!(
            inconclusive.verdicts.get(&hung.id),
            Some(&OracleVerdict::Inconclusive)
        );
        let retry = FakeExecutor::passing();
        let cached_inconclusive = verify(
            &[&hung],
            OraclePolicy::Check,
            &root,
            "sha-one",
            Duration::from_secs(30),
            5,
            &retry,
            &cache_dir,
        );
        assert_eq!(
            retry.calls.get(),
            0,
            "an inconclusive verdict caches like any other"
        );
        assert_eq!(
            cached_inconclusive.verdicts.get(&hung.id),
            Some(&OracleVerdict::Inconclusive)
        );
        let new_sha = FakeExecutor::passing();
        let rerun = verify(
            &[&finding],
            OraclePolicy::Check,
            &root,
            "sha-two",
            Duration::from_secs(30),
            5,
            &new_sha,
            &cache_dir,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(new_sha.calls.get(), 1, "a new sha re-verifies");
        assert!(matches!(
            rerun.verdicts.get(&finding.id),
            Some(OracleVerdict::Refuted { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_signal_killed_command_stays_inconclusive() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("signal-killed")?;
        let cache_dir = cache("signal-killed-cache")?;
        let executor = FakeExecutor {
            killed: true,
            ..FakeExecutor::passing()
        };
        let finding = claim(
            "The build does not compile when the lockfile is absent",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Check,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Inconclusive),
            "an OOM-killed command decided nothing; dead infrastructure never confirms"
        );
        assert_eq!(executor.calls.get(), 1, "the attempt itself was spent");
        Ok(())
    }

    #[test]
    fn an_empty_test_run_proves_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("empty-run")?;
        let cache_dir = cache("empty-run-cache")?;
        let executor = FakeExecutor {
            output_text: Some(
                "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 767 filtered out"
                    .to_owned(),
            ),
            ..FakeExecutor::passing()
        };
        let finding = claim(
            "The login test_login_flow fails on the second call",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Test,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Inconclusive),
            "a filter that matched no test executed zero tests and proves neither side"
        );
        Ok(())
    }

    #[test]
    fn a_passing_run_with_real_output_still_refutes() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("real-run")?;
        let cache_dir = cache("real-run-cache")?;
        let executor = FakeExecutor {
            output_text: Some(
                "running 3 tests\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured"
                    .to_owned(),
            ),
            ..FakeExecutor::passing()
        };
        let finding = claim(
            "The login test_login_flow fails on the second call",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Test,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert!(
            results
                .verdicts
                .get(&finding.id)
                .is_some_and(|verdict| matches!(verdict, OracleVerdict::Refuted { .. })),
            "a green run that actually executed tests refutes the failure claim"
        );
        Ok(())
    }

    #[test]
    fn failing_commands_confirm_failure_claims() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("confirm")?;
        let cache_dir = cache("confirm-cache")?;
        let executor = FakeExecutor {
            force_code: Some(101),
            ..FakeExecutor::passing()
        };
        let finding = claim(
            "The login test_login_flow fails on the second call",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Test,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Confirmed),
            "a failing test (cargo exit 101) confirms the failure the claim asserts"
        );
        Ok(())
    }

    #[test]
    fn a_cargo_usage_error_never_confirms_a_test_claim() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("usage")?;
        let cache_dir = cache("usage-cache")?;
        let executor = FakeExecutor {
            force_code: Some(1),
            ..FakeExecutor::passing()
        };
        let finding = claim(
            "The login test_login_flow fails on the second call",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Test,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Inconclusive),
            "a CLI usage error (cargo exit 1) proves neither side of the claim"
        );
        Ok(())
    }

    #[test]
    fn a_check_usage_error_stays_inconclusive() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("check-usage")?;
        let cache_dir = cache("check-usage-cache")?;
        let executor = FakeExecutor {
            force_code: Some(1),
            ..FakeExecutor::passing()
        };
        let finding = claim(
            "The crate does not compile with the new bound",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Check,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Inconclusive),
            "a CLI usage error under the check policy decides nothing"
        );
        Ok(())
    }

    #[test]
    fn a_compile_failure_confirms_a_check_claim() -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("compile")?;
        let cache_dir = cache("compile-cache")?;
        let executor = FakeExecutor {
            force_code: Some(101),
            ..FakeExecutor::passing()
        };
        let finding = claim(
            "The crate does not compile with the new trait bound",
            3,
            ClaimType::NeedsExecution,
        );
        let results = drive(
            &[&finding],
            OraclePolicy::Check,
            &root,
            &cache_dir,
            &executor,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Confirmed),
            "cargo check's canonical failure exit (101) confirms a compile claim"
        );
        Ok(())
    }

    #[test]
    fn a_dash_leading_test_stem_is_never_a_filter() {
        assert!(
            test_target("the drift in dir/-fast.rs compounds", "mechanism words").is_none(),
            "a dash-leading stem would become a flag argument"
        );
        assert!(
            test_target("the drift in -fast compounds", "mechanism words").is_none(),
            "a lone dash-leading token cleans to nothing usable"
        );
        assert_eq!(
            test_target("see crates/guard.rs for the drop", "mechanism words"),
            Some("guard".to_owned()),
            "ordinary stems keep flowing"
        );
        assert_eq!(
            test_target("the login test_login_flow fails", "mechanism words"),
            Some("test_login_flow".to_owned()),
            "ordinary test-shaped tokens keep flowing"
        );
        assert_eq!(
            test_target("the login -test_foo fails twice", "mechanism words"),
            Some("test_foo".to_owned()),
            "a dash is cleaning noise: the trimmed token is a filter, never a flag"
        );
    }

    #[test]
    fn a_real_failing_test_confirms_through_the_process_executor()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace("real-cargo")?;
        std::fs::write(
            root.join("crates/one/src/lib.rs"),
            "#[test]\nfn always_fails() { assert!(false); }\n",
        )?;
        let cache_dir = cache("real-cargo-cache")?;
        let executor = crate::tools::rust::ProcessExecutor;
        let finding = Finding::new(FindingDraft {
            region: RegionId(0),
            aspect: AspectId("error-paths".to_owned()),
            title: "The crate fails its own verification under load".to_owned(),
            mechanism: "the walk breaks the invariant at the boundary".to_owned(),
            claim: ClaimType::NeedsExecution,
            evidence: vec![Cite {
                file: "crates/one/src/lib.rs".to_owned(),
                line: 3,
            }],
            severity: Severity::Warning,
            fix_hint: None,
        });
        let results = verify(
            &[&finding],
            OraclePolicy::Test,
            &root,
            "real-cargo-sha",
            Duration::from_secs(120),
            5,
            &executor,
            &cache_dir,
        );
        let _unused = std::fs::remove_dir_all(&root);
        let _unused = std::fs::remove_dir_all(&cache_dir);
        assert_eq!(
            results.verdicts.get(&finding.id),
            Some(&OracleVerdict::Confirmed),
            "a real cargo run over a genuinely failing test (exit 101) confirms the claim"
        );
        Ok(())
    }
}
