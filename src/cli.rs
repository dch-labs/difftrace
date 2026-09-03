//! The command line: `difftrace review --repo owner/repo --pr N`.

use clap::Args;
use clap::Parser;
use clap::Subcommand;

use crate::github::RepoRef;

#[derive(Debug, Subcommand)]
pub enum Command {
    Review(ReviewArgs),
}

#[derive(Debug, Parser)]
#[command(
    name = "difftrace",
    version,
    about = "AI pull-request reviewer for GitHub"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[arg(long, help = "Repository as owner/repo")]
    pub repo: String,

    #[arg(long, help = "Pull request number")]
    pub pr: u64,

    #[arg(long, help = "Render the review to stdout instead of posting it")]
    pub dry_run: bool,

    #[arg(
        long,
        help = "Path to a config file (default: ~/.difftrace/config.toml)"
    )]
    pub config: Option<std::path::PathBuf>,
}

pub fn parse_repo(raw: &str) -> Result<RepoRef, String> {
    let Some((owner, repo)) = raw.split_once('/') else {
        return Err(format!("--repo must be owner/repo, got {raw:?}"));
    };
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(format!("--repo must be owner/repo, got {raw:?}"));
    }
    Ok(RepoRef {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_review_command_parses_its_arguments() -> Result<(), Box<dyn std::error::Error>> {
        let cli = Cli::try_parse_from([
            "difftrace",
            "review",
            "--repo",
            "dch-labs/difftrace",
            "--pr",
            "42",
            "--dry-run",
        ])
        .map_err(|err| err.to_string())?;
        let Command::Review(args) = cli.command;
        assert_eq!(args.repo, "dch-labs/difftrace");
        assert_eq!(args.pr, 42);
        assert!(args.dry_run);
        assert_eq!(args.config, None);
        Ok(())
    }

    #[test]
    fn a_missing_pr_is_rejected() {
        assert!(Cli::try_parse_from(["difftrace", "review", "--repo", "a/b"]).is_err());
    }

    #[test]
    fn the_version_flag_reports_the_package_version() -> Result<(), Box<dyn std::error::Error>> {
        let rendered = match Cli::try_parse_from(["difftrace", "--version"]) {
            Err(err) => err.to_string(),
            Ok(_) => return Err("--version must exit rather than parse".into()),
        };
        assert!(
            rendered.contains(env!("CARGO_PKG_VERSION")),
            "version output must carry the package version: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn repo_strings_split_into_owner_and_repo() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_repo("dch-labs/difftrace")?;
        assert_eq!(parsed.owner, "dch-labs");
        assert_eq!(parsed.repo, "difftrace");
        Ok(())
    }

    #[test]
    fn malformed_repo_strings_are_rejected() {
        assert!(parse_repo("justaname").is_err());
        assert!(parse_repo("/repo").is_err());
        assert!(parse_repo("owner/").is_err());
        assert!(parse_repo("a/b/c").is_err());
        assert!(parse_repo("").is_err());
    }
}
