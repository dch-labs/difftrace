//! End-to-end runs of the real binary: the exit-code contract under a
//! scrubbed child environment, and the hosted token gate.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> Result<PathBuf, std::io::Error> {
    let dir = std::env::temp_dir().join(format!(
        "difftrace-binary-e2e-{name}-{}",
        std::process::id()
    ));
    let _unused = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    git(&dir, &["init", "-b", "main"])?;
    Ok(dir)
}

fn git(dir: &Path, args: &[&str]) -> Result<(), std::io::Error> {
    let output = Command::new("git").current_dir(dir).args(args).output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

fn commit(dir: &Path, files: &[(&str, &str)], message: &str) -> Result<(), std::io::Error> {
    for (path, body) in files {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, body)?;
    }
    git(dir, &["add", "-A"])?;
    git(
        dir,
        &[
            "-c",
            "user.email=difftrace@example.invalid",
            "-c",
            "user.name=difftrace",
            "commit",
            "-m",
            message,
        ],
    )
}

#[test]
fn the_binary_review_exits_three_without_providers() -> Result<(), Box<dyn std::error::Error>> {
    let repo = scratch("review")?;
    commit(&repo, &[("src/base.rs", "fn base() {}\n")], "base")?;
    commit(&repo, &[("src/head.rs", "fn head() {}\n")], "head")?;
    let home =
        std::env::temp_dir().join(format!("difftrace-binary-e2e-home-{}", std::process::id()));
    let _unused = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home)?;
    let config = home.join("difftrace.toml");
    std::fs::write(
        &config,
        "[tools]\nenabled = []\n[judges]\ncount = 1\nprofiles = [\"local\"]\n",
    )?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_difftrace"));
    cmd.current_dir(&repo)
        // A hostile ambient environment must not be able to flip the
        // exit-3 premise or raise the child's verbosity: everything
        // but PATH and HOME is scrubbed.
        .env("DIFFTRACE_JUDGE", "bogus-injected")
        .env("RUST_LOG", "trace");
    let output = scrubbed(&mut cmd, &home)
        .args([
            "review",
            "--base",
            "main",
            "--config",
            config.to_str().unwrap_or_default(),
        ])
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(3),
        "a judge that cannot start makes the round partial: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("[TRACE]"),
        "the injected RUST_LOG never reached the child"
    );
    let reviews = home.join(".difftrace").join("reviews");
    let transcripts = std::fs::read_dir(&reviews).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".log"))
            .count()
    });
    assert_eq!(transcripts, 1, "one transcript lands per local run");
    let reports = std::fs::read_dir(&reviews).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".report.json")
            })
            .count()
    });
    assert_eq!(
        reports, 1,
        "one machine-readable report lands per local run"
    );
    let state_dir = home.join(".difftrace").join("state");
    let state_file = std::fs::read_dir(&state_dir).map_or(None, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .next()
    });
    let state_path = state_file.unwrap_or_else(|| state_dir.join("missing.state.json"));
    let text = std::fs::read_to_string(&state_path)?;
    assert!(
        text.contains("\"round\": 0"),
        "the partial round writes no round advance: {text}"
    );
    let _unused = std::fs::remove_dir_all(&repo);
    let _unused = std::fs::remove_dir_all(&home);
    Ok(())
}

fn scrubbed<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    cmd.env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned()),
        )
        .env("HOME", home)
}

#[test]
fn the_hosted_review_without_a_token_exits_two_naming_it() -> Result<(), Box<dyn std::error::Error>>
{
    let home =
        std::env::temp_dir().join(format!("difftrace-binary-e2e-token-{}", std::process::id()));
    let _unused = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home)?;
    let config = home.join("difftrace.toml");
    std::fs::write(&config, "[tools]\nenabled = []\n")?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_difftrace"));
    let output = scrubbed(&mut cmd, &home)
        .args([
            "review",
            "--repo",
            "dch-labs/difftrace",
            "--pr",
            "1",
            "--actor",
            "maintainer",
            "--config",
            config.to_str().unwrap_or_default(),
        ])
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(2),
        "the missing token is an infrastructure failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("GITHUB_TOKEN"),
        "the error names the missing credential"
    );
    let _unused = std::fs::remove_dir_all(&home);
    Ok(())
}

#[test]
fn the_scrub_leaves_only_path_and_home_for_the_child() -> Result<(), std::io::Error> {
    let home = std::env::temp_dir().join(format!("difftrace-e2e-scrub-{}", std::process::id()));
    let _unused = std::fs::create_dir_all(&home);
    let mut cmd = Command::new("/usr/bin/env");
    cmd.env("DIFFTRACE_JUDGE", "bogus").env("RUST_LOG", "trace");
    let scrub = scrubbed(&mut cmd, &home);
    let output = scrub.output()?;
    let env_text = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        !env_text.contains("DIFFTRACE_JUDGE") && !env_text.contains("RUST_LOG"),
        "the injected pair is scrubbed out of the child env: {env_text}"
    );
    assert!(
        env_text.contains("PATH=") && env_text.contains("HOME="),
        "the declared minimum survives the scrub: {env_text}"
    );
    let _unused = std::fs::remove_dir_all(&home);
    Ok(())
}

#[test]
fn help_and_version_print_cleanly_and_exit_zero() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::temp_dir().join(format!(
        "difftrace-binary-e2e-help-home-{}",
        std::process::id()
    ));
    let _unused = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home)?;
    for (label, flag) in [("help", "--help"), ("version", "--version")] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_difftrace"));
        let output = scrubbed(&mut cmd, &home).arg(flag).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{label} exits zero; stdout: {stdout} stderr: {stderr}"
        );
        assert!(
            !stdout.contains("usage error") && !stderr.contains("usage error"),
            "{label} renders without the usage-error label; stdout: {stdout} stderr: {stderr}"
        );
        assert!(!stdout.is_empty(), "{label} prints its text on stdout");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_difftrace"));
    let output = scrubbed(&mut cmd, &home)
        .args(["reject", "--bogus"])
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(2),
        "a parse failure keeps exit 2: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("usage error"),
        "a parse failure keeps the label: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _unused = std::fs::remove_dir_all(&home);
    Ok(())
}
