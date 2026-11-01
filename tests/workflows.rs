//! Structural pins over the release train: the reusable review
//! workflow's contract, the moving-tag release workflow, the caller
//! template's pin, and the local release script's check mode.

use std::path::Path;
use std::process::Command;

fn read(relative: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    Ok(std::fs::read_to_string(path)?)
}

#[test]
fn the_reusable_review_workflow_declares_the_rewrite_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let text = read(".github/workflows/review.yml")?;
    assert!(text.contains("workflow_call:"), "the workflow is reusable");
    for input in ["pr:", "actor:"] {
        assert!(text.contains(input), "input {input} is declared");
    }
    assert!(
        !text.contains("model:") && !text.contains("DIFFTRACE_MODEL"),
        "no dead model knob rides the rewrite workflow"
    );
    for secret in [
        "ZAI_API_KEY:",
        "DIFFTRACE_APP_ID:",
        "DIFFTRACE_APP_PRIVATE_KEY:",
    ] {
        assert!(text.contains(secret), "secret {secret} is required");
    }
    assert!(
        text.contains("--repo") && text.contains("--pr") && text.contains("--actor"),
        "the invocation carries the hosted review flags"
    );
    assert!(
        text.contains("GITHUB_TOKEN:") && text.contains("ZAI_API_KEY:"),
        "the binary receives the app token and the provider key"
    );
    assert!(
        text.contains("gh release download v0.12"),
        "the binary installs from the moving tag"
    );
    assert!(
        text.contains("fork pull requests are not reviewed"),
        "fork heads fail closed"
    );
    assert!(
        text.contains("false) ;;"),
        "the fork guard accepts only false"
    );
    assert!(
        text.contains("*) echo \"::error::fork pull requests are not reviewed"),
        "an empty or missing fork flag rejects instead of passing: {text}"
    );
    assert!(
        !text.contains("if: github.event_name == 'pull_request'"),
        "the fork guard is never scoped to pull_request events only"
    );
    assert!(
        text.contains("headRepositoryOwner"),
        "comment events resolve the pull request and compare owners"
    );
    let guard_at = text
        .find("Reject fork pull requests")
        .ok_or("the guard step exists")?;
    let checkout_at = text
        .find("Check out the pull request head")
        .ok_or("checkout")?;
    let token_at = text.find("Mint the difftrace app token").ok_or("token")?;
    assert!(
        token_at < guard_at && guard_at < checkout_at,
        "token mint, fork guard, then checkout — the guard runs before any external code is read"
    );
    assert!(
        text.contains("endsWith(github.event.comment.user.login, '[bot]')"),
        "bot comments never trigger"
    );
    assert!(
        text.contains("startsWith(github.event.comment.body, '/review')"),
        "only first-line /review commands trigger"
    );
    for mapping in ["PR: ${{", "ACTOR: ${{", "EVENT_NAME: ${{", "HEAD_FORK: ${{"] {
        assert!(
            text.contains(mapping),
            "caller-supplied values reach shell through env mappings, not source"
        );
    }
    for inline in ["PR=\"${{", "ACTOR=\"${{", "FORK=\"${{"] {
        assert!(
            !text.contains(inline),
            "no template expansion is spliced into shell source"
        );
    }
    assert!(
        text.contains("mkdir -p ~/.local/bin") && text.contains(">> \"$GITHUB_PATH\""),
        "the install directory exists and lands on PATH"
    );
    assert!(
        text.contains("sha256sum -c"),
        "the downloaded asset is verified against its checksum before install"
    );
    Ok(())
}

#[test]
fn the_release_workflow_moves_the_moving_tag_in_ci() -> Result<(), Box<dyn std::error::Error>> {
    let text = read(".github/workflows/release.yml")?;
    assert!(
        text.contains("tags: ['v0.12.*']"),
        "every patch release fires the train"
    );
    assert!(
        text.contains("git tag -f") && text.contains("--force"),
        "the moving tag is force-moved by CI"
    );
    assert!(
        text.contains("cut -d. -f1,2"),
        "the moving tag derives from the exact tag"
    );
    assert!(
        text.contains("MOVING=\"$(echo \"$GITHUB_REF_NAME\" | cut -d. -f1,2)\""),
        "the ref already starts with v — no second v is prepended: {text}"
    );
    assert!(
        !text.contains("MOVING=\"v$("),
        "the double-v moving tag is gone"
    );
    assert!(
        text.contains("concurrency:") && text.contains("cancel-in-progress: false"),
        "patch releases serialize through one train lane instead of racing the tag"
    );
    assert!(
        text.contains("ls-remote") && text.contains("owns the moving tag"),
        "an older release checks the newest patch before moving and never rewinds the train"
    );
    assert!(
        text.contains("\"$ASSET\" \"$ASSET.sha256\""),
        "every release carries the tarball and its sha256 sibling"
    );
    assert!(
        text.contains("gh release delete") && text.contains("gh release create \"$MOVING\""),
        "the moving tag owns a release object, republished at the moved tag"
    );
    assert!(
        !has_hardcoded_digest(&text),
        "no local checksum pin surgery — digests ship as release assets, never in-file"
    );
    let binaries = read(".github/workflows/binaries.yml")?;
    assert!(
        !binaries.contains("tags:"),
        "nightly builds only — tags belong to the release train"
    );
    Ok(())
}

fn has_hardcoded_digest(text: &str) -> bool {
    let mut run: u32 = 0;
    for ch in text.chars() {
        if ch.is_ascii_hexdigit() {
            run = run.saturating_add(1);
            if run >= 64 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

#[test]
fn the_caller_template_pins_the_moving_tag_with_the_bump_comment()
-> Result<(), Box<dyn std::error::Error>> {
    let text = read("examples/difftrace-review.yml")?;
    assert!(
        text.contains("review.yml@v0.12 # bump major/minor only"),
        "the caller pins the moving tag with the bump rule: {text}"
    );
    assert!(
        !text.contains("releases/download"),
        "no caller-side binary pins — the callee installs from the tag"
    );
    assert!(
        text.contains("github.event.issue.pull_request != null"),
        "plain issue comments never reach the callee as pull request numbers"
    );
    Ok(())
}

#[test]
fn release_sh_pushes_the_branch_and_tag_atomically() -> Result<(), Box<dyn std::error::Error>> {
    let text = read("release.sh")?;
    assert!(
        text.contains("git push --atomic origin HEAD \"$TAG\""),
        "a rejected branch push can never leave the tag pushed alone: {text}"
    );
    Ok(())
}

#[test]
fn release_check_prints_the_plan_and_touches_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("difftrace-release-check-{}", std::process::id()));
    let _unused = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let initialized = Command::new("git")
        .current_dir(&root)
        .args(["init", "-q", "-b", "main"])
        .output()?;
    assert!(initialized.status.success());
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.12.3\"\n",
    )?;
    std::fs::write(
        root.join("CHANGELOG.md"),
        "## [0.12.3] - today\n- shipped\n",
    )?;
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("release.sh"),
        root.join("release.sh"),
    )?;
    let committed = Command::new("git")
        .current_dir(&root)
        .args(["-c", "user.email=r@e", "-c", "user.name=r", "add", "-A"])
        .output()?;
    assert!(committed.status.success());
    let committed = Command::new("git")
        .current_dir(&root)
        .args([
            "-c",
            "user.email=r@e",
            "-c",
            "user.name=r",
            "commit",
            "-qm",
            "init",
        ])
        .output()?;
    assert!(committed.status.success());
    let output = Command::new("bash")
        .current_dir(&root)
        .arg("release.sh")
        .arg("--check")
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "--check exits zero on a valid plan: {text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("0.12.3") && text.contains("v0.12.3"));
    assert!(
        text.contains("moving tag:  v0.12"),
        "the plan names the moving tag: {text}"
    );
    assert!(
        text.contains("nothing was committed, tagged, or pushed."),
        "the check states it touched nothing"
    );
    let tags = Command::new("git")
        .current_dir(&root)
        .args(["tag"])
        .output()?;
    assert!(
        String::from_utf8_lossy(&tags.stdout).trim().is_empty(),
        "no tag was created"
    );
    let dirty = Command::new("git")
        .current_dir(&root)
        .args(["status", "--porcelain"])
        .output()?;
    assert_eq!(
        String::from_utf8_lossy(&dirty.stdout),
        "",
        "the tree is unchanged"
    );
    let _unused = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn release_check_fails_without_the_changelog_section() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "difftrace-release-check-missing-{}",
        std::process::id()
    ));
    let _unused = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let initialized = Command::new("git")
        .current_dir(&root)
        .args(["init", "-q", "-b", "main"])
        .output()?;
    assert!(initialized.status.success());
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.12.3\"\n",
    )?;
    std::fs::write(
        root.join("CHANGELOG.md"),
        "## [Unreleased]\n- nothing yet\n",
    )?;
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("release.sh"),
        root.join("release.sh"),
    )?;
    let staged = Command::new("git")
        .current_dir(&root)
        .args(["-c", "user.email=r@e", "-c", "user.name=r", "add", "-A"])
        .output()?;
    assert!(staged.status.success());
    let committed = Command::new("git")
        .current_dir(&root)
        .args([
            "-c",
            "user.email=r@e",
            "-c",
            "user.name=r",
            "commit",
            "-qm",
            "init",
        ])
        .output()?;
    assert!(committed.status.success());
    let output = Command::new("bash")
        .current_dir(&root)
        .arg("release.sh")
        .arg("--check")
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "a missing changelog section refuses the release: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _unused = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn the_moving_tag_push_authenticates_with_a_token_the_step_actually_sets()
-> Result<(), Box<dyn std::error::Error>> {
    let text = read(".github/workflows/release.yml")?;
    let step = text
        .split("\n      - ")
        .find(|step| step.contains("x-access-token:"))
        .ok_or("the moving-tag push must build an authenticated REMOTE url")?;
    let name = step
        .lines()
        .find(|line| line.contains("x-access-token:"))
        .and_then(|remote| remote.split("x-access-token:${").nth(1))
        .and_then(|rest| rest.split('}').next())
        .ok_or("the REMOTE url names its token variable")?;
    assert!(
        step.contains(&format!("{name}: ${{{{ secrets.GITHUB_TOKEN }}}}")),
        "the {name} variable used in REMOTE must be set in this step's env block"
    );
    assert!(
        !step.contains("x-access-token:${GITHUB_TOKEN}"),
        "GITHUB_TOKEN is never exported to the shell — the push would send an empty token"
    );
    Ok(())
}
