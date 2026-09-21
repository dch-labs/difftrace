//! The v2 command surface: subcommand dispatch and exit codes.

use crate::error::DtError;

pub const SUBCOMMANDS: [&str; 6] = ["review", "reject", "suppress", "state", "reply", "eval"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Review,
    Reject,
    Suppress,
    State,
    Reply,
    Eval,
}

impl Command {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Reject => "reject",
            Self::Suppress => "suppress",
            Self::State => "state",
            Self::Reply => "reply",
            Self::Eval => "eval",
        }
    }
}

pub fn parse(args: &[String]) -> Result<Command, DtError> {
    let Some(first) = args.first() else {
        return Err(DtError::Cli(format!(
            "a subcommand is required: {}",
            SUBCOMMANDS.join(", ")
        )));
    };
    match first.as_str() {
        "review" => Ok(Command::Review),
        "reject" => Ok(Command::Reject),
        "suppress" => Ok(Command::Suppress),
        "state" => Ok(Command::State),
        "reply" => Ok(Command::Reply),
        "eval" => Ok(Command::Eval),
        other => Err(DtError::Cli(format!(
            "unknown subcommand {other:?}; expected one of: {}",
            SUBCOMMANDS.join(", ")
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Clean,
    BlockingFindings,
    InfrastructureFailure,
    PartialRound,
}

impl RunOutcome {
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::BlockingFindings => 1,
            Self::InfrastructureFailure => 2,
            Self::PartialRound => 3,
        }
    }
}

impl From<DtError> for RunOutcome {
    fn from(err: DtError) -> Self {
        match err {
            DtError::PartialRound { .. } => Self::PartialRound,
            _ => Self::InfrastructureFailure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg(word: &str) -> Vec<String> {
        vec![word.to_owned()]
    }

    #[test]
    fn known_subcommands_parse_and_unknown_ones_list_the_valid_set() {
        for word in SUBCOMMANDS {
            assert!(parse(&arg(word)).is_ok_and(|parsed| parsed.as_str() == word));
        }
        let err = parse(&arg("teleport"))
            .err()
            .unwrap_or(DtError::Cli(String::new()));
        assert!(err.to_string().contains("review"));
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        assert_eq!(RunOutcome::Clean.exit_code(), 0);
        assert_eq!(RunOutcome::BlockingFindings.exit_code(), 1);
        assert_eq!(RunOutcome::InfrastructureFailure.exit_code(), 2);
        assert_eq!(RunOutcome::PartialRound.exit_code(), 3);
    }
}
