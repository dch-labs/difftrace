//! The v2 binary entry point.

use clap::Parser;
use difftrace::cli::{self, Cli, RunOutcome};

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let code = match Cli::try_parse_from(
        std::iter::once("difftrace").chain(args.iter().map(String::as_str)),
    ) {
        Ok(parsed) => match cli::dispatch(&parsed.command) {
            Ok(outcome) => outcome.exit_code(),
            Err(err) => {
                eprintln!("difftrace: {}", difftrace::error::error_chain(&err));
                RunOutcome::from(err).exit_code()
            }
        },
        Err(err) => {
            eprintln!("difftrace: {err}");
            RunOutcome::InfrastructureFailure.exit_code()
        }
    };
    std::process::exit(i32::from(code));
}
