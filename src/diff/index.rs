//! The queryable diff index: files, hunks, per-line side numbering, and
//! `clamp_to_hunk` — a finding's line must be an anchorable line of the
//! diff (a context or added line of some hunk) before it can become a
//! comment; anything else is rejected.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub section: (usize, usize),

    pub old_path: Option<String>,
    pub new_path: String,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    pub anchor_lines: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffIndex {
    source: String,
    files: Vec<FileDiff>,
}

impl DiffIndex {
    pub(crate) fn from_parts(source: String, files: Vec<FileDiff>) -> Self {
        Self { source, files }
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
    pub fn file_section(&self, path: &str) -> Option<String> {
        let file = self.file(path)?;
        let start = file.section.0.saturating_sub(1);
        let count = file
            .section
            .1
            .saturating_sub(file.section.0)
            .saturating_add(1);
        let text = self
            .source
            .lines()
            .skip(start)
            .take(count)
            .collect::<Vec<_>>()
            .join("\n");
        Some(text)
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
        match file.anchor_lines.binary_search(&line) {
            Ok(_) => Some(line),
            Err(_) => None,
        }
    }
}
