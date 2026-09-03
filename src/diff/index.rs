//! The queryable diff index: files, hunks, per-line side numbering, and
//! the `clamp_to_hunk` resolution a finding must pass before it can
//! become a comment.

use crate::error::DifftraceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLine {
    Context { old: usize, new: usize },
    Added { new: usize },
    Removed { old: usize },
}

impl DiffLine {
    #[must_use]
    pub fn new_line(self) -> Option<usize> {
        match self {
            Self::Context { new, .. } | Self::Added { new } => Some(new),
            Self::Removed { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    #[must_use]
    pub fn new_span(&self) -> (usize, usize) {
        if self.new_count == 0 {
            return (0, 0);
        }
        let last = self
            .new_start
            .saturating_add(self.new_count)
            .saturating_sub(1);
        (self.new_start, last)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: String,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    pub anchor_lines: Vec<usize>,
}

impl FileDiff {
    fn in_hunk_span(&self, line: usize) -> bool {
        self.hunks.iter().any(|hunk| {
            let (first, last) = hunk.new_span();
            first > 0 && line >= first && line <= last
        })
    }

    fn nearest_anchor(&self, line: usize) -> Option<usize> {
        self.anchor_lines
            .iter()
            .copied()
            .min_by_key(|anchor| anchor.abs_diff(line))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffIndex {
    files: Vec<FileDiff>,
}

impl DiffIndex {
    pub(crate) fn from_files(files: Vec<FileDiff>) -> Self {
        Self { files }
    }

    pub fn parse(diff_text: &str) -> Result<Self, DifftraceError> {
        crate::diff::parse::parse(diff_text)
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn file_names(&self) -> Vec<&str> {
        self.files
            .iter()
            .map(|file| file.new_path.as_str())
            .collect()
    }

    #[must_use]
    pub fn file(&self, path: &str) -> Option<&FileDiff> {
        self.files
            .iter()
            .find(|file| file.new_path == path || file.old_path.as_deref() == Some(path))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    #[must_use]
    pub fn clamp_to_hunk(&self, file: &str, line: usize) -> Option<usize> {
        let file = self.file(file)?;
        if !file.in_hunk_span(line) {
            return None;
        }
        if file.anchor_lines.binary_search(&line).is_ok() {
            return Some(line);
        }
        file.nearest_anchor(line)
    }
}
