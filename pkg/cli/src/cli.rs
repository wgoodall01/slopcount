//! Command-line surface.

use std::path::PathBuf;

use clap::{ArgAction, Parser, ValueEnum};

use crate::path_ref::PathRef;

/// Count lines of code, broken down by how much of it is tests.
///
/// Takes up to two sources. Each is a directory, or a `git:` revision resolved
/// against the local object database (slopcount never fetches).
///
/// With two sources it reports the net change from the first to the second;
/// with one it reports that source's counts; with none it compares the
/// repository's working tree against its default branch.
#[derive(Debug, Parser)]
#[command(name = "slopcount", version, about, long_about = None)]
#[command(after_help = "\
EXAMPLES:
  slopcount                                  what this branch changed, vs. the default branch
  slopcount .                                count the current directory
  slopcount git:origin/main .                changes in the working tree on top of origin/main
  slopcount dir1 dir2                        differences between two directories
  slopcount git:origin/main git:my-topic     changes on a topic branch
  slopcount --in src git:origin/main .       ...restricted to one subdirectory
")]
pub struct Cli {
    /// What to count: a directory, or a `git:` revision.
    ///
    /// Given two, slopcount reports the change from the first to the second.
    #[arg(value_name = "REF", num_args = 0..=2)]
    pub refs: Vec<PathRef>,

    /// Run as if slopcount were started in this directory.
    ///
    /// Relative paths resolve against it, and `git:` revisions are resolved in
    /// the repository containing it.
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    /// Narrow every source to this subdirectory.
    ///
    /// Applied inside each ref, so `slopcount --in src git:origin/main .`
    /// compares `src/` on the branch with `src/` in the working tree. Paths in
    /// the report are relative to it, which is what lets the two sides line up.
    ///
    /// A subdirectory that exists in one source but not the other is treated
    /// as empty there, so adding or removing a directory reads as added or
    /// removed lines rather than an error.
    #[arg(long = "in", value_name = "PATH")]
    pub subdir: Option<PathBuf>,

    // -- filtering -----------------------------------------------------------
    /// Count only paths matching this glob. Repeatable.
    #[arg(short = 'i', long, value_name = "GLOB", action = ArgAction::Append)]
    pub include: Vec<String>,

    /// Never count paths matching this glob. Repeatable.
    #[arg(short = 'e', long, value_name = "GLOB", action = ArgAction::Append)]
    pub exclude: Vec<String>,

    /// Count only these languages, by name or extension. Repeatable.
    #[arg(short = 'l', long, value_name = "LANG", action = ArgAction::Append)]
    pub language: Vec<String>,

    /// Count files whose language slopcount does not recognise.
    #[arg(long)]
    pub include_unknown: bool,

    /// Skip files larger than this many bytes. 0 lifts the limit.
    #[arg(long, value_name = "BYTES", default_value_t = 2 * 1024 * 1024)]
    pub max_file_size: u64,

    // -- ignore files --------------------------------------------------------
    /// Ignore .gitignore, .ignore and .slopcountignore.
    ///
    /// Applies to git trees too, which carry their ignore files inside them.
    #[arg(long)]
    pub no_ignore: bool,

    /// Count hidden files and directories.
    #[arg(long)]
    pub hidden: bool,

    // -- counting ------------------------------------------------------------
    /// Do not treat blocks like `#[cfg(test)]` or `describe(...)` as tests.
    /// Whole-file rules such as `*_test.go` still apply.
    #[arg(long)]
    pub no_test_blocks: bool,

    /// Count doc strings as code rather than comments.
    #[arg(long)]
    pub doc_strings_as_code: bool,

    /// Count this many files concurrently.
    #[arg(short = 'j', long, value_name = "N", default_value_t = 64)]
    pub jobs: usize,

    // -- output --------------------------------------------------------------
    /// Which dimension to break the report down by.
    #[arg(long, value_enum, default_value_t = Dimension::Language)]
    pub by: Dimension,

    /// Do not group languages into families; one flat row per language.
    #[arg(long)]
    pub no_families: bool,

    /// Shorthand for `--by file`.
    #[arg(long, conflicts_with = "by")]
    pub files: bool,

    /// Directory depth to roll up to, with `--by directory`.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub depth: usize,

    /// Sort rows by this column.
    #[arg(long, value_enum, default_value_t = Sort::Code)]
    pub sort: Sort,

    /// Show only the first N rows.
    #[arg(short = 'n', long, value_name = "N")]
    pub top: Option<usize>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,

    /// When to colour the table.
    #[arg(long, value_enum, default_value_t = Color::Auto)]
    pub color: Color,

    /// List every language slopcount knows about, and exit.
    #[arg(long)]
    pub list_languages: bool,

    /// List every family and the languages in it, and exit.
    #[arg(long)]
    pub list_families: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Dimension {
    /// One row per language, grouped into families.
    Language,
    /// One row per family, with the languages in it rolled up.
    Family,
    /// One row per file extension.
    Extension,
    /// One row per file.
    File,
    /// One row per directory, rolled up to `--depth`.
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Sort {
    /// Total code lines, descending.
    Code,
    /// Comment lines, descending.
    Comments,
    /// Blank lines, descending.
    Blanks,
    /// Test code lines, descending.
    Test,
    /// All lines, descending.
    Lines,
    /// Row label, ascending.
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Color {
    /// Colour when stdout is a terminal.
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// A human-readable table.
    Table,
    /// One JSON object.
    Json,
    /// Comma-separated values, with a header row.
    Csv,
}

impl Cli {
    /// The dimension to report on, accounting for the `--files` shorthand.
    pub fn dimension(&self) -> Dimension {
        if self.files {
            Dimension::File
        } else {
            self.by
        }
    }

    /// Whether the table groups languages into families. Only the language
    /// dimension has families to group by.
    pub fn group_families(&self) -> bool {
        !self.no_families && self.dimension() == Dimension::Language
    }

    /// Whether to emit ANSI escapes, honouring `NO_COLOR` for `--color auto`.
    pub fn color(&self) -> bool {
        use std::io::IsTerminal;
        match self.color {
            Color::Always => true,
            Color::Never => false,
            Color::Auto => {
                std::io::stdout().is_terminal()
                    && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
            }
        }
    }

    /// `--in`, tidied into a form both a filesystem join and a git tree lookup
    /// will accept. `None` when it was absent or named the source's own root.
    pub fn subdir(&self) -> Option<PathBuf> {
        let raw = self.subdir.as_ref()?.to_string_lossy().replace('\\', "/");

        let mut text = raw.trim();
        loop {
            let trimmed = text.trim_start_matches('/').trim_end_matches('/');
            let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
            if trimmed == text {
                break;
            }
            text = trimmed;
        }

        (!text.is_empty() && text != ".").then(|| PathBuf::from(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subdir_of(value: &str) -> Option<PathBuf> {
        Cli {
            subdir: Some(PathBuf::from(value)),
            ..Cli::parse_from(["slopcount"])
        }
        .subdir()
    }

    #[test]
    fn a_plain_subdirectory_is_used_as_written() {
        assert_eq!(subdir_of("src"), Some(PathBuf::from("src")));
        assert_eq!(subdir_of("src/deep"), Some(PathBuf::from("src/deep")));
    }

    #[test]
    fn shell_completion_decoration_is_trimmed() {
        // `--in src/` and `--in ./src` are what tab-completion actually types.
        for value in ["src/", "./src", "./src/", "/src/", ".//src"] {
            assert_eq!(subdir_of(value), Some(PathBuf::from("src")), "{value:?}");
        }
    }

    #[test]
    fn naming_the_root_is_the_same_as_not_narrowing() {
        for value in [".", "./", "", "/", "   "] {
            assert_eq!(subdir_of(value), None, "{value:?}");
        }
    }

    #[test]
    fn absent_when_the_flag_is_not_given() {
        assert_eq!(Cli::parse_from(["slopcount"]).subdir(), None);
    }

    #[test]
    fn the_flag_is_spelled_in() {
        let args = Cli::parse_from(["slopcount", "--in", "src"]);
        assert_eq!(args.subdir(), Some(PathBuf::from("src")));
    }
}
