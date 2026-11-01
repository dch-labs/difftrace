//! Pure diff mechanics: the unified-diff parser behind pinned ranges,
//! per-scope region building, and the cite-resolution authority every
//! review citation must pass.

pub mod regions;

use std::collections::BTreeMap;

use crate::contracts::{Cite, Region, RegionKind};

pub use crate::contracts::PinnedRange as Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Diff,
    Deep,
    Full,
}

impl std::str::FromStr for Scope {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_lowercase().as_str() {
            "diff" => Ok(Self::Diff),
            "deep" => Ok(Self::Deep),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown scope {other:?}; expected diff, deep, or full"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileDiff {
    lines: BTreeMap<u32, LineKind>,
    hunk_start: u32,
    hunk_end: u32,
}

struct HunkWindow<'a> {
    first: Option<u32>,
    last: u32,
    lines: Vec<&'a str>,
}

#[must_use]
pub fn region_diff(diff: &str, file: &str, start: u32, end: u32, context: u32) -> String {
    let lo = start.saturating_sub(context).max(1);
    let hi = end.saturating_add(context);
    let mut windows: Vec<HunkWindow> = Vec::new();
    let mut current: Option<String> = None;
    let mut new_line: u32 = 0;
    let mut old_remaining: u32 = 0;
    let mut new_remaining: u32 = 0;
    let mut in_hunk = false;
    let mut lines = diff.lines();
    let mut lookahead = lines.next();
    while let Some(line) = lookahead {
        lookahead = lines.next();
        if let Some((start, (old_count, new_count), _)) = hunk_header(line) {
            new_line = start.saturating_sub(1);
            old_remaining = old_count;
            new_remaining = new_count;
            in_hunk = true;
            windows.push(HunkWindow {
                first: None,
                last: 0,
                lines: Vec::new(),
            });
            continue;
        }
        if in_hunk {
            let boundary = old_remaining == 0
                && new_remaining == 0
                && line.starts_with("--- ")
                && lookahead.is_some_and(next_starts_new_file);
            if boundary {
                in_hunk = false;
                current = None;
                continue;
            }
            let number = new_line.saturating_add(1);
            let inside = current.as_deref() == Some(file) && lo <= number && number <= hi;
            if line.is_empty() || line.starts_with('+') || line.starts_with(' ') {
                new_line = number;
                new_remaining = new_remaining.saturating_sub(1);
                if line.is_empty() || line.starts_with(' ') {
                    old_remaining = old_remaining.saturating_sub(1);
                }
                if inside {
                    push_kept(&mut windows, line, number);
                }
            } else if line.starts_with('-') {
                old_remaining = old_remaining.saturating_sub(1);
                if inside {
                    push_kept(&mut windows, line, number);
                }
            } else if !line.starts_with('\\') {
                in_hunk = false;
                current = None;
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.split('\t').next().unwrap_or(path);
            current = if path == "/dev/null" {
                None
            } else {
                Some(
                    path.strip_prefix("b/")
                        .map_or(path.to_owned(), str::to_owned),
                )
            };
        }
    }
    render_window(file, lo, hi, &windows)
}

fn next_starts_new_file(next: &str) -> bool {
    next.starts_with("+++ ")
        || next.starts_with("diff ")
        || next.starts_with("index ")
        || hunk_header(next).is_some()
}

fn push_kept<'a>(windows: &mut [HunkWindow<'a>], line: &'a str, number: u32) {
    if let Some(window) = windows.last_mut() {
        if window.first.is_none() {
            window.first = Some(number);
        }
        window.last = number;
        window.lines.push(line);
    }
}

fn render_window(file: &str, lo: u32, hi: u32, windows: &[HunkWindow]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut opened = false;
    for window in windows {
        if window.lines.is_empty() {
            continue;
        }
        if opened {
            parts.push(format!(
                "@@ {file} lines {}-{} @@",
                window.first.unwrap_or(window.last),
                window.last
            ));
        }
        opened = true;
        parts.push(window.lines.join("\n"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(
        "@@ {file} new-side lines {lo}-{hi} @@\n{}",
        parts.join("\n")
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffIndex {
    files: BTreeMap<String, FileDiff>,
}

impl DiffIndex {
    pub fn parse(diff: &str) -> Result<Self, String> {
        let mut files: BTreeMap<String, FileDiff> = BTreeMap::new();
        let mut current: Option<(String, FileDiff)> = None;
        let mut skipping_deletion = false;
        let mut new_line: u32 = 0;
        let mut old_remaining: u32 = 0;
        let mut new_remaining: u32 = 0;
        let mut recognized = false;
        let mut pending_pair = false;
        for (number, raw) in diff.lines().enumerate() {
            let line_number = u32::try_from(number.saturating_add(1)).unwrap_or(0);
            if old_remaining == 0 && new_remaining == 0 {
                if let Some(header) = raw.strip_prefix("+++ ") {
                    if !pending_pair {
                        return Err(format!(
                            "line {line_number}: a +++ header without its --- pair"
                        ));
                    }
                    pending_pair = false;
                    recognized = true;
                    flush(&mut current, &mut files);
                    if let Some(path) = new_path(header) {
                        current = Some((
                            path,
                            FileDiff {
                                lines: BTreeMap::new(),
                                hunk_start: 0,
                                hunk_end: 0,
                            },
                        ));
                        skipping_deletion = false;
                    } else {
                        current = None;
                        skipping_deletion = true;
                    }
                    continue;
                }
                if is_metadata_header(raw) {
                    pending_pair = raw.starts_with("--- ");
                    recognized = true;
                    continue;
                }
                if let Some((start, counts, end)) = hunk_header(raw) {
                    recognized = true;
                    if skipping_deletion {
                        old_remaining = counts.0;
                        new_remaining = counts.1;
                        continue;
                    }
                    let Some((_, file)) = current.as_mut() else {
                        return Err(format!(
                            "line {line_number}: a hunk header outside any file header"
                        ));
                    };
                    new_line = start;
                    old_remaining = counts.0;
                    new_remaining = counts.1;
                    if file.hunk_start == 0 || start < file.hunk_start {
                        file.hunk_start = start;
                    }
                    if end > file.hunk_end {
                        file.hunk_end = end;
                    }
                    continue;
                }
                if raw.starts_with("@@") {
                    return Err(format!("line {line_number}: a malformed hunk header"));
                }
                if raw.is_empty()
                    || raw.starts_with('+')
                    || raw.starts_with('-')
                    || raw.starts_with(' ')
                {
                    return Err(format!(
                        "line {line_number}: a diff body line outside a hunk"
                    ));
                }
                continue;
            }
            if hunk_header(raw).is_some() {
                return Err(format!(
                    "line {line_number}: a new hunk starts before the previous one closed"
                ));
            }
            let kind = if raw.is_empty() || raw.starts_with(' ') {
                LineKind::Context
            } else if raw.starts_with('+') {
                LineKind::Added
            } else if raw.starts_with('-') {
                LineKind::Removed
            } else if raw.starts_with('\\') {
                continue;
            } else {
                return Err(format!(
                    "line {line_number}: a hunk body line without a +, -, or space prefix"
                ));
            };
            match kind {
                LineKind::Removed => {
                    if old_remaining == 0 {
                        return Err(format!(
                            "line {line_number}: a hunk body line beyond the declared counts"
                        ));
                    }
                    old_remaining = old_remaining.saturating_sub(1);
                    if let Some(error) =
                        closure_error(raw, line_number, old_remaining, new_remaining, "--- ")
                    {
                        return Err(error);
                    }
                }
                LineKind::Context | LineKind::Added => {
                    let Some((_, file)) = current.as_mut() else {
                        return Err(format!(
                            "line {line_number}: a hunk body outside any file header"
                        ));
                    };
                    if new_remaining == 0 || (kind == LineKind::Context && old_remaining == 0) {
                        return Err(format!(
                            "line {line_number}: a hunk body line beyond the declared counts"
                        ));
                    }
                    file.lines.insert(new_line, kind);
                    new_line = new_line.saturating_add(1);
                    if kind == LineKind::Context {
                        old_remaining = old_remaining.saturating_sub(1);
                    }
                    new_remaining = new_remaining.saturating_sub(1);
                    if kind == LineKind::Added
                        && let Some(error) =
                            closure_error(raw, line_number, old_remaining, new_remaining, "+++ ")
                    {
                        return Err(error);
                    }
                }
            }
        }
        if old_remaining > 0 || new_remaining > 0 {
            let last = diff.lines().count();
            return Err(format!(
                "line {last}: the diff ends before the last hunk closes"
            ));
        }
        flush(&mut current, &mut files);
        if !recognized && !diff.trim().is_empty() {
            return Err("line 1: no file header or hunk recognized; not a unified diff".to_owned());
        }
        Ok(Self { files })
    }

    #[must_use]
    pub fn resolve(&self, cite: &Cite) -> bool {
        self.files
            .get(&cite.file)
            .is_some_and(|file| file.lines.contains_key(&cite.line))
    }

    #[must_use]
    pub fn clamp_to_region(&self, cite: &Cite, region: &Region) -> bool {
        cite.file == region.file
            && cite.line >= region.start
            && cite.line <= region.end
            && self.resolve(cite)
    }

    #[must_use]
    pub fn files(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    #[must_use]
    pub fn is_added_line(&self, cite: &Cite) -> bool {
        self.files
            .get(&cite.file)
            .is_some_and(|file| file.lines.get(&cite.line) == Some(&LineKind::Added))
    }

    #[must_use]
    pub fn cites_no_added_line(&self, cites: &[Cite]) -> bool {
        !cites.is_empty() && cites.iter().all(|cite| !self.is_added_line(cite))
    }

    #[must_use]
    pub fn added_lines(&self, file: &str) -> Vec<u32> {
        self.files
            .get(file)
            .map(|entry| {
                entry
                    .lines
                    .iter()
                    .filter(|(_, kind)| **kind == LineKind::Added)
                    .map(|(line, _)| *line)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn span(&self, file: &str) -> Option<(u32, u32)> {
        self.files
            .get(file)
            .filter(|entry| !entry.lines.is_empty())
            .map(|entry| (entry.hunk_start, entry.hunk_end))
    }
}

fn flush(current: &mut Option<(String, FileDiff)>, files: &mut BTreeMap<String, FileDiff>) {
    if let Some((path, file)) = current.take() {
        files.insert(path, file);
    }
}

fn new_path(header: &str) -> Option<String> {
    let trimmed = header.split('\t').next().unwrap_or(header).trim();
    if trimmed == "/dev/null" {
        return None;
    }
    let path = trimmed.strip_prefix("b/").unwrap_or(trimmed);
    (!path.is_empty()).then(|| path.to_owned())
}

fn is_metadata_header(raw: &str) -> bool {
    raw.starts_with("--- ")
        || raw.starts_with("diff --git")
        || raw.starts_with("index ")
        || raw.starts_with("old mode")
        || raw.starts_with("new mode")
        || raw.starts_with("new file mode")
        || raw.starts_with("deleted file mode")
        || raw.starts_with("similarity index")
        || raw.starts_with("rename from")
        || raw.starts_with("rename to")
        || raw.starts_with("Binary files")
}

fn closure_error(
    raw: &str,
    line_number: u32,
    old_remaining: u32,
    new_remaining: u32,
    prefix: &str,
) -> Option<String> {
    (raw.starts_with(prefix) && old_remaining == 0 && new_remaining == 0).then(|| {
        format!(
            "line {line_number}: a {prefix}header consumed as hunk body closes the counts exactly"
        )
    })
}

fn hunk_header(raw: &str) -> Option<(u32, (u32, u32), u32)> {
    let rest = raw.strip_prefix("@@ -")?;
    let (old_side, new_side) = rest.split_once(" +")?;
    let old_numbers = old_side.split([' ', '@']).next()?;
    let new_numbers = new_side.split([' ', '@']).next()?;
    let parse_pair = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse::<u32>().ok()?, count.parse::<u32>().ok()?)),
            None => Some((text.parse::<u32>().ok()?, 1)),
        }
    };
    let (start, new_count) = parse_pair(new_numbers)?;
    let (_, old_count) = parse_pair(old_numbers)?;
    let start = start.max(1);
    let end = start.saturating_add(new_count.max(1).saturating_sub(1));
    Some((start, (old_count, new_count), end))
}

pub fn build_regions(diff: &str, scope: Scope) -> Result<Vec<Region>, String> {
    let index = DiffIndex::parse(diff)?;
    match scope {
        Scope::Diff => Ok(regions::added_clusters(&index)),
        Scope::Deep => Ok(regions::file_spans(&index)),
        Scope::Full => {
            Err("the full scope needs the tracked file list — call repo_regions with it".to_owned())
        }
    }
}

#[must_use]
pub fn repo_regions(files: &[String]) -> Vec<Region> {
    files
        .iter()
        .map(|file| Region {
            file: file.clone(),
            start: 1,
            end: u32::MAX,
            kind: RegionKind::Repo,
        })
        .collect()
}

#[must_use]
pub fn repo_regions_bounded(files: &[String], index: &DiffIndex) -> Vec<Region> {
    files
        .iter()
        .map(|file| {
            let end = index.span(file).map_or(u32::MAX, |(_, end)| end);
            Region {
                file: file.clone(),
                start: 1,
                end,
                kind: RegionKind::Repo,
            }
        })
        .collect()
}

pub const REGION_MATERIAL_BUDGET_BYTES: usize = 96 * 1024;

#[must_use]
pub fn fit_regions(
    regions: &[Region],
    diff_text: &str,
    max_lines: u32,
    max_bytes: usize,
) -> Vec<Region> {
    let mut out = Vec::new();
    for region in regions::split_oversized(regions, max_lines) {
        fit_by_bytes(region, diff_text, max_bytes, 0, &mut out);
    }
    out
}

fn fit_by_bytes(
    region: Region,
    diff_text: &str,
    max_bytes: usize,
    depth: u8,
    out: &mut Vec<Region>,
) {
    if region.end == u32::MAX || depth >= 5 {
        out.push(region);
        return;
    }
    let material = region_diff(
        diff_text,
        &region.file,
        region.start,
        region.end,
        crate::judges::prompt::REGION_CONTEXT_LINES,
    );
    if material.len() <= max_bytes {
        out.push(region);
        return;
    }
    let span = region.end.saturating_sub(region.start);
    if span < 2 {
        out.push(region);
        return;
    }
    let mid = region.start.saturating_add(span / 2).max(region.start);
    let lower = Region {
        end: mid,
        ..region.clone()
    };
    let upper = Region {
        start: mid.saturating_add(1).min(region.end),
        ..region
    };
    fit_by_bytes(lower, diff_text, max_bytes, depth.saturating_add(1), out);
    fit_by_bytes(upper, diff_text, max_bytes, depth.saturating_add(1), out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_inclusive_at_both_ends() {
        let diff =
            "--- a/f\n+++ b/f\n@@ -1,9 +1,9 @@\n c1\n c2\n c3\n c4\n c5\n c6\n c7\n c8\n c9\n";
        let window = region_diff(diff, "f", 5, 5, 2);
        assert_eq!(
            window, "@@ f new-side lines 3-7 @@\n c3\n c4\n c5\n c6\n c7",
            "lo and hi are kept, one past each edge is not"
        );
    }

    #[test]
    fn a_single_line_region_with_no_context_keeps_its_own_line() {
        let diff = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n+hello\n";
        assert_eq!(
            region_diff(diff, "f", 1, 1, 0),
            "@@ f new-side lines 1-1 @@\n+hello"
        );
    }

    #[test]
    fn removed_lines_borrow_the_upcoming_new_side_number() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n keep\n-gone\n+fresh\n tail\n";
        assert_eq!(
            region_diff(diff, "f", 2, 2, 1),
            "@@ f new-side lines 1-3 @@\n keep\n-gone\n+fresh\n tail"
        );
    }

    #[test]
    fn numbering_resets_at_each_hunk_header() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a1\n a2\n@@ -10,2 +10,2 @@\n b1\n b2\n";
        assert_eq!(
            region_diff(diff, "f", 10, 10, 0),
            "@@ f new-side lines 10-10 @@\n b1",
            "the second hunk of the same file re-enters with its own numbering"
        );
    }

    #[test]
    fn a_following_files_header_pair_never_leaks_into_the_window() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a1\n+add\n a2\n--- a/g\n+++ b/g\n@@ -1,1 +1,1 @@\n b1\n";
        let window = region_diff(diff, "f", 1, 4, 0);
        assert!(
            window.contains(" a1") && window.contains("+add"),
            "the window keeps its own lines: {window}"
        );
        assert!(
            !window.contains("--- a/g") && !window.contains("+++ b/g"),
            "a separator-less next file's headers are never body: {window}"
        );
    }

    #[test]
    fn a_genuine_header_shaped_body_pair_mid_hunk_keeps_the_window()
    -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/f.sql\n+++ b/f.sql\n@@ -1,4 +1,4 @@\n c0\n--- old banner\n+++ new banner\n c1\n c2\n";
        let index = DiffIndex::parse(diff)?;
        assert!(
            index.resolve(&Cite {
                file: "f.sql".to_owned(),
                line: 2
            }),
            "the pair is ordinary removal and addition to the parser"
        );
        assert_eq!(
            region_diff(diff, "f.sql", 1, 4, 0),
            "@@ f.sql new-side lines 1-4 @@\n c0\n--- old banner\n+++ new banner\n c1\n c2",
            "a body pair inside the declared counts never truncates the materials"
        );
        Ok(())
    }

    #[test]
    fn an_added_line_starting_with_pluses_stays_body() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n base\n+++ oops\n";
        assert_eq!(
            region_diff(diff, "f", 2, 2, 0),
            "@@ f new-side lines 2-2 @@\n+++ oops",
            "a + body line never reads as a file header"
        );
    }

    #[test]
    fn a_window_spanning_hunks_marks_each_boundary() {
        let diff =
            "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n c1\n c2\n c3\n@@ -20,2 +20,2 @@\n b1\n b2\n";
        assert_eq!(
            region_diff(diff, "f", 2, 21, 1),
            "@@ f new-side lines 1-22 @@\n c1\n c2\n c3\n@@ f lines 20-21 @@\n b1\n b2",
            "a second hunk opens under its own subheader, never as a contiguous join"
        );
    }

    #[test]
    fn a_dev_null_target_yields_no_material() {
        let diff = "--- a/gone\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-here\n-too\n";
        assert_eq!(region_diff(diff, "gone", 1, 2, 10), "");
    }

    #[test]
    fn an_unpaired_dashes_line_before_a_new_file_never_leaks_into_the_window()
    -> Result<(), Box<dyn std::error::Error>> {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a1\n a2\n--- a/g\ndiff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -1,1 +1,1 @@\n b1\n";
        let index = DiffIndex::parse(diff)?;
        assert_eq!(
            index.files(),
            vec!["f", "g"],
            "the index consumes the unpaired --- as metadata and keys both files"
        );
        let window = region_diff(diff, "f", 1, 2, 10);
        assert!(
            !window.contains("--- a/g") && !window.contains("diff --git"),
            "the window renders only f's own lines: {window}"
        );
        Ok(())
    }

    #[test]
    fn a_window_with_no_kept_lines_yields_no_material() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a1\n a2\n";
        assert_eq!(region_diff(diff, "f", 50, 60, 0), "");
    }

    #[test]
    fn a_file_of_very_long_lines_splits_by_bytes_not_just_lines()
    -> Result<(), Box<dyn std::error::Error>> {
        let fat_line = format!("+{}", "x".repeat(900));
        let mut diff = String::from("--- a/CHANGELOG.md\n+++ b/CHANGELOG.md\n@@ -0,0 +1,150 @@\n");
        for _line in 0..150 {
            diff.push_str(&fat_line);
            diff.push('\n');
        }
        let index = DiffIndex::parse(&diff)?;
        let regions = regions::added_clusters(&index);
        assert_eq!(
            regions.len(),
            1,
            "150 lines stay one region under the line cap"
        );
        let fit = fit_regions(&regions, &diff, 3_000, 96 * 1024);
        assert!(
            fit.len() >= 2,
            "the ~135KB of materials splits under the byte budget: {} regions",
            fit.len()
        );
        assert!(regions::are_disjoint(&fit));
        for region in &fit {
            let material = region_diff(
                &diff,
                "CHANGELOG.md",
                region.start,
                region.end,
                crate::judges::prompt::REGION_CONTEXT_LINES,
            );
            assert!(
                material.len() <= 96 * 1024,
                "each fitted region carries bounded materials: {} bytes for {}-{}",
                material.len(),
                region.start,
                region.end
            );
        }
        let covered: std::collections::BTreeSet<u32> = fit
            .iter()
            .flat_map(|region| region.start..=region.end)
            .collect();
        for line in 1..=150 {
            assert!(
                covered.contains(&line),
                "line {line} vanished from the fitted regions"
            );
        }
        Ok(())
    }

    #[test]
    fn an_empty_context_line_stays_body_and_keeps_later_hunks_reachable() {
        let diff =
            "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,3 @@\n keep\n\n keep2\n@@ -9,1 +9,1 @@\n tail\n";
        assert_eq!(
            region_diff(diff, "f.rs", 2, 2, 1),
            "@@ f.rs new-side lines 1-3 @@\n keep\n\n keep2",
            "a stripped context line is kept as context, so the window matches its header"
        );
        assert_eq!(
            region_diff(diff, "f.rs", 9, 9, 0),
            "@@ f.rs new-side lines 9-9 @@\n tail",
            "an empty body line never ends the file's hunk chain"
        );
    }
}
