# Benchmarks

slopcount measured against [tokei](https://github.com/XAMPPRocky/tokei),
[cloc](https://github.com/AlDanial/cloc) and the original
[sloccount](https://dwheeler.com/sloccount/), on the Linux kernel and the
Chromium sources — for both **speed** and **whether the counts agree**.

The second half matters more than the first. A line counter that is fast and
wrong is worthless, so every divergence below is chased to a cause.

## Setup

| | |
| --- | --- |
| Machine | Apple M2 Max, 12 cores, 32 GB RAM, macOS 15 |
| Timing | `hyperfine`, warm page cache |
| slopcount | this repo, `--release`, built with stable rustc |
| tokei | 14.0.0 |
| cloc | 2.10 |
| sloccount | 2.26 |

| Corpus | Source | Size | Files |
| --- | --- | ---: | ---: |
| Linux | `torvalds/linux`, `--depth 1` | 1.7 GB | 95,920 |
| Chromium | `chromium/src`, `--depth 1` (no `gclient sync` deps) | 5.3 GB | 506,243 |

`.git` was removed from both trees, so all four tools walk an identical file
set and none is charged for VCS metadata.

slopcount ran with `--max-file-size 0 --doc-strings-as-code`. Its defaults skip
files over 2 MB and count doc strings as comments; leaving those on would have
made it look faster and would have made the counts incomparable. Every other
tool ran with its own defaults.

## Speed

### Linux kernel — 1.7 GB, 95,920 files

| Tool | Mean | σ | Runs | Relative |
| --- | ---: | ---: | ---: | ---: |
| tokei | 5.47 s | 0.12 | 10 | **0.86×** |
| **slopcount** | **6.37 s** | 0.08 | 10 | 1.00× |
| cloc | 79.92 s | 0.12 | 3 | 12.6× |
| sloccount | 204.42 s | 9.70 | 3 | 32.1× |

### Chromium — 5.3 GB, 506,243 files

| Tool | Mean | σ | Runs | Relative |
| --- | ---: | ---: | ---: | ---: |
| tokei | 27.74 s | 5.56 | 10 | **0.81×** |
| **slopcount** | **34.15 s** | 0.37 | 10 | 1.00× |
| cloc | 387.79 s | 10.22 | 3 | 11.4× |
| sloccount | 489.68 s | — | 1 | 14.3× |

slopcount sits about 1.2× behind tokei and 11–32× ahead of the Perl and shell
tools.

Two caveats, stated rather than smoothed over:

- **sloccount's Chromium figure is a single run.** A first attempt under
  hyperfine was killed by the host under memory pressure. sloccount itself
  peaked at only 63 MB, so it was not the culprit — but with one sample there
  is no variance to report.
- **tokei's Chromium timing is unusually noisy** (σ = 5.56 s, range 22.7–38.2 s)
  against slopcount's σ = 0.37 s. Its *mean* is faster; its worst run was not.

### Memory

Peak RSS on the Linux kernel:

| Tool | Peak RSS |
| --- | ---: |
| **slopcount** | **52 MB** |
| sloccount | 60 MB (measured on Chromium) |
| tokei | 389 MB |
| cloc | 545 MB |

slopcount streams each file through a `BufReader`, so its memory is bounded by
the longest line rather than the largest file or the size of the tree.

## Concordance

### slopcount vs tokei

Essentially exact, which is the point — the classifier is a re-implementation
of tokei's.

| Corpus | Δ code | Δ comments | Δ blanks | Languages differing |
| --- | ---: | ---: | ---: | ---: |
| Linux | **+0.00%** | +0.20% | −0.15% | 6 of 51 |
| Chromium | **+0.17%** | −0.16% | −1.32% | 24 of 92 |

Linux, per language — 45 of 51 match to the line:

| Language | slopcount | tokei | Δ |
| --- | ---: | ---: | ---: |
| C | 19,901,488 | 19,901,488 | +0.00% |
| C/C++ Header | 8,524,114 | 8,524,114 | +0.00% |
| Device Tree | 1,769,342 | 1,769,342 | +0.00% |
| ReStructuredText | 633,451 | 633,451 | +0.00% |
| JSON | 613,798 | 613,798 | +0.00% |
| YAML | 541,463 | 541,463 | +0.00% |
| Assembly | 275,947 | 275,947 | +0.00% |
| Shell | 157,318 | 157,318 | +0.00% |
| Rust | 120,095 | 119,998 | +0.08% |
| Python | 105,423 | 105,423 | +0.00% |
| Makefile | 61,609 | 61,609 | +0.00% |
| Perl | 35,998 | 33,867 | +6.29% |

Every divergence has a cause:

| Cause | Where it shows | Which tool is "right" |
| --- | --- | --- |
| **Shebang detection.** slopcount identifies extension-less scripts by `#!/usr/bin/perl`; tokei skips them. | Linux Perl +2,131, AWK +221 | slopcount finds 6 real Perl scripts tokei misses |
| **Doc comments.** tokei re-attributes Rust `///` bodies to a Markdown child language; slopcount counts them as comments of the host file. | Rust comments +40,923 | Deliberate. For telling code from docs, a doc comment is a comment. |
| **Embedded child languages.** tokei moves 2.51 M lines of `<script>`/`<style>` bodies out of HTML into CSS/JS children. | Chromium HTML +114% | cloc agrees with slopcount (+121% vs tokei); tokei is the outlier |
| **Binary detection.** slopcount skips files with NUL bytes. | Chromium TypeScript −18,162 | slopcount — see below |

#### The `.ts` collision

Chromium contains 14 files like `media/test/data/bear0.ts` that are **MPEG
transport-stream video**, not TypeScript. tokei counts them as TypeScript and
extracts 18,163 lines of "code" from binary data. slopcount's NUL-byte check
skips them, which accounts for 18,163 of the 18,162-line gap — the whole thing.

### cloc

cloc diverges from both, in both directions:

| Corpus | Δ code vs tokei | Driver |
| --- | ---: | --- |
| Linux | **−5.78%** | No Device Tree support — 1,769,342 lines invisible |
| Chromium | **+10.73%** | Classifies Chromium's `.grd`/`.xtb` resource files as XML: 4,106,273 lines vs tokei's 886,097 (+363%) |

Grand totals:

| Tool | Linux code | Chromium code |
| --- | ---: | ---: |
| slopcount | 32,843,891 | 43,085,206 |
| tokei | 32,842,404 | 43,010,199 |
| cloc | 30,943,995 | 47,624,102 |
| sloccount | 28,788,358 | 25,339,175 |

### sloccount

sloccount looks far behind — −12% on Linux, −41% on Chromium — but that is
**coverage, not counting error**. Against only the languages it has a counter
for, it is close:

| Corpus | Invisible to sloccount | sloccount vs the subset it can see |
| --- | ---: | ---: |
| Linux | 3,775,455 lines (11.5%) | **−1.0%** |
| Chromium | 13,125,243 lines (30.5%) | **−7.3%** |

A tool from 2004 has no counter for JSON, HTML, XML, TypeScript, JavaScript,
CSS, YAML, Rust or Device Tree. On Chromium that is nearly a third of the
codebase:

| Unseen by sloccount | Chromium lines |
| --- | ---: |
| JSON | 3,093,935 |
| Rust | 2,339,433 |
| HTML | 2,321,554 |
| JavaScript | 2,188,811 |
| TypeScript | 1,241,721 |
| XML | 886,097 |

It also hit **388 parse failures** on Chromium (385 `c_count ERROR - terminated
in string`, 3 `terminated in comment`), so those files contribute nothing.

sloccount is also the only tool here that reports a single physical-SLOC number
per language — no comment or blank breakdown, and so nothing to say about
tests.

## What this found

The concordance check paid for itself: it exposed a real bug in slopcount.

Perl disagreed with tokei by **+7%** on the kernel. A prefix bisect of
`scripts/checkpatch.pl` pinned it to one line:

```perl
$name =~ s/^\"|\"$//g;
```

`parse_end_of_quote` had `self.quote?` hoisted to the top of the function, so
the backslash-escape branches only ran while already inside a string. In tokei
that `?` sits inside a short-circuiting `&&`, so those branches run in **plain
mode** too — which is exactly what stops `\"` in ordinary code from being read
as an opening quote. Without it, that line opened a string literal that stayed
open to the end of the file, and every comment and blank line after it counted
as code.

None of the 206 vendored tokei fixtures contain an escaped quote in plain code,
so the corpus never caught it. 96,000 real kernel files did. Fixed in `ad4844b`
with three regression tests; Linux-wide agreement went from +0.0091% to
+0.0045% of code lines, and Perl now classifies identically on every file both
tools count.

## Reproducing

```sh
git clone --depth 1 https://github.com/torvalds/linux.git
rm -rf linux/.git                     # so every tool sees the same files
cargo build --release

hyperfine --warmup 2 --runs 10 \
  -n slopcount "./target/release/slopcount linux --max-file-size 0 --doc-strings-as-code" \
  -n tokei     "tokei linux"
hyperfine --warmup 1 --runs 3 -n cloc      "cloc linux --quiet"
hyperfine            --runs 3 -n sloccount "sloccount linux"
```

For concordance, dump each tool's per-language counts and compare:

```sh
./target/release/slopcount linux --max-file-size 0 --doc-strings-as-code --format json
tokei linux -o json
cloc linux --quiet --json
```

Note that tokei's per-language rows *exclude* lines it moved into child
languages while its `Total` *includes* them, so compare totals and per-language
rows separately.
