//! Region building per review scope over a parsed diff index.

use crate::contracts::{Region, RegionKind};
use crate::diff::DiffIndex;

const CLUSTER_GAP: u32 = 3;

#[must_use]
pub fn added_clusters(index: &DiffIndex) -> Vec<Region> {
    let mut regions = Vec::new();
    for file in index.files() {
        let added = index.added_lines(file);
        let mut cluster: Option<(u32, u32)> = None;
        for line in added {
            cluster = Some(match cluster {
                None => (line, line),
                Some((start, end)) => {
                    if line.saturating_sub(end) <= CLUSTER_GAP {
                        (start, line)
                    } else {
                        regions.push(added_region(file, start, end));
                        (line, line)
                    }
                }
            });
        }
        if let Some((start, end)) = cluster {
            regions.push(added_region(file, start, end));
        }
    }
    regions
}

fn added_region(file: &str, start: u32, end: u32) -> Region {
    Region {
        file: file.to_owned(),
        start,
        end,
        kind: RegionKind::Added,
    }
}

#[must_use]
pub fn file_spans(index: &DiffIndex) -> Vec<Region> {
    let mut regions = Vec::new();
    for file in index.files() {
        if let Some((start, end)) = index.span(file) {
            regions.push(Region {
                file: file.to_owned(),
                start,
                end,
                kind: RegionKind::File,
            });
        }
    }
    regions
}

#[must_use]
pub fn covers_all_added(index: &DiffIndex, regions: &[Region]) -> bool {
    regions
        .iter()
        .all(|region| region.kind == RegionKind::Added)
        && index.files().iter().all(|file| {
            index.added_lines(file).iter().all(|line| {
                regions.iter().any(|region| {
                    region.file == *file && *line >= region.start && *line <= region.end
                })
            })
        })
}

#[must_use]
pub fn are_disjoint(regions: &[Region]) -> bool {
    regions.iter().enumerate().all(|(i, a)| {
        regions
            .iter()
            .enumerate()
            .all(|(j, b)| i >= j || a.file != b.file || a.end < b.start || b.end < a.start)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Cite;
    use crate::diff::{DiffIndex, Scope, build_regions, repo_regions};

    const DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,5 +1,7 @@
 ctx
-old
+first
+second
 ctx
 ctx
+third
 ctx
@@ -20,3 +20,4 @@
 ctx
+fourth
 ctx
 ctx
diff --git a/gone.rs b/gone.rs
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-here
-too
";

    #[test]
    fn an_added_line_starting_with_pluses_stays_hunk_body() -> Result<(), Box<dyn std::error::Error>>
    {
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,4 @@\n ctx\n+good\n+++ oops\n+after\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(
            index.files(),
            vec!["f.rs"],
            "no phantom file from body content"
        );
        assert!(
            index.resolve(&Cite {
                file: "f.rs".to_owned(),
                line: 3
            }),
            "the real added line is pinned"
        );
        Ok(())
    }

    #[test]
    fn a_removed_line_starting_with_dashes_stays_hunk_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,2 @@\n ctx\n--- x\n ctx\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(index.files(), vec!["f.rs"]);
        Ok(())
    }

    #[test]
    fn an_under_declared_hunk_is_rejected() {
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,5 +1,5 @@\n ctx\n";
        assert!(
            DiffIndex::parse(diff).is_err(),
            "the diff ends before the hunk closes"
        );
    }

    #[test]
    fn an_over_declared_hunk_is_rejected() {
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,1 +1,1 @@\n ctx\n+dropped\n";
        assert!(
            DiffIndex::parse(diff).is_err(),
            "body lines outside the declared hunk"
        );
    }

    #[test]
    fn an_orphan_hunk_without_a_file_header_is_rejected() {
        assert!(DiffIndex::parse("@@ -1,2 +1,2 @@\n ctx\n ctx\n").is_err());
    }

    #[test]
    fn a_nested_b_path_parses_with_one_strip() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/b/src/x.rs\n+++ b/b/src/x.rs\n@@ -1,2 +1,2 @@\n ctx\n ctx\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(
            index.files(),
            vec!["b/src/x.rs"],
            "one b/ prefix goes, the real one stays"
        );
        Ok(())
    }

    #[test]
    fn an_empty_body_line_is_context() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,3 @@\n ctx\n\n ctx\n";
        let index = DiffIndex::parse(diff)?;
        assert!(index.resolve(&Cite {
            file: "f.rs".to_owned(),
            line: 2
        }));
        Ok(())
    }

    #[test]
    fn regions_are_disjoint_and_cover_the_diff_per_scope() -> Result<(), Box<dyn std::error::Error>>
    {
        let index = DiffIndex::parse(DIFF)?;
        let regions = build_regions(DIFF, Scope::Diff)?;
        assert!(are_disjoint(&regions), "added clusters never overlap");
        assert!(
            covers_all_added(&index, &regions),
            "every added line is in a region"
        );
        let deep = build_regions(DIFF, Scope::Deep)?;
        assert_eq!(deep.len(), 1, "one file changed on the new side");
        assert_eq!(
            deep.first().ok_or("region")?.kind,
            crate::contracts::RegionKind::File
        );
        assert!(build_regions(DIFF, Scope::Full).is_err());
        let repo = repo_regions(&["src/lib.rs".to_owned(), "README.md".to_owned()]);
        assert_eq!(repo.len(), 2);
        assert_eq!(
            repo.first().ok_or("region")?.kind,
            crate::contracts::RegionKind::Repo
        );
        Ok(())
    }

    #[test]
    fn added_clusters_merge_across_small_gaps_only() -> Result<(), Box<dyn std::error::Error>> {
        let regions = build_regions(DIFF, Scope::Diff)?;
        let lib: Vec<&Region> = regions
            .iter()
            .filter(|region| region.file == "src/lib.rs")
            .collect();
        let first = lib.first().ok_or("first region")?;
        let second = lib.get(1).ok_or("second region")?;
        assert_eq!(
            (first.start, first.end),
            (2, 6),
            "lines two and six merge: the gap is three context lines"
        );
        assert_eq!(
            (second.start, second.end),
            (21, 21),
            "the far hunk is its own region"
        );
        Ok(())
    }

    #[test]
    fn cite_resolution_accepts_only_pinned_lines() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        assert!(index.resolve(&Cite {
            file: "src/lib.rs".to_owned(),
            line: 2
        }));
        assert!(
            index.resolve(&Cite {
                file: "src/lib.rs".to_owned(),
                line: 5
            }),
            "context lines are pinned too"
        );
        assert!(
            !index.resolve(&Cite {
                file: "src/lib.rs".to_owned(),
                line: 9
            }),
            "a line the diff never touches"
        );
        assert!(
            !index.resolve(&Cite {
                file: "other.rs".to_owned(),
                line: 1
            }),
            "a file the diff never mentions"
        );
        assert!(
            !index.resolve(&Cite {
                file: "gone.rs".to_owned(),
                line: 1
            }),
            "the deleted file has no new side"
        );
        Ok(())
    }

    #[test]
    fn the_index_is_a_pure_function_of_the_pinned_text() -> Result<(), Box<dyn std::error::Error>> {
        let first = DiffIndex::parse(DIFF)?;
        let second = DiffIndex::parse(DIFF)?;
        assert_eq!(first, second);
        let other = DiffIndex::parse(DIFF.replace("+fourth", " fourth").as_str())?;
        assert_ne!(
            first, other,
            "a different line topology is a different index; added text content is not part of it"
        );
        assert_eq!(first.files().len(), 1);
        Ok(())
    }

    #[test]
    fn a_pure_deletion_anchors_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        assert!(index.added_lines("gone.rs").is_empty());
        assert!(index.span("gone.rs").is_none());
        let regions = build_regions(DIFF, Scope::Diff)?;
        assert!(regions.iter().all(|region| region.file != "gone.rs"));
        Ok(())
    }

    #[test]
    fn clamp_admits_only_lines_inside_the_region_span() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let regions = build_regions(DIFF, Scope::Diff)?;
        let region = regions.first().ok_or("region")?.clone();
        assert!(index.clamp_to_region(
            &Cite {
                file: "src/lib.rs".to_owned(),
                line: 2
            },
            &region
        ));
        assert!(
            !index.clamp_to_region(
                &Cite {
                    file: "src/lib.rs".to_owned(),
                    line: 22
                },
                &region
            ),
            "inside the file, outside the region"
        );
        assert!(!index.clamp_to_region(
            &Cite {
                file: "other.rs".to_owned(),
                line: 2
            },
            &region
        ));
        Ok(())
    }

    #[test]
    fn a_no_newline_marker_on_the_old_side_is_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(index.added_lines("f"), vec![1]);
        Ok(())
    }

    #[test]
    fn a_no_newline_marker_on_the_new_side_is_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(index.added_lines("f"), vec![1]);
        Ok(())
    }

    #[test]
    fn a_malformed_hunk_header_names_its_own_line() {
        let outcome = DiffIndex::parse("--- a/f\n+++ b/f\n@@ broken @@\n+body\n");
        assert!(
            matches!(&outcome, Err(err) if err.contains("line 3")),
            "the malformed header itself is named, not the body line after it: {outcome:?}"
        );
    }

    #[test]
    fn non_diff_text_rejects_instead_of_parsing_clean() {
        assert!(
            DiffIndex::parse("fatal: not a git repository\n").is_err(),
            "tool-failure text on the diff channel must not read as a clean review"
        );
    }

    #[test]
    fn an_unclosed_hunk_error_names_the_final_line() {
        let outcome = DiffIndex::parse("--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n+one\n");
        assert!(
            matches!(&outcome, Err(err) if err.contains("line 4")),
            "over-declared hunks carry a line number like every other shape error: {outcome:?}"
        );
    }

    #[test]
    fn a_deletion_only_diff_parses_as_an_empty_index() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-here\n-too\n";
        let index = DiffIndex::parse(diff)?;
        assert!(
            index.files().is_empty(),
            "nothing survived on the new side, so there is nothing to review"
        );
        Ok(())
    }

    #[test]
    fn a_rename_only_diff_parses_as_an_empty_index() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs\n";
        let index = DiffIndex::parse(diff)?;
        assert!(index.files().is_empty());
        Ok(())
    }

    #[test]
    fn a_mode_only_diff_parses_as_an_empty_index() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "diff --git a/script.sh b/script.sh\nold mode 100644\nnew mode 100755\n";
        let index = DiffIndex::parse(diff)?;
        assert!(index.files().is_empty());
        Ok(())
    }

    #[test]
    fn a_binary_only_diff_parses_as_an_empty_index() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "diff --git a/logo.png b/logo.png\nindex 1234567..89abcde 100644\nBinary files a/logo.png and b/logo.png differ\n";
        let index = DiffIndex::parse(diff)?;
        assert!(index.files().is_empty());
        Ok(())
    }
}
