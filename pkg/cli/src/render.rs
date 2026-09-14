//! Turning reports into tables, JSON and CSV.

use std::collections::BTreeMap;

use slopcount_core::report::{SignedStats, Stats};
use slopcount_core::{FileReport, Report};

use crate::cli::{Dimension, Sort};

/// One line of output: a label, how many files it covers, and its counts.
#[derive(Debug, Clone)]
pub struct Row {
    pub label: String,
    pub files: i64,
    pub stats: SignedStats,
}

impl Row {
    fn sort_key(&self, sort: Sort) -> i64 {
        match sort {
            Sort::Code => self.stats.total().code,
            Sort::Comments => self.stats.total().comments,
            Sort::Blanks => self.stats.total().blanks,
            Sort::Test => self.stats.test.code,
            Sort::Lines => self.stats.lines(),
            Sort::Name => 0,
        }
    }
}

/// The label a file contributes along `dimension`.
fn label_of(file: &FileReport, dimension: Dimension, depth: usize) -> String {
    match dimension {
        Dimension::Language => file.language_name().to_string(),
        Dimension::Extension => file.extension(),
        Dimension::File => file.path.display().to_string(),
        Dimension::Directory => {
            let parts: std::path::PathBuf = file.path.components().take(depth).collect();
            let group = if parts == file.path {
                file.path
                    .parent()
                    .unwrap_or(std::path::Path::new(""))
                    .to_path_buf()
            } else {
                parts
            };
            if group.as_os_str().is_empty() {
                ".".to_string()
            } else {
                group.display().to_string()
            }
        }
    }
}

/// Aggregate a report into rows along `dimension`.
pub fn rows(report: &Report, dimension: Dimension, depth: usize) -> Vec<Row> {
    let mut grouped: BTreeMap<String, (i64, Stats)> = BTreeMap::new();
    for file in &report.files {
        let entry = grouped
            .entry(label_of(file, dimension, depth))
            .or_insert((0, Stats::default()));
        entry.0 += 1;
        entry.1 += file.stats;
    }
    grouped
        .into_iter()
        .map(|(label, (files, stats))| Row {
            label,
            files,
            stats: stats.into(),
        })
        .collect()
}

/// Aggregate two reports into rows of net change, `after - before`.
pub fn diff_rows(before: &Report, after: &Report, dimension: Dimension, depth: usize) -> Vec<Row> {
    let mut grouped: BTreeMap<String, (i64, SignedStats)> = BTreeMap::new();
    for (report, sign) in [(after, 1i64), (before, -1i64)] {
        for file in &report.files {
            let entry = grouped
                .entry(label_of(file, dimension, depth))
                .or_insert((0, SignedStats::default()));
            entry.0 += sign;
            let stats = SignedStats::from(file.stats);
            entry.1 = if sign > 0 {
                add(entry.1, stats)
            } else {
                entry.1 - stats
            };
        }
    }
    grouped
        .into_iter()
        .filter(|(_, (files, stats))| *files != 0 || !stats.is_empty())
        .map(|(label, (files, stats))| Row {
            label,
            files,
            stats,
        })
        .collect()
}

fn add(a: SignedStats, b: SignedStats) -> SignedStats {
    SignedStats {
        prod: a.prod + b.prod,
        test: a.test + b.test,
    }
}

/// Sort rows and apply `--top`.
pub fn finish(mut rows: Vec<Row>, sort: Sort, top: Option<usize>) -> Vec<Row> {
    match sort {
        Sort::Name => rows.sort_by(|a, b| a.label.cmp(&b.label)),
        sort => rows.sort_by(|a, b| {
            b.sort_key(sort)
                .abs()
                .cmp(&a.sort_key(sort).abs())
                .then_with(|| a.label.cmp(&b.label))
        }),
    }
    if let Some(top) = top {
        rows.truncate(top);
    }
    rows
}

pub fn total_row(rows: &[Row]) -> Row {
    Row {
        label: "TOTAL".to_string(),
        files: rows.iter().map(|r| r.files).sum(),
        stats: rows
            .iter()
            .fold(SignedStats::default(), |a, r| add(a, r.stats)),
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// `1234567` becomes `1,234,567`, with a sign when `signed` is set.
fn number(n: i64, signed: bool) -> String {
    let mut digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    while digits.len() > 3 {
        let rest = digits.split_off(digits.len() - 3);
        out = if out.is_empty() {
            rest
        } else {
            format!("{rest},{out}")
        };
    }
    let body = if out.is_empty() {
        digits
    } else {
        format!("{digits},{out}")
    };
    if signed && n > 0 {
        format!("+{body}")
    } else if n < 0 {
        format!("-{body}")
    } else {
        body
    }
}

/// The share of code lines that are test code, as a percentage.
///
/// Returns `None` when the figure would not mean anything. That happens in a
/// diff: if a change deletes production code and adds tests, the *net* code
/// change is a denominator the test count can exceed, and "125%" reads as a
/// bug rather than as information. The CODE and TEST columns still show what
/// actually happened.
fn share(part: i64, whole: i64) -> Option<f64> {
    (whole > 0 && (0..=whole).contains(&part)).then(|| 100.0 * part as f64 / whole as f64)
}

fn render_share(part: i64, whole: i64) -> String {
    match share(part, whole) {
        Some(pct) => format!("{pct:.0}%"),
        None => "—".to_string(),
    }
}

fn test_share(stats: &SignedStats) -> String {
    render_share(stats.test.code, stats.total().code)
}

const HEADERS: [&str; 7] = [
    "FILES", "CODE", "COMMENT", "BLANK", "TOTAL", "TEST", "TEST%",
];

fn cells(row: &Row, signed: bool) -> [String; 7] {
    let total = row.stats.total();
    [
        number(row.files, signed),
        number(total.code, signed),
        number(total.comments, signed),
        number(total.blanks, signed),
        number(row.stats.lines(), signed),
        number(row.stats.test.code, signed),
        test_share(&row.stats),
    ]
}

/// Render the table, including a rule and a TOTAL line.
pub fn table(header: &str, label_heading: &str, rows: &[Row], signed: bool) -> String {
    let total = total_row(rows);

    let mut label_width = label_heading.len().max(total.label.len());
    for row in rows {
        label_width = label_width.max(row.label.len());
    }

    let body: Vec<[String; 7]> = rows.iter().map(|r| cells(r, signed)).collect();
    let total_cells = cells(&total, signed);
    let mut widths = [0usize; 7];
    for (i, width) in widths.iter_mut().enumerate() {
        *width = HEADERS[i].len().max(total_cells[i].len());
        for row in &body {
            *width = (*width).max(row[i].len());
        }
    }

    let line = |label: &str, cells: &[String; 7]| {
        let mut s = format!("{label:<label_width$}");
        for (i, cell) in cells.iter().enumerate() {
            s.push_str(&format!("  {:>width$}", cell, width = widths[i]));
        }
        s
    };

    let width = label_width + widths.iter().map(|w| w + 2).sum::<usize>();
    let rule = "─".repeat(width);

    let mut out = String::new();
    out.push_str(header);
    out.push('\n');
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&line(label_heading, &HEADERS.map(|h| h.to_string())));
    out.push('\n');
    out.push_str(&rule);
    out.push('\n');
    for (row, cells) in rows.iter().zip(&body) {
        out.push_str(&line(&row.label, cells));
        out.push('\n');
    }
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&line(&total.label, &total_cells));
    out.push('\n');
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&summary(&total, signed));
    out.push('\n');
    out
}

/// The one-line headline: what share of the work was tests and docs.
fn summary(total: &Row, signed: bool) -> String {
    let stats = total.stats;
    let all = stats.total();
    let verb = if signed { "changed" } else { "counted" };
    format!(
        "{} lines {verb}: {} code, of which {} ({}) is test · {} comment ({} of all lines)",
        number(stats.lines(), signed),
        number(all.code, signed),
        number(stats.test.code, signed),
        render_share(stats.test.code, all.code),
        number(all.comments, signed),
        render_share(all.comments, stats.lines()),
    )
}

pub fn csv(label_heading: &str, rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{},files,code,comments,blanks,total,test_code,test_comments,test_blanks\n",
        label_heading.to_lowercase()
    ));
    for row in rows.iter().chain(std::iter::once(&total_row(rows))) {
        let total = row.stats.total();
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            escape(&row.label),
            row.files,
            total.code,
            total.comments,
            total.blanks,
            row.stats.lines(),
            row.stats.test.code,
            row.stats.test.comments,
            row.stats.test.blanks,
        ));
    }
    out
}

fn escape(field: &str) -> String {
    if field.contains([',', '"', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

pub fn json(source: &str, dimension: &str, rows: &[Row], diff: bool) -> serde_json::Value {
    let row_json = |row: &Row| {
        let total = row.stats.total();
        serde_json::json!({
            "label": row.label,
            "files": row.files,
            "code": total.code,
            "comments": total.comments,
            "blanks": total.blanks,
            "lines": row.stats.lines(),
            "prod": {
                "code": row.stats.prod.code,
                "comments": row.stats.prod.comments,
                "blanks": row.stats.prod.blanks,
            },
            "test": {
                "code": row.stats.test.code,
                "comments": row.stats.test.comments,
                "blanks": row.stats.test.blanks,
            },
        })
    };
    serde_json::json!({
        "source": source,
        "dimension": dimension,
        "diff": diff,
        "rows": rows.iter().map(row_json).collect::<Vec<_>>(),
        "total": row_json(&total_row(rows)),
    })
}
