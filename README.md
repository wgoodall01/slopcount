# slopcount

Counts lines of code in a directory or a git tree, broken down by **code /
comment / blank** — and, unlike `tokei`, by whether the lines are **tests**.

That last axis is the point. It answers the question you actually have about an
agent's output:

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

## Usage

slopcount takes up to two **refs**. A ref is either a path on disk, or a
`git:`-prefixed revision — anything `git rev-parse` accepts:

| Ref | Means |
| --- | --- |
| `.`, `src`, `../other` | a directory on disk |
| `git:origin/main` | a branch |
| `git:fefefefe` | a commit |
| `git:HEAD~2`, `git:v1.0` | any revision |

Give it two and it reports the net change from the first to the second; one and
it reports that ref's counts; none and it compares your working tree against the
repository's default branch.

```sh
slopcount                                # what this branch changed, vs. the default branch
slopcount .                              # count the current directory
slopcount git:origin/main                # count a branch as it stands
slopcount git:origin/main .              # changes in the working tree on top of origin/main
slopcount dir1 dir2                      # differences between two directories
slopcount git:origin/main git:my-topic   # changes on a topic branch
slopcount --in src git:origin/main .     # ...restricted to one subdirectory
```

`--in` narrows every source to a subdirectory. It applies *inside* each ref, so
both sides are rebased to the same root and line up — a file changed in
`src/lib.rs` shows as one net change, not an add and a delete. A subdirectory
that exists on one side but not the other is treated as empty there, so adding
or removing a directory reads as added or removed lines; one that is missing
from *both* sides is an error, since that is almost always a typo.

The default branch is resolved from refs already in the local git database:
`origin/HEAD` when the clone has one (`git clone` writes it), otherwise
`origin/main`, `origin/master`, `main`, `master` in that order. **slopcount
never fetches** — every revision has to be one git already knows about. Outside
a repository, the no-argument form just counts the current directory.

Useful options:

| Option | Effect |
| --- | --- |
| `--by language\|extension\|file\|directory` | what to break the report down by |
| `--files`, `-n N`, `--sort test` | per-file view, top N rows, sorted by test lines |
| `-i GLOB`, `-e GLOB`, `-l LANG` | include / exclude / restrict to a language |
| `--in PATH` | narrow every source to this subdirectory |
| `-C DIR` | resolve paths and revisions as if started in `DIR` |
| `--format table\|json\|csv` | machine-readable output |
| `--no-ignore`, `--hidden` | stop honouring ignore files |
| `--no-test-blocks` | only whole-file test rules, no in-file block detection |
| `--list-languages` | every known language, and whether it detects tests |

Ignore files are honoured by default: `.gitignore` (including global and
nested ones), `.ignore`, and `.slopcountignore`. `.git` is never walked. Git
trees get the same treatment — their ignore files are read *out of the tree* —
except for `.gitignore`, since a file git tracks despite matching it was
committed on purpose.

In a diff, `TEST%` is shown as `—` when it would not mean anything: if a change
deletes production code and adds tests, the *net* code change is a denominator
the test count can exceed, and "125%" would read as a bug. The `CODE` and `TEST`
columns still show what happened.

## How tests are detected

Two independent mechanisms, both driven by
[`pkg/core/data/languages_extra.json`](pkg/core/data/languages_extra.json):

- **Whole files**, by path glob — `**/*_test.go`, `**/test_*.py`,
  `**/*.spec.ts`, `**/tests/**`, and so on.
- **Blocks within a file**, by an opening marker — `#[cfg(test)]`, `#[test]`,
  `func TestX`, `def test_x`, `describe(`, `@Test`, `TEST_F(`. The block runs
  until brace depth returns to zero, or until the indentation returns to the
  marker's level for languages like Python. Markers inside strings and comments
  are ignored, and so are braces inside them.

Add a language or a marker by editing `languages_extra.json`; it is
deep-object-merged over tokei's `languages.json` at startup.

## Layout

```
pkg/core   slopcount_core — the library
  lang     the language database: tokei's, merged with ours
  count    the line classifier, streaming over an AsyncRead
  report   Report / Stats, and the dimensions you can slice them by
  vfs      a read-only VFS: DirVfs over a directory, GitVfs over a tree,
           IgnoreVfs wrapping either one to mask out ignored files,
           EmptyVfs standing in for a source that does not exist
  walk     glue: list a VFS, filter, count concurrently, merge
pkg/cli    slopcount_cli — the `slopcount` binary
  path_ref parsing of `.` / `git:origin/main` into a source
  repo     locating the repo and its default branch (defaulting policy,
           which the library has no opinion about)
  render   rows, tables, JSON and CSV
```

`Report::merge` is a concatenation of per-file results, so folding thousands of
single-file reports together is linear; aggregation along a dimension happens
only when something asks for it. `Report` implements `FromIterator` and `Extend`
over both `FileReport` and `Report`, so results collect straight out of a
stream:

```rust
let report = stream_of_reports.collect::<Report>().await;
```

`IgnoreVfs` wraps any `Vfs` and masks out what its ignore files exclude, reading
those files from the wrapped VFS itself. `DirVfs` has its own built-in support
(it can prune directories mid-walk, which is faster); `IgnoreVfs` is what gives
the same semantics to a `GitVfs`, and it composes with itself.

## Accuracy

The code/comment/blank classifier is a re-implementation of tokei's
character-level state machine. `pkg/core/tests/data` is tokei's own fixture
corpus, vendored verbatim, where each file declares its expected counts in a
header comment; `cargo test` checks every one of them:

```
tokei parity: 200 fixtures matched exactly, 7 known deviations, ...
```

The seven deviations are all tokei's *embedded child language* feature, which
re-attributes the contents of Rust doc comments, Markdown code fences and HTML
`<script>` bodies to a second language. slopcount deliberately does not do this:
for telling code, docs and tests apart, a doc comment is a comment of the file
it is in. They are listed, with reasons, in `pkg/core/tests/tokei_parity.rs`.

## Performance

Counting is spread across the runtime's worker threads, one task per file, with
each file streamed through a `BufReader` so memory is bounded by the longest
line rather than the largest file. On a warm cache, 10M lines across 29k files
takes about 1.9s on an M-series laptop.

## Building

A Cargo workspace. Every third-party version is pinned once in
`[workspace.dependencies]` at the root and opted into with `foo.workspace = true`,
so the two crates can never end up on different copies of a dependency.

```sh
cargo build --release
cargo test
```

## Licence

MIT OR Apache-2.0. The language database and the test corpus come from
[tokei](https://github.com/XAMPPRocky/tokei), which is dual MIT/Apache-2.0
licensed.
