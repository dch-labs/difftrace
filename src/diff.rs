//! The unified-diff model: `parse` builds the queryable [`DiffIndex`],
//! whose `clamp_to_hunk` is the authority deciding which cited lines may
//! become review comments.

pub mod index;
pub mod parse;

pub use index::DiffIndex;
pub use index::DiffLine;
pub use index::FileDiff;
pub use index::Hunk;
