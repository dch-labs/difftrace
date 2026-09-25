//! Pins that hermetic git children ignore inherited `GIT_*` variables.
//! This binary mutates process environment variables, so it deliberately
//! holds a single test: nothing else in this process can spawn a child
//! that inherits the hostile values.

use std::path::{Path, PathBuf};

use difftrace::gateway::PrGateway;
use difftrace::gateway::local::LocalGateway;

struct RestoredGitDir(Option<String>);

impl RestoredGitDir {
    fn engage(hostile: &Path) -> Self {
        let saved = std::env::var("GIT_DIR").ok();
        unsafe { std::env::set_var("GIT_DIR", hostile) };
        Self(saved)
    }
}

impl Drop for RestoredGitDir {
    fn drop(&mut self) {
        match self.0.as_deref() {
            Some(value) => {
                unsafe { std::env::set_var("GIT_DIR", value) };
            }
            None => {
                unsafe { std::env::remove_var("GIT_DIR") };
            }
        }
    }
}

fn scratch(name: &str) -> Result<PathBuf, std::io::Error> {
    let dir = std::env::temp_dir().join(format!(
        "difftrace-hostile-env-{name}-{}",
        std::process::id()
    ));
    let _unused = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    git(&dir, &["init", "-b", "main"])?;
    Ok(dir)
}

fn git(dir: &Path, args: &[&str]) -> Result<(), std::io::Error> {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()?;
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
fn hostile_git_env_cannot_redirect_hermetic_reads() -> Result<(), Box<dyn std::error::Error>> {
    let home = scratch("home")?;
    commit(&home, &[("src/a.rs", "fn anchored() {}\n")], "seed")?;
    let decoy = scratch("decoy")?;
    commit(&decoy, &[("decoy.rs", "fn decoy() {}\n")], "seed")?;
    let guard = RestoredGitDir::engage(&decoy.join(".git"));
    let gateway = LocalGateway::open_at(&home, "main")?;
    assert_eq!(
        gateway.read_file_at_head("src/a.rs")?.content,
        "fn anchored() {}\n",
        "reads still resolve the opened repository with GIT_DIR pointing elsewhere"
    );
    assert_eq!(
        gateway.list_files_at_head()?,
        vec!["src/a.rs".to_owned()],
        "listings never fall through to the decoy repository"
    );
    let hits = gateway.search_at_head("fn anchored", 10)?;
    assert!(
        hits.first().is_some_and(|hit| hit.file == "src/a.rs"),
        "searches stay inside the opened repository"
    );
    drop(guard);
    let _unused = std::fs::remove_dir_all(&home);
    let _unused = std::fs::remove_dir_all(&decoy);
    Ok(())
}
