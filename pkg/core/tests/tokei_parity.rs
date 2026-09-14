//! Differential test against tokei's own fixture corpus.
//!
//! `tests/data` is vendored verbatim from tokei (dual MIT/Apache-2.0), where
//! each file states its expected counts in a header comment:
//!
//! ```text
//! //! 48 lines 36 code 6 comments 6 blanks
//! ```
//!
//! Those headers are tokei's ground truth, so running our counter over the same
//! corpus is the strongest available check that the port is faithful.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use slopcount_core::count::{count_slice, CountConfig};
use slopcount_core::lang::registry;

/// Counts as declared in a fixture's header.
#[derive(Debug, PartialEq, Eq)]
struct Expected {
    lines: u64,
    code: u64,
    comments: u64,
    blanks: u64,
}

/// Pull `N lines`, `N code`, `N comments`, `N blanks` out of a header. The
/// corpus is inconsistent about commas and ordering, so scan for pairs rather
/// than matching a fixed shape.
fn parse_header(text: &str) -> Option<Expected> {
    let mut found: BTreeMap<&str, u64> = BTreeMap::new();
    // The header is on the first line, except where a shebang pushes it down.
    for line in text.lines().take(3) {
        let words: Vec<&str> = line.split_whitespace().collect();
        for pair in words.windows(2) {
            let number = pair[0].trim_end_matches(',');
            let label = pair[1].trim_end_matches(',');
            if let Ok(value) = number.parse::<u64>() {
                if matches!(label, "lines" | "code" | "comments" | "blanks") {
                    found.entry(label).or_insert(value);
                }
            }
        }
        if found.len() == 4 {
            break;
        }
    }
    if found.len() != 4 {
        return None;
    }
    Some(Expected {
        lines: found["lines"],
        code: found["code"],
        comments: found["comments"],
        blanks: found["blanks"],
    })
}

/// Fixtures we knowingly count differently, and why.
///
/// slopcount deliberately does not implement tokei's *embedded child language*
/// feature, which re-attributes the contents of Rust doc comments, Markdown
/// code fences, HTML `<script>`/`<style>` bodies and Jupyter cells to a second
/// language. For slopcount's purpose — telling code, docs and tests apart —
/// a doc comment is simply a comment of its host file, which is both simpler
/// and more useful. Every entry below is a fixture whose expected counts
/// depend on that feature.
const KNOWN_DEVIATIONS: &[(&str, &str)] = &[
    ("rust.rs", "``` fenced code inside `//!` doc comments"),
    (
        "markdown.md",
        "counts fenced code blocks as their own languages",
    ),
    ("html.html", "embedded <script> and <style> bodies"),
    ("svelte.svelte", "embedded <script> and <style> bodies"),
    (
        "jupyter.ipynb",
        "notebook cells are parsed as their embedded language",
    ),
    ("php.php", "embedded HTML outside <?php ?>"),
    ("rakefile.rb", "embedded heredoc languages"),
];

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// tokei's own defaults, so the comparison is like-for-like.
fn tokei_config() -> CountConfig {
    CountConfig {
        // tokei's `treat_doc_strings_as_comments` defaults to unset.
        treat_doc_strings_as_comments: false,
        // Irrelevant here: we compare totals, which fold both buckets back in.
        detect_test_blocks: false,
        skip_binary: false,
    }
}

#[tokio::test]
async fn matches_tokei_on_its_own_fixture_corpus() {
    let registry = registry();
    let deviations: BTreeMap<&str, &str> = KNOWN_DEVIATIONS.iter().copied().collect();

    let mut checked = 0usize;
    let mut skipped_no_header = Vec::new();
    let mut skipped_unknown_language = Vec::new();
    let mut mismatches = Vec::new();
    let mut unexpectedly_passing = Vec::new();

    let mut entries: Vec<PathBuf> = std::fs::read_dir(data_dir())
        .expect("tests/data exists")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).expect("read fixture");
        let text = String::from_utf8_lossy(&bytes);

        let Some(expected) = parse_header(&text) else {
            skipped_no_header.push(name);
            continue;
        };
        let Some(language) = registry.from_path(&path) else {
            skipped_unknown_language.push(name);
            continue;
        };

        let stats = count_slice(&bytes, Some(language), false, &tokei_config()).await;
        let actual = Expected {
            lines: stats.lines(),
            code: stats.total().code,
            comments: stats.total().comments,
            blanks: stats.total().blanks,
        };

        let known = deviations.get(name.as_str());
        match (actual == expected, known) {
            (true, None) => checked += 1,
            (true, Some(_)) => unexpectedly_passing.push(name),
            (false, Some(_)) => {}
            (false, None) => {
                mismatches.push(format!("{name}: expected {expected:?}, got {actual:?}"))
            }
        }
    }

    eprintln!(
        "tokei parity: {checked} fixtures matched exactly, \
         {} known deviations, {} without a header, {} of unknown language",
        KNOWN_DEVIATIONS.len(),
        skipped_no_header.len(),
        skipped_unknown_language.len(),
    );

    assert!(
        checked > 150,
        "expected to check most of the corpus, only checked {checked}"
    );
    assert!(
        mismatches.is_empty(),
        "{} fixtures disagree with tokei:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert!(
        unexpectedly_passing.is_empty(),
        "these are listed as known deviations but now match; \
         remove them from KNOWN_DEVIATIONS:\n{}",
        unexpectedly_passing.join("\n")
    );
}
