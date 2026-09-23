//! The repository gateway seam: local git and hosted GitHub behind one trait.

pub mod local;

use crate::contracts::{DiffSource, FileSnapshot, PinnedRange, SearchHit};
use crate::error::DtError;

pub trait PrGateway {
    fn pinned_range(&self) -> PinnedRange;
    fn diff(&self) -> DiffSource;
    fn read_file_at_head(&self, path: &str) -> Result<FileSnapshot, DtError>;
    fn list_files_at_head(&self) -> Result<Vec<String>, DtError>;
    fn search_at_head(&self, pattern: &str, cap: usize) -> Result<Vec<SearchHit>, DtError>;
}
