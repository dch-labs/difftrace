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
pub fn split_oversized(regions: &[Region], max_lines: u32) -> Vec<Region> {
    let mut out = Vec::new();
    for region in regions {
        if region.end == u32::MAX || max_lines == 0 {
            out.push(region.clone());
            continue;
        }
        let span = region.end.saturating_sub(region.start).saturating_add(1);
        if span <= max_lines {
            out.push(region.clone());
            continue;
        }
        let mut start = region.start;
        while start <= region.end {
            let end = start
                .saturating_add(max_lines.saturating_sub(1))
                .min(region.end);
            out.push(Region {
                file: region.file.clone(),
                start,
                end,
                kind: region.kind,
            });
            start = end.saturating_add(1);
        }
    }
    out
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
    use crate::diff::{
        DiffIndex, Scope, build_regions, region_diff, repo_regions, repo_regions_bounded,
    };

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
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1 +1,4 @@\n ctx\n+good\n+++ oops\n+after\n";
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
    fn an_under_declared_new_count_errors_mid_hunk() {
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,1 @@\n ctx\n ctx\n ctx\n";
        let outcome = DiffIndex::parse(diff);
        assert!(
            matches!(&outcome, Err(err)
                if err.contains("line 5") && err.contains("beyond the declared counts")),
            "context beyond the declared new count pins nothing: {outcome:?}"
        );
    }

    #[test]
    fn an_under_declared_old_count_errors_mid_hunk() {
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,1 +1,3 @@\n ctx\n ctx\n ctx\n";
        let outcome = DiffIndex::parse(diff);
        assert!(
            matches!(&outcome, Err(err)
                if err.contains("line 5") && err.contains("beyond the declared counts")),
            "context beyond the declared old count is as malformed as the new side: {outcome:?}"
        );
    }

    #[test]
    fn a_zero_old_count_hunk_parses_added_lines() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,2 @@\n+one\n+two\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(
            index.added_lines("new.rs"),
            vec![1, 2],
            "added lines count against the new side only, so a zero old count parses"
        );
        Ok(())
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
        let same_topology = DiffIndex::parse(DIFF.replace("+first", "+renamed entirely").as_str())?;
        assert_eq!(
            first, same_topology,
            "added text content is not part of the index"
        );
        let reordered = DiffIndex::parse(DIFF.replace("+third\n ctx", " ctx\n+third").as_str())?;
        assert_ne!(
            first, reordered,
            "a different line topology is a different index"
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
    fn an_emptied_file_contributes_no_deep_scope_region() -> Result<(), Box<dyn std::error::Error>>
    {
        let diff = "--- a/kept.rs\n+++ b/kept.rs\n@@ -1,1 +1,2 @@\n ctx\n+add\n--- a/emptied.rs\n+++ b/emptied.rs\n@@ -1,2 +0,0 @@\n-gone\n-too\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(index.span("kept.rs"), Some((1, 2)));
        assert!(
            index.span("emptied.rs").is_none(),
            "a file emptied but not deleted anchors nothing a cite could resolve"
        );
        let deep = build_regions(diff, Scope::Deep)?;
        assert_eq!(
            deep.len(),
            1,
            "only the file with anchored lines becomes a region"
        );
        assert_eq!(deep.first().ok_or("region")?.file, "kept.rs");
        let diff_scope = build_regions(diff, Scope::Diff)?;
        assert!(
            diff_scope.iter().all(|region| region.file != "emptied.rs"),
            "no scope spends a judge sweep on the emptied file"
        );
        Ok(())
    }

    #[test]
    fn a_timestamped_header_keys_the_bare_path() -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/foo.rs\t2026-01-01 10:00:00 +0000\n+++ b/foo.rs\t2026-01-01 10:00:01 +0000\n@@ -1,2 +1,2 @@\n ctx\n ctx\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(
            index.files(),
            vec!["foo.rs"],
            "the timestamp never keys the file"
        );
        assert!(index.resolve(&Cite {
            file: "foo.rs".to_owned(),
            line: 1
        }));
        assert_eq!(
            region_diff(diff, "foo.rs", 1, 2, 0),
            "@@ foo.rs new-side lines 1-2 @@\n ctx\n ctx",
            "the index and the judge materials key the same file"
        );
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
    fn repo_regions_take_real_ends_from_the_diff_and_leave_untouched_files_whole()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let files = vec![
            "src/lib.rs".to_owned(),
            "README.md".to_owned(),
            "gone.rs".to_owned(),
        ];
        let regions = repo_regions_bounded(&files, &index);
        let by_file = |name: &str| {
            regions
                .iter()
                .find(|region| region.file == name)
                .cloned()
                .ok_or_else(|| Box::<dyn std::error::Error>::from(format!("region {name}")))
        };
        let changed = by_file("src/lib.rs")?;
        assert_eq!(
            (changed.start, changed.end),
            (1, 23),
            "the changed file's region ends at its last anchored diff line"
        );
        assert_eq!(
            by_file("README.md")?.end,
            u32::MAX,
            "a file the diff never touches stays whole"
        );
        assert_eq!(
            by_file("gone.rs")?.end,
            u32::MAX,
            "a deleted file has no new side to bound"
        );
        Ok(())
    }

    #[test]
    fn bounded_repo_regions_split_like_any_other_span() {
        let bounded = Region {
            file: "src/big.rs".to_owned(),
            start: 1,
            end: 10,
            kind: RegionKind::Repo,
        };
        let split = split_oversized(std::slice::from_ref(&bounded), 4);
        assert_eq!(split.len(), 3, "a diff-bounded repo region splits by lines");
        assert!(
            split.iter().all(|region| region.kind == RegionKind::Repo),
            "the chunks keep the repo kind"
        );
        assert!(are_disjoint(&split));
        let whole = Region {
            file: "README.md".to_owned(),
            start: 1,
            end: u32::MAX,
            kind: RegionKind::Repo,
        };
        let kept = split_oversized(std::slice::from_ref(&whole), 4);
        assert_eq!(
            kept.len(),
            1,
            "a file the diff never touches stays one region"
        );
    }

    #[test]
    fn oversized_regions_split_into_disjoint_fitting_chunks()
    -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(DIFF)?;
        let whole = file_spans(&index);
        let split = split_oversized(&whole, 1);
        assert!(
            split.len() > whole.len(),
            "a multi-line span splits at a one-line budget"
        );
        assert!(are_disjoint(&split), "chunks never overlap");
        for (mother, chunks) in whole.iter().map(|region| {
            (
                region,
                split
                    .iter()
                    .filter(|piece| piece.file == region.file)
                    .collect::<Vec<_>>(),
            )
        }) {
            let covered: u32 = chunks
                .iter()
                .map(|piece| piece.end.saturating_sub(piece.start).saturating_add(1))
                .sum();
            let span = mother.end.saturating_sub(mother.start).saturating_add(1);
            assert_eq!(covered, span, "the chunks tile the mother span exactly");
            assert!(
                chunks
                    .iter()
                    .all(|piece| piece.end.saturating_sub(piece.start) < 1)
            );
        }
        let kept = split_oversized(&whole, 4000);
        assert_eq!(kept, whole, "a budget above every span changes nothing");
        Ok(())
    }

    #[test]
    fn split_added_clusters_still_cover_every_added_line() -> Result<(), Box<dyn std::error::Error>>
    {
        let index = DiffIndex::parse(DIFF)?;
        let clusters = added_clusters(&index);
        let split = split_oversized(&clusters, 1);
        assert!(
            covers_all_added(&index, &split),
            "splitting never drops or widens an added line"
        );
        assert!(are_disjoint(&split));
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
    fn an_under_declared_hunk_cannot_swallow_the_next_file_header() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,1 +1,2 @@\n+one\n--- a/g\n+++ b/g\n";
        let outcome = DiffIndex::parse(diff);
        assert!(
            matches!(&outcome, Err(err)
                if err.contains("line 6") && err.contains("closes the counts exactly")),
            "the header-shaped line that closes the counts is the next file being eaten: {outcome:?}"
        );
    }

    #[test]
    fn content_shaped_like_a_header_closing_the_counts_fails_closed() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1 @@\n ctx\n--- x\n";
        let outcome = DiffIndex::parse(diff);
        assert!(
            matches!(&outcome, Err(err) if err.contains("line 5")),
            "genuine `-- ` content closing the counts is indistinguishable from a swallowed header; the parse fails closed rather than guess: {outcome:?}"
        );
    }

    #[test]
    fn a_stray_pluses_line_after_a_closed_hunk_never_mints_a_file() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,1 +1,1 @@\n base\n+++ weird\n";
        let outcome = DiffIndex::parse(diff);
        assert!(
            matches!(&outcome, Err(err)
                if err.contains("line 5") && err.contains("without its --- pair")),
            "a header-only phantom file never enters the index: {outcome:?}"
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
