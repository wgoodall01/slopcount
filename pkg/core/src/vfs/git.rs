//! A [`Vfs`] over a tree in a git object database.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use gix::ThreadSafeRepository;

use super::{Entry, FileStream, Vfs};

/// Some gix errors append their own source location (`, at /…/parse.rs:496`),
/// which is noise in a message aimed at someone who mistyped a branch name.
fn tidy(error: impl std::fmt::Display) -> String {
    let text = error.to_string();
    match text.split_once(", at /") {
        Some((message, _)) => message.to_string(),
        None => text,
    }
}

/// A [`Vfs`] over one tree, optionally narrowed to a subdirectory of it.
#[derive(Debug)]
pub struct GitVfs {
    repo: ThreadSafeRepository,
    /// The revision as the user wrote it, for `describe`.
    revision: String,
    /// The resolved tree object.
    tree_id: gix::ObjectId,
    /// Path within the tree that the VFS is rooted at, if any.
    subpath: Option<PathBuf>,
}

impl GitVfs {
    /// Open the repository containing `path` and resolve `revision` to a tree.
    ///
    /// `subpath` narrows the VFS to a subdirectory, and is interpreted relative
    /// to the repository root.
    pub fn open(
        path: impl AsRef<Path>,
        revision: &str,
        subpath: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::open_optional_subpath(path, revision, subpath)?.ok_or_else(|| {
            let sub = subpath.unwrap_or_else(|| Path::new("")).display();
            anyhow::anyhow!("path {sub} does not exist in revision {revision:?}")
        })
    }

    /// As [`GitVfs::open`], but distinguishes "that subdirectory is not in this
    /// tree" from "that revision does not exist".
    ///
    /// `Ok(None)` means only the former. A directory that exists on one side of
    /// a comparison and not the other is ordinary — it was added or removed —
    /// and the caller can treat the missing side as empty rather than failing.
    pub fn open_optional_subpath(
        path: impl AsRef<Path>,
        revision: &str,
        subpath: Option<&Path>,
    ) -> anyhow::Result<Option<Self>> {
        let repo = gix::discover(path.as_ref())?;

        let mut tree = repo
            .rev_parse_single(revision)
            .map_err(|e| anyhow::anyhow!("could not resolve revision {revision:?}: {}", tidy(e)))?
            .object()?
            .peel_to_tree()?;

        if let Some(sub) = subpath {
            let sub_str = sub.to_string_lossy().replace('\\', "/");
            let sub_str = sub_str.trim_matches('/');
            if !sub_str.is_empty() {
                let Some(entry) = tree.peel_to_entry_by_path(Path::new(sub_str))? else {
                    return Ok(None);
                };
                tree = entry.object()?.peel_to_tree().map_err(|_| {
                    anyhow::anyhow!("path {sub_str:?} in {revision:?} is not a directory")
                })?;
            }
        }

        let tree_id = tree.id;
        drop(tree);
        Ok(Some(Self {
            tree_id,
            repo: repo.into_sync(),
            revision: revision.to_string(),
            subpath: subpath.map(Path::to_path_buf),
        }))
    }

    /// The commit or tree this VFS resolved to.
    pub fn tree_id(&self) -> gix::ObjectId {
        self.tree_id
    }

    fn tree<'a>(&self, repo: &'a gix::Repository) -> anyhow::Result<gix::Tree<'a>> {
        Ok(repo.find_object(self.tree_id)?.peel_to_tree()?)
    }
}

#[async_trait]
impl Vfs for GitVfs {
    fn describe(&self) -> String {
        match &self.subpath {
            Some(sub) if !sub.as_os_str().is_empty() => {
                format!("{}:{}", self.revision, sub.display())
            }
            _ => self.revision.clone(),
        }
    }

    async fn list(&self) -> anyhow::Result<Vec<Entry>> {
        let repo = self.repo.to_thread_local();
        let tree = self.tree(&repo)?;

        let mut recorder = gix::traverse::tree::Recorder::default();
        tree.traverse().breadthfirst(&mut recorder)?;

        let mut entries = Vec::new();
        for record in recorder.records {
            // Blobs only: skip directories, submodules and symlinks.
            if !record.mode.is_blob() {
                continue;
            }
            let path = String::from_utf8_lossy(&record.filepath).into_owned();
            entries.push(Entry {
                path: PathBuf::from(path),
                size: None,
                content_id: Some(record.oid.to_string()),
            });
        }
        Ok(entries)
    }

    async fn open(&self, path: &Path) -> anyhow::Result<FileStream> {
        let repo = self.repo.to_thread_local();
        let mut tree = self.tree(&repo)?;
        let entry = tree.peel_to_entry_by_path(path)?.ok_or_else(|| {
            anyhow::anyhow!("{} not found in {}", path.display(), self.describe())
        })?;
        // Blobs are already materialised in memory by the object database, so
        // there is nothing to stream from; hand back a cursor over the bytes.
        let data = entry.object()?.detach().data;
        Ok(Box::new(std::io::Cursor::new(data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::process::Command;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;

    /// A scratch git repository driven through the `git` CLI, so the fixtures
    /// are built by the same tool users will point slopcount at.
    struct Repo(TempDir);

    impl Repo {
        fn init() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let repo = Repo(dir);
            repo.git(&["init", "-q", "-b", "main"]);
            repo.git(&["config", "user.email", "test@example.com"]);
            repo.git(&["config", "user.name", "Test"]);
            repo
        }

        fn path(&self) -> &Path {
            self.0.path()
        }

        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .args(args)
                .current_dir(self.path())
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn write(&self, path: &str, contents: &str) {
            let full = self.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }

        fn commit(&self, message: &str) -> String {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", message]);
            self.git(&["rev-parse", "HEAD"])
        }
    }

    async fn paths(vfs: &GitVfs) -> BTreeSet<String> {
        vfs.list()
            .await
            .expect("list")
            .into_iter()
            .map(|e| e.path.to_string_lossy().into_owned())
            .collect()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // -- listing and reading -------------------------------------------------

    #[tokio::test]
    async fn lists_the_files_of_a_commit() {
        let repo = Repo::init();
        repo.write("a.rs", "fn a() {}\n");
        repo.write("src/b.rs", "fn b() {}\n");
        repo.commit("first");

        let vfs = GitVfs::open(repo.path(), "HEAD", None).unwrap();
        assert_eq!(paths(&vfs).await, set(&["a.rs", "src/b.rs"]));
    }

    #[tokio::test]
    async fn reads_blob_contents() {
        let repo = Repo::init();
        repo.write("a.rs", "fn a() {}\n");
        repo.commit("first");

        let vfs = GitVfs::open(repo.path(), "HEAD", None).unwrap();
        let mut stream = vfs.open(Path::new("a.rs")).await.unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, "fn a() {}\n");
    }

    #[tokio::test]
    async fn sees_the_old_contents_of_an_older_revision() {
        let repo = Repo::init();
        repo.write("a.rs", "old\n");
        let first = repo.commit("first");
        repo.write("a.rs", "new\n");
        repo.commit("second");

        let old = GitVfs::open(repo.path(), &first, None).unwrap();
        let mut buf = String::new();
        old.open(Path::new("a.rs"))
            .await
            .unwrap()
            .read_to_string(&mut buf)
            .await
            .unwrap();
        assert_eq!(buf, "old\n");
    }

    #[tokio::test]
    async fn a_file_deleted_later_is_still_present_in_the_older_tree() {
        let repo = Repo::init();
        repo.write("gone.rs", "x\n");
        repo.write("stays.rs", "y\n");
        let first = repo.commit("first");
        std::fs::remove_file(repo.path().join("gone.rs")).unwrap();
        repo.commit("second");

        let old = GitVfs::open(repo.path(), &first, None).unwrap();
        assert_eq!(paths(&old).await, set(&["gone.rs", "stays.rs"]));
        let new = GitVfs::open(repo.path(), "HEAD", None).unwrap();
        assert_eq!(paths(&new).await, set(&["stays.rs"]));
    }

    #[tokio::test]
    async fn untracked_and_ignored_files_are_invisible() {
        let repo = Repo::init();
        repo.write(".gitignore", "ignored.rs\n");
        repo.write("tracked.rs", "x\n");
        repo.commit("first");
        repo.write("untracked.rs", "y\n");
        repo.write("ignored.rs", "z\n");

        let vfs = GitVfs::open(repo.path(), "HEAD", None).unwrap();
        assert_eq!(paths(&vfs).await, set(&[".gitignore", "tracked.rs"]));
    }

    #[tokio::test]
    async fn a_subpath_narrows_the_tree_and_rebases_paths() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.write("src/b.rs", "y\n");
        repo.write("src/deep/c.rs", "z\n");
        repo.commit("first");

        let vfs = GitVfs::open(repo.path(), "HEAD", Some(Path::new("src"))).unwrap();
        assert_eq!(paths(&vfs).await, set(&["b.rs", "deep/c.rs"]));

        let mut buf = String::new();
        vfs.open(Path::new("b.rs"))
            .await
            .unwrap()
            .read_to_string(&mut buf)
            .await
            .unwrap();
        assert_eq!(buf, "y\n");
    }

    #[tokio::test]
    async fn an_empty_subpath_is_the_whole_tree() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.commit("first");

        let vfs = GitVfs::open(repo.path(), "HEAD", Some(Path::new(""))).unwrap();
        assert_eq!(paths(&vfs).await, set(&["a.rs"]));
    }

    #[tokio::test]
    async fn entries_carry_the_blob_hash_as_a_content_id() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.write("same.rs", "x\n");
        repo.write("other.rs", "y\n");
        repo.commit("first");

        let vfs = GitVfs::open(repo.path(), "HEAD", None).unwrap();
        let entries = vfs.list().await.unwrap();
        let id = |name: &str| {
            entries
                .iter()
                .find(|e| e.path.to_str() == Some(name))
                .unwrap()
                .content_id
                .clone()
                .unwrap()
        };
        // Identical contents share a blob, which is what lets the diff skip
        // unchanged files.
        assert_eq!(id("a.rs"), id("same.rs"));
        assert_ne!(id("a.rs"), id("other.rs"));
    }

    #[tokio::test]
    async fn branches_and_tags_resolve() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.commit("first");
        repo.git(&["tag", "v1"]);

        for spec in ["main", "v1", "HEAD"] {
            let vfs = GitVfs::open(repo.path(), spec, None).unwrap();
            assert_eq!(paths(&vfs).await, set(&["a.rs"]), "spec {spec}");
        }
    }

    #[tokio::test]
    async fn the_repository_is_discovered_from_a_subdirectory() {
        let repo = Repo::init();
        repo.write("src/b.rs", "y\n");
        repo.commit("first");

        let vfs = GitVfs::open(repo.path().join("src"), "HEAD", None).unwrap();
        assert_eq!(paths(&vfs).await, set(&["src/b.rs"]));
    }

    #[tokio::test]
    async fn describe_names_the_revision_and_subpath() {
        let repo = Repo::init();
        repo.write("src/b.rs", "y\n");
        repo.commit("first");

        let whole = GitVfs::open(repo.path(), "HEAD", None).unwrap();
        assert_eq!(whole.describe(), "HEAD");
        let sub = GitVfs::open(repo.path(), "HEAD", Some(Path::new("src"))).unwrap();
        assert_eq!(sub.describe(), "HEAD:src");
    }

    // -- error paths ---------------------------------------------------------

    #[test]
    fn gix_source_locations_are_stripped_from_messages() {
        assert_eq!(
            tidy("couldn't parse revision: \"x\", at /home/u/.cargo/registry/a/b.rs:496"),
            "couldn't parse revision: \"x\""
        );
        assert_eq!(tidy("a plain message"), "a plain message");
    }

    #[tokio::test]
    async fn an_unknown_revision_is_an_error() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.commit("first");

        let err = GitVfs::open(repo.path(), "no-such-ref", None).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("no-such-ref"),
            "unhelpful error: {message}"
        );
        assert!(
            !message.contains(".cargo/registry"),
            "error leaks a gix source path: {message}"
        );
    }

    #[tokio::test]
    async fn a_missing_subpath_is_an_error() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.commit("first");

        assert!(GitVfs::open(repo.path(), "HEAD", Some(Path::new("nope"))).is_err());
    }

    #[tokio::test]
    async fn a_subpath_that_is_a_file_is_an_error() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.commit("first");

        assert!(GitVfs::open(repo.path(), "HEAD", Some(Path::new("a.rs"))).is_err());
    }

    #[tokio::test]
    async fn opening_a_path_that_is_not_in_the_tree_is_an_error() {
        let repo = Repo::init();
        repo.write("a.rs", "x\n");
        repo.commit("first");

        let vfs = GitVfs::open(repo.path(), "HEAD", None).unwrap();
        assert!(vfs.open(Path::new("nope.rs")).await.is_err());
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_repository_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(GitVfs::open(dir.path(), "HEAD", None).is_err());
    }
}
