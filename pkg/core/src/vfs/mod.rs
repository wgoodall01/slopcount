//! A thin, read-only virtual filesystem.
//!
//! slopcount needs to count the same way over two very different sources: a
//! directory on disk, and a tree inside a git object database. Both are exposed
//! as a flat list of relative paths plus a way to stream the bytes at one of
//! them, which is all the counter ever needs.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::io::AsyncRead;

mod dir;
mod empty;
mod git;
mod ignore_vfs;

pub use dir::{DirVfs, IgnoreConfig};
pub use empty::EmptyVfs;
pub use git::GitVfs;
pub use ignore_vfs::{IgnoreRules, IgnoreVfs};

/// A boxed byte stream, as returned by [`Vfs::open`].
pub type FileStream = Box<dyn AsyncRead + Send + Unpin>;

/// One file visible through a [`Vfs`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    /// Path relative to the VFS root, always using `/` separators on unix.
    pub path: PathBuf,
    /// Size in bytes, when the source knows it without reading the file.
    pub size: Option<u64>,
    /// An opaque content identity — a git blob hash, for sources that have one.
    ///
    /// Two entries with equal, present ids are guaranteed to have identical
    /// contents. `None` means "unknown", never "empty".
    pub content_id: Option<String>,
}

impl Entry {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            size: None,
            content_id: None,
        }
    }
}

/// A read-only source of files.
#[async_trait]
pub trait Vfs: Send + Sync {
    /// A short description of the root, for error messages and headings.
    fn describe(&self) -> String;

    /// Every file the VFS exposes, in unspecified order.
    async fn list(&self) -> anyhow::Result<Vec<Entry>>;

    /// Stream the contents of one file.
    async fn open(&self, path: &Path) -> anyhow::Result<FileStream>;
}

#[async_trait]
impl<T: Vfs + ?Sized> Vfs for Box<T> {
    fn describe(&self) -> String {
        (**self).describe()
    }
    async fn list(&self) -> anyhow::Result<Vec<Entry>> {
        (**self).list().await
    }
    async fn open(&self, path: &Path) -> anyhow::Result<FileStream> {
        (**self).open(path).await
    }
}

#[async_trait]
impl<T: Vfs + ?Sized> Vfs for std::sync::Arc<T> {
    fn describe(&self) -> String {
        (**self).describe()
    }
    async fn list(&self) -> anyhow::Result<Vec<Entry>> {
        (**self).list().await
    }
    async fn open(&self, path: &Path) -> anyhow::Result<FileStream> {
        (**self).open(path).await
    }
}
