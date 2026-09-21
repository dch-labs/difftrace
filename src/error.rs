//! The v2 error model.

pub type JudgeId = String;

#[derive(Debug, thiserror::Error)]
pub enum DtError {
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("git operation failed: {0}")]
    Git(String),
    #[error("gateway operation failed: {0}")]
    Gateway(String),
    #[error("provider call failed: {0}")]
    Provider(String),
    #[error("judge {judge} did not answer: {cause}")]
    JudgeAbsent { judge: JudgeId, cause: String },
    #[error("partial round — judges absent: {absent:?}")]
    PartialRound { absent: Vec<JudgeId> },
    #[error("the oracle exhausted its budget before resolving the claim")]
    OracleBudget,
    #[error("persisted state is corrupt: {0}")]
    StateCorrupt(String),
    #[error("command line error: {0}")]
    Cli(String),
}

#[must_use]
pub fn error_chain(error: &DtError) -> String {
    let mut chain = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dt_error_has_a_professional_display() {
        let samples = [
            DtError::Config("judges profile list is empty".to_owned()),
            DtError::Git("bad revision".to_owned()),
            DtError::Gateway("thread post failed".to_owned()),
            DtError::Provider("429 after every retry".to_owned()),
            DtError::JudgeAbsent {
                judge: "zai:glm-5.3".to_owned(),
                cause: "stream timed out".to_owned(),
            },
            DtError::PartialRound {
                absent: vec!["local".to_owned()],
            },
            DtError::OracleBudget,
            DtError::StateCorrupt("marker JSON truncated".to_owned()),
            DtError::Cli("unknown subcommand".to_owned()),
        ];
        for error in &samples {
            let text = error_chain(error);
            assert!(
                text.len() > 12 && !text.contains("DtError") && !text.starts_with(' '),
                "every variant renders as a professional sentence: {text}"
            );
        }
    }
}
