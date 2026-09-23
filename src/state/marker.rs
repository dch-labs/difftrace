//! Marker backend: state as JSON inside an HTML comment in the verdict body.

use crate::contracts::RoundState;
use crate::error::DtError;

const MARKER_START: &str = "<!-- difftrace:state ";
const MARKER_END: &str = " -->";

pub fn embed(body: &str, state: &RoundState) -> Result<String, DtError> {
    let block = marker_block(state)?;
    if let Ok(Some((start, _))) = tail_state(body) {
        let end = body.trim_end().len();
        let mut out = String::with_capacity(body.len().saturating_add(block.len()));
        if let Some(kept) = body.get(..start) {
            out.push_str(kept);
        }
        out.push_str(&block);
        if let Some(kept) = body.get(end..) {
            out.push_str(kept);
        }
        return Ok(out);
    }
    if body.is_empty() {
        Ok(format!("{block}\n"))
    } else {
        Ok(format!("{body}\n\n{block}\n"))
    }
}

pub fn extract(body: &str) -> Result<Option<RoundState>, DtError> {
    match tail_state(body)? {
        Some((_start, state)) => Ok(Some(state)),
        None => Ok(None),
    }
}

fn marker_block(state: &RoundState) -> Result<String, DtError> {
    let json = serde_json::to_string(state)
        .map_err(|err| DtError::StateCorrupt(format!("state cannot be serialized: {err}")))?;
    Ok(format!(
        "{MARKER_START}{}{MARKER_END}",
        json.replace('>', "\\u003e")
    ))
}

fn tail_state(body: &str) -> Result<Option<(usize, RoundState)>, DtError> {
    let trimmed = body.trim_end();
    let end = trimmed.len();
    if !trimmed.ends_with(MARKER_END) {
        return Ok(None);
    }
    let mut shaped = false;
    for (start, _) in trimmed.rmatch_indices(MARKER_START) {
        let json_start = start.saturating_add(MARKER_START.len());
        let json_end = end.saturating_sub(MARKER_END.len());
        if json_start >= json_end {
            continue;
        }
        let Some(json) = trimmed.get(json_start..json_end) else {
            continue;
        };
        shaped = true;
        if let Ok(state) = serde_json::from_str::<RoundState>(json) {
            return Ok(Some((start, state)));
        }
    }
    if shaped {
        Err(DtError::StateCorrupt(
            "marker state is corrupt: the tail block does not parse".to_owned(),
        ))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Issue, IssueIdentity, IssueStatus};
    use crate::state::file;

    fn sample(title: &str) -> RoundState {
        RoundState {
            checkout: "work".to_owned(),
            round: 2,
            issues: vec![Issue {
                identity: IssueIdentity {
                    file: "src/a.rs".to_owned(),
                    line: 10,
                    title_tokens: vec![title.to_owned()],
                },
                first_seen: "s1".to_owned(),
                last_seen: "s2".to_owned(),
                status: IssueStatus::Open,
                misses: 0,
                votes_seen: vec![(1, 2)],
            }],
            suppressions: Vec::new(),
        }
    }

    #[test]
    fn extract_from_a_body_without_a_block_is_none() -> Result<(), DtError> {
        assert_eq!(extract("## Verdict\n\nclean round")?, None);
        Ok(())
    }

    #[test]
    fn both_serializers_round_trip() -> Result<(), DtError> {
        let state = sample("off by one");
        let body = embed("## Verdict\n\nclean round", &state)?;
        assert_eq!(extract(&body)?, Some(state.clone()));
        let dir =
            std::env::temp_dir().join(format!("difftrace-state-marker-{}", std::process::id()));
        let path = dir.join("work.state.json");
        file::save(&path, &state)?;
        let back = file::load(&path, "work")?;
        let _unused = std::fs::remove_dir_all(&dir);
        assert_eq!(back, state);
        Ok(())
    }

    #[test]
    fn a_greater_than_in_the_state_survives_the_marker() -> Result<(), DtError> {
        let state = sample("if a > b the branch is wrong");
        let body = embed("## Verdict", &state)?;
        let start = body
            .find(MARKER_START)
            .ok_or_else(|| DtError::StateCorrupt("marker missing".to_owned()))?;
        let end = body
            .rfind(MARKER_END)
            .ok_or_else(|| DtError::StateCorrupt("marker missing".to_owned()))?;
        let json = body
            .get(start.saturating_add(MARKER_START.len())..end)
            .unwrap_or("");
        assert!(
            !json.contains('>'),
            "every greater-than is escaped so state can never close the host comment: {json}"
        );
        assert!(json.contains("\\u003e"));
        assert_eq!(extract(&body)?, Some(state));
        Ok(())
    }

    #[test]
    fn embed_replaces_an_existing_state_block() -> Result<(), DtError> {
        let first = embed("## Verdict", &sample("first issue"))?;
        let second = embed(&first, &sample("second issue"))?;
        assert_eq!(
            second.matches(MARKER_START).count(),
            1,
            "re-embedding swaps the block instead of stacking a second one"
        );
        assert_eq!(extract(&second)?, Some(sample("second issue")));
        assert!(second.starts_with("## Verdict"));
        Ok(())
    }

    #[test]
    fn a_body_quoting_the_marker_prefix_survives_embedding() -> Result<(), DtError> {
        let body =
            "## Verdict\n\nthe format `<!-- difftrace:state {json} -->` is quoted for reference";
        let embedded = embed(body, &sample("off by one"))?;
        assert!(
            embedded.contains(body),
            "body text is preserved byte-for-byte"
        );
        assert_eq!(extract(&embedded)?, Some(sample("off by one")));
        let reembedded = embed(&embedded, &sample("second issue"))?;
        assert_eq!(
            reembedded.matches(MARKER_START).count(),
            2,
            "re-embedding swaps the tail block; the quoted prefix stays body text"
        );
        assert!(reembedded.contains(body));
        assert_eq!(extract(&reembedded)?, Some(sample("second issue")));
        Ok(())
    }

    #[test]
    fn an_unparseable_tail_block_is_corrupt_not_absent() -> Result<(), DtError> {
        let outcome = extract("<!-- difftrace:state not json -->");
        assert!(
            outcome.is_err_and(|err| matches!(err, DtError::StateCorrupt(_))),
            "a tail-shaped block that does not parse is corruption, not a clean absence"
        );
        assert_eq!(
            extract("<!-- difftrace:state -->")?,
            None,
            "a literal empty-payload quote is body text, not a state block"
        );
        Ok(())
    }
}
