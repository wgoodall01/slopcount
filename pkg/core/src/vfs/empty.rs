//! A [`Vfs`] with nothing in it.

use std::path::Path;

use async_trait::async_trait;

use super::{Entry, FileStream, Vfs};

/// A source that contains no files.
///
/// Stands in for one side of a comparison that does not exist — a directory
/// added on a branch has no counterpart in the branch it is compared against —
/// so the diff reports every line as added rather than failing.
#[derive(Debug, Clone)]
pub struct EmptyVfs {
    description: String,
}

impl EmptyVfs {
    /// `description` is what [`Vfs::describe`] reports, so it should still say
    /// what was looked for and not merely that it was empty.
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
        }
    }
}

#[async_trait]
impl Vfs for EmptyVfs {
    fn describe(&self) -> String {
        self.description.clone()
    }

    async fn list(&self) -> anyhow::Result<Vec<Entry>> {
        Ok(Vec::new())
    }

    async fn open(&self, path: &Path) -> anyhow::Result<FileStream> {
        anyhow::bail!(
            "{} is empty, so {} cannot be read",
            self.description,
            path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Report;
    use crate::walk::{walk, Globs, WalkConfig};

    #[tokio::test]
    async fn it_lists_nothing() {
        let vfs = EmptyVfs::new("origin/main:src (absent)");
        assert!(vfs.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn describe_still_says_what_was_looked_for() {
        let vfs = EmptyVfs::new("origin/main:src (absent)");
        assert_eq!(vfs.describe(), "origin/main:src (absent)");
    }

    #[tokio::test]
    async fn opening_anything_is_an_error() {
        let vfs = EmptyVfs::new("nothing");
        assert!(vfs.open(Path::new("a.rs")).await.is_err());
    }

    #[tokio::test]
    async fn walking_it_gives_an_empty_report() {
        let vfs = EmptyVfs::new("nothing");
        let report = walk(&vfs, &Globs::default(), &WalkConfig::default())
            .await
            .unwrap();
        assert_eq!(report, Report::new());
    }
}
