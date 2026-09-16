//! The examples printed in README.md, kept here so they are compiled and run
//! by `cargo test --examples` and cannot drift from the API.

use std::sync::Arc;

use slopcount_core::count::CountConfig;
use slopcount_core::vfs::{DirVfs, GitVfs, IgnoreConfig, IgnoreRules, IgnoreVfs, Vfs};
use slopcount_core::{changed_paths, walk, walk_shared, Globs, Report, WalkConfig};

/// Count a directory and print the test share.
async fn count_a_directory(path: &std::path::Path) -> anyhow::Result<()> {
    let vfs = DirVfs::new(path, IgnoreConfig::default());
    let report = walk(&vfs, &Globs::default(), &WalkConfig::default()).await?;

    let totals = report.totals();
    println!("{} lines of code", totals.total().code);
    println!("{} of them tests", totals.test.code);
    if let Some(ratio) = totals.test_ratio() {
        println!("{:.0}% test", ratio * 100.0);
    }
    Ok(())
}

/// Break a report down along any dimension.
async fn break_it_down(path: &std::path::Path) -> anyhow::Result<()> {
    let vfs = DirVfs::new(path, IgnoreConfig::default());
    let report = walk(&vfs, &Globs::default(), &WalkConfig::default()).await?;

    for (language, stats) in report.by_language() {
        println!(
            "{language:<12} {:>6} code  {:>6} test",
            stats.total().code,
            stats.test.code
        );
    }

    // Or roll languages up into their families: JavaScript, Documentation, ...
    for (family, stats) in report.by_family() {
        println!("{family:<14} {:>6} code", stats.total().code);
    }

    // Or group by anything you like.
    let by_test_file = report.group_by(|file| file.is_test_file);
    println!("{:?}", by_test_file.get(&true).map(|s| s.total().code));
    Ok(())
}

/// Count a git tree, honouring ignore files committed inside it.
async fn count_a_git_tree(repo: &std::path::Path) -> anyhow::Result<()> {
    let tree = GitVfs::open(repo, "origin/main", None)?;
    let vfs = IgnoreVfs::new(tree, IgnoreRules::for_git_tree());

    let report = walk(&vfs, &Globs::default(), &WalkConfig::default()).await?;
    println!("{} files", report.len());
    Ok(())
}

/// Diff two revisions, counting only the files that actually differ.
async fn diff_two_revisions(repo: &std::path::Path) -> anyhow::Result<()> {
    let before = Arc::new(GitVfs::open(repo, "origin/main", None)?);
    let after = Arc::new(GitVfs::open(repo, "HEAD", None)?);

    // Git blob hashes identify content, so unchanged files are skipped.
    let changed = changed_paths(&before.list().await?, &after.list().await?);
    let config = WalkConfig {
        only_paths: Some(changed),
        ..WalkConfig::default()
    };

    let globs = Globs::default();
    let (before_report, _) = walk_shared(before, &globs, &config).await?;
    let (after_report, _) = walk_shared(after, &globs, &config).await?;

    let added_tests =
        after_report.totals().test.code as i64 - before_report.totals().test.code as i64;
    println!("{added_tests:+} test lines");
    Ok(())
}

/// Filter what gets counted.
async fn filtering(path: &std::path::Path) -> anyhow::Result<()> {
    let vfs = DirVfs::new(path, IgnoreConfig::default());

    let globs = Globs::new(
        vec!["src/".to_string()],         // include
        vec!["**/vendor/**".to_string()], // exclude
    );
    let config = WalkConfig {
        languages: Some(vec![slopcount_core::registry().resolve("rust").unwrap()]),
        count: CountConfig {
            detect_test_blocks: true,
            treat_doc_strings_as_comments: true,
            skip_binary: true,
        },
        max_file_size: Some(1024 * 1024),
        ..WalkConfig::default()
    };

    let report = walk(&vfs, &globs, &config).await?;
    println!("{:?}", report.totals());
    Ok(())
}

/// Count one file yourself, without a VFS.
async fn count_one_stream() -> anyhow::Result<()> {
    use slopcount_core::count::{count, CountOutcome};
    use slopcount_core::registry;

    let source = b"fn main() {}\n// a comment\n";
    let rust = registry().resolve("rust");

    match count(&source[..], rust, false, &CountConfig::default()).await? {
        CountOutcome::Counted(stats) => println!(
            "{} code, {} comments",
            stats.total().code,
            stats.total().comments
        ),
        CountOutcome::Binary => println!("binary"),
    }
    Ok(())
}

/// Reports collect straight out of a stream.
async fn collecting(reports: impl futures::Stream<Item = Report> + Unpin) -> Report {
    use futures::StreamExt;
    reports.collect::<Report>().await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let here = std::path::Path::new(".");

    count_a_directory(here).await?;
    break_it_down(here).await?;
    filtering(here).await?;
    count_one_stream().await?;

    let empty = collecting(futures::stream::iter(Vec::<Report>::new())).await;
    assert!(empty.is_empty());

    // The git examples need a repository; run them only when we are in one.
    if GitVfs::open(here, "HEAD", None).is_ok() {
        count_a_git_tree(here).await?;
        diff_two_revisions(here).await?;
    }
    Ok(())
}
