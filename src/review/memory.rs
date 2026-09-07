//! Cross-review memory: a small, pruned file of learnings loaded into
//! the reviewer's rubric and appended after each posted review. In CI
//! the path is backed by the workflow's actions/cache entry; locally it
//! is an ordinary file.

use std::path::Path;

const MEMORY_CAP: usize = 16_384;

#[must_use]
pub fn load(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    let total = content.chars().count();
    let skip = total.saturating_sub(MEMORY_CAP);
    Some(content.chars().skip(skip).collect())
}

pub fn append(path: &Path, section: &str) -> std::io::Result<()> {
    let existing = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => {
                let quarantine = path.with_extension("corrupt");
                std::fs::write(&quarantine, err.into_bytes())?;
                String::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let pruned = prune(&format!("{existing}{section}"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, pruned)
}

#[must_use]
fn prune(content: &str) -> String {
    if content.chars().count() <= MEMORY_CAP {
        return content.to_owned();
    }
    let chunks: Vec<&str> = content.split("## ").collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut total = 0usize;
    for chunk in chunks.iter().rev() {
        let len = chunk.chars().count();
        let would = total.saturating_add(len);
        if !kept.is_empty() && would > MEMORY_CAP {
            break;
        }
        kept.push(chunk);
        total = would;
    }
    kept.reverse();
    kept.join("## ")
}

#[must_use]
pub fn learning_section(
    head_sha: &str,
    raised: &[String],
    fixed: &[String],
    unreviewed: &[String],
) -> String {
    let short = crate::review::registry::short_sha(head_sha);
    let state = if !unreviewed.is_empty() {
        "incomplete".to_owned()
    } else if raised.is_empty() {
        "clean".to_owned()
    } else {
        format!("{} raised", raised.len())
    };
    format!(
        "\n## review {short} ({state})\n- raised: {}\n- fixed: {}\n- unreviewed: {}\n",
        raised.join("; "),
        fixed.join("; "),
        unreviewed.join("; "),
    )
}

#[must_use]
pub fn errored_section(head_sha: &str) -> String {
    let short = crate::review::registry::short_sha(head_sha);
    format!(
        "\n## review {short} (errored)\n- the round errored; whether a review posted is not recorded\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("difftrace-memory-{}-{name}", std::process::id()))
    }

    #[test]
    fn load_returns_none_for_a_missing_or_empty_file() -> Result<(), Box<dyn std::error::Error>> {
        let path = scratch("load-missing");
        assert!(load(&path).is_none());
        std::fs::write(&path, "   \n")?;
        assert!(load(&path).is_none());
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn the_errored_section_names_the_short_sha_without_claiming_a_posting() {
        let section = errored_section("9f3b2c1full-sha");
        assert!(section.contains("(errored)"));
        assert!(section.contains("9f3b2c1"));
        assert!(section.contains("not recorded"));
        assert!(
            !section.contains("clean"),
            "an errored round is never clean"
        );
    }

    #[test]
    fn append_prunes_and_keeps_the_newest_sections() -> Result<(), Box<dyn std::error::Error>> {
        let path = scratch("append-prune");
        std::fs::remove_file(&path).ok();
        let body = "x".repeat(MEMORY_CAP / 4);
        for name in [
            "review aaa",
            "review bbb",
            "review ccc",
            "review ddd",
            "review eee",
        ] {
            append(&path, &format!("## {name}\n{body}\n"))?;
        }
        let stored = load(&path).ok_or("expected memory")?;
        assert!(
            !stored.contains("review aaa") && !stored.contains("review bbb"),
            "the oldest sections are pruned once the cap is exceeded"
        );
        assert!(stored.contains("review ccc"));
        assert!(stored.contains("review ddd"));
        assert!(stored.contains("review eee"));
        assert!(stored.chars().count() <= MEMORY_CAP + 64);
        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn a_learning_section_names_the_commit_state_and_titles() {
        let section = learning_section(
            "ebbbcbbda7580a1eb00119b01b8e3baecf4c1a5c",
            &["Lost signal".to_owned()],
            &["Old finding".to_owned()],
            &[],
        );
        assert!(section.contains("## review ebbb"));
        assert!(section.contains("1 raised"));
        assert!(section.contains("raised: Lost signal"));
        assert!(section.contains("fixed: Old finding"));
        let clean = learning_section("9f3b2c1", &[], &[], &[]);
        assert!(clean.contains("(clean)"));
        let struck = learning_section(
            "ebbbcbbda7580a1eb00119b01b8e3baecf4c1a5c",
            &["Struck finding".to_owned()],
            &[],
            &[],
        );
        assert!(
            struck.contains("1 raised") && !struck.contains("(clean)"),
            "verifier-struck findings count as raised, never as clean"
        );
        let incomplete = learning_section(
            "ebbbcbbda7580a1eb00119b01b8e3baecf4c1a5c",
            &[],
            &[],
            &["src/main.rs".to_owned()],
        );
        assert!(
            incomplete.contains("(incomplete)") && incomplete.contains("unreviewed: src/main.rs"),
            "a round with unreviewed files is never recorded as clean"
        );
    }

    #[test]
    fn append_quarantines_a_corrupt_memory_file_instead_of_overwriting()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = scratch("quarantine");
        let corrupt: Vec<u8> = vec![0xC3, 0x28, b'#', b'#', b' ', b'r', b'a'];
        std::fs::write(&path, &corrupt)?;
        append(
            &path,
            "## review aaa
- raised: fresh
",
        )?;
        let sidecar = path.with_extension("corrupt");
        assert_eq!(
            std::fs::read(&sidecar)?,
            corrupt,
            "the corrupt bytes are preserved beside the file"
        );
        let stored = std::fs::read_to_string(&path)?;
        assert!(stored.contains("review aaa"));
        std::fs::remove_file(&path)?;
        std::fs::remove_file(&sidecar)?;
        Ok(())
    }
}
