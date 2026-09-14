//! Count lines of code in a directory or a git tree, broken down by whether
//! they are code, comments, blanks — and whether they are tests.

pub mod count;
pub mod lang;
pub mod report;
pub mod vfs;
pub mod walk;

pub use count::{count, CountConfig, CountOutcome};
pub use lang::{registry, LanguageId, Registry};
pub use report::{diff_by, Counts, FileReport, Report, SignedStats, Stats};
pub use vfs::{DirVfs, EmptyVfs, Entry, GitVfs, IgnoreConfig, IgnoreRules, IgnoreVfs, Vfs};
pub use walk::{changed_paths, walk, walk_detailed, walk_shared, Globs, WalkConfig};
