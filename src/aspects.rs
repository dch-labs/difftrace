//! The aspect registry: the design's check vocabulary with judge
//! definitions and mechanical severity ceilings, the config-editable
//! default set, and per-path overrides resolved by the longest
//! matching glob.

use std::collections::BTreeMap;

use crate::config::{AspectOverride, AspectsConfig};
use crate::contracts::{AspectId, Severity};
use crate::error::DtError;

pub struct Aspect {
    pub id: AspectId,
    pub definition: String,
    pub ceiling: Severity,
}

pub struct AspectRegistry {
    vocabulary: Vec<Aspect>,
    default_set: Vec<AspectId>,
    overrides: BTreeMap<String, AspectOverride>,
}

impl AspectRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_config(config: &AspectsConfig) -> Result<Self, DtError> {
        let vocabulary = default_vocabulary();
        let known = |id: &str| vocabulary.iter().any(|aspect| aspect.id.0 == id);
        let reject_unknown = |ids: &[String], position: &str| -> Result<(), DtError> {
            let unknown = ids
                .iter()
                .filter(|id| !known(id))
                .cloned()
                .collect::<Vec<_>>();
            if unknown.is_empty() {
                Ok(())
            } else {
                Err(DtError::Config(format!(
                    "unknown aspect id(s) in {position}: {}; the vocabulary is: {}",
                    unknown.join(", "),
                    vocabulary_ids(&vocabulary).join(", ")
                )))
            }
        };
        reject_unknown(&config.default, "aspects.default")?;
        for (glob, override_entry) in &config.paths {
            if let Some(replacement) = &override_entry.replace {
                reject_unknown(replacement, &format!("aspects.paths.{glob}.replace"))?;
            }
            reject_unknown(
                &override_entry.extra,
                &format!("aspects.paths.{glob}.extra"),
            )?;
        }
        Ok(Self {
            default_set: config
                .default
                .iter()
                .map(|id| AspectId(id.clone()))
                .collect(),
            overrides: config.paths.clone(),
            vocabulary,
        })
    }

    #[must_use]
    pub fn contains(&self, aspect: &AspectId) -> bool {
        self.vocabulary.iter().any(|entry| &entry.id == aspect)
    }

    #[must_use]
    pub fn ceiling(&self, aspect: &AspectId) -> Severity {
        self.vocabulary
            .iter()
            .find(|entry| &entry.id == aspect)
            .map_or(Severity::Nitpick, |entry| entry.ceiling)
    }

    #[must_use]
    pub fn aspects_for(&self, path: &str) -> Vec<AspectId> {
        let winning = self
            .overrides
            .iter()
            .filter(|(glob, _)| glob_matches(glob, path))
            .max_by_key(|(glob, _)| (glob.len(), (*glob).to_owned()))
            .map(|(_, override_entry)| override_entry);
        let Some(override_entry) = winning else {
            return self.default_set.clone();
        };
        if let Some(replacement) = &override_entry.replace {
            return replacement.iter().map(|id| AspectId(id.clone())).collect();
        }
        let mut set = self.default_set.clone();
        for id in &override_entry.extra {
            let aspect = AspectId(id.clone());
            if !set.contains(&aspect) {
                set.push(aspect);
            }
        }
        set
    }

    #[must_use]
    pub fn definitions_block(&self, ids: &[AspectId]) -> String {
        let lines = ids.iter().filter_map(|id| {
            self.vocabulary
                .iter()
                .find(|entry| &entry.id == id)
                .map(|aspect| format!("- {}: {}\n", aspect.id.0, aspect.definition))
        });
        std::iter::once("Aspects to check, one verdict each:\n".to_owned())
            .chain(lines)
            .collect()
    }
}

impl Default for AspectRegistry {
    fn default() -> Self {
        Self {
            default_set: AspectsConfig::default()
                .default
                .iter()
                .map(|id| AspectId(id.clone()))
                .collect(),
            overrides: BTreeMap::new(),
            vocabulary: default_vocabulary(),
        }
    }
}

fn default_vocabulary() -> Vec<Aspect> {
    let entry = |id: &str, definition: &str, ceiling: Severity| Aspect {
        id: AspectId(id.to_owned()),
        definition: definition.to_owned(),
        ceiling,
    };
    vec![
        entry(
            "error-paths",
            "error propagation, failure paths, Result/Option contracts",
            Severity::Critical,
        ),
        entry(
            "lifecycle",
            "resources/tasks/listeners: creation, drop, recreation, events in handoff gaps",
            Severity::Critical,
        ),
        entry(
            "concurrency",
            "races, ordering, cancellation, stop idempotency",
            Severity::Critical,
        ),
        entry(
            "security",
            "injection, authorization, secrets, unbounded external input",
            Severity::Critical,
        ),
        entry(
            "api-contract",
            "doc/schema/wire diverging from behavior; contracts a caller builds on",
            Severity::Critical,
        ),
        entry(
            "tests",
            "coverage of changed behavior; broken fixtures; tests that kill processes",
            Severity::Warning,
        ),
        entry(
            "fixtures",
            "(tests/**) goldens correctness, names, determinism",
            Severity::Warning,
        ),
        entry(
            "docs",
            "(*.md) accuracy of behavioral claims",
            Severity::Suggestion,
        ),
        entry(
            "canon",
            "(*.rs, *.md) house canon: every source file opens with a short //! module header; shipped artifacts carry no references to planning docs the repo does not contain; a doc's promised behavior matches the code that ships",
            Severity::Suggestion,
        ),
    ]
}

fn vocabulary_ids(vocabulary: &[Aspect]) -> Vec<&str> {
    vocabulary
        .iter()
        .map(|aspect| aspect.id.0.as_str())
        .collect()
}

#[must_use]
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern_parts = pattern.split('/').collect::<Vec<_>>();
    let path_parts = path.split('/').collect::<Vec<_>>();
    match_segments(&pattern_parts, &path_parts)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| {
            path.get(skip..)
                .is_some_and(|tail| match_segments(rest, tail))
        }),
        Some((segment, rest)) => path.split_first().is_some_and(|(&part, path_rest)| {
            segment_matches(segment, part) && match_segments(rest, path_rest)
        }),
    }
}

fn segment_matches(pattern: &str, text: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix('*') {
        if segment_matches(rest, text) {
            return true;
        }
        return !text.is_empty() && segment_matches(pattern, tail(text));
    }
    match (pattern.chars().next(), text.chars().next()) {
        (Some(expected), Some(actual)) if expected == actual => {
            segment_matches(tail(pattern), tail(text))
        }
        (None, None) => true,
        _ => false,
    }
}

fn tail(text: &str) -> &str {
    match text.char_indices().nth(1) {
        Some((index, _)) => text.split_at(index).1,
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_from(toml: &str) -> Result<AspectRegistry, DtError> {
        let config = crate::config::DifftraceConfig::from_toml_str(toml)?;
        AspectRegistry::from_config(&config.aspects)
    }

    fn ids(values: &[&str]) -> Vec<AspectId> {
        values.iter().map(|id| AspectId((*id).to_owned())).collect()
    }

    #[test]
    fn the_docs_aspect_can_never_emit_a_warning() {
        let registry = AspectRegistry::new();
        for aspect in [
            "error-paths",
            "lifecycle",
            "concurrency",
            "security",
            "api-contract",
        ] {
            assert_eq!(
                registry.ceiling(&AspectId(aspect.to_owned())),
                Severity::Critical,
                "{aspect} allows critical"
            );
        }
        assert_eq!(
            registry.ceiling(&AspectId("tests".to_owned())),
            Severity::Warning
        );
        assert_eq!(
            registry.ceiling(&AspectId("fixtures".to_owned())),
            Severity::Warning
        );
        assert_eq!(
            registry.ceiling(&AspectId("docs".to_owned())),
            Severity::Suggestion,
            "the docs ceiling is code, not prompt wording"
        );
        assert!(Severity::Warning > Severity::Suggestion);
    }

    #[test]
    fn the_default_run_set_is_the_config_six() {
        let registry = AspectRegistry::new();
        assert_eq!(
            registry.aspects_for("src/main.rs"),
            ids(&[
                "error-paths",
                "lifecycle",
                "concurrency",
                "security",
                "api-contract",
                "tests",
            ])
        );
    }

    #[test]
    fn path_replace_swaps_the_whole_set_and_extra_appends() -> Result<(), DtError> {
        let registry = registry_from(
            "[aspects.paths.\"**/tests/**\"]\nextra = [\"fixtures\"]\n[aspects.paths.\"**/*.md\"]\nreplace = [\"docs\"]\n",
        )?;
        let rust = registry.aspects_for("src/lib.rs");
        assert_eq!(
            rust,
            ids(&[
                "error-paths",
                "lifecycle",
                "concurrency",
                "security",
                "api-contract",
                "tests",
            ])
        );
        let test_file = registry.aspects_for("crates/x/tests/permission.rs");
        assert_eq!(
            test_file,
            ids(&[
                "error-paths",
                "lifecycle",
                "concurrency",
                "security",
                "api-contract",
                "tests",
                "fixtures",
            ]),
            "extra appends to the default set"
        );
        let markdown = registry.aspects_for("README.md");
        assert_eq!(markdown, ids(&["docs"]), "replace swaps the whole set");
        let nested = registry.aspects_for("docs/guide/intro.md");
        assert_eq!(nested, ids(&["docs"]));
        let deduped =
            registry_from("[aspects.paths.\"**/tests/**\"]\nextra = [\"fixtures\", \"tests\"]\n")?;
        assert_eq!(
            deduped.aspects_for("tests/a.rs").len(),
            7,
            "an extra naming an existing aspect dedups"
        );
        Ok(())
    }

    #[test]
    fn longest_glob_wins() -> Result<(), DtError> {
        let registry = registry_from(
            "[aspects.paths.\"**/tests/**\"]\nextra = [\"fixtures\"]\n[aspects.paths.\"**/golden.rs\"]\nreplace = [\"fixtures\"]\n",
        )?;
        assert_eq!(
            registry.aspects_for("src/tests/golden.rs"),
            ids(&["fixtures"]),
            "the longer matching glob decides"
        );
        assert_eq!(
            registry.aspects_for("src/tests/other.rs").len(),
            7,
            "the shorter override still applies where it is the only match"
        );
        assert_eq!(
            registry.aspects_for("src/golden.rs"),
            ids(&["fixtures"]),
            "the longer glob matches on its own too"
        );
        Ok(())
    }

    #[test]
    fn unknown_aspect_ids_in_config_are_rejected() {
        let in_default = registry_from("[aspects]\ndefault = [\"error-paths\", \"vibes\"]\n");
        assert!(in_default.is_err_and(|err| err.to_string().contains("vibes")));
        let in_extra = registry_from("[aspects.paths.\"**/x\"]\nextra = [\"goldens\"]\n");
        assert!(in_extra.is_err_and(|err| err.to_string().contains("goldens")));
        let in_replace =
            registry_from("[aspects.paths.\"**/x\"]\nreplace = [\"docs\", \"tone\"]\n");
        assert!(in_replace.is_err_and(|err| err.to_string().contains("tone")));
    }

    #[test]
    fn definitions_block_lists_each_aspect_with_its_definition() {
        let registry = AspectRegistry::new();
        let set = registry.aspects_for("src/lib.rs");
        let block = registry.definitions_block(&set);
        assert!(block.starts_with("Aspects to check, one verdict each:\n"));
        assert!(
            block.contains(
                "- error-paths: error propagation, failure paths, Result/Option contracts\n"
            ),
            "definitions are verbatim from the design table: {block}"
        );
        let markdown = registry.definitions_block(&ids(&["docs"]));
        assert!(markdown.contains("- docs: (*.md) accuracy of behavioral claims\n"));
        assert!(
            markdown.lines().count() == 2,
            "one line per requested aspect"
        );
    }

    #[test]
    fn glob_double_star_spans_segments_and_single_star_does_not() {
        assert!(glob_matches("**/tests/**", "crates/x/tests/a.rs"));
        assert!(glob_matches("**/tests/**", "tests/a.rs"));
        assert!(!glob_matches("**/tests/**", "src/intests/a.rs"));
        assert!(glob_matches("**/*.md", "README.md"));
        assert!(glob_matches("**/*.md", "docs/guide/intro.md"));
        assert!(!glob_matches("*.md", "docs/guide/intro.md"));
        assert!(glob_matches("a/**/b", "a/b"));
        assert!(glob_matches("a/**/b", "a/x/y/b"));
        assert!(!glob_matches("a/**/b", "a/x/y/c"));
        assert!(!glob_matches("**/tests/**", "src/lib.rs"));
    }
}
