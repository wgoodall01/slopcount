# slopcount

Counts lines of code in a directory or a git tree, broken down by **code /
comment / blank** — and, crucially, by whether the lines are **tests**.

```
$ slopcount
slopcount  origin/main → .  (net change)
─────────────────────────────────────────────────────────
LANGUAGE  FILES  CODE  COMMENT  BLANK  TOTAL  TEST  TEST%
─────────────────────────────────────────────────────────
Rust          0   +15        0     +4    +19   +12    80%
─────────────────────────────────────────────────────────
TOTAL         0   +15        0     +4    +19   +12    80%
─────────────────────────────────────────────────────────
+19 lines changed: +15 code, of which +12 (80%) is test · 0 comment (0% of all lines)
```

Eighty percent of that branch was tests.

## Why

[`sloccount`](https://dwheeler.com/sloccount/) counted source lines so you could
estimate what a codebase cost to write. That question has aged oddly now that
agents write a good deal of the darn code: the interesting number isn't how much
was produced, it's *what kind* of thing was produced.

So: a riff on the name, and a riff on the idea. When you hand a branch to an
agent and it comes back with four thousand lines, you want to know whether that
was the feature you asked for, a zillion low-value unit tests, or a pile of
docs nobody will read. `slopcount` breaks the answer out along that axis.

Test detection is the part other line counters don't do —
[`tokei`](https://github.com/XAMPPRocky/tokei) will tell you code vs. comments
vs. blanks, very fast and very accurately, but it has no notion of a test. The
code/comment/blank classifier here is a re-implementation of tokei's, and the
language database is tokei's; see [Accuracy](#accuracy) below.

## Install

```sh
cargo install --path pkg/cli
```

Or run it out of the workspace with `cargo run --release -- <args>`.

## CLI usage

slopcount takes up to two **refs**. A ref is either a path on disk or a
`git:`-prefixed revision:

| Ref | Means |
| --- | --- |
| `.`, `src`, `../other` | a directory on disk |
| `git:origin/main` | a branch |
| `git:fefefefe` | a commit |
| `git:HEAD~2`, `git:v1.0` | anything `git rev-parse` accepts |

Give it **two** and it reports the net change from the first to the second;
**one** and it reports that ref's counts; **none** and it compares your working
tree against the repository's default branch.

```sh
slopcount                                # what this branch changed, vs. the default branch
slopcount .                              # count the current directory
slopcount git:origin/main                # count a branch as it stands
slopcount git:origin/main .              # changes in the working tree on top of origin/main
slopcount dir1 dir2                      # differences between two directories
slopcount git:origin/main git:my-topic   # changes on a topic branch
slopcount --in src git:origin/main .     # ...restricted to one subdirectory
```

The default branch is resolved from refs already in the local git database:
`origin/HEAD` when the clone has one (`git clone` writes it), otherwise
`origin/main`, `origin/master`, `main`, `master` in that order. **slopcount
never fetches** — every revision has to be one git already knows about. Outside
a repository, the no-argument form just counts the current directory.

### Options

| Option | Effect |
| --- | --- |
| `--in PATH` | narrow every source to this subdirectory |
| `-C DIR` | resolve paths and revisions as if started in `DIR` |
| `--by language\|extension\|file\|directory` | what to break the report down by |
| `--files` | shorthand for `--by file` |
| `--depth N` | directory depth to roll up to, with `--by directory` |
| `--sort code\|comments\|blanks\|test\|lines\|name` | sort rows by a column |
| `-n N` | show only the first N rows |
| `-i GLOB`, `-e GLOB` | include / exclude paths |
| `-l LANG` | count only these languages, by name or extension |
| `--format table\|json\|csv` | machine-readable output |
| `--no-ignore`, `--hidden` | stop honouring ignore files |
| `--no-test-blocks` | only whole-file test rules, no in-file block detection |
| `--doc-strings-as-code` | count doc strings as code rather than comments |
| `--include-unknown` | count files whose language isn't recognised |
| `--max-file-size BYTES` | skip large files (default 2 MiB; `0` lifts it) |
| `-j N` | count this many files concurrently |
| `--list-languages` | every known language, and whether it detects tests |

### Reading the output

`CODE`, `COMMENT` and `BLANK` are totals; `TEST` is the subset of `CODE` that is
test code, and `TEST%` is that share. So in the header example, 15 code lines
changed and 12 of them were tests.

In a diff, `TEST%` shows `—` when it would not mean anything: if a change
deletes production code and adds tests, the *net* code change is a denominator
the test count can exceed, and "125%" would read as a bug rather than as
information. `CODE` and `TEST` still show what actually happened.

`--in` applies *inside* each ref, so both sides are rebased to the same root and
line up — a change to `src/lib.rs` shows as one net change, not an add and a
delete. A subdirectory present on one side but not the other counts as empty
there, so adding or removing a directory reads as added or removed lines; one
missing from *both* sides is an error, since that is almost always a typo.

### Ignore files

Honoured by default: `.gitignore` (including global and nested ones), `.ignore`,
and `.slopcountignore`. `.git` is never walked, and hidden files are skipped
unless you pass `--hidden`.

Git trees get the same treatment — their ignore files are read *out of the
tree* — except for `.gitignore`, since a file git tracks despite matching it was
committed on purpose.

### Other output formats

```sh
$ slopcount git:origin/topic --files --format csv
file,files,code,comments,blanks,total,test_code,test_comments,test_blanks
src/lib.rs,1,18,0,4,22,12,0,2
TOTAL,1,18,0,4,22,12,0,2
```

`--format json` emits one object with a `rows` array and a `total`, each row
carrying `code`/`comments`/`blanks` totals plus separate `prod` and `test`
breakdowns, and a top-level `diff` flag.

## How tests are detected

Two independent mechanisms, both driven by
[`pkg/core/data/languages_extra.json`](pkg/core/data/languages_extra.json):

- **Whole files**, by path glob — `**/*_test.go`, `**/test_*.py`,
  `**/*.spec.ts`, `**/tests/**`, and so on.
- **Blocks within a file**, by an opening marker — `#[cfg(test)]`, `#[test]`,
  `func TestX`, `def test_x`, `describe(`, `@Test`, `TEST_F(`. The block runs
  until brace depth returns to zero, or until indentation returns to the
  marker's level for languages like Python. Markers and braces inside strings
  and comments are ignored.

Add a language or a marker by editing `languages_extra.json`; it is
deep-object-merged over tokei's `languages.json` at startup, so you only write
the delta.

## Library usage

The counting lives in `slopcount_core`; the CLI is a thin wrapper over it.
Everything below is compiled and run as
[`pkg/core/examples/readme.rs`](pkg/core/examples/readme.rs), so it cannot drift
from the API.

```toml
[dependencies]
slopcount_core = { path = "pkg/core" }
```

### Count a directory

```rust
use slopcount_core::vfs::{DirVfs, IgnoreConfig};
use slopcount_core::{walk, Globs, WalkConfig};

let vfs = DirVfs::new(path, IgnoreConfig::default());
let report = walk(&vfs, &Globs::default(), &WalkConfig::default()).await?;

let totals = report.totals();
println!("{} lines of code", totals.total().code);
println!("{} of them tests", totals.test.code);
if let Some(ratio) = totals.test_ratio() {
    println!("{:.0}% test", ratio * 100.0);
}
```

`Stats` splits every count into `prod` and `test`, each a `Counts { code,
comments, blanks }`. `total()` folds the two back together.

### Slice the report

Aggregation is deferred until you ask for it, so grouping is cheap and you can
group by anything:

```rust
for (language, stats) in report.by_language() {
    println!("{language:<12} {:>6} code  {:>6} test", stats.total().code, stats.test.code);
}

let by_test_file = report.group_by(|file| file.is_test_file);
```

There is also `by_extension()`, `by_path()` and `by_directory(depth)`.
`Report::merge` is a concatenation of per-file results, so folding thousands of
them together is linear, and `Report` implements `FromIterator` and `Extend`
over both `FileReport` and `Report` — results collect straight out of a stream:

```rust
let report = stream_of_reports.collect::<Report>().await;
```

### Count a git tree

```rust
use slopcount_core::vfs::{GitVfs, IgnoreRules, IgnoreVfs};

let tree = GitVfs::open(repo, "origin/main", None)?;
let vfs = IgnoreVfs::new(tree, IgnoreRules::for_git_tree());

let report = walk(&vfs, &Globs::default(), &WalkConfig::default()).await?;
```

### Diff two revisions

Git blob hashes identify content, so unchanged files never get read:

```rust
let before = Arc::new(GitVfs::open(repo, "origin/main", None)?);
let after = Arc::new(GitVfs::open(repo, "HEAD", None)?);

let changed = changed_paths(&before.list().await?, &after.list().await?);
let config = WalkConfig { only_paths: Some(changed), ..WalkConfig::default() };

let (before_report, _) = walk_shared(before, &globs, &config).await?;
let (after_report, _) = walk_shared(after, &globs, &config).await?;
```

`walk` counts concurrently on one task; `walk_shared` takes an `Arc` and spawns
a task per file, spreading the work across the runtime's threads. There is also
`walk_detailed`, which returns the files it had to skip alongside the report.

### Filter what gets counted

```rust
let globs = Globs::new(
    vec!["src/".to_string()],          // include
    vec!["**/vendor/**".to_string()],  // exclude
);
let config = WalkConfig {
    languages: Some(vec![registry().resolve("rust").unwrap()]),
    count: CountConfig {
        detect_test_blocks: true,
        treat_doc_strings_as_comments: true,
        skip_binary: true,
    },
    max_file_size: Some(1024 * 1024),
    ..WalkConfig::default()
};
```

### Count a single stream

The classifier takes any `AsyncRead`, so you can skip the VFS entirely:

```rust
use slopcount_core::count::{count, CountConfig, CountOutcome};
use slopcount_core::registry;

match count(&source[..], registry().resolve("rust"), false, &CountConfig::default()).await? {
    CountOutcome::Counted(stats) => println!("{} code", stats.total().code),
    CountOutcome::Binary => println!("binary"),
}
```

### Write your own source

`Vfs` is a small read-only trait — a flat list of paths plus a way to stream the
bytes at one — so a tarball, an HTTP endpoint or an in-memory fixture all work:

```rust
#[async_trait]
pub trait Vfs: Send + Sync {
    fn describe(&self) -> String;
    async fn list(&self) -> anyhow::Result<Vec<Entry>>;
    async fn open(&self, path: &Path) -> anyhow::Result<FileStream>;
}
```

Implementations included: `DirVfs` (a directory, applying ignore rules as it
walks), `GitVfs` (a tree in a git object database), `IgnoreVfs` (wraps any VFS
and masks out what its ignore files exclude, reading them from the wrapped VFS
itself), and `EmptyVfs` (nothing, for a source that does not exist).

## Accuracy

The code/comment/blank classifier is a re-implementation of the
character-level state machine in [tokei](https://github.com/XAMPPRocky/tokei),
and the language database is tokei's
[`languages.json`](https://github.com/XAMPPRocky/tokei/blob/master/languages.json)
— 300-odd languages' comment, string and doc-string syntax — interpreted at
runtime rather than code-generated.

`pkg/core/tests/data` is tokei's own fixture corpus, vendored verbatim, where
each file declares its expected counts in a header comment. `cargo test` checks
every one:

```
tokei parity: 200 fixtures matched exactly, 7 known deviations, ...
```

The seven deviations are all tokei's *embedded child language* feature, which
re-attributes the contents of Rust doc comments, Markdown code fences and HTML
`<script>` bodies to a second language. slopcount deliberately does not do this:
for telling code, docs and tests apart, a doc comment is a comment of the file
it is in. They are listed with reasons in `pkg/core/tests/tokei_parity.rs`.

## Benchmarks

Measured against tokei, cloc and the original sloccount on the Linux kernel and
the Chromium sources. Full detail, including how every divergence was chased to
a cause, is in [BENCHMARKS.md](BENCHMARKS.md).

Wall time, warm cache, M2 Max:

| Tool | Linux (1.7 GB, 96k files) | Chromium (5.3 GB, 506k files) | Peak RSS |
| --- | ---: | ---: | ---: |
| tokei | 5.47 s | 27.74 s | 389 MB |
| **slopcount** | **6.37 s** | **34.15 s** | **52 MB** |
| cloc | 79.92 s | 387.79 s | 545 MB |
| sloccount | 204.42 s | 489.68 s | 60 MB |

About 1.2× behind tokei, and 11–32× ahead of the Perl and shell tools. Counting
is spread across the runtime's worker threads, one task per file, and each file
is streamed through a `BufReader`, so memory is bounded by the longest line
rather than the largest file or the size of the tree.

Counts agree with tokei to **+0.00%** of code lines on the Linux kernel (45 of
51 languages match to the line) and **+0.17%** on Chromium:

| Tool | Δ code vs tokei, Linux | Δ code vs tokei, Chromium |
| --- | ---: | ---: |
| **slopcount** | **+0.00%** | **+0.17%** |
| cloc | −5.78% | +10.73% |
| sloccount | −12.3% | −41.1% |

The remaining slopcount differences are all understood: shebang detection finds
extension-less scripts tokei skips; doc comments stay with their host file
rather than moving to a Markdown child; and binary detection skips 14 MPEG
transport-stream files in Chromium that tokei counts as 18,163 lines of
"TypeScript". cloc's swings are missing Device Tree support on Linux and
treating Chromium's `.grd` resources as XML. sloccount is only −1.0% against the
languages it actually has counters for — a 2004 tool has no JSON, HTML,
TypeScript or Rust, which is 30% of Chromium.

Running this comparison found a real bug in slopcount: an escaped quote in
plain code (`s/^\"|\"$//g`) opened a string literal that swallowed the rest of
the file. None of the 206 tokei fixtures contained that shape; 96,000 kernel
files did.

## Layout

```
pkg/core   slopcount_core — the library
  lang     the language database: tokei's, merged with ours
  count    the line classifier, streaming over an AsyncRead
  report   Report / Stats, and the dimensions you can slice them by
  vfs      DirVfs, GitVfs, IgnoreVfs, EmptyVfs
  walk     glue: list a VFS, filter, count concurrently, merge
pkg/cli    slopcount_cli — the `slopcount` binary
  path_ref parsing of `.` / `git:origin/main` into a source
  repo     locating the repo and its default branch
  render   rows, tables, JSON and CSV
```

## Building

A Cargo workspace. Every third-party version is pinned once in
`[workspace.dependencies]` at the root and opted into with
`foo.workspace = true`, so the two crates can never end up on different copies
of a dependency.

```sh
cargo build --release
cargo test
```

## Licence

MIT OR Apache-2.0.

The language database and the test corpus come from
[tokei](https://github.com/XAMPPRocky/tokei) by XAMPPRocky, which is dual
MIT/Apache-2.0 licensed — thank you. The counting algorithm is a
re-implementation of tokei's.
