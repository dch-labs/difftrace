//! Mutation recipes, manifest parsing, validation, and application.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::error::DtError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recipe {
    OffByOne,
    DroppedAwait,
    Unwrap,
    Leak,
    BrokenTest,
    FalseDoc,
}

impl Recipe {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OffByOne => "off-by-one",
            Self::DroppedAwait => "dropped-await",
            Self::Unwrap => "unwrap",
            Self::Leak => "leak",
            Self::BrokenTest => "broken-test",
            Self::FalseDoc => "false-doc",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub repo: String,
    pub base: String,
    pub recipe: Recipe,
    pub file: String,
    pub anchor: String,
    pub expect_title_tokens: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PinnedRange {
    pub base: String,
    pub head: String,
}

pub fn parse_manifest(text: &str) -> Result<Vec<Case>, DtError> {
    let manifest: Manifest = toml::from_str(text).map_err(|err| DtError::Mutation {
        case: "manifest".to_owned(),
        message: err.to_string(),
    })?;
    if manifest.case.is_empty() {
        return Err(DtError::Mutation {
            case: "manifest".to_owned(),
            message: "no [[case]] entries".to_owned(),
        });
    }
    for case in &manifest.case {
        if case.base.len() != 40 || !case.base.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(DtError::Mutation {
                case: case_label(case),
                message: format!("base must be a full 40-hex sha, got {}", case.base),
            });
        }
        if case.expect_title_tokens.is_empty() {
            return Err(DtError::Mutation {
                case: case_label(case),
                message: "expect_title_tokens must not be empty".to_owned(),
            });
        }
    }
    Ok(manifest.case)
}

#[derive(Debug, Deserialize)]
struct Manifest {
    case: Vec<Case>,
}

fn case_label(case: &Case) -> String {
    format!(
        "{}@{}[{}]",
        case.repo,
        case.base.get(..7).unwrap_or(case.base.as_str()),
        case.recipe.as_str()
    )
}

pub fn validate(
    case: &Case,
    show_file: impl Fn(&str, &str) -> Result<String, DtError>,
) -> Result<(), DtError> {
    let content = show_file(&case.base, &case.file).map_err(|err| DtError::Mutation {
        case: case_label(case),
        message: err.to_string(),
    })?;
    transform(case.recipe, &content, &case.anchor)
        .map(|_| ())
        .map_err(|err| DtError::Mutation {
            case: case_label(case),
            message: err.to_string(),
        })?;
    Ok(())
}

#[must_use]
pub fn anchor_key(anchor: &str) -> String {
    anchor.trim().to_owned()
}

fn locate(content: &str, anchor: &str) -> Result<usize, DtError> {
    let key = anchor_key(anchor);
    let mut hits = content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim() == key)
        .map(|(idx, _)| idx);
    let first = hits.next();
    let duplicate = hits.next().is_some();
    match (first, duplicate) {
        (Some(idx), false) => Ok(idx),
        (Some(_), true) => Err(DtError::Mutation {
            case: "anchor".to_owned(),
            message: format!("anchor matches more than one line: {key}"),
        }),
        (None, _) => Err(DtError::Mutation {
            case: "anchor".to_owned(),
            message: format!("anchor matches no line: {key}"),
        }),
    }
}

pub fn transform(recipe: Recipe, content: &str, anchor: &str) -> Result<String, DtError> {
    if content.contains('\r') {
        return Err(DtError::Mutation {
            case: "transform".to_owned(),
            message: "target file contains CR bytes — refusing to renormalize line endings"
                .to_owned(),
        });
    }
    let idx = locate(content, anchor)?;
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = Vec::new();
    for (number, line) in lines.iter().enumerate() {
        match recipe {
            Recipe::OffByOne => {
                if number == idx {
                    let trimmed = line.trim();
                    if !trimmed.contains(" < ") {
                        return Err(not_applicable(recipe, anchor));
                    }
                    out.push(line.replacen(" < ", " <= ", 1));
                } else {
                    out.push((*line).to_owned());
                }
            }
            Recipe::DroppedAwait => {
                if number == idx {
                    let trimmed = line.trim();
                    let Some(statement) = trimmed.strip_suffix(".await;") else {
                        return Err(not_applicable(recipe, anchor));
                    };
                    let indent = &line[..line.len().saturating_sub(line.trim_start().len())];
                    out.push(format!("{indent}{statement};"));
                } else {
                    out.push((*line).to_owned());
                }
            }
            Recipe::Unwrap => {
                if number == idx {
                    let trimmed = line.trim();
                    let Some(expr) = trimmed.strip_suffix("?;") else {
                        return Err(not_applicable(recipe, anchor));
                    };
                    let indent = &line[..line.len().saturating_sub(line.trim_start().len())];
                    out.push(format!("{indent}{expr}.unwrap();"));
                } else {
                    out.push((*line).to_owned());
                }
            }
            Recipe::Leak => {
                if number == idx {
                    let trimmed = line.trim();
                    if !(trimmed.starts_with("drop(") && trimmed.ends_with(");")) {
                        return Err(not_applicable(recipe, anchor));
                    }
                } else {
                    out.push((*line).to_owned());
                }
            }
            Recipe::BrokenTest => {
                if number == idx {
                    let trimmed = line.trim();
                    let Some(open) = trimmed.strip_suffix('{') else {
                        return Err(not_applicable(recipe, anchor));
                    };
                    let _ = open;
                    let indent = &line[..line.len().saturating_sub(line.trim_start().len())];
                    out.push((*line).to_owned());
                    out.push(format!(
                        "{indent}    assert!(false, \"this test is broken\");"
                    ));
                } else {
                    out.push((*line).to_owned());
                }
            }
            Recipe::FalseDoc => {
                if number == idx {
                    let trimmed = line.trim();
                    if !trimmed.starts_with("pub fn ") {
                        return Err(not_applicable(recipe, anchor));
                    }
                    let indent = &line[..line.len().saturating_sub(line.trim_start().len())];
                    out.push(format!("{indent}/// # Panics"));
                    out.push(format!(
                        "{indent}/// Panics if the input is empty — the caller must check first."
                    ));
                    out.push((*line).to_owned());
                } else {
                    out.push((*line).to_owned());
                }
            }
        }
    }
    let mut mutated = out.join("\n");
    if content.ends_with('\n') {
        mutated.push('\n');
    }
    Ok(mutated)
}

fn not_applicable(recipe: Recipe, anchor: &str) -> DtError {
    DtError::Mutation {
        case: recipe.as_str().to_owned(),
        message: format!("recipe not applicable to anchor: {anchor}"),
    }
}

pub fn show_file_at(repo_root: &Path, sha: &str, file: &str) -> Result<String, DtError> {
    git(repo_root, &["show", &format!("{sha}:{file}")])
}

pub fn apply(
    case: &Case,
    src_repo: &Path,
    work_root: &Path,
    check: bool,
) -> Result<PinnedRange, DtError> {
    let work = work_root.join(format!(
        "{}-{}",
        case.repo,
        case.base.get(..7).unwrap_or(case.base.as_str())
    ));
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(DtError::Io)?;
    }
    std::fs::create_dir_all(work_root).map_err(DtError::Io)?;
    git(
        src_repo,
        &[
            "clone",
            &src_repo.to_string_lossy(),
            &work.to_string_lossy(),
        ],
    )
    .map_err(|err| DtError::Mutation {
        case: case_label(case),
        message: err.to_string(),
    })?;
    git(&work, &["checkout", &case.base]).map_err(|err| DtError::Mutation {
        case: case_label(case),
        message: err.to_string(),
    })?;
    let path = work.join(&case.file);
    let content = std::fs::read_to_string(&path).map_err(DtError::Io)?;
    let mutated =
        transform(case.recipe, &content, &case.anchor).map_err(|err| DtError::Mutation {
            case: case_label(case),
            message: err.to_string(),
        })?;
    std::fs::write(&path, mutated).map_err(DtError::Io)?;
    git(&work, &["add", &case.file]).map_err(|err| DtError::Mutation {
        case: case_label(case),
        message: err.to_string(),
    })?;
    git(
        &work,
        &[
            "-c",
            "user.email=eval@difftrace.invalid",
            "-c",
            "user.name=dt eval",
            "commit",
            "-m",
            &format!("mutation: {}", case.recipe.as_str()),
        ],
    )
    .map_err(|err| DtError::Mutation {
        case: case_label(case),
        message: err.to_string(),
    })?;
    if check {
        let output = std::process::Command::new("cargo")
            .current_dir(&work)
            .args(["check"])
            .output()
            .map_err(|err| DtError::Mutation {
                case: case_label(case),
                message: format!("cargo spawn: {err}"),
            })?;
        if !output.status.success() {
            return Err(DtError::Mutation {
                case: case_label(case),
                message: format!(
                    "cargo check failed after mutation: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
    }
    let head = git(&work, &["rev-parse", "HEAD"])
        .map_err(|err| DtError::Mutation {
            case: case_label(case),
            message: err.to_string(),
        })?
        .trim()
        .to_owned();
    Ok(PinnedRange {
        base: case.base.clone(),
        head,
    })
}

fn git(repo: &Path, args: &[&str]) -> Result<String, DtError> {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|err| DtError::Git {
            repo: repo.to_owned(),
            message: format!("spawn: {err}"),
        })?;
    if !output.status.success() {
        return Err(DtError::Git {
            repo: repo.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[must_use]
pub fn token_set(case: &Case) -> BTreeSet<String> {
    case.expect_title_tokens
        .iter()
        .map(|token| token.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_recipe_transforms_its_pattern() {
        let off_by_one =
            "fn check() {\n    if self.window.len() < self.cap {\n        return;\n    }\n}\n";
        let mutated = transform(
            Recipe::OffByOne,
            off_by_one,
            "if self.window.len() < self.cap {",
        )
        .unwrap_or_default();
        assert!(mutated.contains("<= self.cap"), "the < flips to <=");

        let await_line =
            "async fn run() {\n    self.collect_memories(&input, &mut out).await;\n}\n";
        let mutated = transform(
            Recipe::DroppedAwait,
            await_line,
            "self.collect_memories(&input, &mut out).await;",
        )
        .unwrap_or_default();
        assert!(mutated.contains("self.collect_memories(&input, &mut out);"));
        assert!(!mutated.contains(".await"));

        let unwrap_line = "fn parse() {\n    let rest = msg.strip_prefix(\"HTTP \")?;\n}\n";
        let mutated = transform(
            Recipe::Unwrap,
            unwrap_line,
            "let rest = msg.strip_prefix(\"HTTP \")?;",
        )
        .unwrap_or_default();
        assert!(mutated.contains(".strip_prefix(\"HTTP \").unwrap();"));

        let leak_line = "fn scoped() {\n    drop(guard);\n}\n";
        let mutated = transform(Recipe::Leak, leak_line, "drop(guard);").unwrap_or_default();
        assert!(
            !mutated.contains("drop(guard);"),
            "the drop line is removed"
        );

        let test_line = "#[test]\nfn accepts_injected_http_client() {\n    let client = MockApiClient::new();\n}\n";
        let mutated = transform(
            Recipe::BrokenTest,
            test_line,
            "fn accepts_injected_http_client() {",
        )
        .unwrap_or_default();
        assert!(mutated.contains("assert!(false, \"this test is broken\");"));

        let doc_line = "pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {\n    0.0\n}\n";
        let mutated = transform(
            Recipe::FalseDoc,
            doc_line,
            "pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {",
        )
        .unwrap_or_default();
        assert!(mutated.contains("/// # Panics"));
        assert!(mutated.contains("/// Panics if the input is empty"));
    }

    #[test]
    fn a_crlf_target_is_refused_rather_than_renormalized() {
        let content = "fn run() {\r\n    drop(guard);\r\n}\r\n";
        let result = transform(Recipe::Leak, content, "drop(guard);");
        assert!(result.is_err_and(|err| err.to_string().contains("CR")));
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_termination() {
        let content = "fn scoped() {\n    drop(guard);\n}";
        let mutated = transform(Recipe::Leak, content, "drop(guard);").unwrap_or_default();
        assert!(!mutated.ends_with('\n'), "no terminator is invented");
        assert!(!mutated.contains("drop(guard);"));
    }

    #[test]
    fn a_trailing_whitespace_anchor_keeps_the_statement_intact() {
        let content = "async fn run() {\n    self.flush().await;  \n}\n";
        let mutated =
            transform(Recipe::DroppedAwait, content, "self.flush().await;").unwrap_or_default();
        assert!(
            mutated.contains("    self.flush();"),
            "leading indent survives, trailing spaces drop, statement intact: {mutated}"
        );
        assert!(!mutated.contains(".await"));
    }

    #[test]
    fn an_anchor_matching_zero_or_many_lines_is_rejected() {
        let content = "fn one() {\n    drop(guard);\n}\nfn two() {\n    drop(guard);\n}\n";
        let duplicate = transform(Recipe::Leak, content, "drop(guard);");
        assert!(duplicate.is_err(), "two matching lines are ambiguous");
        let missing = transform(Recipe::Leak, content, "drop(other);");
        assert!(missing.is_err(), "no matching line is an error");
    }

    #[test]
    fn a_recipe_not_matching_its_anchor_shape_is_rejected() {
        let content = "fn one() {\n    drop(guard);\n}\n";
        assert!(transform(Recipe::OffByOne, content, "drop(guard);").is_err());
        assert!(transform(Recipe::FalseDoc, content, "drop(guard);").is_err());
    }

    #[test]
    fn a_malformed_manifest_entry_is_rejected() {
        assert!(parse_manifest("not toml at all [[").is_err());
        assert!(
            parse_manifest("").is_err(),
            "an empty manifest has no cases"
        );
        let missing_field = "[[case]]\nrepo = \"loopctl\"\n";
        assert!(parse_manifest(missing_field).is_err());
        let bad_recipe = "[[case]]\nrepo = \"loopctl\"\nbase = \"e78f73a5c8e132b7a2103ee36549524f26897dac\"\nrecipe = \"teleport\"\nfile = \"src/api.rs\"\nanchor = \"drop(stream);\"\nexpect_title_tokens = [\"leak\"]\n";
        assert!(parse_manifest(bad_recipe).is_err());
        for base in ["e78f73a", "e78f7", ""] {
            let short_sha = format!(
                "[[case]]\nrepo = \"loopctl\"\nbase = \"{base}\"\nrecipe = \"leak\"\nfile = \"src/api.rs\"\nanchor = \"drop(stream);\"\nexpect_title_tokens = [\"leak\"]\n"
            );
            assert!(
                parse_manifest(&short_sha).is_err(),
                "base {base:?} is a typed error"
            );
        }
    }

    #[test]
    fn apply_pins_a_range_over_a_synthetic_repo() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("dt-eval-mutate-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        git(&dir, &["init", "-b", "trunk"])?;
        git(&dir, &["config", "user.email", "eval@example.com"])?;
        git(&dir, &["config", "user.name", "dt eval"])?;
        let file = dir.join("src").join("lib.rs");
        std::fs::create_dir_all(file.parent().ok_or("parent")?)?;
        std::fs::write(&file, "fn scoped() {\n    drop(guard);\n}\n")?;
        git(&dir, &["add", "."])?;
        git(&dir, &["commit", "-m", "base"])?;
        let base = git(&dir, &["rev-parse", "HEAD"])?.trim().to_owned();
        let case = Case {
            repo: "synthetic".to_owned(),
            base: base.clone(),
            recipe: Recipe::Leak,
            file: "src/lib.rs".to_owned(),
            anchor: "drop(guard);".to_owned(),
            expect_title_tokens: vec!["leak".to_owned()],
        };
        let work_root = dir.join("work");
        let range = apply(&case, &dir, &work_root, false)?;
        assert_eq!(range.base, base);
        assert_ne!(range.head, base, "the mutation is its own commit");
        let short = base.get(..7).unwrap_or(base.as_str()).to_owned();
        let work = work_root.join(format!("synthetic-{short}"));
        let diff = git(&work, &["diff", &range.base, &range.head])?;
        assert!(
            diff.lines()
                .any(|line| line.starts_with('-') && line.contains("drop(guard);")),
            "the diff removes the drop"
        );
        assert!(
            !diff
                .lines()
                .any(|line| line.starts_with('+') && line.contains("drop(guard);")),
            "the drop is not re-added"
        );
        drop_scratch(&dir);
        drop_scratch(&work);
        drop_scratch(&work_root);
        Ok(())
    }

    fn drop_scratch(dir: &Path) {
        for entry in std::fs::read_dir(dir).into_iter().flatten() {
            let Ok(entry) = entry else { continue };
            if entry.path().is_dir() {
                drop_scratch(&entry.path());
            } else {
                let _unused = std::fs::remove_file(entry.path());
            }
        }
        let _unused = std::fs::remove_dir(dir);
    }
}
