//! `slopcount` — count lines of code, and tell tests apart from everything else.

mod cli;
mod path_ref;
mod render;
mod repo;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use slopcount_core::count::CountConfig;
use slopcount_core::{
    changed_paths, merge_base_label, registry, walk_shared, DirVfs, EmptyVfs, GitVfs, Globs,
    IgnoreConfig, IgnoreRules, IgnoreVfs, Report, Vfs, WalkConfig,
};

use cli::{Cli, Dimension, Format};
use path_ref::PathRef;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    if args.list_languages {
        list_languages();
        return Ok(());
    }

    if args.list_families {
        list_families();
        return Ok(());
    }

    render::set_color(args.color() && args.format == Format::Table);

    let output = run(&args).await?;
    print!("{output}");
    Ok(())
}

/// The revision the working tree grew out of, as the second side of the
/// default comparison.
const HEAD: &str = "HEAD";

/// Tacked onto every "could not resolve that" error: the revision being
/// missing rather than mistyped is a common enough surprise to name.
const NEVER_FETCHES: &str =
    "(slopcount never fetches; the revision must already be in the local git database)";

/// A resolved source, or `None` when `--in` named a subdirectory that source
/// does not contain.
type MaybeSource = Option<Arc<dyn Vfs>>;

/// What the positional arguments resolved to.
enum Source {
    /// Count one thing.
    Single(Arc<dyn Vfs>),
    /// Report the net change from `before` to `after`.
    Diff {
        before: Arc<dyn Vfs>,
        after: Arc<dyn Vfs>,
    },
}

async fn run(args: &Cli) -> anyhow::Result<String> {
    let globs = Globs::new(args.include.clone(), args.exclude.clone());
    let config = walk_config(args)?;

    let dimension = args.dimension();
    let group = args.group_families();
    let heading = match dimension {
        Dimension::Language => "LANGUAGE",
        Dimension::Family => "FAMILY",
        Dimension::Extension => "EXTENSION",
        Dimension::File => "FILE",
        Dimension::Directory => "DIRECTORY",
    };

    match resolve(args)? {
        Source::Single(vfs) => {
            let source = vfs.describe();
            let report = count(vfs, &globs, &config).await?;
            let rows = render::finish(
                render::rows(&report, dimension, args.depth, group),
                args.sort,
                args.top,
            );
            let rows = arrange(args, rows);
            Ok(emit(args, &source, heading, &rows, false))
        }

        Source::Diff { before, after } => {
            // Only files whose contents actually differ need counting. A
            // directory cannot identify its contents, so comparisons involving
            // one fall back to counting both sides in full.
            let changed = changed_paths(&before.list().await?, &after.list().await?);
            let config = WalkConfig {
                only_paths: Some(changed),
                ..config
            };

            let source = format!("{} → {}", before.describe(), after.describe());
            let before_report = count(before, &globs, &config).await?;
            let after_report = count(after, &globs, &config).await?;

            let rows = render::finish(
                render::diff_rows(&before_report, &after_report, dimension, args.depth, group),
                args.sort,
                args.top,
            );
            let rows = arrange(args, rows);
            Ok(emit(args, &source, heading, &rows, true))
        }
    }
}

/// Interleave family roll-ups into the rows, for the table only: CSV and JSON
/// stay one row per language and carry the family as a field, so nothing
/// downstream has to know to skip the roll-ups.
fn arrange(args: &Cli, rows: Vec<render::Row>) -> Vec<render::Row> {
    match args.format {
        Format::Table => render::group_families(rows, args.sort),
        _ => rows,
    }
}

/// Turn the positional arguments into something to count.
///
/// - two refs: the change from the first to the second;
/// - one ref: just that ref;
/// - none: the repository's working tree against its default branch, or, if
///   there is no repository to compare within, simply the current directory.
fn resolve(args: &Cli) -> anyhow::Result<Source> {
    match args.refs.as_slice() {
        [] => default_source(args),
        [only] => Ok(Source::Single(require(args, only, open(args, only)?)?)),
        [before, after] => {
            let opened = (open(args, before)?, open(args, after)?);
            pair(args, before, after, opened)
        }
        // clap caps the positional at two.
        _ => unreachable!("at most two refs"),
    }
}

/// Build a diff from two sources, either of which may be missing `--in`.
///
/// One missing side is ordinary: the directory was added or removed, and the
/// missing side counts as empty. Both missing means `--in` names a directory
/// that is nowhere to be found, which is almost always a typo.
fn pair(
    args: &Cli,
    before_ref: &PathRef,
    after_ref: &PathRef,
    opened: (MaybeSource, MaybeSource),
) -> anyhow::Result<Source> {
    match opened {
        (Some(before), Some(after)) => Ok(Source::Diff { before, after }),
        (None, None) => {
            let subdir = args.subdir().unwrap_or_default();
            anyhow::bail!(
                "{} does not exist in {before_ref} or in {after_ref}",
                subdir.display()
            )
        }
        (before, after) => Ok(Source::Diff {
            before: before.unwrap_or_else(|| absent(args, before_ref)),
            after: after.unwrap_or_else(|| absent(args, after_ref)),
        }),
    }
}

/// A stand-in for a source whose `--in` subdirectory is not there.
///
/// Described the way the source would have described itself had it existed, so
/// the two sides of the header read alike.
fn absent(args: &Cli, reference: &PathRef) -> Arc<dyn Vfs> {
    let subdir = args.subdir();
    // A revision names its subdirectory with a colon, the way `describe` does.
    let git = |revision: String| match &subdir {
        Some(subdir) => format!("{revision}:{}", subdir.display()),
        None => revision,
    };
    let name = match reference {
        PathRef::GitCommitish(revision) => git(revision.clone()),
        PathRef::GitMergeBase(one, two) => git(merge_base_label(one, two)),
        PathRef::Fs(path) => {
            let root = fs_root(args, path);
            match &subdir {
                Some(subdir) => root.join(subdir).display().to_string(),
                None => root.display().to_string(),
            }
        }
    };
    Arc::new(EmptyVfs::new(format!("{name} (absent)")))
}

/// Turn a missing single source into an error; there is no other side to make
/// its absence meaningful.
fn require(args: &Cli, reference: &PathRef, opened: MaybeSource) -> anyhow::Result<Arc<dyn Vfs>> {
    opened.ok_or_else(|| {
        let subdir = args.subdir().unwrap_or_default();
        anyhow::anyhow!("{} does not exist in {reference}", subdir.display())
    })
}

/// With no arguments: compare the working tree against the default branch.
///
/// The baseline is the *merge base* rather than the branch tip, so whatever
/// landed on the default branch since this branch left it does not read as
/// though this branch had deleted it.
///
/// Sitting on the default branch with nothing uncommitted is the one case with
/// no change to report; the tree is then simply counted.
fn default_source(args: &Cli) -> anyhow::Result<Source> {
    let base = base_dir(args);

    let Some(root) = repo::root(&base) else {
        // Not in a repository (or a bare one): nothing to compare against.
        let here = PathRef::here();
        return Ok(Source::Single(require(args, &here, open(args, &here)?)?));
    };
    let after_ref = PathRef::Fs(root.clone());
    let after = dir_source_in(args, &root);

    let Some(branch) = repo::default_branch(&base) else {
        eprintln!(
            "slopcount: no default branch found in {}; counting the working tree instead",
            root.display()
        );
        return Ok(Source::Single(require(args, &after_ref, after)?));
    };

    if repo::on_branch(&base, &branch) && !repo::is_dirty(&base) {
        return Ok(Source::Single(require(args, &after_ref, after)?));
    }

    // An unborn HEAD, or an orphan branch such as `gh-pages`, has no commit in
    // common with the default branch. There is no branch point to compare from
    // then, so the branch itself is the only baseline left — and the
    // no-argument form should still report something.
    let (before_ref, before) = match merge_base_source(args, &branch, HEAD) {
        Ok(before) => (
            PathRef::GitMergeBase(branch.clone(), HEAD.to_string()),
            before,
        ),
        Err(_) => {
            eprintln!(
                "slopcount: HEAD has no commit in common with {branch}; \
                 comparing against {branch} itself"
            );
            (
                PathRef::GitCommitish(branch.clone()),
                git_source(args, &branch)?,
            )
        }
    };
    pair(args, &before_ref, &after_ref, (before, after))
}

/// Open one ref as a VFS.
///
/// `Ok(None)` means the ref itself is fine but `--in` names a subdirectory it
/// does not contain; the caller decides whether that is an error.
fn open(args: &Cli, reference: &PathRef) -> anyhow::Result<MaybeSource> {
    match reference {
        PathRef::Fs(path) => {
            let path = fs_root(args, path);
            if !path.exists() {
                anyhow::bail!("no such path: {}", path.display());
            }
            Ok(dir_source_in(args, &path))
        }
        PathRef::GitCommitish(revision) => git_source(args, revision),
        PathRef::GitMergeBase(one, two) => merge_base_source(args, one, two),
    }
}

fn emit(args: &Cli, source: &str, heading: &str, rows: &[render::Row], diff: bool) -> String {
    match args.format {
        Format::Table => {
            let header = if diff {
                format!("slopcount  {source}  (net change)")
            } else {
                format!("slopcount  {source}")
            };
            render::table(&header, heading, rows, diff)
        }
        Format::Csv => render::csv(heading, rows),
        Format::Json => {
            let value = render::json(source, &heading.to_lowercase(), rows, diff);
            format!("{}\n", serde_json::to_string_pretty(&value).expect("json"))
        }
    }
}

/// Walk a source, reporting any unreadable files on stderr rather than failing.
async fn count<V: Vfs + ?Sized + 'static>(
    vfs: Arc<V>,
    globs: &Globs,
    config: &WalkConfig,
) -> anyhow::Result<Report> {
    let (report, errors) = walk_shared(vfs, globs, config).await?;
    for error in &errors {
        eprintln!(
            "slopcount: skipped {}: {}",
            error.path.display(),
            error.error
        );
    }
    Ok(report)
}

/// Where a filesystem ref actually points, before `--in` narrows it.
///
/// `join` on an absolute path yields that path, so this handles both; without
/// `-C` the path is used as written, so it reads back the way it was typed.
fn fs_root(args: &Cli, path: &Path) -> PathBuf {
    match &args.repo {
        Some(base) => base.join(path),
        None => path.to_path_buf(),
    }
}

/// The directory everything is resolved against: `-C`, else the cwd.
fn base_dir(args: &Cli) -> PathBuf {
    args.repo.clone().unwrap_or_else(|| PathBuf::from("."))
}

/// A directory source, narrowed by `--in`. `None` when that subdirectory is
/// not present under `path`.
fn dir_source_in(args: &Cli, path: &Path) -> MaybeSource {
    let root = match args.subdir() {
        Some(subdir) => path.join(subdir),
        None => path.to_path_buf(),
    };
    if !root.exists() {
        return None;
    }

    let ignore = IgnoreConfig {
        respect_gitignore: !args.no_ignore,
        respect_ignore_files: !args.no_ignore,
        include_hidden: args.hidden,
        ..IgnoreConfig::default()
    };
    Some(Arc::new(DirVfs::new(&root, ignore)))
}

fn git_source(args: &Cli, revision: &str) -> anyhow::Result<MaybeSource> {
    let base = base_dir(args);
    let opened = GitVfs::open_optional_subpath(&base, revision, args.subdir().as_deref())
        .with_context(|| {
            format!(
                "reading git revision {revision:?} from {}\n{NEVER_FETCHES}",
                base.display()
            )
        })?;
    Ok(opened.map(|vfs| git_ignores(args, vfs)))
}

/// The merge base of two revisions: what the second branched from, as far as
/// the local object database can tell.
fn merge_base_source(args: &Cli, one: &str, two: &str) -> anyhow::Result<MaybeSource> {
    let base = base_dir(args);
    let opened =
        GitVfs::open_merge_base(&base, one, two, args.subdir().as_deref()).with_context(|| {
            format!(
                "finding the merge base of {one:?} and {two:?} in {}\n{NEVER_FETCHES}",
                base.display()
            )
        })?;
    Ok(opened.map(|vfs| git_ignores(args, vfs)))
}

/// Apply the ignore rules to a git tree.
///
/// A git tree carries its ignore files inside it, so the rules have to be
/// applied on top rather than during the walk. `.gitignore` is not among
/// them: a file git tracks despite matching it was committed on purpose.
fn git_ignores(args: &Cli, vfs: GitVfs) -> Arc<dyn Vfs> {
    let mut rules = IgnoreRules::for_git_tree();
    rules.include_hidden = args.hidden;
    if args.no_ignore {
        rules.ignore_files.clear();
    }
    Arc::new(IgnoreVfs::new(vfs, rules))
}

fn walk_config(args: &Cli) -> anyhow::Result<WalkConfig> {
    let registry = registry();
    let languages = if args.language.is_empty() {
        None
    } else {
        let mut ids = Vec::new();
        for name in &args.language {
            let id = registry
                .resolve(name)
                .with_context(|| format!("unknown language: {name}"))?;
            ids.push(id);
        }
        Some(ids)
    };

    Ok(WalkConfig {
        count: CountConfig {
            treat_doc_strings_as_comments: !args.doc_strings_as_code,
            detect_test_blocks: !args.no_test_blocks,
            skip_binary: true,
        },
        languages,
        include_unknown: args.include_unknown,
        use_shebangs: true,
        max_file_size: (args.max_file_size > 0).then_some(args.max_file_size),
        concurrency: args.jobs.max(1),
        only_paths: None,
    })
}

fn list_families() {
    let registry = registry();
    let mut families: Vec<(&str, Vec<&str>)> = registry
        .families()
        .iter()
        .map(|f| {
            let mut names: Vec<&str> = f
                .languages
                .iter()
                .map(|id| registry.get(*id).name.as_str())
                .collect();
            names.sort();
            (f.name.as_str(), names)
        })
        .collect();
    families.sort();
    let width = families.iter().map(|f| f.0.len()).max().unwrap_or(0);
    println!("{:<width$}  LANGUAGES", "FAMILY");
    for (name, languages) in families {
        println!("{name:<width$}  {}", languages.join(", "));
    }
}

fn list_languages() {
    let registry = registry();
    let mut rows: Vec<(&str, String, bool)> = registry
        .languages()
        .iter()
        .map(|l| (l.name.as_str(), l.extensions.join(", "), l.detects_tests()))
        .collect();
    rows.sort();
    let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    println!("{:<width$}  TESTS  EXTENSIONS", "LANGUAGE");
    for (name, extensions, detects) in rows {
        let mark = if detects { "yes" } else { "-" };
        println!("{name:<width$}  {mark:<5}  {extensions}");
    }
}
