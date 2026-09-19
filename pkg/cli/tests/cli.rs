//! End-to-end tests that run the real binary.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_slopcount"))
}

/// Run slopcount and require success, returning stdout.
fn run(args: &[&str]) -> String {
    let out = bin().args(args).output().expect("run slopcount");
    assert!(
        out.status.success(),
        "slopcount {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 output")
}

fn tree(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, contents).unwrap();
    }
    dir
}

const LIB_RS: &str = "\
//! Docs.

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
";

fn json(args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&run(args)).expect("valid json")
}

fn row<'a>(value: &'a serde_json::Value, label: &str) -> &'a serde_json::Value {
    value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == label)
        .unwrap_or_else(|| panic!("no row {label:?} in {value}"))
}

// -- directory mode ----------------------------------------------------------

#[test]
fn counts_a_directory_and_separates_test_code() {
    let dir = tree(&[("src/lib.rs", LIB_RS)]);
    let out = json(&[dir.path().to_str().unwrap(), "--format", "json"]);

    let rust = row(&out, "Rust");
    assert_eq!(rust["files"], 1);
    // The `#[cfg(test)]` block, but not the function above it.
    assert_eq!(rust["test"]["code"], 7);
    assert_eq!(rust["impl"]["code"], 3);
    assert_eq!(rust["impl"]["comments"], 1);
    assert_eq!(out["diff"], false);
}

#[test]
fn defaults_to_the_current_directory() {
    let dir = tree(&[("a.rs", "fn a() {}\n")]);
    let out = bin()
        .current_dir(dir.path())
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["total"]["impl"]["code"], 1);
}

#[test]
fn the_table_output_names_its_columns_and_totals() {
    let dir = tree(&[("src/lib.rs", LIB_RS)]);
    let table = run(&[dir.path().to_str().unwrap()]);
    for expected in [
        "LANGUAGE", "IMPL", "DOC", "TEST", "TOTAL", "DOC%", "TEST%", "Rust",
    ] {
        assert!(
            table.contains(expected),
            "missing {expected:?} in:\n{table}"
        );
    }
    assert!(table.contains("of code"), "no summary line in:\n{table}");
}

/// `IMPL`, `DOC` and `TEST` name disjoint sets of lines: the implementation
/// column must not double-count the lines the test column already claims.
#[test]
fn the_impl_column_excludes_test_lines() {
    let dir = tree(&[("src/lib.rs", LIB_RS)]);
    let table = run(&[dir.path().to_str().unwrap(), "--color", "never"]);
    let total = table
        .lines()
        .find(|l| l.starts_with("TOTAL"))
        .unwrap_or_else(|| panic!("no TOTAL row in:\n{table}"));
    let cells: Vec<&str> = total.split_whitespace().skip(1).collect();
    // FILES, IMPL, DOC, TEST: the 3 production code lines, not all 10.
    assert_eq!(&cells[..4], &["1", "3", "1", "7"], "in:\n{table}");
    assert!(
        table.contains("3 implementation · 7 test"),
        "summary does not separate impl from test in:\n{table}"
    );
}

#[test]
fn csv_output_has_a_header_and_a_total_row() {
    let dir = tree(&[("src/lib.rs", LIB_RS)]);
    let csv = run(&[dir.path().to_str().unwrap(), "--format", "csv"]);
    let mut lines = csv.lines();
    assert_eq!(
        lines.next().unwrap(),
        "family,language,files,impl_code,impl_comments,impl_blanks,test_code,test_comments,test_blanks,total"
    );
    assert!(csv.lines().any(|l| l.starts_with("Systems,Rust,")));
    assert!(csv.lines().any(|l| l.starts_with(",TOTAL,")));
}

/// The whole point of `impl`/`test`: they partition the lines, so a consumer
/// can add fields without double-counting. A roll-up spanning the two would
/// break that, so assert there isn't one.
#[test]
fn json_counts_are_disjoint_and_sum_to_the_line_count() {
    let dir = tree(&[("src/lib.rs", LIB_RS)]);
    let out = json(&[dir.path().to_str().unwrap(), "--format", "json"]);
    let total = &out["total"];

    let n = |bucket: &str, field: &str| total[bucket][field].as_i64().unwrap();
    let summed: i64 = ["impl", "test"]
        .iter()
        .flat_map(|b| ["code", "comments", "blanks"].map(|f| n(b, f)))
        .sum();
    assert_eq!(summed, total["lines"].as_i64().unwrap(), "in {out}");

    for rolled_up in ["code", "comments", "blanks", "prod"] {
        assert!(
            total.get(rolled_up).is_none(),
            "{rolled_up:?} spans impl and test; it would double-count in {out}"
        );
    }
}

#[test]
fn csv_counts_are_disjoint_and_sum_to_the_total_column() {
    let dir = tree(&[("src/lib.rs", LIB_RS)]);
    let csv = run(&[dir.path().to_str().unwrap(), "--format", "csv"]);
    let total = csv
        .lines()
        .find(|l| l.starts_with(",TOTAL,"))
        .unwrap_or_else(|| panic!("no TOTAL row in:\n{csv}"));
    // family, label, files, then the six disjoint cells and the total.
    let cells: Vec<i64> = total
        .split(',')
        .skip(3)
        .map(|c| c.parse().unwrap())
        .collect();
    let (counts, total_cell) = cells.split_at(6);
    assert_eq!(counts.iter().sum::<i64>(), total_cell[0], "in:\n{csv}");
}

// -- families ----------------------------------------------------------------

#[test]
fn the_table_groups_languages_under_their_family() {
    let dir = tree(&[
        ("src/lib.rs", LIB_RS),
        ("web/app.ts", "export const a = 1;\n"),
        ("web/app.tsx", "export const b = 1;\n"),
    ]);
    let table = run(&[dir.path().to_str().unwrap()]);

    for expected in ["LANGUAGE", "JavaScript", "Systems", "├─  ", "└─  "] {
        assert!(
            table.contains(expected),
            "missing {expected:?} in:\n{table}"
        );
    }
    // The family roll-up comes before the languages it covers.
    let family = table.find("JavaScript").unwrap();
    assert!(family < table.find("TypeScript").unwrap());
    assert!(family < table.find("TSX").unwrap());
}

#[test]
fn families_can_be_turned_off() {
    let dir = tree(&[("web/app.ts", "export const a = 1;\n")]);
    let table = run(&[dir.path().to_str().unwrap(), "--no-families"]);
    assert!(
        !table.contains("├─") && !table.contains("└─"),
        "still grouped:\n{table}"
    );
    assert!(table.contains("TypeScript"));
}

#[test]
fn a_family_row_sums_the_languages_in_it() {
    let dir = tree(&[
        ("a.ts", "export const a = 1;\n"),
        ("b.tsx", "export const b = 1;\n"),
        ("c.rs", "fn c() {}\n"),
    ]);
    let out = json(&[
        dir.path().to_str().unwrap(),
        "--format",
        "json",
        "--by",
        "family",
    ]);
    assert_eq!(row(&out, "JavaScript")["impl"]["code"], 2);
    assert_eq!(row(&out, "JavaScript")["files"], 2);
    assert_eq!(row(&out, "Systems")["impl"]["code"], 1);
}

#[test]
fn json_rows_carry_the_family_of_each_language() {
    let dir = tree(&[("a.tsx", "export const a = 1;\n")]);
    let out = json(&[dir.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(row(&out, "TSX")["family"], "JavaScript");
}

#[test]
fn a_language_no_family_claims_lands_in_other() {
    // Hex0 is not in families.json.
    let dir = tree(&[("a.hex0", "00\n")]);
    let out = json(&[dir.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(row(&out, "Hex0")["family"], "Other");
}

#[test]
fn listing_families_names_their_languages() {
    let out = run(&["--list-families"]);
    assert!(out.contains("FAMILY"));
    let line = out
        .lines()
        .find(|l| l.starts_with("JavaScript"))
        .expect("a JavaScript family");
    for language in ["TypeScript", "TSX", "JSX"] {
        assert!(line.contains(language), "missing {language:?} in {line:?}");
    }
}

#[test]
fn color_is_off_unless_asked_for() {
    let dir = tree(&[("a.rs", "fn a() {}\n")]);
    let path = dir.path().to_str().unwrap();
    // Output is a pipe here, so `auto` means no escapes.
    assert!(!run(&[path]).contains('\x1b'));
    assert!(run(&[path, "--color", "always"]).contains('\x1b'));
    assert!(!run(&[path, "--color", "never"]).contains('\x1b'));
}

// -- dimensions and filters --------------------------------------------------

#[test]
fn every_dimension_produces_rows() {
    let dir = tree(&[
        ("src/lib.rs", LIB_RS),
        ("web/app.ts", "export const a = 1;\n"),
    ]);
    let path = dir.path().to_str().unwrap();

    assert!(row(
        &json(&[path, "--format", "json", "--by", "language"]),
        "Rust"
    )["impl"]["code"]
        .is_number());
    assert!(row(
        &json(&[path, "--format", "json", "--by", "extension"]),
        "rs"
    )["impl"]["code"]
        .is_number());
    assert!(row(
        &json(&[path, "--format", "json", "--by", "directory"]),
        "src"
    )["impl"]["code"]
        .is_number());

    let by_file = json(&[path, "--format", "json", "--files"]);
    assert!(row(&by_file, "src/lib.rs")["impl"]["code"].is_number());
}

#[test]
fn a_language_filter_restricts_the_report() {
    let dir = tree(&[("a.rs", "fn a() {}\n"), ("b.py", "x = 1\n")]);
    let out = json(&[
        dir.path().to_str().unwrap(),
        "--format",
        "json",
        "-l",
        "rust",
    ]);
    assert_eq!(out["rows"].as_array().unwrap().len(), 1);
    assert_eq!(out["rows"][0]["label"], "Rust");
}

#[test]
fn include_and_exclude_globs_apply() {
    let dir = tree(&[
        ("keep.rs", "fn a() {}\n"),
        ("vendor/skip.rs", "fn b() {}\n"),
    ]);
    let path = dir.path().to_str().unwrap();

    let excluded = json(&[path, "--format", "json", "-e", "vendor/"]);
    assert_eq!(excluded["total"]["impl"]["code"], 1);

    let included = json(&[path, "--format", "json", "-i", "vendor/"]);
    assert_eq!(included["total"]["impl"]["code"], 1);
    assert_eq!(row(&included, "Rust")["files"], 1);
}

#[test]
fn top_limits_the_number_of_rows() {
    let dir = tree(&[
        ("a.rs", "fn a() {}\n"),
        ("b.py", "x = 1\n"),
        ("c.ts", "const a = 1;\n"),
    ]);
    let out = json(&[dir.path().to_str().unwrap(), "--format", "json", "-n", "2"]);
    assert_eq!(out["rows"].as_array().unwrap().len(), 2);
}

#[test]
fn sorting_by_name_orders_rows_alphabetically() {
    let dir = tree(&[("a.rs", "fn a() {}\n"), ("b.py", "x = 1\n")]);
    let out = json(&[
        dir.path().to_str().unwrap(),
        "--format",
        "json",
        "--sort",
        "name",
    ]);
    let labels: Vec<&str> = out["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Python", "Rust"]);
}

#[test]
fn ignore_files_are_respected_unless_disabled() {
    let dir = tree(&[
        (".gitignore", "generated/\n"),
        ("a.rs", "fn a() {}\n"),
        ("generated/b.rs", "fn b() {}\n"),
    ]);
    let path = dir.path().to_str().unwrap();
    assert_eq!(
        json(&[path, "--format", "json"])["total"]["impl"]["code"],
        1
    );
    assert_eq!(
        json(&[path, "--format", "json", "--no-ignore", "--hidden"])["total"]["impl"]["code"],
        2
    );
}

#[test]
fn test_block_detection_can_be_disabled() {
    let dir = tree(&[("src/lib.rs", LIB_RS)]);
    let out = json(&[
        dir.path().to_str().unwrap(),
        "--format",
        "json",
        "--no-test-blocks",
    ]);
    assert_eq!(row(&out, "Rust")["test"]["code"], 0);
}

// -- git modes ---------------------------------------------------------------

struct Repo(TempDir);

impl Repo {
    fn init() -> Self {
        let repo = Repo(tempfile::tempdir().unwrap());
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@example.com"]);
        repo.git(&["config", "user.name", "T"]);
        repo
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
    fn write(&self, path: &str, contents: &str) {
        let full = self.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, contents).unwrap();
    }
    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// Fabricate the remote-tracking refs a clone would have, without a remote
    /// to fetch from — slopcount must work entirely from the local database.
    fn set_origin_head(&self, branch: &str) {
        let head = format!("refs/remotes/origin/{branch}");
        self.git(&["update-ref", &head, "HEAD"]);
        self.git(&["symbolic-ref", "refs/remotes/origin/HEAD", &head]);
    }
}

#[test]
fn counts_a_single_git_revision() {
    let repo = Repo::init();
    repo.write("src/lib.rs", LIB_RS);
    repo.commit("first");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD",
        "--format",
        "json",
    ]);
    assert_eq!(out["source"], "HEAD");
    assert_eq!(row(&out, "Rust")["test"]["code"], 7);
}

#[test]
fn a_revision_sees_the_tree_as_it_was() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.write("a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
    repo.commit("second");

    let path = repo.path().to_str().unwrap();
    assert_eq!(
        json(&["-C", path, "git:HEAD~1", "--format", "json"])["total"]["impl"]["code"],
        1
    );
    assert_eq!(
        json(&["-C", path, "git:HEAD", "--format", "json"])["total"]["impl"]["code"],
        3
    );
}

#[test]
fn a_range_reports_the_net_change() {
    let repo = Repo::init();
    repo.write("src/lib.rs", "pub fn a() {}\n");
    repo.commit("first");
    repo.write(
        "src/lib.rs",
        "pub fn a() {}\npub fn b() {}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn x() {}\n}\n",
    );
    repo.commit("second");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--format",
        "json",
    ]);
    assert_eq!(out["diff"], true);
    let rust = row(&out, "Rust");
    // One production line added, and the whole five-line test block.
    assert_eq!(rust["impl"]["code"], 1);
    assert_eq!(rust["test"]["code"], 5);
    // The file existed before and after, so the file count does not move.
    assert_eq!(rust["files"], 0);
}

#[test]
fn a_range_reports_deletions_as_negative() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
    repo.commit("first");
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("second");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], -2);
}

#[test]
fn a_deleted_file_shows_as_a_negative_file_count() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.write("b.rs", "fn b() {}\n");
    repo.commit("first");
    std::fs::remove_file(repo.path().join("b.rs")).unwrap();
    repo.commit("second");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--format",
        "json",
    ]);
    assert_eq!(row(&out, "Rust")["files"], -1);
}

#[test]
fn an_open_ended_range_compares_against_the_working_directory() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    // Uncommitted work.
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD",
        ".",
        "--format",
        "json",
    ]);
    assert_eq!(out["diff"], true);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn an_unchanged_range_reports_nothing() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.git(&["commit", "-q", "--allow-empty", "-m", "empty"]);

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--format",
        "json",
    ]);
    assert!(out["rows"].as_array().unwrap().is_empty());
    assert_eq!(out["total"]["impl"]["code"], 0);
}

#[test]
fn a_git_tree_can_be_narrowed_with_an_include_glob() {
    let repo = Repo::init();
    repo.write("src/a.rs", "fn a() {}\n");
    repo.write("other/b.rs", "fn b() {}\nfn c() {}\n");
    repo.commit("first");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD",
        "-i",
        "src/",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn a_git_tree_honours_a_committed_slopcountignore() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.write("generated/b.rs", "fn b() {}\nfn c() {}\n");
    repo.write(".slopcountignore", "generated/\n");
    repo.commit("first");

    let path = repo.path().to_str().unwrap();
    assert_eq!(
        json(&["-C", path, "git:HEAD", "--format", "json"])["total"]["impl"]["code"],
        1
    );
    // ...unless the rules are switched off, which also reveals the dotfile.
    assert_eq!(
        json(&[
            "-C",
            path,
            "git:HEAD",
            "--format",
            "json",
            "--no-ignore",
            "--hidden",
        ])["total"]["impl"]["code"],
        3
    );
}

#[test]
fn a_git_tree_still_counts_files_that_gitignore_would_exclude() {
    // git tracks it despite `.gitignore`, so it was committed on purpose.
    let repo = Repo::init();
    repo.write(".gitignore", "tracked.rs\n");
    repo.write("tracked.rs", "fn a() {}\n");
    repo.git(&["add", "-Af"]);
    repo.git(&["commit", "-q", "-m", "first"]);

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn a_git_tree_hides_dotfiles_like_a_directory_walk_does() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.write(".config/b.rs", "fn b() {}\n");
    repo.commit("first");

    let path = repo.path().to_str().unwrap();
    assert_eq!(
        json(&["-C", path, "git:HEAD", "--format", "json"])["total"]["impl"]["code"],
        1
    );
    assert_eq!(
        json(&["-C", path, "git:HEAD", "--format", "json", "--hidden"])["total"]["impl"]["code"],
        2
    );
}

// -- the no-argument default -------------------------------------------------

#[test]
fn with_no_arguments_it_compares_the_working_tree_to_the_default_branch() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");

    // Work done since the default branch, committed and not.
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");
    repo.commit("second");
    repo.write("c.rs", "fn c() {}\n");

    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["diff"], true);
    assert_eq!(
        out["source"],
        format!("merge-base(origin/main, HEAD) → {}", repo.path().display())
    );
    // One line committed on top, plus one uncommitted.
    assert_eq!(out["total"]["impl"]["code"], 2);
}

#[test]
fn the_default_branch_comes_from_origin_head_when_it_exists() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    // A clone of a repo whose default branch is `trunk`, not `main`.
    repo.set_origin_head("trunk");

    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert!(
        out["source"]
            .as_str()
            .unwrap()
            .starts_with("merge-base(origin/trunk, HEAD) →"),
        "got {}",
        out["source"]
    );
}

#[test]
fn the_default_branch_falls_back_to_a_local_branch() {
    // No remote at all: `main` is all there is to compare against.
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");

    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["diff"], true);
    assert!(
        out["source"]
            .as_str()
            .unwrap()
            .starts_with("merge-base(main, HEAD) →"),
        "got {}",
        out["source"]
    );
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn on_the_default_branch_with_nothing_uncommitted_it_just_counts() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");

    // Nothing to compare against: the tree *is* the default branch.
    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["diff"], false);
    assert_eq!(out["source"], repo.path().display().to_string());
    assert_eq!(out["total"]["impl"]["code"], 2);
}

#[test]
fn an_untracked_file_is_work_in_progress() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");
    repo.write("new.rs", "fn new() {}\n");

    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["diff"], true);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn an_ignored_file_does_not_make_the_tree_busy() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.write(".gitignore", "built.rs\n");
    repo.commit("first");
    repo.set_origin_head("main");
    repo.write("built.rs", "fn built() {}\n");

    // Build output is not work in progress.
    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["diff"], false);
}

#[test]
fn a_staged_change_is_enough_to_compare() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");
    repo.git(&["add", "-A"]);

    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["diff"], true);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn what_landed_on_the_default_branch_meanwhile_is_not_counted() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");

    repo.git(&["checkout", "-q", "-b", "topic"]);
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");
    repo.commit("topic work");

    // Two more lines land on the default branch after the branch point.
    repo.git(&["checkout", "-q", "main"]);
    repo.write("a.rs", "fn a() {}\nfn y() {}\nfn z() {}\n");
    repo.commit("main moves on");
    repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    repo.git(&["checkout", "-q", "topic"]);

    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(
        out["source"],
        format!("merge-base(origin/main, HEAD) → {}", repo.path().display())
    );
    // The one line the topic branch added, not two deletions of main's work.
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn a_detached_head_is_on_no_branch_and_so_is_compared() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");
    repo.git(&["checkout", "-q", "--detach"]);

    let out = json(&["-C", repo.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["diff"], true);
    assert_eq!(out["total"]["impl"]["code"], 0);
}

#[test]
fn an_orphan_branch_falls_back_to_the_default_branch_itself() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");

    // No commit in common, so there is no branch point to compare from.
    repo.git(&["checkout", "-q", "--orphan", "pages"]);
    repo.git(&["rm", "-q", "-rf", "."]);
    repo.write("b.rs", "fn b() {}\nfn c() {}\n");
    repo.commit("unrelated history");

    let out = bin()
        .args(["-C", repo.path().to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no commit in common"),
        "unexplained fallback: {stderr}"
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        value["source"],
        format!("origin/main → {}", repo.path().display())
    );
}

#[test]
fn with_no_arguments_outside_a_repository_it_just_counts() {
    let dir = tree(&[("a.rs", "fn a() {}\n")]);
    let out = bin()
        .current_dir(dir.path())
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["diff"], false);
    assert_eq!(value["total"]["impl"]["code"], 1);
}

#[test]
fn with_no_arguments_it_covers_the_whole_repo_from_a_subdirectory() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.write("src/b.rs", "fn b() {}\n");
    repo.commit("first");
    repo.set_origin_head("main");
    repo.write("src/b.rs", "fn b() {}\nfn c() {}\n");
    repo.write("d.rs", "fn d() {}\n");

    // Run from `src`, but the comparison is the repository, not the subtree.
    let out = bin()
        .current_dir(repo.path().join("src"))
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["total"]["impl"]["code"], 2);
}

// -- two sources -------------------------------------------------------------

#[test]
fn two_directories_are_compared() {
    let before = tree(&[("src/lib.rs", "fn a() {}\nfn b() {}\n")]);
    let after = tree(&[
        ("src/lib.rs", "fn a() {}\n"),
        ("src/extra.rs", "fn c() {}\nfn d() {}\n"),
    ]);
    let out = json(&[
        before.path().to_str().unwrap(),
        after.path().to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(out["diff"], true);
    // Two lines gone from lib.rs, two new files' worth added.
    assert_eq!(out["total"]["impl"]["code"], 1);
    assert_eq!(out["total"]["files"], 1);
}

#[test]
fn a_git_revision_can_be_compared_against_a_directory() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD",
        ".",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn two_revisions_of_different_branches_are_compared() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.git(&["checkout", "-q", "-b", "topic"]);
    repo.write(
        "a.rs",
        "fn a() {}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn x() {}\n}\n",
    );
    repo.commit("tests");
    repo.git(&["checkout", "-q", "main"]);

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:main",
        "git:topic",
        "--format",
        "json",
    ]);
    assert_eq!(out["source"], "main → topic");
    // Everything the topic branch added was tests.
    assert_eq!(row(&out, "Rust")["test"]["code"], 5);
    assert_eq!(row(&out, "Rust")["impl"]["code"], 0);
}

// -- merge bases -------------------------------------------------------------

/// `main` and `topic` share one commit, then both move on: main by two lines,
/// topic by one.
fn diverged() -> Repo {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.git(&["checkout", "-q", "-b", "topic"]);
    repo.write("a.rs", "fn a() {}\nfn b() {}\n");
    repo.commit("topic work");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("a.rs", "fn a() {}\nfn c() {}\nfn d() {}\n");
    repo.commit("main moves on");
    repo
}

#[test]
fn a_merge_base_ignores_what_landed_on_the_other_branch() {
    let repo = diverged();

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git-merge:main:topic",
        "git:topic",
        "--format",
        "json",
    ]);
    // The one line the topic branch added; main's two are not in the diff.
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn a_merge_base_is_named_by_its_two_sides() {
    let repo = diverged();

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git-merge:main:topic",
        "git:topic",
        "--format",
        "json",
    ]);
    assert_eq!(out["source"], "merge-base(main, topic) → topic");
}

#[test]
fn a_merge_base_can_be_counted_on_its_own() {
    let repo = diverged();

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git-merge:main:topic",
        "--format",
        "json",
    ]);
    assert_eq!(out["source"], "merge-base(main, topic)");
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn an_unresolvable_side_of_a_merge_base_fails_before_counting() {
    let repo = diverged();
    let out = bin()
        .args(["-C", repo.path().to_str().unwrap(), "git-merge:main:nope"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nope"), "unhelpful error: {stderr}");
    assert!(
        stderr.contains("never fetches"),
        "unhelpful error: {stderr}"
    );
}

// -- ref notation ------------------------------------------------------------

#[test]
fn a_bare_word_is_treated_as_a_path_not_a_revision() {
    // A repository with a branch called `main` *and* a directory called
    // `main`, so the two readings give different answers.
    let repo = Repo::init();
    repo.write("outside.rs", "fn a() {}\nfn b() {}\n");
    repo.write("main/inside.rs", "fn c() {}\n");
    repo.commit("first");
    let path = repo.path().to_str().unwrap();

    // Bare: the directory, so only what is inside it.
    assert_eq!(
        json(&["-C", path, "main", "--format", "json"])["total"]["impl"]["code"],
        1
    );
    // Prefixed: the branch, so the whole tree.
    assert_eq!(
        json(&["-C", path, "git:main", "--format", "json"])["total"]["impl"]["code"],
        3
    );
}

#[test]
fn a_path_that_does_not_exist_fails_before_counting() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let out = bin().arg(missing.to_str().unwrap()).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope"));
}

#[test]
fn an_empty_git_revision_is_rejected() {
    let out = bin().args(["git:"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("git:origin/main"));
}

#[test]
fn a_merge_base_with_one_revision_is_rejected() {
    let out = bin().args(["git-merge:main"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("git-merge:origin/main:HEAD"));
}

#[test]
fn more_than_two_refs_is_rejected() {
    let out = bin().args(["a", "b", "c"]).output().unwrap();
    assert!(!out.status.success());
}

// -- --in --------------------------------------------------------------------

#[test]
fn in_narrows_a_single_source() {
    let dir = tree(&[
        ("src/a.rs", "fn a() {}\n"),
        ("other/b.rs", "fn b() {}\nfn c() {}\n"),
    ]);
    let path = dir.path().to_str().unwrap();

    assert_eq!(
        json(&[path, "--format", "json"])["total"]["impl"]["code"],
        3
    );
    assert_eq!(
        json(&[path, "--in", "src", "--format", "json"])["total"]["impl"]["code"],
        1
    );
}

#[test]
fn in_narrows_both_sides_of_a_directory_comparison() {
    let before = tree(&[
        ("src/lib.rs", "fn a() {}\n"),
        ("noise/x.rs", "fn x() {}\nfn y() {}\n"),
    ]);
    let after = tree(&[
        ("src/lib.rs", "fn a() {}\nfn b() {}\n"),
        // Changes outside `src` must not show up at all.
        ("noise/x.rs", "fn x() {}\nfn y() {}\nfn z() {}\nfn w() {}\n"),
    ]);
    let out = json(&[
        before.path().to_str().unwrap(),
        after.path().to_str().unwrap(),
        "--in",
        "src",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn in_narrows_both_sides_of_a_git_comparison() {
    let repo = Repo::init();
    repo.write("src/lib.rs", "fn a() {}\n");
    repo.write("docs/x.md", "hello\n");
    repo.commit("first");
    repo.write(
        "src/lib.rs",
        "fn a() {}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn x() {}\n}\n",
    );
    repo.write("docs/x.md", "hello\nthere\nagain\n");
    repo.commit("second");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--in",
        "src",
        "--format",
        "json",
    ]);
    assert_eq!(out["source"], "HEAD~1:src → HEAD:src");
    // The docs change is outside `--in`, so it is not counted.
    assert_eq!(row(&out, "Rust")["test"]["code"], 5);
    assert!(out["rows"].as_array().unwrap().len() == 1);
}

#[test]
fn in_rebases_paths_so_the_two_sides_line_up() {
    let repo = Repo::init();
    repo.write("src/lib.rs", "fn a() {}\n");
    repo.commit("first");
    repo.write("src/lib.rs", "fn a() {}\nfn b() {}\n");
    repo.commit("second");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--in",
        "src",
        "--files",
        "--format",
        "json",
    ]);
    // Relative to `--in`, not to the repository root.
    assert_eq!(out["rows"][0]["label"], "lib.rs");
    // The same file on both sides, so it is one net change, not an add plus a
    // delete -- which is what proves the rebasing lines the sides up.
    assert_eq!(out["rows"][0]["files"], 0);
    assert_eq!(out["rows"][0]["impl"]["code"], 1);
}

#[test]
fn a_subdirectory_present_on_only_one_side_counts_as_added() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.write("fresh/b.rs", "fn b() {}\nfn c() {}\n");
    repo.commit("second");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--in",
        "fresh",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], 2);
    assert_eq!(out["total"]["files"], 1);
    assert!(
        out["source"].as_str().unwrap().contains("(absent)"),
        "the missing side should say so: {}",
        out["source"]
    );
}

#[test]
fn a_subdirectory_removed_between_the_sides_counts_as_deleted() {
    let repo = Repo::init();
    repo.write("gone/b.rs", "fn b() {}\nfn c() {}\n");
    repo.commit("first");
    std::fs::remove_dir_all(repo.path().join("gone")).unwrap();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("second");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD~1",
        "git:HEAD",
        "--in",
        "gone",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], -2);
    assert_eq!(out["total"]["files"], -1);
}

#[test]
fn a_subdirectory_missing_from_both_sides_is_an_error() {
    // Almost always a typo, so it should not silently report zero.
    let repo = Repo::init();
    repo.write("src/a.rs", "fn a() {}\n");
    repo.commit("first");
    repo.write("src/a.rs", "fn a() {}\nfn b() {}\n");
    repo.commit("second");

    let out = bin()
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "git:HEAD~1",
            "git:HEAD",
            "--in",
            "scr",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("scr"), "unhelpful error: {stderr}");
}

#[test]
fn a_subdirectory_missing_from_a_single_source_is_an_error() {
    let dir = tree(&[("src/a.rs", "fn a() {}\n")]);
    let out = bin()
        .args([dir.path().to_str().unwrap(), "--in", "scr"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("scr"));
}

#[test]
fn in_applies_to_the_no_argument_default() {
    let repo = Repo::init();
    repo.write("src/a.rs", "fn a() {}\n");
    repo.write("docs/x.md", "hello\n");
    repo.commit("first");
    repo.set_origin_head("main");
    repo.write("src/a.rs", "fn a() {}\nfn b() {}\n");
    repo.write("docs/x.md", "hello\nthere\nmore\n");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "--in",
        "src",
        "--format",
        "json",
    ]);
    assert_eq!(out["diff"], true);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn in_can_name_a_nested_directory() {
    let repo = Repo::init();
    repo.write("a/b/c.rs", "fn c() {}\n");
    repo.write("a/other.rs", "fn o() {}\nfn p() {}\n");
    repo.commit("first");

    let out = json(&[
        "-C",
        repo.path().to_str().unwrap(),
        "git:HEAD",
        "--in",
        "a/b",
        "--format",
        "json",
    ]);
    assert_eq!(out["total"]["impl"]["code"], 1);
}

#[test]
fn in_tolerates_a_trailing_slash() {
    let dir = tree(&[("src/a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
    let path = dir.path().to_str().unwrap();
    for spelling in ["src", "src/", "./src"] {
        assert_eq!(
            json(&[path, "--in", spelling, "--format", "json"])["total"]["impl"]["code"],
            1,
            "{spelling:?}"
        );
    }
}

// -- errors and utilities ----------------------------------------------------

#[test]
fn an_unknown_revision_fails_with_a_message() {
    let repo = Repo::init();
    repo.write("a.rs", "fn a() {}\n");
    repo.commit("first");

    let out = bin()
        .args(["-C", repo.path().to_str().unwrap(), "git:nope"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nope"), "unhelpful stderr: {stderr}");
}

#[test]
fn an_unknown_language_fails_with_a_message() {
    let dir = tree(&[("a.rs", "fn a() {}\n")]);
    let out = bin()
        .args([dir.path().to_str().unwrap(), "-l", "nosuchlang"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nosuchlang"));
}

#[test]
fn a_missing_directory_fails() {
    let out = bin().arg("/definitely/not/a/real/path").output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn listing_languages_shows_which_ones_detect_tests() {
    let out = run(&["--list-languages"]);
    assert!(out.contains("LANGUAGE"));
    assert!(out.contains("Rust"));
    assert!(out.lines().count() > 300);
}

#[test]
fn an_empty_directory_reports_zero() {
    let dir = tempfile::tempdir().unwrap();
    let out = json(&[dir.path().to_str().unwrap(), "--format", "json"]);
    assert_eq!(out["total"]["impl"]["code"], 0);
    assert!(out["rows"].as_array().unwrap().is_empty());
}
