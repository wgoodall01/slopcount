//! Walking a [`Vfs`] and counting everything in it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use globset::{Glob, GlobSet, GlobSetBuilder};
use tokio::io::AsyncReadExt;

use crate::count::{count, CountConfig, CountOutcome};
use crate::lang::{registry, LanguageId};
use crate::report::{FileReport, Report};
use crate::vfs::{Entry, Vfs};

/// Path filters applied to every entry a [`Vfs`] produces.
#[derive(Debug, Default, Clone)]
pub struct Globs {
    /// If non-empty, only paths matching one of these are counted.
    pub include: Vec<String>,
    /// Paths matching one of these are never counted.
    pub exclude: Vec<String>,
}

impl Globs {
    pub fn new(include: Vec<String>, exclude: Vec<String>) -> Self {
        Self { include, exclude }
    }

    fn compile(&self) -> anyhow::Result<CompiledGlobs> {
        Ok(CompiledGlobs {
            include: build(&self.include)?,
            exclude: build(&self.exclude)?,
            has_include: !self.include.is_empty(),
        })
    }
}

struct CompiledGlobs {
    include: GlobSet,
    exclude: GlobSet,
    has_include: bool,
}

impl CompiledGlobs {
    fn allows(&self, path: &std::path::Path) -> bool {
        if self.exclude.is_match(path) {
            return false;
        }
        !self.has_include || self.include.is_match(path)
    }
}

/// Compile user-supplied globs, being forgiving about the two shapes people
/// actually type: `*.rs` should match at any depth, and `src/` should mean
/// everything beneath it.
fn build(patterns: &[String]) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        builder.add(Glob::new(pattern)?);
        if let Some(dir) = pattern.strip_suffix('/') {
            builder.add(Glob::new(&format!("{dir}/**"))?);
        } else if !pattern.contains('/') {
            builder.add(Glob::new(&format!("**/{pattern}"))?);
        }
    }
    Ok(builder.build()?)
}

/// How to walk and what to count.
#[derive(Debug, Clone)]
pub struct WalkConfig {
    /// Line-classification settings.
    pub count: CountConfig,
    /// Count only these languages, when set.
    pub languages: Option<Vec<LanguageId>>,
    /// Count files whose language could not be determined.
    pub include_unknown: bool,
    /// Read the first line of extension-less files to look for a shebang.
    pub use_shebangs: bool,
    /// Skip files larger than this, in bytes. Generated blobs and vendored
    /// bundles dominate totals in a way nobody means.
    pub max_file_size: Option<u64>,
    /// How many files to count concurrently.
    pub concurrency: usize,
    /// When set, count only these exact paths. Used by the diff, which already
    /// knows which files changed and need not re-read the rest of the tree.
    pub only_paths: Option<HashSet<PathBuf>>,
}

impl Default for WalkConfig {
    fn default() -> Self {
        Self {
            count: CountConfig::default(),
            languages: None,
            include_unknown: false,
            use_shebangs: true,
            max_file_size: Some(2 * 1024 * 1024),
            concurrency: 64,
            only_paths: None,
        }
    }
}

/// A file that could not be counted.
#[derive(Debug)]
pub struct WalkError {
    pub path: PathBuf,
    pub error: anyhow::Error,
}

/// Count every file in `vfs` that passes `globs`.
///
/// Files are counted concurrently, but on a single task: the classifier is
/// CPU-bound, so a large tree saturates one core. Prefer [`walk_shared`] when
/// the VFS can be put in an [`Arc`] — it spreads the same work across the
/// runtime's worker threads.
pub async fn walk<V>(vfs: &V, globs: &Globs, config: &WalkConfig) -> anyhow::Result<Report>
where
    V: Vfs + ?Sized,
{
    Ok(walk_detailed(vfs, globs, config).await?.0)
}

/// As [`walk`], but also returns the files that were skipped because of an
/// error, so a caller can surface them.
pub async fn walk_detailed<V>(
    vfs: &V,
    globs: &Globs,
    config: &WalkConfig,
) -> anyhow::Result<(Report, Vec<WalkError>)>
where
    V: Vfs + ?Sized,
{
    let candidates = candidates(vfs, globs, config).await?;

    let results = stream::iter(candidates)
        .map(|entry| async move { count_entry(vfs, entry, config).await })
        .buffer_unordered(config.concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

    Ok(collect(results))
}

/// As [`walk`], but each file is counted in its own task, so the work is spread
/// across a multi-threaded runtime rather than serialised onto one core.
pub async fn walk_shared<V>(
    vfs: Arc<V>,
    globs: &Globs,
    config: &WalkConfig,
) -> anyhow::Result<(Report, Vec<WalkError>)>
where
    V: Vfs + ?Sized + 'static,
{
    let candidates = candidates(&*vfs, globs, config).await?;
    // Shared rather than cloned per file: the config carries the set of paths
    // the diff restricted the walk to, which can be large.
    let config = Arc::new(config.clone());

    let results = stream::iter(candidates)
        .map(|entry| {
            let vfs = Arc::clone(&vfs);
            let config = Arc::clone(&config);
            tokio::spawn(async move { count_entry(&*vfs, entry, &config).await })
        })
        .buffer_unordered(config.concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

    // A panic in a counting task should not take down the whole walk.
    let results = results
        .into_iter()
        .map(|joined| match joined {
            Ok(result) => result,
            Err(e) => Err(WalkError {
                path: PathBuf::from("<unknown>"),
                error: anyhow::anyhow!("counting task failed: {e}"),
            }),
        })
        .collect();

    Ok(collect(results))
}

/// The entries of `vfs` that survive every filter in `globs` and `config`.
async fn candidates<V>(vfs: &V, globs: &Globs, config: &WalkConfig) -> anyhow::Result<Vec<Entry>>
where
    V: Vfs + ?Sized,
{
    let globs = globs.compile()?;
    Ok(vfs
        .list()
        .await?
        .into_iter()
        .filter(|e| globs.allows(&e.path))
        .filter(|e| match &config.only_paths {
            Some(only) => only.contains(&e.path),
            None => true,
        })
        .filter(|e| match (config.max_file_size, e.size) {
            (Some(max), Some(size)) => size <= max,
            _ => true,
        })
        .collect())
}

fn collect(results: Vec<Result<Option<FileReport>, WalkError>>) -> (Report, Vec<WalkError>) {
    let mut report = Report::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(Some(file)) => report.files.push(file),
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
    }
    (report, errors)
}

/// Count a single entry. `Ok(None)` means the file was deliberately skipped.
async fn count_entry<V>(
    vfs: &V,
    entry: Entry,
    config: &WalkConfig,
) -> Result<Option<FileReport>, WalkError>
where
    V: Vfs + ?Sized,
{
    let registry = registry();
    let path = entry.path;

    let mut language = registry.from_path(&path);

    // Extension-less files are usually scripts; `#!/usr/bin/env python3` is the
    // only thing that identifies them.
    if language.is_none() && config.use_shebangs && path.extension().is_none() {
        language = peek_shebang(vfs, &path).await;
    }

    if language.is_none() && !config.include_unknown {
        return Ok(None);
    }
    if let (Some(filter), Some(id)) = (&config.languages, language) {
        if !filter.contains(&id) {
            return Ok(None);
        }
    } else if config.languages.is_some() && language.is_none() {
        return Ok(None);
    }

    let is_test_file = registry.path_is_test(&path, language);

    let stream = vfs.open(&path).await.map_err(|error| WalkError {
        path: path.clone(),
        error,
    })?;

    match count(stream, language, is_test_file, &config.count).await {
        Ok(CountOutcome::Counted(stats)) => Ok(Some(FileReport {
            path,
            language,
            is_test_file,
            stats,
        })),
        Ok(CountOutcome::Binary) => Ok(None),
        Err(e) => Err(WalkError {
            path,
            error: e.into(),
        }),
    }
}

/// Read just enough of a file to check for a shebang line.
async fn peek_shebang<V>(vfs: &V, path: &std::path::Path) -> Option<LanguageId>
where
    V: Vfs + ?Sized,
{
    const PEEK: usize = 128;
    let mut stream = vfs.open(path).await.ok()?;
    let mut buf = [0u8; PEEK];
    let mut filled = 0;
    // A short read is not EOF, so loop until the buffer is full or the file is.
    while filled < PEEK {
        match stream.read(&mut buf[filled..]).await {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    let head = &buf[..filled];
    let first_line = head.split(|b| *b == b'\n').next()?;
    registry().from_shebang(std::str::from_utf8(first_line).ok()?)
}

/// Which paths differ between two VFS listings.
///
/// Entries are considered unchanged only when both sides report the same
/// [`Entry::content_id`]; a source that cannot identify its content (a plain
/// directory) reports every one of its files as changed, which is correct but
/// conservative.
pub fn changed_paths(before: &[Entry], after: &[Entry]) -> HashSet<PathBuf> {
    use std::collections::HashMap;

    let index: HashMap<&PathBuf, &Option<String>> =
        before.iter().map(|e| (&e.path, &e.content_id)).collect();

    let mut changed: HashSet<PathBuf> = HashSet::new();
    for entry in after {
        let unchanged = match index.get(&entry.path) {
            Some(before_id) => entry.content_id.is_some() && *before_id == &entry.content_id,
            None => false,
        };
        if !unchanged {
            changed.insert(entry.path.clone());
        }
    }
    // Deletions: present before, gone after.
    let after_paths: HashSet<&PathBuf> = after.iter().map(|e| &e.path).collect();
    for entry in before {
        if !after_paths.contains(&entry.path) {
            changed.insert(entry.path.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::registry;
    use crate::vfs::{DirVfs, IgnoreConfig};
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    fn tree(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }
        dir
    }

    fn vfs(dir: &TempDir) -> DirVfs {
        DirVfs::new(dir.path(), IgnoreConfig::default())
    }

    async fn run(dir: &TempDir, globs: &Globs, config: &WalkConfig) -> Report {
        walk(&vfs(dir), globs, config).await.unwrap()
    }

    fn counted(report: &Report) -> BTreeSet<String> {
        report
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // -- the happy path ------------------------------------------------------

    #[tokio::test]
    async fn counts_every_recognised_file() {
        let dir = tree(&[("a.rs", "fn a() {}\n// c\n\n"), ("b.py", "x = 1\n# c\n")]);
        let report = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        assert_eq!(counted(&report), set(&["a.rs", "b.py"]));
        assert_eq!(
            report.totals().prod,
            crate::report::Counts {
                code: 2,
                comments: 2,
                blanks: 1,
            }
        );
    }

    #[tokio::test]
    async fn language_and_test_flags_are_attached_to_each_file() {
        let dir = tree(&[("src/a.rs", "fn a() {}\n"), ("tests/it.rs", "fn t() {}\n")]);
        let report = run(&dir, &Globs::default(), &WalkConfig::default()).await;

        let file = |name: &str| {
            report
                .files
                .iter()
                .find(|f| f.path.to_string_lossy().replace('\\', "/") == name)
                .unwrap()
        };
        assert_eq!(file("src/a.rs").language, registry().by_key("Rust"));
        assert!(!file("src/a.rs").is_test_file);
        assert!(file("tests/it.rs").is_test_file);
        assert_eq!(file("tests/it.rs").stats.test.code, 1);
    }

    #[tokio::test]
    async fn an_empty_tree_gives_an_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        assert!(report.is_empty());
    }

    #[tokio::test]
    async fn counting_many_files_concurrently_is_complete() {
        // More files than the concurrency limit, so the buffering is exercised.
        let files: Vec<(String, String)> = (0..200)
            .map(|i| (format!("f{i}.rs"), "fn a() {}\n".to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let dir = tree(&refs);
        let report = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        assert_eq!(report.len(), 200);
        assert_eq!(report.totals().prod.code, 200);
    }

    // -- filtering -----------------------------------------------------------

    #[tokio::test]
    async fn unknown_languages_are_skipped_unless_asked_for() {
        let dir = tree(&[("a.rs", "fn a() {}\n"), ("b.zzzznope", "line\n")]);

        let default = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        assert_eq!(counted(&default), set(&["a.rs"]));

        let config = WalkConfig {
            include_unknown: true,
            ..WalkConfig::default()
        };
        let with_unknown = run(&dir, &Globs::default(), &config).await;
        assert_eq!(counted(&with_unknown), set(&["a.rs", "b.zzzznope"]));
    }

    #[tokio::test]
    async fn an_include_glob_restricts_the_walk() {
        let dir = tree(&[("a.rs", "x\n"), ("b.py", "x\n"), ("src/c.rs", "x\n")]);

        let only_rust = Globs::new(vec!["*.rs".into()], vec![]);
        let report = run(&dir, &only_rust, &WalkConfig::default()).await;
        assert_eq!(counted(&report), set(&["a.rs", "src/c.rs"]));
    }

    #[tokio::test]
    async fn an_include_glob_with_a_slash_is_anchored_at_the_root() {
        let dir = tree(&[
            ("a.rs", "x\n"),
            ("src/c.rs", "x\n"),
            ("deep/src/d.rs", "x\n"),
        ]);
        let report = run(
            &dir,
            &Globs::new(vec!["src/*.rs".into()], vec![]),
            &WalkConfig::default(),
        )
        .await;
        assert_eq!(counted(&report), set(&["src/c.rs"]));
    }

    #[tokio::test]
    async fn a_trailing_slash_means_everything_under_that_directory() {
        let dir = tree(&[
            ("a.rs", "x\n"),
            ("src/c.rs", "x\n"),
            ("src/deep/d.rs", "x\n"),
        ]);
        let report = run(
            &dir,
            &Globs::new(vec!["src/".into()], vec![]),
            &WalkConfig::default(),
        )
        .await;
        assert_eq!(counted(&report), set(&["src/c.rs", "src/deep/d.rs"]));
    }

    #[tokio::test]
    async fn an_exclude_glob_wins_over_an_include() {
        let dir = tree(&[("a.rs", "x\n"), ("vendor/b.rs", "x\n")]);
        let globs = Globs::new(vec!["*.rs".into()], vec!["vendor/".into()]);
        let report = run(&dir, &globs, &WalkConfig::default()).await;
        assert_eq!(counted(&report), set(&["a.rs"]));
    }

    #[tokio::test]
    async fn a_language_filter_keeps_only_that_language() {
        let dir = tree(&[("a.rs", "x\n"), ("b.py", "x\n")]);
        let config = WalkConfig {
            languages: Some(vec![registry().by_key("Rust").unwrap()]),
            ..WalkConfig::default()
        };
        let report = run(&dir, &Globs::default(), &config).await;
        assert_eq!(counted(&report), set(&["a.rs"]));
    }

    #[tokio::test]
    async fn a_language_filter_excludes_unknown_files() {
        let dir = tree(&[("a.rs", "x\n"), ("b.zzzznope", "x\n")]);
        let config = WalkConfig {
            languages: Some(vec![registry().by_key("Rust").unwrap()]),
            include_unknown: true,
            ..WalkConfig::default()
        };
        let report = run(&dir, &Globs::default(), &config).await;
        assert_eq!(counted(&report), set(&["a.rs"]));
    }

    #[tokio::test]
    async fn oversized_files_are_skipped() {
        let big = format!("fn a() {{}}\n{}", "// pad\n".repeat(2000));
        let dir = tree(&[("small.rs", "fn a() {}\n"), ("big.rs", big.as_str())]);
        let config = WalkConfig {
            max_file_size: Some(100),
            ..WalkConfig::default()
        };
        let report = run(&dir, &Globs::default(), &config).await;
        assert_eq!(counted(&report), set(&["small.rs"]));
    }

    #[tokio::test]
    async fn the_size_limit_can_be_lifted() {
        let dir = tree(&[("big.rs", "fn a() {}\n")]);
        let config = WalkConfig {
            max_file_size: None,
            ..WalkConfig::default()
        };
        assert_eq!(run(&dir, &Globs::default(), &config).await.len(), 1);
    }

    #[tokio::test]
    async fn binary_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), b"fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), b"\x00\x01\x02binary\n").unwrap();
        let report = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        assert_eq!(counted(&report), set(&["a.rs"]));
    }

    // -- shebangs ------------------------------------------------------------

    #[tokio::test]
    async fn extension_less_scripts_are_identified_by_their_shebang() {
        let dir = tree(&[("script", "#!/usr/bin/env python3\n# c\nx = 1\n")]);
        let report = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        assert_eq!(counted(&report), set(&["script"]));
        assert_eq!(report.files[0].language, registry().by_key("Python"));
        assert_eq!(report.files[0].stats.prod.comments, 2);
    }

    #[tokio::test]
    async fn shebang_detection_can_be_switched_off() {
        let dir = tree(&[("script", "#!/usr/bin/env python3\nx = 1\n")]);
        let config = WalkConfig {
            use_shebangs: false,
            ..WalkConfig::default()
        };
        assert!(run(&dir, &Globs::default(), &config).await.is_empty());
    }

    #[tokio::test]
    async fn an_extension_less_file_without_a_shebang_stays_unknown() {
        let dir = tree(&[("NOTES", "just text\n")]);
        assert!(run(&dir, &Globs::default(), &WalkConfig::default())
            .await
            .is_empty());
    }

    // -- ignore files --------------------------------------------------------

    #[tokio::test]
    async fn the_walk_honours_the_vfs_ignore_rules() {
        let dir = tree(&[
            (".gitignore", "generated/\n"),
            ("a.rs", "fn a() {}\n"),
            ("generated/big.rs", "fn b() {}\n"),
        ]);
        let report = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        assert_eq!(counted(&report), set(&["a.rs"]));
    }

    // -- errors --------------------------------------------------------------

    #[tokio::test]
    async fn a_failing_list_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = DirVfs::new(dir.path().join("missing"), IgnoreConfig::default());
        assert!(walk(&vfs, &Globs::default(), &WalkConfig::default())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_invalid_glob_is_an_error() {
        let dir = tree(&[("a.rs", "x\n")]);
        let globs = Globs::new(vec!["[".into()], vec![]);
        assert!(walk(&vfs(&dir), &globs, &WalkConfig::default())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn detailed_walks_report_no_errors_on_a_clean_tree() {
        let dir = tree(&[("a.rs", "fn a() {}\n")]);
        let (report, errors) = walk_detailed(&vfs(&dir), &Globs::default(), &WalkConfig::default())
            .await
            .unwrap();
        assert_eq!(report.len(), 1);
        assert!(errors.is_empty());
    }

    // -- change detection ----------------------------------------------------

    #[test]
    fn identical_listings_have_no_changes() {
        let entries = vec![
            Entry {
                path: PathBuf::from("a.rs"),
                size: None,
                content_id: Some("aaa".into()),
            },
            Entry {
                path: PathBuf::from("b.rs"),
                size: None,
                content_id: Some("bbb".into()),
            },
        ];
        assert!(changed_paths(&entries, &entries).is_empty());
    }

    #[test]
    fn added_modified_and_deleted_paths_are_all_changes() {
        let before = vec![
            Entry {
                path: PathBuf::from("same.rs"),
                size: None,
                content_id: Some("aaa".into()),
            },
            Entry {
                path: PathBuf::from("edited.rs"),
                size: None,
                content_id: Some("bbb".into()),
            },
            Entry {
                path: PathBuf::from("deleted.rs"),
                size: None,
                content_id: Some("ccc".into()),
            },
        ];
        let after = vec![
            Entry {
                path: PathBuf::from("same.rs"),
                size: None,
                content_id: Some("aaa".into()),
            },
            Entry {
                path: PathBuf::from("edited.rs"),
                size: None,
                content_id: Some("BBB".into()),
            },
            Entry {
                path: PathBuf::from("added.rs"),
                size: None,
                content_id: Some("ddd".into()),
            },
        ];
        let changed = changed_paths(&before, &after);
        assert_eq!(
            changed,
            ["edited.rs", "deleted.rs", "added.rs"]
                .iter()
                .map(PathBuf::from)
                .collect()
        );
    }

    #[test]
    fn a_source_without_content_ids_reports_everything_as_changed() {
        let before = vec![Entry::new("a.rs")];
        let after = vec![Entry::new("a.rs")];
        assert_eq!(changed_paths(&before, &after).len(), 1);
    }

    #[tokio::test]
    async fn only_paths_restricts_the_walk() {
        let dir = tree(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        let config = WalkConfig {
            only_paths: Some([PathBuf::from("a.rs")].into_iter().collect()),
            ..WalkConfig::default()
        };
        let report = run(&dir, &Globs::default(), &config).await;
        assert_eq!(counted(&report), set(&["a.rs"]));
    }

    // -- git parity ----------------------------------------------------------

    #[tokio::test]
    async fn a_git_tree_and_the_working_directory_agree() {
        use crate::vfs::GitVfs;
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        std::fs::write(
            dir.path().join("a.rs"),
            "fn a() {}\n// c\n\n#[cfg(test)]\nmod t {\n    fn b() {}\n}\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "first"]);

        let from_dir = run(&dir, &Globs::default(), &WalkConfig::default()).await;
        let tree = GitVfs::open(dir.path(), "HEAD", None).unwrap();
        let from_git = walk(&tree, &Globs::default(), &WalkConfig::default())
            .await
            .unwrap();

        assert_eq!(from_dir.totals(), from_git.totals());
        assert_eq!(from_git.totals().test.code, 4);
        assert_eq!(from_git.totals().prod.code, 1);
    }
}
