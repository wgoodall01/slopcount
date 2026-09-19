# slopcount

Counts lines of code in a directory or a git tree, broken down into
**implementation / documentation / test** — three disjoint buckets, so no line
is counted twice.

```
$ slopcount
slopcount  origin/main → .  (net change)
────────────────────────────────────────────────────
LANGUAGE  FILES  IMPL  DOC  TEST  TOTAL  DOC%  TEST%
────────────────────────────────────────────────────
Systems       0    +3    0   +12    +19    0%    80%
└─  Rust      0    +3    0   +12    +19    0%    80%
────────────────────────────────────────────────────
TOTAL         0    +3    0   +12    +19    0%    80%
────────────────────────────────────────────────────
+19 lines changed: +3 implementation · +12 test (80% of code) · 0 documentation (0% of all lines)
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

## Installation

Every tagged release publishes prebuilt `slopcount` binaries for macOS
(aarch64), Linux (x86_64, aarch64) and Windows (x86_64, aarch64).

**With [`cargo binstall`](https://github.com/cargo-bins/cargo-binstall)** —
downloads the prebuilt binary for your platform, no compiler needed:

```sh
cargo binstall --git https://github.com/wgoodall01/slopcount slopcount_cli
```

**With `cargo install`** — builds from source:

```sh
cargo install --git https://github.com/wgoodall01/slopcount slopcount_cli
```

**From the release page** — grab the archive for your platform from
[the latest release](https://github.com/wgoodall01/slopcount/releases/latest),
unpack it, and drop `slopcount` somewhere on your `PATH`:

```sh
curl -fsSL -o slopcount.tar.gz \
  https://github.com/wgoodall01/slopcount/releases/latest/download/slopcount-aarch64-apple-darwin.tar.gz
tar xzf slopcount.tar.gz
install -m755 slopcount-aarch64-apple-darwin/slopcount ~/.local/bin/slopcount
```

Swap the target triple for `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc` or
`aarch64-pc-windows-msvc` as needed; the Windows archives are `.zip`. Each
release also carries a `SHA256SUMS` file.

Or run it out of the workspace with `cargo run --release -- <args>`.

## CLI usage

slopcount takes up to two **refs**. A ref is a path on disk, a `git:`-prefixed
revision, or the merge base of two revisions:

| Ref | Means |
| --- | --- |
| `.`, `src`, `../other` | a directory on disk |
| `git:origin/main` | a branch |
| `git:fefefefe` | a commit |
| `git:HEAD~2`, `git:v1.0` | anything `git rev-parse` accepts |
| `git-merge:origin/main:HEAD` | where those two last agreed |

A merge base is what a topic branch grew from, so
`slopcount git-merge:origin/main:HEAD .` reports what the branch added, and
ignores whatever landed on `origin/main` in the meantime. It reads as
`merge-base(origin/main, HEAD)` in the report header.

Give it **two** and it reports the net change from the first to the second;
**one** and it reports that ref's counts; **none** and it compares your working
tree against the point where you left the default branch.

```sh
slopcount                                # what this branch changed, since it left the default branch
slopcount .                              # count the current directory
slopcount git:origin/main                # count a branch as it stands
slopcount git:origin/main .              # changes in the working tree on top of origin/main
slopcount dir1 dir2                      # differences between two directories
slopcount git:origin/main git:my-topic   # changes on a topic branch
slopcount git-merge:origin/main:HEAD .   # ...against where the branch left origin/main
slopcount --in src git:origin/main .     # ...restricted to one subdirectory
```

The default branch is resolved from refs already in the local git database:
`origin/HEAD` when the clone has one (`git clone` writes it), otherwise
`origin/main`, `origin/master`, `main`, `master` in that order. **slopcount
never fetches** — every revision has to be one git already knows about.

The baseline is the *merge base*, not the branch tip, so whatever landed on
`origin/main` since you branched is not reported as though you had deleted it.
Two cases skip the comparison: sitting on the default branch with nothing
uncommitted just counts the tree, since there is no change to show, and being
outside a repository just counts the current directory. Uncommitted means
staged, unstaged **or** untracked — but never ignored, so build output does not
make a tree look busy.

### Options

| Option | Effect |
| --- | --- |
| `--in PATH` | narrow every source to this subdirectory |
| `-C DIR` | resolve paths and revisions as if started in `DIR` |
| `--by language\|family\|extension\|file\|directory` | what to break the report down by |
| `--no-families` | one flat row per language, no family grouping |
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
| `--color auto\|always\|never` | ANSI colour in the table (auto: only on a terminal) |
| `--list-languages` | every known language, and whether it detects tests |
| `--list-families` | every family, and the languages in it |

### Reading the output

`IMPL`, `DOC` and `TEST` are **disjoint**: no line is in more than one of them.
`IMPL` is code that is not test code, `DOC` is comment lines — documentation, in
the sense that matters here — and `TEST` is test code. `TEST%` is the test share
of code, `IMPL + TEST`, so in the header example 15 code lines changed and 12 of
them were tests. `DOC%` is documentation as a share of every line counted.

`TOTAL` is every line, blanks included; blanks have no column of their own, so
`IMPL`, `DOC` and `TEST` do not add up to it. `--format csv` and `--format json`
still carry the blank counts, and `--sort blanks` still sorts on them.

In a diff, a percentage shows `—` when it would not mean anything: if a change
deletes implementation code and adds tests, the *net* code change is a
denominator the test count can exceed, and "125%" would read as a bug rather
than as information. `IMPL`, `DOC` and `TEST` still show what actually happened.

`--in` applies *inside* each ref, so both sides are rebased to the same root and
line up — a change to `src/lib.rs` shows as one net change, not an add and a
delete. A subdirectory present on one side but not the other counts as empty
there, so adding or removing a directory reads as added or removed lines; one
missing from *both* sides is an error, since that is almost always a typo.

### Families

Languages come grouped into **families**: TypeScript, TSX, JSX and JavaScript
are all `JavaScript`; Markdown and reStructuredText are `Documentation`; JSON,
YAML and TOML are `Configuration`. The family is the number you usually want —
"how much frontend did this branch add" is rarely a question about `.tsx` files
specifically.

```
$ slopcount web-app
slopcount  web-app
──────────────────────────────────────────────────────────
LANGUAGE        FILES  IMPL  DOC  TEST  TOTAL  DOC%  TEST%
──────────────────────────────────────────────────────────
Configuration       2     3    0     0      3    0%     0%
├─  YAML            1     2    0     0      2    0%     0%
└─  JSON            1     1    0     0      1    0%     0%
JavaScript          3     3    0     0      3    0%     0%
├─  JavaScript      1     1    0     0      1    0%     0%
├─  TSX             1     1    0     0      1    0%     0%
└─  TypeScript      1     1    0     0      1    0%     0%
Systems             1     1    1     0      2   50%     0%
└─  Rust            1     1    1     0      2   50%     0%
Documentation       1     0    2     0      3   67%      —
└─  Markdown        1     0    2     0      3   67%      —
──────────────────────────────────────────────────────────
TOTAL               7     7    3     0     11   27%     0%
──────────────────────────────────────────────────────────
11 lines counted: 7 implementation · 0 test (0% of code) · 3 documentation (27% of all lines)
```

The flush-left row is the family, and it is the sum of the languages indented
beneath it; families are ordered by the same column the rows are sorted on. `--by family` drops the per-language
rows and reports families alone, and `--no-families` goes the other way, back to
one flat row per language. A language no family claims reports as `Other`.

The definitions live in
[`pkg/core/data/families.json`](pkg/core/data/families.json) — a family name and
the language *keys* from `languages.json` that belong to it. A language may be
in at most one family, and naming one that does not exist is an error rather
than a silent miss. `slopcount --list-families` prints what is currently
defined.

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
file,files,impl_code,impl_comments,impl_blanks,test_code,test_comments,test_blanks,total
src/lib.rs,1,6,0,2,12,0,2,22
TOTAL,1,6,0,2,12,0,2,22
```

`--format json` emits one object with a `rows` array and a `total`, each row
carrying `impl` and `test` objects of `code`/`comments`/`blanks`, a `lines`
count, and a top-level `diff` flag.

Both formats follow the table: every count is disjoint. The six `impl_*` and
`test_*` cells partition the lines counted and sum to `total` (`lines` in JSON),
and there are deliberately no roll-ups spanning the two — so adding fields
together can never double-count. The table's `DOC` column is
`impl_comments + test_comments`; its `TEST%` is `test_code / (impl_code +
test_code)`.

JSON and CSV stay one row per language: the family rides along as a `family`
field (JSON) or a leading `family` column (CSV) rather than as a roll-up row, so
nothing downstream has to know to skip a subtotal. The tree is a table-only
affair, as is colour — both are off when output is not a terminal.

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

`Stats` splits every count into `prod` (the `IMPL` column) and `test`, each a
`Counts { code, comments, blanks }`. The two are disjoint; `total()` folds them
back together.

### Slice the report

Aggregation is deferred until you ask for it, so grouping is cheap and you can
group by anything:

```rust
for (language, stats) in report.by_language() {
    println!("{language:<12} {:>6} code  {:>6} test", stats.total().code, stats.test.code);
}

let by_test_file = report.group_by(|file| file.is_test_file);
```

There is also `by_family()`, `by_extension()`, `by_path()` and
`by_directory(depth)`.
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
a cause, is in [BENCHMARKS.md](BENCHMARKS.md); the whole thing is reproducible
with [`scripts/benchmark.nu`](scripts/benchmark.nu).

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

## Layout

```
pkg/core   slopcount_core — the library
  lang     the language database: tokei's, merged with ours, plus families
  count    the line classifier, streaming over an AsyncRead
  report   Report / Stats, and the dimensions you can slice them by
  vfs      DirVfs, GitVfs, IgnoreVfs, EmptyVfs
  walk     glue: list a VFS, filter, count concurrently, merge
pkg/cli    slopcount_cli — the `slopcount` binary
  path_ref parsing of `.` / `git:origin/main` / `git-merge:a:b` into a source
  repo     locating the repo and its default branch
  render   rows, tables, JSON and CSV
scripts    benchmark.nu — the BENCHMARKS.md methodology, start to finish
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

`make check` runs the same gate CI does (clippy, tests, `cargo fmt --check`).
`make release` bumps the version with
[`cargo-release`](https://github.com/crate-ci/cargo-release), commits and tags
it, then prints the `git push` that cuts the GitHub release. `LEVEL=minor make
release` bumps a minor version instead of a patch.

## Licence

MIT OR Apache-2.0.

The language database and the test corpus come from
[tokei](https://github.com/XAMPPRocky/tokei) by XAMPPRocky, which is dual
MIT/Apache-2.0 licensed — thank you. The counting algorithm is a
re-implementation of tokei's.
