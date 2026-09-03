//! The unified-diff parser. Hunk-count tracking disambiguates file
//! headers from removed lines whose content begins `--`.

use crate::diff::index::DiffIndex;
use crate::diff::index::DiffLine;
use crate::diff::index::FileDiff;
use crate::diff::index::Hunk;
use crate::error::DifftraceError;

pub(super) fn parse(diff_text: &str) -> Result<DiffIndex, DifftraceError> {
    let mut files = Vec::new();
    let mut current: Option<FileBuilder> = None;
    let line_count = diff_text.lines().count();
    for (idx, raw) in diff_text.lines().enumerate() {
        let line_no = idx.saturating_add(1);
        if let Some(header) = raw.strip_prefix("diff --git ") {
            let end_line = line_no.saturating_sub(1);
            flush(&mut files, &mut current, end_line)?;
            current = Some(FileBuilder::new(header, line_no)?);
        } else if let Some(builder) = current.as_mut() {
            builder.push_line(raw, line_no)?;
        }
    }
    flush(&mut files, &mut current, line_count)?;
    Ok(DiffIndex::from_parts(diff_text.to_owned(), files))
}

fn flush(
    files: &mut Vec<FileDiff>,
    current: &mut Option<FileBuilder>,
    end_line: usize,
) -> Result<(), DifftraceError> {
    if let Some(builder) = current.take() {
        files.push(builder.finish(end_line)?);
    }
    Ok(())
}

fn parse_error(line: usize, reason: &str) -> DifftraceError {
    DifftraceError::DiffParse {
        line,
        reason: reason.to_owned(),
    }
}

struct FileBuilder {
    start_line: usize,
    git_old: Option<String>,
    git_new: Option<String>,
    dash_old: Option<String>,
    dash_new: Option<String>,
    binary: bool,
    hunks: Vec<Hunk>,
    active: Option<ActiveHunk>,
    anchors: Vec<usize>,
}

struct ActiveHunk {
    hunk: Hunk,
    header_line: usize,
    next_old: usize,
    next_new: usize,
    remaining_old: usize,
    remaining_new: usize,
}

impl FileBuilder {
    fn new(header: &str, line_no: usize) -> Result<Self, DifftraceError> {
        let (git_old, git_new) = split_git_header(header, line_no)?;
        Ok(Self {
            start_line: line_no,
            git_old,
            git_new,
            dash_old: None,
            dash_new: None,
            binary: false,
            hunks: Vec::new(),
            active: None,
            anchors: Vec::new(),
        })
    }

    fn push_line(&mut self, raw: &str, line_no: usize) -> Result<(), DifftraceError> {
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.remaining_old > 0 || a.remaining_new > 0)
        {
            return self.push_hunk_body(raw, line_no);
        }
        self.seal_hunk();
        match header_path(raw, "--- ") {
            HeaderPath::Path(path) => self.dash_old = Some(path),
            HeaderPath::DevNull => self.dash_old = None,
            HeaderPath::NotHeader => match header_path(raw, "+++ ") {
                HeaderPath::Path(path) => self.dash_new = Some(path),
                HeaderPath::DevNull => self.dash_new = None,
                HeaderPath::NotHeader => {
                    if raw.starts_with("Binary files") {
                        self.binary = true;
                    } else if raw.starts_with("@@") {
                        self.start_hunk(raw, line_no)?;
                    } else if raw.starts_with('+') || raw.starts_with('-') || raw.starts_with(' ') {
                        return Err(parse_error(line_no, "diff body line outside a hunk"));
                    }
                }
            },
        }
        Ok(())
    }

    fn push_hunk_body(&mut self, raw: &str, line_no: usize) -> Result<(), DifftraceError> {
        if raw.starts_with("@@") {
            return Err(parse_error(
                line_no,
                "hunk header reached before the previous hunk consumed its declared counts",
            ));
        }
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        let line = if raw.starts_with('+') {
            active.remaining_new = active.remaining_new.saturating_sub(1);
            let new = active.next_new;
            active.next_new = active.next_new.saturating_add(1);
            DiffLine::Added { new }
        } else if raw.starts_with('-') {
            active.remaining_old = active.remaining_old.saturating_sub(1);
            let old = active.next_old;
            active.next_old = active.next_old.saturating_add(1);
            DiffLine::Removed { old }
        } else if raw.starts_with(' ') || raw.is_empty() {
            active.remaining_old = active.remaining_old.saturating_sub(1);
            active.remaining_new = active.remaining_new.saturating_sub(1);
            let old = active.next_old;
            let new = active.next_new;
            active.next_old = active.next_old.saturating_add(1);
            active.next_new = active.next_new.saturating_add(1);
            DiffLine::Context { old, new }
        } else {
            return Ok(());
        };
        if let Some(anchor) = line.new_line() {
            self.anchors.push(anchor);
        }
        active.hunk.lines.push(line);
        Ok(())
    }

    fn start_hunk(&mut self, raw: &str, line_no: usize) -> Result<(), DifftraceError> {
        let (old_start, old_count, new_start, new_count) = parse_hunk_header(raw, line_no)?;
        self.active = Some(ActiveHunk {
            hunk: Hunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            },
            header_line: line_no,
            next_old: old_start,
            next_new: new_start,
            remaining_old: old_count,
            remaining_new: new_count,
        });
        Ok(())
    }

    fn seal_hunk(&mut self) {
        if let Some(active) = self.active.take() {
            self.hunks.push(active.hunk);
        }
    }

    fn finish(mut self, end_line: usize) -> Result<FileDiff, DifftraceError> {
        if let Some(active) = &self.active
            && (active.remaining_old > 0 || active.remaining_new > 0)
        {
            return Err(parse_error(
                active.header_line,
                "hunk declares more lines than the diff provides",
            ));
        }
        self.seal_hunk();
        let mut anchors = self.anchors;
        anchors.sort_unstable();
        let old_path = self.dash_old.clone();
        let new_path = self
            .dash_new
            .or(self.dash_old)
            .or(self.git_new)
            .or(self.git_old)
            .unwrap_or_default();
        Ok(FileDiff {
            section: (self.start_line, end_line),
            old_path,
            new_path,
            hunks: self.hunks,
            binary: self.binary,
            anchor_lines: anchors,
        })
    }
}

fn split_git_header(
    header: &str,
    line_no: usize,
) -> Result<(Option<String>, Option<String>), DifftraceError> {
    if header.is_empty() {
        return Err(parse_error(line_no, "empty diff --git header"));
    }
    let mut first_pair: Option<(String, String)> = None;
    let mut equal_pair: Option<(String, String)> = None;
    for (idx, _) in header.match_indices(" b/") {
        let (old_raw, new_raw) = header.split_at(idx);
        let Some(new) = new_raw.strip_prefix(" b/") else {
            continue;
        };
        let old = old_raw.strip_prefix("a/").unwrap_or(old_raw);
        let pair = (old.to_owned(), new.to_owned());
        if old == new {
            equal_pair = Some(pair);
            break;
        }
        if first_pair.is_none() {
            first_pair = Some(pair);
        }
    }
    let pair = equal_pair.or(first_pair);
    Ok(pair.map_or((None, None), |(old, new)| (Some(old), Some(new))))
}

enum HeaderPath {
    NotHeader,
    Path(String),
    DevNull,
}

fn header_path(raw: &str, marker: &str) -> HeaderPath {
    let Some(rest) = raw.strip_prefix(marker) else {
        return HeaderPath::NotHeader;
    };
    if rest == "/dev/null" {
        return HeaderPath::DevNull;
    }
    let path = rest
        .strip_prefix("a/")
        .or_else(|| rest.strip_prefix("b/"))
        .unwrap_or(rest);
    HeaderPath::Path(path.to_owned())
}

fn parse_hunk_header(
    raw: &str,
    line_no: usize,
) -> Result<(usize, usize, usize, usize), DifftraceError> {
    let body = raw
        .strip_prefix("@@ ")
        .and_then(|rest| rest.split_once(" @@"))
        .map(|(ranges, _heading)| ranges)
        .ok_or_else(|| parse_error(line_no, "malformed hunk header"))?;
    let mut parts = body.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| parse_error(line_no, "hunk header missing old-side range"))?;
    let new = parts
        .next()
        .ok_or_else(|| parse_error(line_no, "hunk header missing new-side range"))?;
    let (old_start, old_count) = parse_range(old, line_no)?;
    let (new_start, new_count) = parse_range(new, line_no)?;
    Ok((old_start, old_count, new_start, new_count))
}

fn parse_range(member: &str, line_no: usize) -> Result<(usize, usize), DifftraceError> {
    let body = member
        .strip_prefix('-')
        .or_else(|| member.strip_prefix('+'))
        .ok_or_else(|| parse_error(line_no, "hunk range without side prefix"))?;
    let mut fields = body.split(',');
    let start = fields
        .next()
        .ok_or_else(|| parse_error(line_no, "empty hunk range"))?;
    let start = start
        .parse::<usize>()
        .map_err(|_| parse_error(line_no, "non-numeric hunk start"))?;
    let count = match fields.next() {
        Some(count) => count
            .parse::<usize>()
            .map_err(|_| parse_error(line_no, "non-numeric hunk count"))?,
        None => 1,
    };
    Ok((start, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@
 fn main() {
     let x = 1;
-    let y = 2;
+    let y = 3;
+    let z = 4;
 }
diff --git a/README.md b/README.md
index 3333333..4444444 100644
--- a/README.md
+++ b/README.md
@@ -10,3 +10,3 @@
 intro
-old line
+new line
 tail
";

    #[test]
    fn multi_file_diff_parses_both_files() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(SAMPLE)?;
        assert_eq!(index.len(), 2);
        assert_eq!(index.file_names(), vec!["src/lib.rs", "README.md"]);
        Ok(())
    }

    #[test]
    fn lines_are_numbered_on_their_sides() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(SAMPLE)?;
        let file = index.file("src/lib.rs").ok_or("expected a value")?;
        let hunk = file.hunks.first().ok_or("expected a value")?;
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        let added: Vec<usize> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                DiffLine::Added { new } => Some(*new),
                _ => None,
            })
            .collect();
        assert_eq!(added, vec![3, 4]);
        let removed: Vec<usize> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                DiffLine::Removed { old } => Some(*old),
                _ => None,
            })
            .collect();
        assert_eq!(removed, vec![3]);
        assert_eq!(file.anchor_lines, vec![1, 2, 3, 4, 5]);
        Ok(())
    }

    #[test]
    fn every_anchorable_line_clamps_to_itself() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(SAMPLE)?;
        for name in index.file_names() {
            let file = index.file(name).ok_or("expected a value")?;
            for anchor in &file.anchor_lines {
                assert_eq!(index.clamp_to_hunk(name, *anchor), Some(*anchor));
            }
        }
        Ok(())
    }

    #[test]
    fn a_line_outside_hunk_spans_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(SAMPLE)?;
        assert_eq!(index.clamp_to_hunk("src/lib.rs", 99), None);
        assert_eq!(index.clamp_to_hunk("README.md", 99), None);
        assert_eq!(index.clamp_to_hunk("absent.rs", 5), None);
        Ok(())
    }

    #[test]
    fn a_context_line_inside_a_hunk_clamps_to_itself() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse(SAMPLE)?;
        assert_eq!(index.clamp_to_hunk("src/lib.rs", 1), Some(1));
        assert_eq!(index.clamp_to_hunk("README.md", 12), Some(12));
        Ok(())
    }

    #[test]
    fn a_new_file_has_only_new_side_anchors() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/added.txt b/added.txt
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/added.txt
@@ -0,0 +1,2 @@
+first
+second
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("added.txt").ok_or("expected a value")?;
        assert_eq!(file.old_path, None);
        assert_eq!(file.anchor_lines, vec![1, 2]);
        assert_eq!(index.clamp_to_hunk("added.txt", 1), Some(1));
        Ok(())
    }

    #[test]
    fn a_deleted_file_has_no_new_side_anchors() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 1111111..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-first
-second
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("gone.txt").ok_or("expected a value")?;
        assert_eq!(file.new_path, "gone.txt");
        assert!(file.anchor_lines.is_empty());
        assert_eq!(index.clamp_to_hunk("gone.txt", 1), None);
        Ok(())
    }

    #[test]
    fn a_removed_line_clamps_to_the_nearest_new_side_anchor()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -5,3 +5,3 @@
 ctx-a
-removed
+added
 ctx-b
";
        let index = DiffIndex::parse(raw)?;
        // New side runs 5 ctx, 6 added, 7 ctx; the removed line was old 6.
        assert_eq!(index.clamp_to_hunk("f.rs", 6), Some(6));
        Ok(())
    }

    #[test]
    fn a_pure_deletion_hunk_clamps_to_surrounding_context() -> Result<(), Box<dyn std::error::Error>>
    {
        let raw = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -5,3 +5,2 @@
 ctx-a
-removed
 ctx-b
";
        let index = DiffIndex::parse(raw)?;
        // New side is 5 ctx-a, 6 ctx-b; old 6 (removed) maps to 5 or 6.
        let clamped = index.clamp_to_hunk("f.rs", 6);
        assert!(clamped == Some(5) || clamped == Some(6));
        Ok(())
    }

    #[test]
    fn omitted_hunk_counts_default_to_one() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -9 +9 @@
-old
+new
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("f.rs").ok_or("expected a value")?;
        let hunk = file.hunks.first().ok_or("expected a value")?;
        assert_eq!(hunk.old_count, 1);
        assert_eq!(hunk.new_count, 1);
        assert_eq!(file.anchor_lines, vec![9]);
        Ok(())
    }

    #[test]
    fn a_binary_file_carries_no_hunks() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/logo.png b/logo.png
index 1111111..2222222 100644
Binary files a/logo.png and b/logo.png differ
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("logo.png").ok_or("expected a value")?;
        assert!(file.binary);
        assert!(file.hunks.is_empty());
        assert_eq!(index.clamp_to_hunk("logo.png", 1), None);
        Ok(())
    }

    #[test]
    fn a_removed_line_starting_with_dashes_stays_hunk_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/notes.md b/notes.md
--- a/notes.md
+++ b/notes.md
@@ -1,3 +1,2 @@
 head
--- a/dash-prefixed note
 tail
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("notes.md").ok_or("expected a value")?;
        assert_eq!(file.hunks.len(), 1);
        assert_eq!(file.new_path, "notes.md");
        Ok(())
    }

    #[test]
    fn a_no_newline_marker_is_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/f.txt b/f.txt
--- a/f.txt
+++ b/f.txt
@@ -1 +1 @@
-first
\\ No newline at end of file
+second
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("f.txt").ok_or("expected a value")?;
        assert_eq!(file.anchor_lines, vec![1]);
        Ok(())
    }

    #[test]
    fn a_malformed_hunk_header_names_the_line() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ broken @@
+line
";
        let err = DiffIndex::parse(raw).err().ok_or("expected an error")?;
        assert!(err.to_string().contains('4'), "error names the line: {err}");
        Ok(())
    }

    #[test]
    fn an_empty_diff_yields_an_empty_index() -> Result<(), Box<dyn std::error::Error>> {
        let index = DiffIndex::parse("")?;
        assert!(index.is_empty());
        assert_eq!(index.file_names(), Vec::<&str>::new());
        Ok(())
    }

    #[test]
    fn a_file_section_carries_only_that_files_raw_text() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "diff --git a/first.rs b/first.rs\n--- a/first.rs\n+++ b/first.rs\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/mid.rs b/mid.rs\n--- a/mid.rs\n+++ b/mid.rs\n@@ -1 +1 @@\n-c\n+d\ndiff --git a/last.rs b/last.rs\n--- a/last.rs\n+++ b/last.rs\n@@ -1 +1 @@\n-e\n+f\n";
        let index = DiffIndex::parse(raw)?;
        let first = index.file_section("first.rs").ok_or("expected a value")?;
        assert_eq!(
            first,
            "diff --git a/first.rs b/first.rs\n--- a/first.rs\n+++ b/first.rs\n@@ -1 +1 @@\n-a\n+b"
        );
        let mid = index.file_section("mid.rs").ok_or("expected a value")?;
        assert!(mid.starts_with("diff --git a/mid.rs b/mid.rs"));
        assert!(mid.contains("+d"));
        assert!(!mid.contains("first.rs"));
        assert!(!mid.contains("last.rs"));
        let last = index.file_section("last.rs").ok_or("expected a value")?;
        assert!(last.starts_with("diff --git a/last.rs b/last.rs"));
        assert!(last.contains("+f"));
        assert!(!last.contains("mid.rs"));
        assert_eq!(index.file_section("absent.rs"), None);
        Ok(())
    }

    #[test]
    fn consecutive_hunks_renumber_both_sides_from_their_headers()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = "diff --git a/two.rs b/two.rs
--- a/two.rs
+++ b/two.rs
@@ -2,3 +2,3 @@
 ctx
-old
+new
 tail
@@ -20,2 +20,3 @@
 keep
+add
 done
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("two.rs").ok_or("expected a value")?;
        assert_eq!(file.hunks.len(), 2);
        let first = file.hunks.first().ok_or("expected a value")?;
        assert_eq!((first.old_start, first.new_start), (2, 2));
        let second = file.hunks.get(1).ok_or("expected a value")?;
        assert_eq!((second.old_start, second.new_start), (20, 20));
        let added: Vec<usize> = second
            .lines
            .iter()
            .filter_map(|line| match line {
                DiffLine::Added { new } => Some(*new),
                _ => None,
            })
            .collect();
        assert_eq!(added, vec![21]);
        assert_eq!(file.anchor_lines, vec![2, 3, 4, 20, 21, 22]);
        assert_eq!(index.clamp_to_hunk("two.rs", 21), Some(21));
        Ok(())
    }

    #[test]
    fn a_deleted_file_with_an_embedded_b_path_keeps_its_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = "diff --git a/foo b/bar.rs b/foo b/bar.rs
deleted file mode 100644
index 1111111..0000000
--- a/foo b/bar.rs
+++ /dev/null
@@ -1 +0,0 @@
-gone
";
        let index = DiffIndex::parse(raw)?;
        let file = index.file("foo b/bar.rs").ok_or("expected a value")?;
        assert_eq!(file.new_path, "foo b/bar.rs");
        assert_eq!(file.old_path.as_deref(), Some("foo b/bar.rs"));
        assert!(file.anchor_lines.is_empty());
        Ok(())
    }

    #[test]
    fn an_embedded_b_path_splits_where_both_sides_agree() -> Result<(), Box<dyn std::error::Error>>
    {
        let (old, new) = split_git_header("foo b/bar.rs b/foo b/bar.rs", 1)?;
        assert_eq!(old.as_deref(), Some("foo b/bar.rs"));
        assert_eq!(new.as_deref(), Some("foo b/bar.rs"));
        Ok(())
    }

    #[test]
    fn a_rename_header_keeps_the_first_split() -> Result<(), Box<dyn std::error::Error>> {
        let (old, new) = split_git_header("old.rs b/new.rs", 1)?;
        assert_eq!(old.as_deref(), Some("old.rs"));
        assert_eq!(new.as_deref(), Some("new.rs"));
        Ok(())
    }

    #[test]
    fn an_over_declared_hunk_fails_the_parse() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "diff --git a/h.rs b/h.rs
--- a/h.rs
+++ b/h.rs
@@ -1,5 +1,5 @@
+one
";
        let err = DiffIndex::parse(raw).err().ok_or("expected an error")?;
        assert!(
            err.to_string().contains("declares more lines"),
            "error names the contradiction: {err}"
        );
        assert!(matches!(err, DifftraceError::DiffParse { line: 4, .. }));
        Ok(())
    }

    #[test]
    fn an_over_declared_hunk_fails_at_the_next_file_too() -> Result<(), Box<dyn std::error::Error>>
    {
        let raw = "diff --git a/h.rs b/h.rs
--- a/h.rs
+++ b/h.rs
@@ -1,5 +1,5 @@
+one
diff --git a/next.rs b/next.rs
--- a/next.rs
+++ b/next.rs
@@ -1 +1 @@
-a
+b
";
        let err = DiffIndex::parse(raw).err().ok_or("expected an error")?;
        assert!(
            err.to_string().contains("declares more lines"),
            "error names the contradiction: {err}"
        );
        Ok(())
    }

    #[test]
    fn an_under_declared_hunk_fails_at_the_next_header() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "diff --git a/h.rs b/h.rs
--- a/h.rs
+++ b/h.rs
@@ -1,2 +1,2 @@
+one
+two
+three
";
        let err = DiffIndex::parse(raw).err().ok_or("expected an error")?;
        assert!(
            err.to_string().contains("declares more lines"),
            "under-declared counts surface the same way: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_hunk_header_inside_unclosed_counts_fails_the_parse()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = "diff --git a/h.rs b/h.rs
--- a/h.rs
+++ b/h.rs
@@ -1,4 +1,4 @@
+one
@@ -9 +9 @@
-old
+new
";
        let err = DiffIndex::parse(raw).err().ok_or("expected an error")?;
        assert!(
            err.to_string().contains("before the previous hunk"),
            "error names the contradiction: {err}"
        );
        assert!(matches!(err, DifftraceError::DiffParse { line: 6, .. }));
        Ok(())
    }
}
