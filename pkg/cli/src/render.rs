//! Turning reports into tables, JSON and CSV.

use std::collections::BTreeMap;

use slopcount_core::report::{SignedStats, Stats};
use slopcount_core::{FileReport, Report};

use crate::cli::{Dimension, Sort};

/// What a row is, once rows have been grouped into families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// An ordinary, ungrouped row.
    Plain,
    /// A family roll-up: the sum of the member rows that follow it.
    Family,
    /// A language inside the family above it. `last` draws the elbow.
    Member { last: bool },
}

/// One line of output: a label, how many files it covers, and its counts.
#[derive(Debug, Clone)]
pub struct Row {
    pub label: String,
    pub files: i64,
    pub stats: SignedStats,
    /// The family this row belongs to, when the report is grouped.
    pub family: Option<String>,
    pub kind: RowKind,
}

impl Row {
    pub fn new(label: String, files: i64, stats: SignedStats) -> Row {
        Row {
            label,
            files,
            stats,
            family: None,
            kind: RowKind::Plain,
        }
    }

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

/// The family a file contributes to, when `dimension` is grouped by family.
fn family_of(file: &FileReport, dimension: Dimension, group: bool) -> Option<String> {
    (group && dimension == Dimension::Language).then(|| file.family_name().to_string())
}

/// The label a file contributes along `dimension`.
fn label_of(file: &FileReport, dimension: Dimension, depth: usize) -> String {
    match dimension {
        Dimension::Language => file.language_name().to_string(),
        Dimension::Family => file.family_name().to_string(),
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
pub fn rows(report: &Report, dimension: Dimension, depth: usize, group: bool) -> Vec<Row> {
    let mut grouped: BTreeMap<(Option<String>, String), (i64, Stats)> = BTreeMap::new();
    for file in &report.files {
        let key = (
            family_of(file, dimension, group),
            label_of(file, dimension, depth),
        );
        let entry = grouped.entry(key).or_insert((0, Stats::default()));
        entry.0 += 1;
        entry.1 += file.stats;
    }
    grouped
        .into_iter()
        .map(|((family, label), (files, stats))| Row {
            family,
            ..Row::new(label, files, stats.into())
        })
        .collect()
}

/// Aggregate two reports into rows of net change, `after - before`.
pub fn diff_rows(
    before: &Report,
    after: &Report,
    dimension: Dimension,
    depth: usize,
    group: bool,
) -> Vec<Row> {
    let mut grouped: BTreeMap<(Option<String>, String), (i64, SignedStats)> = BTreeMap::new();
    for (report, sign) in [(after, 1i64), (before, -1i64)] {
        for file in &report.files {
            let key = (
                family_of(file, dimension, group),
                label_of(file, dimension, depth),
            );
            let entry = grouped.entry(key).or_insert((0, SignedStats::default()));
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
        .map(|((family, label), (files, stats))| Row {
            family,
            ..Row::new(label, files, stats)
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
    let rows: Vec<&Row> = rows.iter().filter(|r| r.kind != RowKind::Family).collect();
    Row::new(
        "TOTAL".to_string(),
        rows.iter().map(|r| r.files).sum(),
        rows.iter()
            .fold(SignedStats::default(), |a, r| add(a, r.stats)),
    )
}

/// Interleave family roll-ups with the rows they cover.
///
/// Families keep the order their strongest member had, which is what makes the
/// grouped table read the same way the flat one did — except when sorting by
/// name, where the families sort alphabetically too.
pub fn group_families(rows: Vec<Row>, sort: Sort) -> Vec<Row> {
    if rows.iter().all(|r| r.family.is_none()) {
        return rows;
    }

    // Preserve the incoming order of both the families and their members;
    // `rows` arrives already sorted.
    let mut order: Vec<String> = Vec::new();
    let mut members: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for row in rows {
        let family = row.family.clone().unwrap_or_default();
        if !members.contains_key(&family) {
            order.push(family.clone());
        }
        members.entry(family).or_default().push(row);
    }

    let mut out = Vec::new();
    for family in order {
        let mut children = members.remove(&family).expect("family was just inserted");
        let total = Row {
            family: Some(family.clone()),
            kind: RowKind::Family,
            ..total_row(&children)
        };
        let mut header = total;
        header.label = family.clone();
        if let Some(last) = children.last_mut() {
            last.kind = RowKind::Member { last: true };
        }
        for child in children.iter_mut().filter(|c| c.kind == RowKind::Plain) {
            child.kind = RowKind::Member { last: false };
        }
        out.push((header, children));
    }

    match sort {
        Sort::Name => out.sort_by(|a, b| a.0.label.cmp(&b.0.label)),
        sort => out.sort_by(|a, b| {
            b.0.sort_key(sort)
                .abs()
                .cmp(&a.0.sort_key(sort).abs())
                .then_with(|| a.0.label.cmp(&b.0.label))
        }),
    }

    out.into_iter()
        .flat_map(|(header, children)| std::iter::once(header).chain(children))
        .collect()
}

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// Whether to emit ANSI escapes. Decided once, by `main`, from `--color` and
/// whether stdout is a terminal.
static COLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn set_color(enabled: bool) {
    let _ = COLOR.set(enabled);
}

fn color() -> bool {
    *COLOR.get().unwrap_or(&false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    None,
    Bold,
    Dim,
    Added,
    Removed,
}

impl Style {
    /// `self` unless it is [`Style::None`], in which case `other`. Lets a row
    /// style (bold for a family) win over a per-cell one.
    fn or(self, other: Style) -> Style {
        match self {
            Style::None => other,
            style => style,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Style::None => "",
            Style::Bold => "\x1b[1m",
            Style::Dim => "\x1b[2m",
            Style::Added => "\x1b[32m",
            Style::Removed => "\x1b[31m",
        }
    }
}

fn paint(text: &str, style: Style) -> String {
    if !color() || style == Style::None || text.is_empty() {
        return text.to_string();
    }
    format!("{}{text}\x1b[0m", style.code())
}

/// Gains and losses get colour in a diff; plain counts stay plain.
fn number_style(cell: &str, signed: bool) -> Style {
    if !signed {
        return Style::None;
    }
    let trimmed = cell.trim_start();
    if trimmed.starts_with('+') {
        Style::Added
    } else if trimmed.starts_with('-') {
        Style::Removed
    } else {
        Style::None
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

/// The share of one count in another, as a percentage.
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

/// `DOC` is comment lines — documentation, in the sense that matters here.
/// Blanks have no column of their own; they are in `TOTAL`, which is every
/// line counted.
const HEADERS: [&str; 7] = ["FILES", "CODE", "DOC", "TEST", "TOTAL", "DOC%", "TEST%"];

fn cells(row: &Row, signed: bool) -> [String; 7] {
    let total = row.stats.total();
    [
        number(row.files, signed),
        number(total.code, signed),
        number(total.comments, signed),
        number(row.stats.test.code, signed),
        number(row.stats.lines(), signed),
        render_share(total.comments, row.stats.lines()),
        test_share(&row.stats),
    ]
}

/// Render the table, including a rule and a TOTAL line.
///
/// When the rows carry families, the label column becomes a tree: the family
/// roll-up flush left, its languages beneath it behind a box-drawing glyph.
pub fn table(header: &str, label_heading: &str, rows: &[Row], signed: bool) -> String {
    let total = total_row(rows);

    let labels: Vec<LabelCell> = rows.iter().map(label_cell).collect();
    let heading_label = LabelCell::plain(label_heading);
    let total_label = LabelCell::plain(&total.label);

    let mut label_width = heading_label.width().max(total_label.width());
    for label in &labels {
        label_width = label_width.max(label.width());
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

    // Cells are padded before they are coloured, so the escape sequences never
    // take part in the column arithmetic.
    let line = |label: &LabelCell, cells: &[String; 7], style: Style| {
        let mut s = label.render(label_width, style);
        for (i, cell) in cells.iter().enumerate() {
            let padded = format!("{cell:>width$}", width = widths[i]);
            s.push_str("  ");
            s.push_str(&paint(&padded, style.or(number_style(cell, signed))));
        }
        s
    };

    let width = label_width + widths.iter().map(|w| w + 2).sum::<usize>();
    let rule = paint(&"─".repeat(width), Style::Dim);

    let mut out = String::new();
    out.push_str(&paint(header, Style::Bold));
    out.push('\n');
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&line(
        &heading_label,
        &HEADERS.map(|h| h.to_string()),
        Style::Dim,
    ));
    out.push('\n');
    out.push_str(&rule);
    out.push('\n');
    for ((row, cells), label) in rows.iter().zip(&body).zip(&labels) {
        let style = match row.kind {
            RowKind::Family => Style::Bold,
            _ => Style::None,
        };
        out.push_str(&line(label, cells, style));
        out.push('\n');
    }
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&line(&total_label, &total_cells, Style::Bold));
    out.push('\n');
    out.push_str(&rule);
    out.push('\n');
    out.push_str(&summary(&total, signed));
    out.push('\n');
    out
}

/// The label column of one row: an optional tree glyph, then the name.
///
/// The two are kept apart so the glyph can be dimmed on its own while the name
/// takes the row's style.
#[derive(Debug, Clone)]
struct LabelCell {
    prefix: &'static str,
    text: String,
}

impl LabelCell {
    fn plain(text: &str) -> LabelCell {
        LabelCell {
            prefix: "",
            text: text.to_string(),
        }
    }

    fn width(&self) -> usize {
        width_of(self.prefix) + width_of(&self.text)
    }

    /// Pad to `width` and paint: the glyph dim, the name in the row's style.
    fn render(&self, width: usize, style: Style) -> String {
        let pad = width.saturating_sub(self.width());
        format!(
            "{}{}{:pad$}",
            paint(self.prefix, Style::Dim),
            paint(&self.text, style),
            "",
            pad = pad
        )
    }
}

fn label_cell(row: &Row) -> LabelCell {
    let prefix = match row.kind {
        RowKind::Member { last: false } => "├─  ",
        RowKind::Member { last: true } => "└─  ",
        RowKind::Family | RowKind::Plain => "",
    };
    LabelCell {
        prefix,
        text: row.label.clone(),
    }
}

/// Character count, which is the display width for the labels here: language
/// and family names are ASCII, and the tree glyphs are single-width.
fn width_of(s: &str) -> usize {
    s.chars().count()
}

/// The one-line headline: what share of the work was tests and docs.
fn summary(total: &Row, signed: bool) -> String {
    let stats = total.stats;
    let all = stats.total();
    let verb = if signed { "changed" } else { "counted" };
    format!(
        "{} lines {verb}: {} code, of which {} ({}) is test · {} documentation ({} of all lines)",
        number(stats.lines(), signed),
        number(all.code, signed),
        number(stats.test.code, signed),
        render_share(stats.test.code, all.code),
        number(all.comments, signed),
        render_share(all.comments, stats.lines()),
    )
}

pub fn csv(label_heading: &str, rows: &[Row]) -> String {
    let families = rows.iter().any(|r| r.family.is_some());
    let mut out = String::new();
    out.push_str(&format!(
        "{}{},files,code,comments,blanks,total,test_code,test_comments,test_blanks\n",
        if families { "family," } else { "" },
        label_heading.to_lowercase()
    ));
    for row in rows.iter().chain(std::iter::once(&total_row(rows))) {
        let total = row.stats.total();
        if families {
            out.push_str(&format!("{},", escape(row.family.as_deref().unwrap_or(""))));
        }
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
        let mut value = serde_json::json!({
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
        });
        if let Some(family) = &row.family {
            value["family"] = serde_json::Value::String(family.clone());
        }
        value
    };
    serde_json::json!({
        "source": source,
        "dimension": dimension,
        "diff": diff,
        "rows": rows.iter().map(row_json).collect::<Vec<_>>(),
        "total": row_json(&total_row(rows)),
    })
}
