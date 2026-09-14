//! A [`Vfs`] wrapper that hides ignored files.
//!
//! [`DirVfs`](super::DirVfs) applies ignore rules while it walks, which lets it
//! prune whole directories and never stat them. That is not available to every
//! source: a [`GitVfs`](super::GitVfs) hands back a flat list of blobs, and any
//! ignore file lives *inside the tree* rather than on disk.
//!
//! [`IgnoreVfs`] closes that gap. It wraps any VFS, reads the ignore files out
//! of the wrapped VFS itself, and masks out everything they exclude.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;
use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;

use super::{Entry, FileStream, Vfs};

/// Directories that are version-control metadata, never source.
const VCS_DIRS: [&str; 4] = [".git", ".hg", ".svn", ".jj"];

/// Which files an [`IgnoreVfs`] hides.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    /// Names of ignore files to read from the wrapped VFS, in gitignore syntax.
    pub ignore_files: Vec<String>,
    /// Keep dotfiles and the contents of dot-directories.
    pub include_hidden: bool,
    /// Hide `.git` and friends even when hidden files are included.
    pub skip_vcs_dirs: bool,
}

impl Default for IgnoreRules {
    fn default() -> Self {
        Self {
            ignore_files: [".gitignore", ".ignore", ".slopcountignore"]
                .map(String::from)
                .to_vec(),
            include_hidden: false,
            skip_vcs_dirs: true,
        }
    }
}

impl IgnoreRules {
    /// Rules suitable for a git tree.
    ///
    /// `.gitignore` is deliberately left out: a file that git tracks despite
    /// matching `.gitignore` was committed on purpose, and hiding it would
    /// misreport what is actually in the tree.
    pub fn for_git_tree() -> Self {
        Self {
            ignore_files: [".ignore", ".slopcountignore"].map(String::from).to_vec(),
            ..Self::default()
        }
    }

    /// Hide nothing at all.
    pub fn none() -> Self {
        Self {
            ignore_files: Vec::new(),
            include_hidden: true,
            skip_vcs_dirs: false,
        }
    }

    /// Whether these rules could hide anything.
    fn is_noop(&self) -> bool {
        self.ignore_files.is_empty() && self.include_hidden && !self.skip_vcs_dirs
    }
}

/// Wraps a [`Vfs`] and masks out the files its ignore rules exclude.
///
/// The wrapped VFS is listed once and the result cached, so the ignore files
/// are read a single time and [`Vfs::open`] can reject a masked path without
/// re-reading them. A `Vfs` is a read-only snapshot, so this costs nothing in
/// freshness.
#[derive(Debug)]
pub struct IgnoreVfs<V: ?Sized> {
    rules: IgnoreRules,
    state: OnceCell<State>,
    inner: V,
}

#[derive(Debug)]
struct State {
    /// Every entry the wrapped VFS reported, ignored ones included.
    entries: Vec<Entry>,
    /// `(directory the file sat in, its rules)`, deepest directory first, so
    /// the most specific ignore file gets the first say.
    matchers: Vec<(PathBuf, Gitignore)>,
}

impl<V: Vfs> IgnoreVfs<V> {
    pub fn new(inner: V, rules: IgnoreRules) -> Self {
        Self {
            inner,
            rules,
            state: OnceCell::new(),
        }
    }

    /// Wrap with the default rules.
    pub fn with_defaults(inner: V) -> Self {
        Self::new(inner, IgnoreRules::default())
    }

    /// The wrapped VFS.
    pub fn inner(&self) -> &V {
        &self.inner
    }
}

impl<V: Vfs + ?Sized> IgnoreVfs<V> {
    /// List the wrapped VFS and build its ignore matchers, once.
    async fn state(&self) -> anyhow::Result<&State> {
        self.state
            .get_or_try_init(|| async {
                let entries = self.inner.list().await?;
                let mut matchers = Vec::new();

                for entry in &entries {
                    let Some(name) = entry.path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    if !self.rules.ignore_files.iter().any(|f| f == name) {
                        continue;
                    }

                    let directory = entry
                        .path
                        .parent()
                        .unwrap_or_else(|| Path::new(""))
                        .to_path_buf();
                    let text = self.read(&entry.path).await?;

                    // Patterns are resolved against the directory the ignore
                    // file sits in; we strip that prefix ourselves before
                    // matching, so the builder's own root is irrelevant.
                    let mut builder = GitignoreBuilder::new("");
                    for line in text.lines() {
                        builder
                            .add_line(None, line)
                            .map_err(|e| anyhow::anyhow!("in {}: {e}", entry.path.display()))?;
                    }
                    matchers.push((directory, builder.build()?));
                }

                // Deepest first: a nested ignore file overrides its parents.
                matchers.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.components().count()));

                Ok::<State, anyhow::Error>(State { entries, matchers })
            })
            .await
    }

    async fn read(&self, path: &Path) -> anyhow::Result<String> {
        let mut stream = self.inner.open(path).await?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Whether `path` is masked out.
    fn is_ignored(&self, state: &State, path: &Path) -> bool {
        let components = || path.components().filter_map(|c| c.as_os_str().to_str());

        if self.rules.skip_vcs_dirs && components().any(|c| VCS_DIRS.contains(&c)) {
            return true;
        }
        if !self.rules.include_hidden
            && components().any(|c| c.starts_with('.') && c != "." && c != "..")
        {
            return true;
        }

        for (directory, rules) in &state.matchers {
            // An ignore file only governs its own subtree.
            let Ok(relative) = path.strip_prefix(directory) else {
                continue;
            };
            // `matched_path_or_any_parents` also tests each parent directory,
            // which is what makes a rule like `target/` hide `target/a/b.rs`.
            match rules.matched_path_or_any_parents(relative, false) {
                Match::Ignore(_) => return true,
                // A negated rule (`!keep.rs`) settles it; stop looking at the
                // shallower ignore files this one overrides.
                Match::Whitelist(_) => return false,
                Match::None => continue,
            }
        }
        false
    }
}

#[async_trait]
impl<V: Vfs + ?Sized> Vfs for IgnoreVfs<V> {
    fn describe(&self) -> String {
        self.inner.describe()
    }

    async fn list(&self) -> anyhow::Result<Vec<Entry>> {
        if self.rules.is_noop() {
            return self.inner.list().await;
        }
        let state = self.state().await?;
        Ok(state
            .entries
            .iter()
            .filter(|e| !self.is_ignored(state, &e.path))
            .cloned()
            .collect())
    }

    async fn open(&self, path: &Path) -> anyhow::Result<FileStream> {
        if !self.rules.is_noop() {
            let state = self.state().await?;
            if self.is_ignored(state, path) {
                anyhow::bail!("{} is ignored", path.display());
            }
        }
        self.inner.open(path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An in-memory VFS, so these tests exercise the wrapper rather than a
    /// particular backing store.
    #[derive(Debug, Default)]
    struct MemVfs {
        files: Vec<(String, String)>,
        lists: AtomicUsize,
    }

    impl MemVfs {
        fn new(files: &[(&str, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(p, c)| (p.to_string(), c.to_string()))
                    .collect(),
                lists: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Vfs for MemVfs {
        fn describe(&self) -> String {
            "memory".to_string()
        }

        async fn list(&self) -> anyhow::Result<Vec<Entry>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            Ok(self.files.iter().map(|(p, _)| Entry::new(p)).collect())
        }

        async fn open(&self, path: &Path) -> anyhow::Result<FileStream> {
            let wanted = path.to_string_lossy().into_owned();
            let (_, contents) = self
                .files
                .iter()
                .find(|(p, _)| *p == wanted)
                .ok_or_else(|| anyhow::anyhow!("no such file: {wanted}"))?;
            Ok(Box::new(std::io::Cursor::new(
                contents.clone().into_bytes(),
            )))
        }
    }

    async fn visible(files: &[(&str, &str)], rules: IgnoreRules) -> BTreeSet<String> {
        let vfs = IgnoreVfs::new(MemVfs::new(files), rules);
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

    /// The default rules, but keeping dotfiles, so tests can see the ignore
    /// files themselves and assert on the gitignore logic in isolation.
    fn rules_showing_hidden() -> IgnoreRules {
        IgnoreRules {
            include_hidden: true,
            ..IgnoreRules::default()
        }
    }

    // -- pass-through --------------------------------------------------------

    #[tokio::test]
    async fn a_tree_with_no_ignore_files_is_unchanged() {
        let files = [("a.rs", ""), ("src/b.rs", "")];
        assert_eq!(
            visible(&files, IgnoreRules::default()).await,
            set(&["a.rs", "src/b.rs"])
        );
    }

    #[tokio::test]
    async fn describe_delegates_to_the_wrapped_vfs() {
        let vfs = IgnoreVfs::with_defaults(MemVfs::new(&[]));
        assert_eq!(vfs.describe(), "memory");
    }

    #[tokio::test]
    async fn open_delegates_for_a_visible_file() {
        let vfs = IgnoreVfs::with_defaults(MemVfs::new(&[("a.rs", "contents")]));
        let mut stream = vfs.open(Path::new("a.rs")).await.unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, "contents");
    }

    // -- gitignore semantics -------------------------------------------------

    #[tokio::test]
    async fn a_root_ignore_file_hides_matching_paths() {
        let files = [
            (".gitignore", "target/\n*.log\n"),
            ("a.rs", ""),
            ("target/b.rs", ""),
            ("target/deep/c.rs", ""),
            ("d.log", ""),
        ];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&[".gitignore", "a.rs"])
        );
    }

    #[tokio::test]
    async fn a_directory_rule_hides_everything_beneath_it() {
        let files = [
            (".gitignore", "vendor\n"),
            ("vendor/a/b/c.rs", ""),
            ("keep.rs", ""),
        ];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&[".gitignore", "keep.rs"])
        );
    }

    #[tokio::test]
    async fn a_nested_ignore_file_only_governs_its_own_subtree() {
        let files = [
            ("sub/.gitignore", "*.rs\n"),
            ("a.rs", ""),
            ("sub/b.rs", ""),
            ("sub/deep/c.rs", ""),
            ("sub/d.txt", ""),
        ];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&["a.rs", "sub/.gitignore", "sub/d.txt"])
        );
    }

    #[tokio::test]
    async fn a_nested_negation_overrides_a_parent_rule() {
        let files = [
            (".gitignore", "*.rs\n"),
            ("sub/.gitignore", "!keep.rs\n"),
            ("a.rs", ""),
            ("sub/keep.rs", ""),
            ("sub/other.rs", ""),
        ];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&[".gitignore", "sub/.gitignore", "sub/keep.rs"])
        );
    }

    #[tokio::test]
    async fn a_later_line_wins_within_one_ignore_file() {
        let files = [
            (".gitignore", "*.rs\n!keep.rs\n"),
            ("keep.rs", ""),
            ("drop.rs", ""),
        ];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&[".gitignore", "keep.rs"])
        );
    }

    #[tokio::test]
    async fn comments_and_blank_lines_in_an_ignore_file_are_ignored() {
        let files = [
            (".gitignore", "# a comment\n\n*.log\n"),
            ("a.rs", ""),
            ("b.log", ""),
        ];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&[".gitignore", "a.rs"])
        );
    }

    #[tokio::test]
    async fn every_configured_ignore_file_name_is_read() {
        let files = [
            (".gitignore", "a.rs\n"),
            (".ignore", "b.rs\n"),
            (".slopcountignore", "c.rs\n"),
            ("a.rs", ""),
            ("b.rs", ""),
            ("c.rs", ""),
            ("d.rs", ""),
        ];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&[".gitignore", ".ignore", ".slopcountignore", "d.rs"])
        );
    }

    #[tokio::test]
    async fn git_tree_rules_skip_gitignore_but_honour_the_others() {
        // A file git tracks despite `.gitignore` was committed on purpose.
        let files = [
            (".gitignore", "tracked.rs\n"),
            (".slopcountignore", "generated.rs\n"),
            ("tracked.rs", ""),
            ("generated.rs", ""),
        ];
        let rules = IgnoreRules {
            include_hidden: true,
            ..IgnoreRules::for_git_tree()
        };
        assert_eq!(
            visible(&files, rules).await,
            set(&[".gitignore", ".slopcountignore", "tracked.rs"])
        );
    }

    // -- hidden and VCS directories ------------------------------------------

    #[tokio::test]
    async fn hidden_files_are_masked_by_default() {
        let files = [("a.rs", ""), (".hidden/b.rs", ""), (".dotfile", "")];
        assert_eq!(
            visible(&files, IgnoreRules::default()).await,
            set(&["a.rs"])
        );
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&["a.rs", ".hidden/b.rs", ".dotfile"])
        );
    }

    #[tokio::test]
    async fn vcs_directories_are_masked_even_when_hidden_files_are_shown() {
        let files = [("a.rs", ""), (".git/config", ""), (".hg/store", "")];
        assert_eq!(
            visible(&files, rules_showing_hidden()).await,
            set(&["a.rs"])
        );
    }

    #[tokio::test]
    async fn rules_that_hide_nothing_pass_everything_through() {
        let files = [
            (".gitignore", "*.rs\n"),
            ("a.rs", ""),
            (".git/config", ""),
            (".hidden", ""),
        ];
        assert_eq!(
            visible(&files, IgnoreRules::none()).await,
            set(&[".gitignore", "a.rs", ".git/config", ".hidden"])
        );
    }

    // -- open() is masked too ------------------------------------------------

    #[tokio::test]
    async fn opening_an_ignored_file_is_an_error() {
        let vfs = IgnoreVfs::new(
            MemVfs::new(&[(".gitignore", "secret.rs\n"), ("secret.rs", "x")]),
            rules_showing_hidden(),
        );
        let err = match vfs.open(Path::new("secret.rs")).await {
            Err(err) => err,
            Ok(_) => panic!("an ignored file should not open"),
        };
        assert!(err.to_string().contains("ignored"), "got: {err}");
    }

    #[tokio::test]
    async fn opening_a_hidden_file_is_an_error() {
        let vfs = IgnoreVfs::with_defaults(MemVfs::new(&[(".env", "SECRET=1")]));
        assert!(vfs.open(Path::new(".env")).await.is_err());
    }

    // -- caching -------------------------------------------------------------

    #[tokio::test]
    async fn the_wrapped_vfs_is_listed_only_once() {
        let vfs = IgnoreVfs::new(
            MemVfs::new(&[(".gitignore", "b.rs\n"), ("a.rs", "x"), ("b.rs", "y")]),
            rules_showing_hidden(),
        );
        vfs.list().await.unwrap();
        vfs.list().await.unwrap();
        vfs.open(Path::new("a.rs")).await.unwrap();
        assert_eq!(vfs.inner().lists.load(Ordering::SeqCst), 1);
    }

    // -- composition ---------------------------------------------------------

    #[tokio::test]
    async fn wrappers_compose() {
        let inner = IgnoreVfs::new(
            MemVfs::new(&[
                (".slopcountignore", "generated.rs\n"),
                ("a.rs", ""),
                ("generated.rs", ""),
                ("b.log", ""),
            ]),
            rules_showing_hidden(),
        );
        let outer = IgnoreVfs::new(
            inner,
            IgnoreRules {
                ignore_files: vec![".slopcountignore".to_string()],
                include_hidden: false,
                skip_vcs_dirs: true,
            },
        );
        // The inner wrapper drops `generated.rs`; the outer one drops the
        // dotfile it used to do so.
        let paths: BTreeSet<String> = outer
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, set(&["a.rs", "b.log"]));
    }

    #[tokio::test]
    async fn a_malformed_ignore_file_is_reported_with_its_path() {
        let vfs = IgnoreVfs::new(
            MemVfs::new(&[("sub/.gitignore", "a{b\n")]),
            rules_showing_hidden(),
        );
        let err = vfs.list().await.unwrap_err();
        assert!(
            err.to_string().contains("sub/.gitignore"),
            "unhelpful error: {err}"
        );
    }
}
