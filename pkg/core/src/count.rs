//! Line counting.
//!
//! The code/comment/blank classifier is a re-implementation of tokei's
//! character-level state machine (tokei is dual MIT/Apache-2.0 licensed). The
//! machine has three modes:
//!
//! - *plain*: blanks count as blanks, quotes can open string mode, comment
//!   delimiters can open comment mode;
//! - *string*: comment delimiters are inert;
//! - *comment*: quotes are inert, and delimiters may nest.
//!
//! On top of that we track a fourth, orthogonal axis that tokei does not model:
//! whether the line is *test* code. See [`TestTracker`].
//!
//! Unlike tokei we never hold the whole file: lines are pulled from an
//! [`AsyncRead`] through a [`BufReader`], so memory is bounded by the longest
//! line. This is sound because no comment or string delimiter in the language
//! database contains a newline, so no delimiter can straddle a line boundary.

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::lang::{BlockStyle, LanguageDef, LanguageId};
use crate::report::{LineKind, Stats};

/// Knobs that change how lines are classified.
#[derive(Debug, Clone)]
pub struct CountConfig {
    /// Count doc strings (Python's `"""..."""`) as comments rather than code.
    pub treat_doc_strings_as_comments: bool,
    /// Recognise test code *inside* a file, not just whole test files.
    pub detect_test_blocks: bool,
    /// Skip files that look binary instead of counting them.
    pub skip_binary: bool,
}

impl Default for CountConfig {
    fn default() -> Self {
        Self {
            treat_doc_strings_as_comments: true,
            detect_test_blocks: true,
            skip_binary: true,
        }
    }
}

/// How many lines a test marker may wait for its opening brace before we decide
/// it was a false positive and give up on the block.
const MAX_LINES_AWAITING_BRACE: usize = 10;

/// Outcome of counting a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CountOutcome {
    Counted(Stats),
    /// The file contained a NUL byte within the first block read.
    Binary,
}

/// Count one file, streaming it from `reader`.
///
/// `language` of `None` means the language is unknown; such files are counted
/// with a trivial syntax (every non-blank line is code), which keeps them
/// visible in totals instead of silently vanishing.
pub async fn count<R>(
    reader: R,
    language: Option<LanguageId>,
    is_test_file: bool,
    config: &CountConfig,
) -> std::io::Result<CountOutcome>
where
    R: AsyncRead + Unpin,
{
    let registry = crate::lang::registry();
    let lang = language.map(|id| registry.get(id));

    let mut reader = BufReader::new(reader);
    let mut counter = Counter::new(lang, is_test_file, config);
    let mut line = Vec::with_capacity(256);
    let mut first = true;

    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).await?;
        if n == 0 {
            break;
        }
        if first {
            first = false;
            if config.skip_binary && line.contains(&0) {
                return Ok(CountOutcome::Binary);
            }
        }
        // Strip the line terminator; a trailing `\r` would otherwise defeat
        // suffix matching on closing delimiters.
        let mut raw = line.as_slice();
        if raw.last() == Some(&b'\n') {
            raw = &raw[..raw.len() - 1];
        }
        if raw.last() == Some(&b'\r') {
            raw = &raw[..raw.len() - 1];
        }
        counter.push_line(raw);
    }

    Ok(CountOutcome::Counted(counter.finish()))
}

/// Convenience wrapper for counting an in-memory buffer.
pub async fn count_slice(
    bytes: &[u8],
    language: Option<LanguageId>,
    is_test_file: bool,
    config: &CountConfig,
) -> Stats {
    match count(bytes, language, is_test_file, config).await {
        Ok(CountOutcome::Counted(stats)) => stats,
        // Reading a slice cannot fail, and callers of this helper are asking
        // for counts regardless of binary-ness.
        _ => Stats::default(),
    }
}

// ---------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------

struct Counter<'a> {
    lang: Option<&'a LanguageDef>,
    config: &'a CountConfig,
    stats: Stats,

    /// The closing delimiter of the string we are inside, if any.
    quote: Option<&'a str>,
    quote_is_doc: bool,
    quote_is_verbatim: bool,
    /// Closing delimiters of the comments we are inside, outermost first.
    stack: Vec<&'a str>,

    is_test_file: bool,
    tests: TestTracker,
}

impl<'a> Counter<'a> {
    fn new(lang: Option<&'a LanguageDef>, is_test_file: bool, config: &'a CountConfig) -> Self {
        let track_blocks = config.detect_test_blocks
            && !is_test_file
            && lang.is_some_and(|l| !l.test_blocks.is_empty());
        Self {
            lang,
            config,
            stats: Stats::default(),
            quote: None,
            quote_is_doc: false,
            quote_is_verbatim: false,
            stack: Vec::new(),
            is_test_file,
            tests: TestTracker::new(track_blocks),
        }
    }

    fn finish(self) -> Stats {
        self.stats
    }

    fn is_plain(&self) -> bool {
        self.quote.is_none() && self.stack.is_empty()
    }

    fn push_line(&mut self, raw: &[u8]) {
        let Some(lang) = self.lang else {
            // Unknown language: no comment or string syntax to speak of.
            let kind = if raw.trim().is_empty() {
                LineKind::Blank
            } else {
                LineKind::Code
            };
            let is_test = self.is_test_file;
            self.stats.record(kind, is_test);
            return;
        };

        // FORTRAN only treats a marker as a comment in the first column, so
        // trimming leading whitespace would change the answer.
        let line = if lang.is_fortran { raw } else { raw.trim() };

        // Blank lines are only blank in plain mode; a blank line inside a block
        // comment is part of the comment.
        if self.is_plain() && line.trim().is_empty() {
            let is_test = self.tests.classify_blank(self.is_test_file);
            self.stats.record(LineKind::Blank, is_test);
            return;
        }

        let started_in_comments = !self.stack.is_empty()
            || (self.config.treat_doc_strings_as_comments
                && self.quote.is_some()
                && self.quote_is_doc);

        let plain_at_start = self.is_plain();
        let scan = self.scan_line(lang, line);

        let kind = if lang.literate || self.line_is_comment(lang, line, started_in_comments) {
            LineKind::Comment
        } else {
            LineKind::Code
        };

        let is_test = self.tests.classify(
            self.is_test_file,
            lang,
            raw,
            line,
            plain_at_start,
            scan.brace_delta,
        );
        self.stats.record(kind, is_test);
    }

    /// Walk the line one byte at a time, updating string/comment state, and
    /// report what we learned about it.
    fn scan_line(&mut self, lang: &'a LanguageDef, line: &[u8]) -> LineScan {
        let mut scan = LineScan::default();
        let mut skip = 0usize;

        let mut i = 0;
        while i < line.len() {
            if skip > 0 {
                skip -= 1;
                i += 1;
                continue;
            }
            let window = &line[i..];
            if window.trim().is_empty() {
                break;
            }

            // Closing an open string or comment takes priority over anything
            // that closing delimiter's text might otherwise start.
            if let Some(n) = self
                .parse_end_of_quote(lang, window)
                .or_else(|| self.parse_end_of_multi_line(window))
            {
                skip = n.saturating_sub(1);
                i += 1;
                continue;
            }
            if self.quote.is_some() {
                i += 1;
                continue;
            }

            if let Some(n) = self
                .parse_quote(lang, window)
                .or_else(|| self.parse_multi_line_comment(lang, window))
            {
                skip = n.saturating_sub(1);
                i += 1;
                continue;
            }

            // A line comment swallows the rest of the line, braces included.
            if self.stack.is_empty()
                && lang
                    .line_comments
                    .iter()
                    .any(|c| window.starts_with(c.as_bytes()))
            {
                break;
            }

            if self.is_plain() {
                match window[0] {
                    b'{' => scan.brace_delta += 1,
                    b'}' => scan.brace_delta -= 1,
                    _ => {}
                }
            }
            i += 1;
        }

        scan
    }

    /// tokei's heuristics for whether a whole line reads as a comment.
    fn line_is_comment(
        &self,
        lang: &'a LanguageDef,
        line: &[u8],
        started_in_comments: bool,
    ) -> bool {
        let trimmed = line.trim();

        if self.quote.is_some() {
            // Inside a string. Only doc strings can read as comments, and only
            // when configured to.
            return self.quote_is_doc && self.config.treat_doc_strings_as_comments;
        }

        // We left a doc string on this line, having started the line in one.
        if started_in_comments
            && lang
                .doc_quotes
                .iter()
                .any(|(_, end)| contains(line, end.as_bytes()))
        {
            return true;
        }

        // `// x`, or a block comment opened and closed on one line.
        let spans_whole_line = |(start, end): &(String, String)| {
            trimmed.starts_with(start.as_bytes())
                && trimmed.ends_with(end.as_bytes())
                // `\"\"\"` alone must not match as both its own opener and closer.
                && trimmed.len() >= start.len() + end.len()
        };
        let whole_line_is_comment = lang
            .line_comments
            .iter()
            .any(|c| trimmed.starts_with(c.as_bytes()))
            || lang.any_multi_line_comments.iter().any(spans_whole_line)
            // A doc string opened and closed on one line, e.g. Python's
            // `\"\"\"One-line summary.\"\"\"`.
            || (self.config.treat_doc_strings_as_comments
                && lang.doc_quotes.iter().any(spans_whole_line));
        if whole_line_is_comment || started_in_comments {
            return true;
        }

        // The line opened a block comment that is still open.
        match self.stack.last() {
            Some(open) => lang
                .any_multi_line_comments
                .iter()
                .any(|(start, end)| end == open && trimmed.starts_with(start.as_bytes())),
            None => false,
        }
    }

    fn parse_quote(&mut self, lang: &'a LanguageDef, window: &[u8]) -> Option<usize> {
        if !self.stack.is_empty() {
            return None;
        }
        // Doc quotes first: `"""` must beat `"`.
        for (set, is_doc, is_verbatim) in [
            (&lang.doc_quotes, true, false),
            (&lang.verbatim_quotes, false, true),
            (&lang.quotes, false, false),
        ] {
            if let Some((start, end)) = set.iter().find(|(s, _)| window.starts_with(s.as_bytes())) {
                self.quote = Some(end);
                self.quote_is_doc = is_doc;
                self.quote_is_verbatim = is_verbatim;
                return Some(start.len());
            }
        }
        None
    }

    fn parse_end_of_quote(&mut self, lang: &'a LanguageDef, window: &[u8]) -> Option<usize> {
        let quote = self.quote?;
        if window.starts_with(quote.as_bytes()) {
            self.quote = None;
            return Some(quote.len());
        }
        if self.quote_is_verbatim {
            return None;
        }
        // An escaped backslash, or an escaped quote character: skip both bytes
        // so the escapee cannot close the string.
        if window.starts_with(br"\\") {
            return Some(2);
        }
        if window.starts_with(br"\")
            && lang
                .quotes
                .iter()
                .any(|(start, _)| window[1..].starts_with(start.as_bytes()))
        {
            return Some(2);
        }
        None
    }

    fn parse_multi_line_comment(&mut self, lang: &'a LanguageDef, window: &[u8]) -> Option<usize> {
        if self.quote.is_some() {
            return None;
        }
        for (start, end) in lang.multi_line_comments.iter().chain(&lang.nested_comments) {
            if window.starts_with(start.as_bytes()) {
                // Only push when the comment can actually nest; otherwise the
                // inner delimiter is just text inside the outer comment.
                let nests = lang.nested
                    || lang
                        .nested_comments
                        .iter()
                        .any(|(s, e)| s == start && e == end);
                if self.stack.is_empty() || nests {
                    self.stack.push(end);
                }
                return Some(start.len());
            }
        }
        None
    }

    fn parse_end_of_multi_line(&mut self, window: &[u8]) -> Option<usize> {
        let last = *self.stack.last()?;
        if window.starts_with(last.as_bytes()) {
            self.stack.pop();
            Some(last.len())
        } else {
            None
        }
    }
}

#[derive(Debug, Default)]
struct LineScan {
    /// Net `{` minus `}` seen in plain mode on this line.
    brace_delta: i32,
}

// ---------------------------------------------------------------------------
// Test detection
// ---------------------------------------------------------------------------

/// Tracks whether we are inside a region of test code.
///
/// A region opens when a line starts with one of the language's `test_blocks`
/// markers (`#[cfg(test)]`, `func TestFoo`, `describe(`, …) and closes at the
/// end of the block that marker introduces — either when brace depth returns to
/// zero, or, for indentation-delimited languages, when the indentation returns
/// to the marker's level.
#[derive(Debug)]
struct TestTracker {
    enabled: bool,
    state: Option<BlockState>,
}

#[derive(Debug)]
struct BlockState {
    style: BlockStyle,
    /// Indentation of the marker line, for [`BlockStyle::Indent`].
    indent: usize,
    depth: i32,
    /// Whether the opening brace has been seen yet.
    opened: bool,
    /// Lines elapsed since the marker, while still waiting for `{`.
    waiting: usize,
}

impl TestTracker {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            state: None,
        }
    }

    /// Blank lines never open or close a region; they simply inherit it.
    fn classify_blank(&self, is_test_file: bool) -> bool {
        is_test_file || self.state.is_some()
    }

    fn classify(
        &mut self,
        is_test_file: bool,
        lang: &LanguageDef,
        raw: &[u8],
        line: &[u8],
        plain_at_start: bool,
        brace_delta: i32,
    ) -> bool {
        if is_test_file {
            return true;
        }
        if !self.enabled {
            return false;
        }

        let indent = raw.len() - raw.trim_start().len();

        if self.state.is_none() {
            // Markers are only meaningful in plain mode; the same text inside a
            // string or comment means nothing.
            if plain_at_start
                && lang
                    .test_blocks
                    .iter()
                    .any(|m| line.starts_with(m.as_bytes()))
            {
                self.state = Some(BlockState {
                    style: lang.test_block_style,
                    indent,
                    depth: brace_delta,
                    opened: brace_delta > 0,
                    waiting: 0,
                });
                return true;
            }
            return false;
        }

        let state = self.state.as_mut().expect("checked above");
        match state.style {
            BlockStyle::Indent => {
                // The region covers everything indented deeper than the marker.
                if indent > state.indent {
                    true
                } else {
                    self.state = None;
                    // This line may itself open a new region.
                    self.classify(is_test_file, lang, raw, line, plain_at_start, brace_delta)
                }
            }
            BlockStyle::Braces => {
                state.depth += brace_delta;
                if brace_delta > 0 {
                    state.opened = true;
                }
                if state.opened {
                    if state.depth <= 0 {
                        // The closing brace is the last line of the region.
                        self.state = None;
                    }
                    true
                } else {
                    state.waiting += 1;
                    if state.waiting > MAX_LINES_AWAITING_BRACE {
                        // The marker never introduced a block; treat it as a
                        // false positive and re-examine this line.
                        self.state = None;
                        self.classify(is_test_file, lang, raw, line, plain_at_start, brace_delta)
                    } else {
                        true
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Byte-slice helpers
// ---------------------------------------------------------------------------

/// `str::trim` and friends, for byte slices. tokei has the same helper.
trait SliceExt {
    fn trim(&self) -> &Self;
    fn trim_start(&self) -> &Self;
}

impl SliceExt for [u8] {
    fn trim(&self) -> &[u8] {
        let start = self.iter().position(|c| !c.is_ascii_whitespace());
        match start {
            Some(start) => {
                let end = self
                    .iter()
                    .rposition(|c| !c.is_ascii_whitespace())
                    .unwrap_or(start);
                &self[start..=end]
            }
            None => &[],
        }
    }

    fn trim_start(&self) -> &[u8] {
        match self.iter().position(|c| !c.is_ascii_whitespace()) {
            Some(start) => &self[start..],
            None => &[],
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::registry;
    use crate::report::Counts;

    /// Count `src` as the language with the given `languages.json` key.
    fn count_as(key: &str, src: &str) -> Stats {
        count_with(key, src, &CountConfig::default())
    }

    fn count_with(key: &str, src: &str, config: &CountConfig) -> Stats {
        let id = registry()
            .by_key(key)
            .unwrap_or_else(|| panic!("unknown language key {key:?}"));
        block_on(count_slice(src.as_bytes(), Some(id), false, config))
    }

    /// Count `src` as a whole-file test.
    fn count_test_file(key: &str, src: &str) -> Stats {
        let id = registry().by_key(key).unwrap();
        block_on(count_slice(
            src.as_bytes(),
            Some(id),
            true,
            &CountConfig::default(),
        ))
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// `(code, comments, blanks)` of the non-test bucket.
    fn prod(s: &Stats) -> (u64, u64, u64) {
        (s.prod.code, s.prod.comments, s.prod.blanks)
    }

    /// `(code, comments, blanks)` of the test bucket.
    fn test(s: &Stats) -> (u64, u64, u64) {
        (s.test.code, s.test.comments, s.test.blanks)
    }

    fn total(s: &Stats) -> Counts {
        s.total()
    }

    // -- basic classification ------------------------------------------------

    #[test]
    fn empty_input_counts_nothing() {
        assert_eq!(count_as("Rust", ""), Stats::default());
    }

    #[test]
    fn counts_code_comments_and_blanks() {
        let src = "\
fn main() {

    // a comment
    println!(\"hi\");
}
";
        assert_eq!(prod(&count_as("Rust", src)), (3, 1, 1));
    }

    #[test]
    fn a_file_without_a_trailing_newline_still_counts_its_last_line() {
        assert_eq!(prod(&count_as("Rust", "let x = 1;")), (1, 0, 0));
    }

    #[test]
    fn windows_line_endings_are_stripped() {
        let src = "fn a() {}\r\n\r\n// c\r\n";
        assert_eq!(prod(&count_as("Rust", src)), (1, 1, 1));
    }

    #[test]
    fn whitespace_only_lines_are_blank() {
        assert_eq!(prod(&count_as("Rust", "\t \n   \n\n")), (0, 0, 3));
    }

    // -- line comments -------------------------------------------------------

    #[test]
    fn trailing_line_comments_count_as_code() {
        // The line does work, so it is code; tokei attributes the whole line to
        // whichever category it leads with.
        assert_eq!(prod(&count_as("Rust", "let x = 1; // set x\n")), (1, 0, 0));
    }

    #[test]
    fn indented_line_comments_are_comments() {
        assert_eq!(prod(&count_as("Rust", "        // hi\n")), (0, 1, 0));
    }

    #[test]
    fn line_comment_markers_inside_strings_are_not_comments() {
        assert_eq!(
            prod(&count_as("Rust", "let url = \"https://example.com\";\n")),
            (1, 0, 0)
        );
    }

    #[test]
    fn a_hash_is_only_a_comment_in_languages_that_say_so() {
        assert_eq!(prod(&count_as("Python", "# hi\n")), (0, 1, 0));
        // In Rust `#` opens an attribute, not a comment.
        assert_eq!(prod(&count_as("Rust", "#![allow(dead_code)]\n")), (1, 0, 0));
    }

    // -- block comments ------------------------------------------------------

    #[test]
    fn single_line_block_comment() {
        assert_eq!(prod(&count_as("Rust", "/* hi */\n")), (0, 1, 0));
    }

    #[test]
    fn multi_line_block_comment() {
        let src = "/*\n * hi\n */\nfn a() {}\n";
        assert_eq!(prod(&count_as("Rust", src)), (1, 3, 0));
    }

    #[test]
    fn blank_lines_inside_a_block_comment_count_as_comment() {
        // The line is inside the comment, so it is not a blank separating code.
        let src = "/*\n\n*/\n";
        assert_eq!(prod(&count_as("Rust", src)), (0, 3, 0));
    }

    #[test]
    fn code_after_a_block_comment_ends_on_the_same_line_is_code() {
        assert_eq!(prod(&count_as("Rust", "/* hi */ let x = 1;\n")), (1, 0, 0));
    }

    #[test]
    fn rust_block_comments_nest() {
        let src = "/* outer /* inner */ still outer */\nlet x = 1;\n";
        assert_eq!(prod(&count_as("Rust", src)), (1, 1, 0));
    }

    #[test]
    fn c_block_comments_do_not_nest() {
        // In C the first `*/` closes the comment, so the rest is code.
        let src = "/* outer /* inner */ x();\n";
        assert_eq!(prod(&count_as("C", src)), (1, 0, 0));
    }

    #[test]
    fn block_comment_delimiters_inside_strings_are_inert() {
        let src = "let s = \"/*\";\nlet t = 1;\n";
        assert_eq!(prod(&count_as("Rust", src)), (2, 0, 0));
    }

    #[test]
    fn an_unterminated_block_comment_swallows_the_rest_of_the_file() {
        let src = "/* oops\nfn a() {}\nfn b() {}\n";
        assert_eq!(prod(&count_as("Rust", src)), (0, 3, 0));
    }

    #[test]
    fn d_has_its_own_nesting_comment_delimiters() {
        let src = "/+ outer /+ inner +/ still outer +/\nint x;\n";
        assert_eq!(prod(&count_as("D", src)), (1, 1, 0));
    }

    // -- strings -------------------------------------------------------------

    #[test]
    fn multi_line_strings_are_code() {
        let src = "let s = \"one\ntwo\nthree\";\n";
        assert_eq!(prod(&count_as("Rust", src)), (3, 0, 0));
    }

    #[test]
    fn escaped_quotes_do_not_end_a_string() {
        let src = "let s = \"a \\\" /* b\";\nlet t = 1;\n";
        assert_eq!(prod(&count_as("Rust", src)), (2, 0, 0));
    }

    #[test]
    fn an_escaped_backslash_does_not_escape_the_closing_quote() {
        // "a\\" is a complete string; the `//` that follows is a comment.
        let src = "let s = \"a\\\\\"; // done\nlet t = 1;\n";
        assert_eq!(prod(&count_as("Rust", src)), (2, 0, 0));
    }

    #[test]
    fn verbatim_strings_do_not_honour_backslash_escapes() {
        let src = "let s = r#\"a \\\"#;\nlet t = 1;\n";
        assert_eq!(prod(&count_as("Rust", src)), (2, 0, 0));
    }

    // -- doc strings ---------------------------------------------------------

    #[test]
    fn python_doc_strings_count_as_comments_by_default() {
        let src = "def f():\n    \"\"\"Docs.\n\n    More docs.\n    \"\"\"\n    return 1\n";
        let stats = count_as("Python", src);
        assert_eq!(prod(&stats), (2, 4, 0));
    }

    #[test]
    fn python_doc_strings_count_as_code_when_configured() {
        let config = CountConfig {
            treat_doc_strings_as_comments: false,
            ..CountConfig::default()
        };
        let src = "def f():\n    \"\"\"Docs.\"\"\"\n    return 1\n";
        assert_eq!(prod(&count_with("Python", src, &config)), (3, 0, 0));
    }

    #[test]
    fn a_single_line_doc_string_is_a_comment() {
        let src = "\"\"\"Module docs.\"\"\"\nimport os\n";
        assert_eq!(prod(&count_as("Python", src)), (1, 1, 0));
    }

    // -- literate languages --------------------------------------------------

    #[test]
    fn markdown_prose_counts_as_comments() {
        let src = "# Title\n\nSome prose.\n";
        let stats = count_as("Markdown", src);
        assert_eq!(stats.prod.code, 0);
        assert_eq!(stats.prod.comments, 2);
        assert_eq!(stats.prod.blanks, 1);
    }

    // -- unknown languages ---------------------------------------------------

    #[test]
    fn unknown_languages_count_every_non_blank_line_as_code() {
        let stats = block_on(count_slice(
            b"one\n\ntwo\n",
            None,
            false,
            &CountConfig::default(),
        ));
        assert_eq!(prod(&stats), (2, 0, 1));
    }

    // -- binary detection ----------------------------------------------------

    #[test]
    fn files_with_nul_bytes_are_reported_as_binary() {
        let id = registry().by_key("Rust").unwrap();
        let outcome = block_on(count(
            &b"\x7fELF\x00\x00\x01"[..],
            Some(id),
            false,
            &CountConfig::default(),
        ))
        .unwrap();
        assert_eq!(outcome, CountOutcome::Binary);
    }

    #[test]
    fn binary_detection_can_be_switched_off() {
        let config = CountConfig {
            skip_binary: false,
            ..CountConfig::default()
        };
        let outcome = block_on(count(&b"\x00\n"[..], None, false, &config)).unwrap();
        assert!(matches!(outcome, CountOutcome::Counted(_)));
    }

    // -- test detection: whole files -----------------------------------------

    #[test]
    fn a_test_file_puts_every_line_in_the_test_bucket() {
        let src = "fn a() {}\n// c\n\n";
        let stats = count_test_file("Rust", src);
        assert_eq!(prod(&stats), (0, 0, 0));
        assert_eq!(test(&stats), (1, 1, 1));
    }

    // -- test detection: blocks ----------------------------------------------

    #[test]
    fn rust_cfg_test_modules_are_test_code() {
        let src = "\
fn real() {
    work();
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}

fn also_real() {}
";
        let stats = count_as("Rust", src);
        // `fn real`, its body, its brace, and `fn also_real`.
        assert_eq!(prod(&stats).0, 4);
        // The whole `#[cfg(test)]` block, braces included.
        assert_eq!(test(&stats).0, 7);
        assert_eq!(total(&stats).lines(), 13);
    }

    #[test]
    fn a_bare_rust_test_attribute_covers_just_its_function() {
        let src = "\
#[test]
fn one() {
    ok();
}
fn two() {
    work();
}
";
        let stats = count_as("Rust", src);
        assert_eq!(test(&stats).0, 4);
        assert_eq!(prod(&stats).0, 3);
    }

    #[test]
    fn blank_lines_inside_a_test_block_are_test_blanks() {
        let src = "#[cfg(test)]\nmod tests {\n\n    fn a() {}\n}\n";
        let stats = count_as("Rust", src);
        assert_eq!(test(&stats), (4, 0, 1));
        assert_eq!(prod(&stats), (0, 0, 0));
    }

    #[test]
    fn go_test_functions_are_test_code() {
        let src = "\
package main

func Add(a, b int) int {
\treturn a + b
}

func TestAdd(t *testing.T) {
\tif Add(1, 2) != 3 {
\t\tt.Fatal(\"bad\")
\t}
}
";
        let stats = count_as("Go", src);
        assert_eq!(test(&stats).0, 5);
        assert_eq!(prod(&stats).0, 4);
    }

    #[test]
    fn python_test_functions_end_at_the_dedent() {
        let src = "\
def real():
    return 1

def test_real():
    assert real() == 1

def also_real():
    return 2
";
        let stats = count_as("Python", src);
        assert_eq!(test(&stats).0, 2);
        assert_eq!(prod(&stats).0, 4);
    }

    #[test]
    fn python_test_classes_are_test_code() {
        let src = "\
class TestThing:
    def test_a(self):
        assert True

    def test_b(self):
        assert True

x = 1
";
        let stats = count_as("Python", src);
        assert_eq!(test(&stats).0, 5);
        assert_eq!(test(&stats).2, 2);
        assert_eq!(prod(&stats).0, 1);
    }

    #[test]
    fn javascript_describe_blocks_are_test_code() {
        let src = "\
export function add(a, b) {
  return a + b;
}

describe('add', () => {
  it('adds', () => {
    expect(add(1, 2)).toBe(3);
  });
});
";
        let stats = count_as("JavaScript", src);
        assert_eq!(test(&stats).0, 5);
        assert_eq!(prod(&stats).0, 3);
    }

    #[test]
    fn test_markers_inside_strings_do_not_open_a_block() {
        // The marker must appear in plain mode at the start of a line.
        let src = "let s = \"\n#[cfg(test)]\n\";\nfn real() {}\n";
        let stats = count_as("Rust", src);
        assert_eq!(test(&stats).0, 0);
        assert_eq!(prod(&stats).0, 4);
    }

    #[test]
    fn test_markers_inside_comments_do_not_open_a_block() {
        let src = "/*\n#[cfg(test)]\n*/\nfn real() {}\n";
        let stats = count_as("Rust", src);
        assert_eq!(test(&stats).0, 0);
        assert_eq!(prod(&stats), (1, 3, 0));
    }

    #[test]
    fn braces_inside_strings_do_not_close_a_test_block() {
        let src = "\
#[cfg(test)]
mod tests {
    fn a() {
        let s = \"}\";
    }
}
fn real() {}
";
        let stats = count_as("Rust", src);
        assert_eq!(test(&stats).0, 6);
        assert_eq!(prod(&stats).0, 1);
    }

    #[test]
    fn braces_inside_line_comments_do_not_close_a_test_block() {
        let src = "#[cfg(test)]\nmod tests {\n    // }\n    fn a() {}\n}\nfn real() {}\n";
        let stats = count_as("Rust", src);
        assert_eq!(test(&stats), (4, 1, 0));
        assert_eq!(prod(&stats).0, 1);
    }

    #[test]
    fn a_marker_that_never_opens_a_block_is_abandoned() {
        // Ten lines of grace, then the tracker gives up rather than marking the
        // rest of the file as tests.
        let mut src = String::from("#[test]\n");
        for i in 0..40 {
            src.push_str(&format!("let x{i} = {i};\n"));
        }
        let stats = count_as("Rust", &src);
        assert_eq!(total(&stats).code, 41);
        assert!(
            stats.prod.code >= 30,
            "most lines should stay production, got {stats:?}"
        );
    }

    #[test]
    fn test_block_detection_can_be_switched_off() {
        let config = CountConfig {
            detect_test_blocks: false,
            ..CountConfig::default()
        };
        let src = "#[cfg(test)]\nmod tests {\n    fn a() {}\n}\n";
        let stats = count_with("Rust", src, &config);
        assert_eq!(test(&stats).0, 0);
        assert_eq!(prod(&stats).0, 4);
    }

    #[test]
    fn languages_without_markers_report_no_test_blocks() {
        let src = "body { color: red; }\n";
        let stats = count_as("Css", src);
        assert_eq!(test(&stats).0, 0);
        assert_eq!(prod(&stats).0, 1);
    }

    // -- Stats bookkeeping ---------------------------------------------------

    #[test]
    fn every_line_lands_in_exactly_one_bucket() {
        let src = "\
fn a() {
    // c
}

#[cfg(test)]
mod t {
    // c

    fn b() {}
}
";
        let stats = count_as("Rust", src);
        assert_eq!(total(&stats).lines(), src.lines().count() as u64);
    }

    #[test]
    fn test_ratio_is_the_share_of_code_lines() {
        let stats = Stats {
            prod: Counts {
                code: 3,
                ..Counts::default()
            },
            test: Counts {
                code: 1,
                ..Counts::default()
            },
        };
        assert_eq!(stats.test_ratio(), Some(0.25));
        assert_eq!(Stats::default().test_ratio(), None);
    }
}
