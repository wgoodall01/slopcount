//! Counting results and the ways of slicing them.

use std::collections::BTreeMap;
use std::ops::{Add, AddAssign, Sub};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lang::{registry, LanguageId};

/// Line counts along the code/comment/blank axis.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub code: u64,
    pub comments: u64,
    pub blanks: u64,
}

impl Counts {
    pub fn lines(&self) -> u64 {
        self.code + self.comments + self.blanks
    }

    pub fn is_empty(&self) -> bool {
        self.lines() == 0
    }
}

impl Add for Counts {
    type Output = Counts;
    fn add(self, rhs: Counts) -> Counts {
        Counts {
            code: self.code + rhs.code,
            comments: self.comments + rhs.comments,
            blanks: self.blanks + rhs.blanks,
        }
    }
}

impl AddAssign for Counts {
    fn add_assign(&mut self, rhs: Counts) {
        *self = *self + rhs;
    }
}

/// Line counts split by whether the lines are test code.
///
/// This is the unit of aggregation: every dimension slopcount reports on is a
/// map from some key to one of these.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    /// Lines that are not test code.
    pub prod: Counts,
    /// Lines that are test code.
    pub test: Counts,
}

impl Stats {
    pub fn total(&self) -> Counts {
        self.prod + self.test
    }

    pub fn lines(&self) -> u64 {
        self.total().lines()
    }

    pub fn is_empty(&self) -> bool {
        self.lines() == 0
    }

    /// The share of counted code lines that are test code, in `0.0..=1.0`.
    /// `None` when there are no code lines to take a ratio of.
    pub fn test_ratio(&self) -> Option<f64> {
        let total = self.total().code;
        (total > 0).then(|| self.test.code as f64 / total as f64)
    }

    /// Record one classified line in the appropriate bucket.
    ///
    /// Named `record` rather than `add` so it cannot be shadowed by the
    /// by-value `Add::add` during method resolution.
    pub fn record(&mut self, kind: LineKind, is_test: bool) {
        let bucket = if is_test {
            &mut self.test
        } else {
            &mut self.prod
        };
        match kind {
            LineKind::Code => bucket.code += 1,
            LineKind::Comment => bucket.comments += 1,
            LineKind::Blank => bucket.blanks += 1,
        }
    }
}

impl Add for Stats {
    type Output = Stats;
    fn add(self, rhs: Stats) -> Stats {
        Stats {
            prod: self.prod + rhs.prod,
            test: self.test + rhs.test,
        }
    }
}

impl AddAssign for Stats {
    fn add_assign(&mut self, rhs: Stats) {
        *self = *self + rhs;
    }
}

/// What a single line was classified as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Code,
    Comment,
    Blank,
}

/// The result of counting one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    /// Path relative to the root of the VFS the file came from.
    pub path: PathBuf,
    /// `None` for files whose language could not be determined.
    pub language: Option<LanguageId>,
    /// Set when the file's *path* marked it as test code, as opposed to
    /// individual blocks within it.
    pub is_test_file: bool,
    pub stats: Stats,
}

impl FileReport {
    /// The name of the family the file's language belongs to, e.g. `JavaScript`
    /// for a `.tsx` file. Languages no family claims report `Other`.
    pub fn family_name(&self) -> &'static str {
        registry().family_name(self.language)
    }

    pub fn language_name(&self) -> &'static str {
        match self.language {
            Some(id) => registry().get(id).name.as_str(),
            None => "Unknown",
        }
    }

    /// The file's extension, lowercased. Files without one report their whole
    /// filename, which is what makes `Makefile` and `Dockerfile` legible.
    pub fn extension(&self) -> String {
        match self.path.extension().and_then(|e| e.to_str()) {
            Some(ext) => ext.to_ascii_lowercase(),
            None => self
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
        }
    }
}

/// A collection of per-file results.
///
/// Merging is a concatenation, so folding thousands of single-file reports
/// together is linear and allocation-light. Aggregation along any dimension is
/// deferred until something actually asks for it.
///
/// `Report` implements [`FromIterator`] and [`Extend`] over both [`FileReport`]
/// and `Report`, so results collect straight out of an iterator or a stream:
///
/// ```no_run
/// # use futures::stream::{self, StreamExt};
/// # use slopcount_core::{FileReport, Report};
/// # async fn example(stream: impl StreamExt<Item = Report> + Unpin) -> Report {
/// stream.collect::<Report>().await
/// # }
/// ```
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub files: Vec<FileReport>,
}

impl Report {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_file(file: FileReport) -> Self {
        Self { files: vec![file] }
    }

    /// Join two reports.
    pub fn merge(mut a: Report, b: Report) -> Report {
        a.absorb(b);
        a
    }

    /// Merge `other` into `self` in place, reusing `self`'s allocation.
    pub fn absorb(&mut self, other: Report) {
        self.files.extend(other.files);
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Grand totals across every file.
    pub fn totals(&self) -> Stats {
        self.files.iter().fold(Stats::default(), |a, f| a + f.stats)
    }

    /// Aggregate along an arbitrary dimension.
    pub fn group_by<K, F>(&self, key: F) -> BTreeMap<K, Stats>
    where
        K: Ord,
        F: Fn(&FileReport) -> K,
    {
        let mut out = BTreeMap::new();
        for file in &self.files {
            *out.entry(key(file)).or_insert_with(Stats::default) += file.stats;
        }
        out
    }

    pub fn by_language(&self) -> BTreeMap<&'static str, Stats> {
        self.group_by(FileReport::language_name)
    }

    pub fn by_family(&self) -> BTreeMap<&'static str, Stats> {
        self.group_by(FileReport::family_name)
    }

    pub fn by_extension(&self) -> BTreeMap<String, Stats> {
        self.group_by(FileReport::extension)
    }

    /// As [`Report::group_by`], but the key may borrow from the report.
    pub fn group_by_ref<'s, K, F>(&'s self, key: F) -> BTreeMap<K, Stats>
    where
        K: Ord,
        F: Fn(&'s FileReport) -> K,
    {
        let mut out = BTreeMap::new();
        for file in &self.files {
            *out.entry(key(file)).or_insert_with(Stats::default) += file.stats;
        }
        out
    }

    pub fn by_path(&self) -> BTreeMap<&Path, Stats> {
        self.group_by_ref(|f| f.path.as_path())
    }

    /// Aggregate by the first `depth` path components, which is the useful
    /// dimension for "which part of the tree did the agent touch".
    pub fn by_directory(&self, depth: usize) -> BTreeMap<PathBuf, Stats> {
        self.group_by(|f| {
            let parts: PathBuf = f.path.components().take(depth).collect();
            // A file shallower than `depth` groups under its own parent.
            let group = if parts == f.path {
                // The file is shallower than `depth`; group it under its
                // directory, which for a top-level file is the root itself.
                f.path.parent().unwrap_or(Path::new("")).to_path_buf()
            } else {
                parts
            };
            if group.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                group
            }
        })
    }

    /// Sort files in place, largest first, by total lines.
    pub fn sort_by_size(&mut self) {
        self.files
            .sort_by_key(|f| std::cmp::Reverse(f.stats.lines()));
    }
}

impl Extend<FileReport> for Report {
    fn extend<I: IntoIterator<Item = FileReport>>(&mut self, iter: I) {
        self.files.extend(iter);
    }
}

impl Extend<Report> for Report {
    fn extend<I: IntoIterator<Item = Report>>(&mut self, iter: I) {
        for report in iter {
            self.absorb(report);
        }
    }
}

impl FromIterator<Report> for Report {
    fn from_iter<I: IntoIterator<Item = Report>>(iter: I) -> Self {
        let mut out = Report::new();
        for report in iter {
            out.absorb(report);
        }
        out
    }
}

impl FromIterator<FileReport> for Report {
    fn from_iter<I: IntoIterator<Item = FileReport>>(iter: I) -> Self {
        Report {
            files: iter.into_iter().collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Signed arithmetic, for diffing two reports.
// ---------------------------------------------------------------------------

/// Line counts that may be negative, produced by diffing two [`Report`]s.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCounts {
    pub code: i64,
    pub comments: i64,
    pub blanks: i64,
}

impl SignedCounts {
    pub fn lines(&self) -> i64 {
        self.code + self.comments + self.blanks
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl From<Counts> for SignedCounts {
    fn from(c: Counts) -> Self {
        Self {
            code: c.code as i64,
            comments: c.comments as i64,
            blanks: c.blanks as i64,
        }
    }
}

impl Sub for SignedCounts {
    type Output = SignedCounts;
    fn sub(self, rhs: SignedCounts) -> SignedCounts {
        SignedCounts {
            code: self.code - rhs.code,
            comments: self.comments - rhs.comments,
            blanks: self.blanks - rhs.blanks,
        }
    }
}

impl Add for SignedCounts {
    type Output = SignedCounts;
    fn add(self, rhs: SignedCounts) -> SignedCounts {
        SignedCounts {
            code: self.code + rhs.code,
            comments: self.comments + rhs.comments,
            blanks: self.blanks + rhs.blanks,
        }
    }
}

/// [`Stats`] that may be negative.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedStats {
    pub prod: SignedCounts,
    pub test: SignedCounts,
}

impl SignedStats {
    pub fn total(&self) -> SignedCounts {
        self.prod + self.test
    }

    pub fn lines(&self) -> i64 {
        self.total().lines()
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl From<Stats> for SignedStats {
    fn from(s: Stats) -> Self {
        Self {
            prod: s.prod.into(),
            test: s.test.into(),
        }
    }
}

impl Sub for SignedStats {
    type Output = SignedStats;
    fn sub(self, rhs: SignedStats) -> SignedStats {
        SignedStats {
            prod: self.prod - rhs.prod,
            test: self.test - rhs.test,
        }
    }
}

/// The net change between two reports: `after - before`, keyed by some
/// dimension. Keys present in only one side are treated as zero on the other.
pub fn diff_by<K: Ord + Clone>(
    before: &BTreeMap<K, Stats>,
    after: &BTreeMap<K, Stats>,
) -> BTreeMap<K, SignedStats> {
    let mut out: BTreeMap<K, SignedStats> = BTreeMap::new();
    for (k, v) in after {
        out.insert(k.clone(), SignedStats::from(*v));
    }
    for (k, v) in before {
        let entry = out.entry(k.clone()).or_default();
        *entry = *entry - SignedStats::from(*v);
    }
    out.retain(|_, v| !v.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::registry;

    fn counts(code: u64, comments: u64, blanks: u64) -> Counts {
        Counts {
            code,
            comments,
            blanks,
        }
    }

    fn file(path: &str, lang: &str, prod: Counts, test: Counts) -> FileReport {
        FileReport {
            path: PathBuf::from(path),
            language: registry().by_key(lang),
            is_test_file: path.starts_with("tests/"),
            stats: Stats { prod, test },
        }
    }

    fn sample() -> Report {
        Report {
            files: vec![
                file("src/main.rs", "Rust", counts(10, 2, 3), counts(0, 0, 0)),
                file("src/lib.rs", "Rust", counts(20, 5, 4), counts(7, 1, 0)),
                file("tests/it.rs", "Rust", counts(0, 0, 0), counts(30, 3, 5)),
                file(
                    "web/app.ts",
                    "TypeScript",
                    counts(40, 1, 2),
                    counts(0, 0, 0),
                ),
            ],
        }
    }

    // -- arithmetic ----------------------------------------------------------

    #[test]
    fn counts_add_componentwise() {
        assert_eq!(counts(1, 2, 3) + counts(10, 20, 30), counts(11, 22, 33));
        assert_eq!(counts(1, 2, 3).lines(), 6);
        assert!(Counts::default().is_empty());
    }

    #[test]
    fn stats_add_both_buckets_independently() {
        let a = Stats {
            prod: counts(1, 0, 0),
            test: counts(0, 2, 0),
        };
        let b = Stats {
            prod: counts(0, 0, 3),
            test: counts(4, 0, 0),
        };
        let sum = a + b;
        assert_eq!(sum.prod, counts(1, 0, 3));
        assert_eq!(sum.test, counts(4, 2, 0));
        assert_eq!(sum.total(), counts(5, 2, 3));
        assert_eq!(sum.lines(), 10);
    }

    #[test]
    fn adding_a_line_lands_in_the_right_bucket() {
        let mut stats = Stats::default();
        stats.record(LineKind::Code, false);
        stats.record(LineKind::Comment, false);
        stats.record(LineKind::Blank, true);
        stats.record(LineKind::Code, true);
        assert_eq!(stats.prod, counts(1, 1, 0));
        assert_eq!(stats.test, counts(1, 0, 1));
    }

    #[test]
    fn test_ratio_ignores_comments_and_blanks() {
        let stats = Stats {
            prod: counts(1, 100, 100),
            test: counts(3, 0, 0),
        };
        assert_eq!(stats.test_ratio(), Some(0.75));
    }

    // -- merging -------------------------------------------------------------

    #[test]
    fn merging_concatenates_files() {
        let a = Report::from_file(file("a.rs", "Rust", counts(1, 0, 0), Counts::default()));
        let b = Report::from_file(file("b.rs", "Rust", counts(2, 0, 0), Counts::default()));
        let merged = Report::merge(a, b);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged.totals().prod, counts(3, 0, 0));
    }

    #[test]
    fn merging_is_associative() {
        let make =
            |n: u64| Report::from_file(file("x.rs", "Rust", counts(n, 0, 0), Counts::default()));
        let left = Report::merge(Report::merge(make(1), make(2)), make(3));
        let right = Report::merge(make(1), Report::merge(make(2), make(3)));
        assert_eq!(left.totals(), right.totals());
    }

    #[test]
    fn merging_with_an_empty_report_changes_nothing() {
        let report = sample();
        let totals = report.totals();
        assert_eq!(Report::merge(report, Report::new()).totals(), totals);
    }

    #[test]
    fn an_empty_report_has_zero_totals() {
        let report = Report::new();
        assert!(report.is_empty());
        assert_eq!(report.totals(), Stats::default());
        assert_eq!(report.totals().test_ratio(), None);
    }

    #[test]
    fn extending_absorbs_both_reports_and_files() {
        let mut report = Report::new();
        report.extend([file("a.rs", "Rust", counts(1, 0, 0), Counts::default())]);
        report.extend([Report::from_file(file(
            "b.rs",
            "Rust",
            counts(2, 0, 0),
            Counts::default(),
        ))]);
        assert_eq!(report.len(), 2);
        assert_eq!(report.totals().prod.code, 3);
    }

    #[tokio::test]
    async fn a_stream_of_reports_collects_into_one() {
        use futures::stream::{self, StreamExt};

        let report = stream::iter(1..=3)
            .map(|n| Report::from_file(file("x.rs", "Rust", counts(n, 0, 0), Counts::default())))
            .collect::<Report>()
            .await;
        assert_eq!(report.len(), 3);
        assert_eq!(report.totals().prod.code, 6);
    }

    #[tokio::test]
    async fn a_stream_of_file_reports_collects_into_one() {
        use futures::stream::{self, StreamExt};

        let report = stream::iter(1..=3)
            .map(|n| file("x.rs", "Rust", counts(n, 0, 0), Counts::default()))
            .collect::<Report>()
            .await;
        assert_eq!(report.totals().prod.code, 6);
    }

    #[test]
    fn collecting_reports_merges_them() {
        let report: Report = (1..=3)
            .map(|n| Report::from_file(file("x.rs", "Rust", counts(n, 0, 0), Counts::default())))
            .collect();
        assert_eq!(report.totals().prod.code, 6);
    }

    // -- aggregation ---------------------------------------------------------

    #[test]
    fn totals_sum_every_file() {
        let totals = sample().totals();
        assert_eq!(totals.prod, counts(70, 8, 9));
        assert_eq!(totals.test, counts(37, 4, 5));
    }

    #[test]
    fn grouping_by_language_merges_files_of_that_language() {
        let by_language = sample().by_language();
        assert_eq!(by_language.len(), 2);
        assert_eq!(by_language["Rust"].prod, counts(30, 7, 7));
        assert_eq!(by_language["Rust"].test, counts(37, 4, 5));
        assert_eq!(by_language["TypeScript"].prod, counts(40, 1, 2));
    }

    #[test]
    fn files_of_unknown_language_group_together() {
        let report = Report {
            files: vec![FileReport {
                path: PathBuf::from("a.wat"),
                language: None,
                is_test_file: false,
                stats: Stats::default(),
            }],
        };
        assert_eq!(report.files[0].language_name(), "Unknown");
        assert!(report.by_language().contains_key("Unknown"));
    }

    #[test]
    fn grouping_by_family_rolls_related_languages_together() {
        let by_family = sample().by_family();
        // Rust is in Systems; TypeScript is in the JavaScript family.
        assert_eq!(by_family["Systems"].total(), counts(67, 11, 12));
        assert_eq!(by_family["JavaScript"].total(), counts(40, 1, 2));
    }

    #[test]
    fn a_language_no_family_claims_falls_under_other() {
        let report = Report {
            files: vec![FileReport {
                path: PathBuf::from("a.wat"),
                language: None,
                is_test_file: false,
                stats: Stats {
                    prod: counts(1, 0, 0),
                    test: Counts::default(),
                },
            }],
        };
        assert_eq!(report.files[0].family_name(), "Other");
        assert_eq!(report.by_family()["Other"].prod.code, 1);
    }

    #[test]
    fn grouping_by_extension_lowercases_and_falls_back_to_the_filename() {
        let report = Report {
            files: vec![
                file("a.RS", "Rust", counts(1, 0, 0), Counts::default()),
                file("b.rs", "Rust", counts(1, 0, 0), Counts::default()),
                file("Makefile", "Makefile", counts(5, 0, 0), Counts::default()),
            ],
        };
        let by_extension = report.by_extension();
        assert_eq!(by_extension["rs"].prod.code, 2);
        assert_eq!(by_extension["Makefile"].prod.code, 5);
    }

    #[test]
    fn grouping_by_path_keeps_files_separate() {
        let report = sample();
        let by_path = report.by_path();
        assert_eq!(by_path.len(), 4);
        assert_eq!(by_path[Path::new("src/main.rs")].prod.code, 10);
    }

    #[test]
    fn grouping_by_directory_rolls_up_to_the_requested_depth() {
        let by_dir = sample().by_directory(1);
        assert_eq!(by_dir[Path::new("src")].total(), counts(37, 8, 7));
        assert_eq!(by_dir[Path::new("tests")].total(), counts(30, 3, 5));
        assert_eq!(by_dir[Path::new("web")].total(), counts(40, 1, 2));
    }

    #[test]
    fn a_file_shallower_than_the_grouping_depth_groups_under_its_parent() {
        let report = Report {
            files: vec![file(
                "README.md",
                "Markdown",
                counts(3, 0, 0),
                Counts::default(),
            )],
        };
        let by_dir = report.by_directory(2);
        assert_eq!(by_dir[Path::new(".")].prod.code, 3);
    }

    #[test]
    fn grouping_by_an_arbitrary_dimension_works() {
        let by_test_file = sample().group_by(|f| f.is_test_file);
        assert_eq!(by_test_file[&true].test.code, 30);
        assert_eq!(by_test_file[&false].prod.code, 70);
    }

    #[test]
    fn sorting_by_size_puts_the_biggest_file_first() {
        let mut report = sample();
        report.sort_by_size();
        assert_eq!(report.files[0].path, Path::new("web/app.ts"));
    }

    // -- diffing -------------------------------------------------------------

    #[test]
    fn diffing_reports_the_net_change_per_key() {
        let before = sample().by_language();
        let mut after_report = sample();
        after_report.files.push(file(
            "web/new.ts",
            "TypeScript",
            counts(5, 0, 0),
            Counts::default(),
        ));
        let diff = diff_by(&before, &after_report.by_language());

        // Rust is unchanged, so it drops out entirely.
        assert!(!diff.contains_key("Rust"));
        assert_eq!(diff["TypeScript"].prod.code, 5);
    }

    #[test]
    fn a_key_that_only_exists_before_reports_a_negative_change() {
        let before = sample().by_language();
        let after = Report::new().by_language();
        let diff = diff_by(&before, &after);
        assert_eq!(diff["TypeScript"].prod.code, -40);
        assert_eq!(diff["Rust"].test.code, -37);
    }

    #[test]
    fn diffing_a_report_against_itself_is_empty() {
        let by_language = sample().by_language();
        assert!(diff_by(&by_language, &by_language).is_empty());
    }

    #[test]
    fn signed_stats_sum_their_buckets() {
        let stats = SignedStats::from(Stats {
            prod: counts(1, 2, 3),
            test: counts(10, 20, 30),
        }) - SignedStats::from(Stats {
            prod: counts(5, 0, 0),
            test: counts(0, 0, 0),
        });
        assert_eq!(stats.prod.code, -4);
        assert_eq!(stats.total().code, 6);
        assert_eq!(stats.lines(), 61);
    }
}
