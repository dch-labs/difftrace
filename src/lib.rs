//! difftrace — a diff-review agent with a contractual model of
//! truth: the product of a run is a coverage matrix, findings are
//! exceptions confirmed by a judge quorum or mechanical verification,
//! and the verdict is assembled mechanically.

pub mod cli;
pub mod config;
pub mod error;
pub mod execclaims;
pub mod identity;

pub mod aspects;
pub mod contracts;
pub mod diff;
pub mod gateway;
pub mod hosted;
pub mod judges;
pub mod logging;

pub mod loopctl_shim {
    pub use loopctl::structured::ThinkingEffort;
}
pub mod oracle;
pub mod orchestrator;
pub mod ratelimit;
pub mod reconcile;
pub mod render;
pub mod reply;
pub mod state;
pub mod tools;

#[cfg(test)]
mod tests {
    #[test]
    fn every_source_file_opens_with_a_module_header() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    let opens = text
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .is_some_and(|line| line.trim_start().starts_with("//!"));
                    if !opens {
                        missing.push(
                            path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                        );
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "the canon aspect promises //! headers; these files open without one: {missing:?}"
        );
    }

    #[test]
    fn no_indented_comments_outside_test_modules() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    let stripped = crate::judges::flow::stripped_lines(&text);
                    let mut depth: i32 = 0;
                    let mut test_depths: Vec<i32> = Vec::new();
                    for (index, line) in text.lines().enumerate() {
                        let clean = stripped.get(index).map_or("", String::as_str);
                        if clean.trim_start().starts_with("#[cfg(test)]") {
                            test_depths.push(depth);
                            continue;
                        }
                        for ch in clean.chars() {
                            match ch {
                                '{' => depth = depth.saturating_add(1),
                                '}' => {
                                    depth = depth.saturating_sub(1);
                                    test_depths.retain(|open| *open < depth);
                                }
                                _ => {}
                            }
                        }
                        let in_test_module = test_depths.iter().any(|open| depth > *open);
                        let indented = line.len() > line.trim_start().len();
                        if indented && line.trim_start().starts_with("//") && !in_test_module {
                            offenders.push(format!(
                                "{}:{}",
                                path.display(),
                                index.saturating_add(1)
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "indented // comments live only inside test modules; these sit outside: {offenders:?}"
        );
    }

    #[test]
    fn no_source_file_carries_a_per_item_doc_comment() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let flagged = std::fs::read_to_string(&path)
                        .unwrap_or_default()
                        .lines()
                        .any(|line| {
                            let trimmed = line.trim_start();
                            trimmed.starts_with("///") && !trimmed.starts_with("////")
                        });
                    if flagged {
                        offenders.push(
                            path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                        );
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the minimal-docs regime bans per-item doc comments; these files carry one: {offenders:?}"
        );
    }
}
