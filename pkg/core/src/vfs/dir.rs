//! A [`Vfs`] over a real directory, honouring ignore files.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ignore::WalkBuilder;

use super::{Entry, FileStream, Vfs};

/// Which ignore rules a [`DirVfs`] applies while walking.
#[derive(Debug, Clone)]
pub struct IgnoreConfig {
    /// Honour `.gitignore`, `.git/info/exclude` and the global gitignore.
    pub respect_gitignore: bool,
    /// Honour `.ignore` and `.slopcountignore`.
    pub respect_ignore_files: bool,
    /// Descend into dot-directories and count dotfiles.
    pub include_hidden: bool,
    /// Never descend into `.git`, regardless of the above.
    pub skip_vcs_dirs: bool,
    /// Follow symbolic links.
    pub follow_links: bool,
    /// Extra ignore-file names to honour, in addition to the defaults.
    pub custom_ignore_files: Vec<String>,
}

impl Default for IgnoreConfig {
    fn default() -> Self {
        Self {
            respect_gitignore: true,
            respect_ignore_files: true,
            include_hidden: false,
            skip_vcs_dirs: true,
            follow_links: false,
            custom_ignore_files: vec![".slopcountignore".to_string()],
        }
    }
}

impl IgnoreConfig {
    /// Count everything, ignoring every ignore file.
    pub fn none() -> Self {
        Self {
            respect_gitignore: false,
            respect_ignore_files: false,
            include_hidden: true,
            skip_vcs_dirs: true,
            follow_links: false,
            custom_ignore_files: Vec::new(),
        }
    }
}

/// A [`Vfs`] rooted at a directory on disk.
#[derive(Debug, Clone)]
pub struct DirVfs {
    root: PathBuf,
    ignore: IgnoreConfig,
}

impl DirVfs {
    pub fn new(root: impl Into<PathBuf>, ignore: IgnoreConfig) -> Self {
        Self {
            root: root.into(),
            ignore,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl Vfs for DirVfs {
    fn describe(&self) -> String {
        self.root.display().to_string()
    }

    async fn list(&self) -> anyhow::Result<Vec<Entry>> {
        let root = self.root.clone();
        let cfg = self.ignore.clone();

        // The `ignore` walker is synchronous and does real filesystem work, so
        // it belongs on the blocking pool rather than the async runtime.
        tokio::task::spawn_blocking(move || {
            if !root.exists() {
                anyhow::bail!("no such path: {}", root.display());
            }

            let mut builder = WalkBuilder::new(&root);
            builder
                .hidden(!cfg.include_hidden)
                .follow_links(cfg.follow_links)
                .git_ignore(cfg.respect_gitignore)
                .git_global(cfg.respect_gitignore)
                .git_exclude(cfg.respect_gitignore)
                .ignore(cfg.respect_ignore_files)
                .parents(true)
                // Apply `.gitignore` even when the directory is not itself a
                // git repository; that is what a user means by "sensible".
                .require_git(false);
            if cfg.respect_ignore_files {
                for name in &cfg.custom_ignore_files {
                    builder.add_custom_ignore_filename(name);
                }
            }
            if cfg.skip_vcs_dirs {
                let mut overrides = ignore::overrides::OverrideBuilder::new(&root);
                for dir in ["!.git/**", "!.hg/**", "!.svn/**", "!.jj/**"] {
                    overrides.add(dir)?;
                }
                builder.overrides(overrides.build()?);
            }

            let mut entries = Vec::new();
            for result in builder.build() {
                let entry = match result {
                    Ok(entry) => entry,
                    // A single unreadable directory should not abort the walk.
                    Err(_) => continue,
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let Ok(relative) = entry.path().strip_prefix(&root) else {
                    continue;
                };
                entries.push(Entry {
                    path: relative.to_path_buf(),
                    size: entry.metadata().ok().map(|m| m.len()),
                    content_id: None,
                });
            }
            Ok(entries)
        })
        .await?
    }

    async fn open(&self, path: &Path) -> anyhow::Result<FileStream> {
        let full = self.root.join(path);
        let file = tokio::fs::File::open(&full).await?;
        Ok(Box::new(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;

    /// Build a directory tree from `(relative path, contents)` pairs.
    fn tree(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (path, contents) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, contents).unwrap();
        }
        dir
    }

    async fn paths(vfs: &DirVfs) -> BTreeSet<String> {
        vfs.list()
            .await
            .expect("list")
            .into_iter()
            .map(|e| e.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn lists_files_recursively_with_relative_paths() {
        let dir = tree(&[("a.rs", "x"), ("src/b.rs", "y"), ("src/deep/c.rs", "z")]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(
            paths(&vfs).await,
            set(&["a.rs", "src/b.rs", "src/deep/c.rs"])
        );
    }

    #[tokio::test]
    async fn directories_are_not_listed_as_entries() {
        let dir = tree(&[("src/b.rs", "y")]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(paths(&vfs).await, set(&["src/b.rs"]));
    }

    #[tokio::test]
    async fn an_empty_directory_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert!(paths(&vfs).await.is_empty());
    }

    #[tokio::test]
    async fn listing_a_missing_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = DirVfs::new(dir.path().join("nope"), IgnoreConfig::default());
        assert!(vfs.list().await.is_err());
    }

    #[tokio::test]
    async fn open_streams_file_contents() {
        let dir = tree(&[("src/b.rs", "hello world")]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        let mut stream = vfs.open(Path::new("src/b.rs")).await.unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, "hello world");
    }

    #[tokio::test]
    async fn open_reads_a_file_larger_than_one_buffer() {
        let big = "x".repeat(500_000);
        let dir = tree(&[("big.rs", big.as_str())]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        let mut stream = vfs.open(Path::new("big.rs")).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf.len(), 500_000);
    }

    #[tokio::test]
    async fn opening_a_missing_file_is_an_error() {
        let dir = tree(&[("a.rs", "x")]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert!(vfs.open(Path::new("nope.rs")).await.is_err());
    }

    #[tokio::test]
    async fn entries_report_file_size() {
        let dir = tree(&[("a.rs", "12345")]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        let entries = vfs.list().await.unwrap();
        assert_eq!(entries[0].size, Some(5));
    }

    // -- ignore handling -----------------------------------------------------

    #[tokio::test]
    async fn gitignore_is_respected() {
        let dir = tree(&[
            (".gitignore", "target/\n*.log\n"),
            ("a.rs", "x"),
            ("target/b.rs", "y"),
            ("c.log", "z"),
        ]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(paths(&vfs).await, set(&["a.rs"]));
    }

    #[tokio::test]
    async fn nested_gitignores_apply_to_their_subtree() {
        let dir = tree(&[
            ("a.rs", "x"),
            ("sub/.gitignore", "*.rs\n"),
            ("sub/b.rs", "y"),
            ("sub/c.txt", "z"),
        ]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(paths(&vfs).await, set(&["a.rs", "sub/c.txt"]));
    }

    #[tokio::test]
    async fn slopcountignore_is_respected() {
        let dir = tree(&[
            (".slopcountignore", "vendor/\n"),
            ("a.rs", "x"),
            ("vendor/b.rs", "y"),
        ]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(paths(&vfs).await, set(&["a.rs"]));
    }

    #[tokio::test]
    async fn dot_ignore_files_are_respected() {
        let dir = tree(&[
            (".ignore", "skipme.rs\n"),
            ("a.rs", "x"),
            ("skipme.rs", "y"),
        ]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(paths(&vfs).await, set(&["a.rs"]));
    }

    #[tokio::test]
    async fn ignore_rules_can_be_switched_off() {
        let dir = tree(&[
            (".gitignore", "target/\n"),
            ("a.rs", "x"),
            ("target/b.rs", "y"),
        ]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::none());
        assert_eq!(
            paths(&vfs).await,
            set(&[".gitignore", "a.rs", "target/b.rs"])
        );
    }

    #[tokio::test]
    async fn hidden_files_are_skipped_by_default_but_can_be_included() {
        let dir = tree(&[("a.rs", "x"), (".hidden/b.rs", "y"), (".dotfile", "z")]);

        let default = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(paths(&default).await, set(&["a.rs"]));

        let hidden = DirVfs::new(dir.path(), IgnoreConfig::none());
        assert_eq!(
            paths(&hidden).await,
            set(&["a.rs", ".hidden/b.rs", ".dotfile"])
        );
    }

    #[tokio::test]
    async fn the_git_directory_is_never_walked() {
        let dir = tree(&[
            ("a.rs", "x"),
            (".git/config", "y"),
            (".git/objects/ab/cd", "z"),
        ]);
        // Even with every ignore rule disabled and hidden files included.
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::none());
        assert_eq!(paths(&vfs).await, set(&["a.rs"]));
    }

    #[tokio::test]
    async fn dir_entries_have_no_content_id() {
        let dir = tree(&[("a.rs", "x")]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(vfs.list().await.unwrap()[0].content_id, None);
    }

    #[tokio::test]
    async fn describe_names_the_root() {
        let dir = tree(&[("a.rs", "x")]);
        let vfs = DirVfs::new(dir.path(), IgnoreConfig::default());
        assert_eq!(vfs.describe(), dir.path().display().to_string());
    }
}
