//! File backend: one JSON state file per checkout, written atomically.

use std::path::{Path, PathBuf};

use crate::contracts::RoundState;
use crate::error::DtError;

#[must_use]
pub fn state_path(dir: &Path, checkout: &str) -> PathBuf {
    let sanitized: String = checkout
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let label = if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "_".to_owned()
    } else {
        sanitized
    };
    dir.join(format!("{label}.state.json"))
}

pub fn load(path: &Path, checkout: &str) -> Result<RoundState, DtError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let state: RoundState = serde_json::from_str(&text)
                .map_err(|err| DtError::StateCorrupt(format!("{}: {err}", path.display())))?;
            if state.checkout != checkout {
                return Err(DtError::StateCorrupt(format!(
                    "{} holds state for checkout {:?}, not {checkout:?}",
                    path.display(),
                    state.checkout
                )));
            }
            Ok(state)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(fresh(checkout)),
        Err(err) => Err(DtError::StateCorrupt(format!(
            "could not read {}: {err}",
            path.display()
        ))),
    }
}

pub fn save(path: &Path, state: &RoundState) -> Result<(), DtError> {
    let json = serde_json::to_string_pretty(state)
        .map_err(|err| DtError::StateCorrupt(format!("state cannot be serialized: {err}")))?;
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            DtError::StateCorrupt(format!("could not create {}: {err}", parent.display()))
        })?;
    }
    std::fs::write(&tmp, json).map_err(|err| {
        DtError::StateCorrupt(format!("could not write {}: {err}", tmp.display()))
    })?;
    std::fs::rename(&tmp, path).map_err(|err| {
        DtError::StateCorrupt(format!("could not publish {}: {err}", path.display()))
    })
}

fn fresh(checkout: &str) -> RoundState {
    RoundState {
        checkout: checkout.to_owned(),
        round: 0,
        issues: Vec::new(),
        suppressions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Issue, IssueIdentity, IssueStatus};

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "difftrace-state-file-{}-{name}",
            std::process::id()
        ))
    }

    fn sample() -> RoundState {
        RoundState {
            checkout: "work".to_owned(),
            round: 4,
            issues: vec![Issue {
                identity: IssueIdentity {
                    file: "src/a.rs".to_owned(),
                    line: 10,
                    title_tokens: vec!["off".to_owned(), "one".to_owned()],
                },
                first_seen: "s1".to_owned(),
                last_seen: "s4".to_owned(),
                status: IssueStatus::Open,
                misses: 1,
                votes_seen: vec![(1, 2), (2, 3)],
            }],
            suppressions: Vec::new(),
        }
    }

    #[test]
    fn file_backend_round_trips() -> Result<(), DtError> {
        let dir = scratch("round-trip");
        let path = state_path(&dir, "work");
        save(&path, &sample())?;
        let back = load(&path, "work")?;
        let _unused = std::fs::remove_dir_all(&dir);
        assert_eq!(back, sample());
        Ok(())
    }

    #[test]
    fn missing_file_loads_a_default() -> Result<(), DtError> {
        let path = scratch("missing").join("absent.state.json");
        let state = load(&path, "brand-new")?;
        assert_eq!(state.checkout, "brand-new");
        assert_eq!(state.round, 0);
        assert!(state.issues.is_empty());
        Ok(())
    }

    #[test]
    fn corrupt_state_errors_as_state_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let dir = scratch("corrupt");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("work.state.json");
        std::fs::write(&path, "{ not json")?;
        let outcome = load(&path, "work");
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(matches!(outcome, Err(DtError::StateCorrupt(_))));
        Ok(())
    }

    #[test]
    fn save_creates_parent_directories() -> Result<(), DtError> {
        let dir = scratch("parents");
        let path = dir.join("nested/deep/work.state.json");
        save(&path, &sample())?;
        let back = load(&path, "work")?;
        let _unused = std::fs::remove_dir_all(&dir);
        assert_eq!(back, sample());
        Ok(())
    }

    #[test]
    fn a_hostile_checkout_label_cannot_escape_the_state_dir() {
        let base = Path::new("/tmp/difftrace-state");
        let escaped = state_path(base, "../../etc/passwd");
        assert_eq!(
            escaped.parent(),
            Some(base),
            "path separators and dots become one flat file name"
        );
        assert_eq!(state_path(base, ".."), base.join("_.state.json"));
        assert_eq!(state_path(base, ""), base.join("_.state.json"));
    }

    #[test]
    fn a_state_file_from_another_checkout_errors() -> Result<(), DtError> {
        let dir = scratch("mismatch");
        let path = state_path(&dir, "work");
        save(&path, &sample())?;
        let outcome = load(&path, "different");
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(outcome, Err(DtError::StateCorrupt(_))),
            "state saved for one checkout must not load under another label"
        );
        Ok(())
    }

    #[test]
    fn colliding_checkout_labels_error_instead_of_bleeding_state() -> Result<(), DtError> {
        let dir = scratch("collision");
        let path = state_path(&dir, "feature/x");
        let mut state = sample();
        state.checkout = "feature/x".to_owned();
        save(&path, &state)?;
        let outcome = load(&state_path(&dir, "feature_x"), "feature_x");
        let _unused = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(outcome, Err(DtError::StateCorrupt(_))),
            "sanitization collisions surface as a checkout mismatch, never as foreign state"
        );
        Ok(())
    }

    #[test]
    fn legacy_state_without_misses_parses() -> Result<(), DtError> {
        let text = r#"{"checkout":"work","round":3,"issues":[{"identity":{"file":"src/a.rs","line":10,"title_tokens":["off","by","one"]},"first_seen":"s1","last_seen":"s2","status":{"status":"open"}}],"suppressions":[]}"#;
        let state: RoundState =
            serde_json::from_str(text).map_err(|err| DtError::StateCorrupt(err.to_string()))?;
        assert!(matches!(state.issues.first(), Some(issue)
            if issue.misses == 0 && issue.votes_seen.is_empty()));
        Ok(())
    }
}
