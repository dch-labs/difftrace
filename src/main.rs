//! The v2 binary entry point.

use difftrace::cli::{self, RunOutcome};

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let code = match cli::parse(&args) {
        Ok(command) => {
            eprintln!(
                "difftrace {} — the {} subcommand is not yet implemented on the v2 skeleton",
                env!("CARGO_PKG_VERSION"),
                command.as_str()
            );
            RunOutcome::InfrastructureFailure.exit_code()
        }
        Err(err) => {
            eprintln!("difftrace: {}", difftrace::error::error_chain(&err));
            RunOutcome::from(err).exit_code()
        }
    };
    std::process::exit(i32::from(code));
}
