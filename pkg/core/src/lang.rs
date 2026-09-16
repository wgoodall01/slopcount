//! Language definitions.
//!
//! The data comes from three embedded JSON files: tokei's `languages.json` (the
//! upstream syntax database); our `languages_extra.json`, which is
//! deep-object-merged on top of it and carries the extra dimensions slopcount
//! adds — chiefly, how to recognise test code; and `families.json`, which
//! groups related languages so they can be reported together.
//!
//! Unlike tokei we interpret the database at runtime instead of generating code
//! from it. Counting is disk-IO-bound in practice, and the registry is built
//! exactly once per process.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use aho_corasick::{AhoCorasick, AhoCorasickKind, MatchKind, StartKind};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{Map, Value};

const LANGUAGES_JSON: &str = include_str!("../data/languages.json");
const LANGUAGES_EXTRA_JSON: &str = include_str!("../data/languages_extra.json");
const FAMILIES_JSON: &str = include_str!("../data/families.json");

/// The family a language falls into when `families.json` does not claim it.
pub const OTHER_FAMILY: &str = "Other";

/// An index into [`Registry::languages`]. Cheap to copy, compare and hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageId(pub u16);

/// An index into [`Registry::families`]. Cheap to copy, compare and hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FamilyId(pub u16);

/// A group of related languages, counted together in reports.
#[derive(Debug)]
pub struct FamilyDef {
    pub id: FamilyId,
    /// The name as it appears in the report, e.g. `JavaScript`.
    pub name: String,
    /// The languages that belong to the family.
    pub languages: Vec<LanguageId>,
}

/// How a test block opened by a marker is delimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockStyle {
    /// The block runs until brace depth returns to zero after the first `{`.
    #[default]
    Braces,
    /// The block runs while lines are indented deeper than the marker line.
    Indent,
}

/// Everything slopcount knows about one language.
#[derive(Debug)]
pub struct LanguageDef {
    pub id: LanguageId,
    /// The key in `languages.json`, e.g. `Cpp`.
    pub key: String,
    /// The human-facing name, e.g. `C++`.
    pub name: String,
    /// The family this language was grouped into, if any.
    pub family: Option<FamilyId>,

    pub line_comments: Vec<String>,
    pub multi_line_comments: Vec<(String, String)>,
    pub nested_comments: Vec<(String, String)>,
    /// `multi_line_comments` followed by `nested_comments`.
    pub any_multi_line_comments: Vec<(String, String)>,
    pub quotes: Vec<(String, String)>,
    pub verbatim_quotes: Vec<(String, String)>,
    pub doc_quotes: Vec<(String, String)>,

    pub nested: bool,
    pub literate: bool,
    pub is_fortran: bool,

    /// Substrings whose absence from a line means the line can be classified
    /// without running the character-level state machine over it.
    pub important_syntax: AhoCorasick,

    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub path_suffixes: Vec<String>,

    /// Paths matching these globs are counted entirely as test code.
    pub test_paths: GlobSet,
    /// Line prefixes that open a region of test code.
    pub test_blocks: Vec<String>,
    pub test_block_style: BlockStyle,
}

impl LanguageDef {
    /// Whether this language has any way at all of marking test code.
    pub fn detects_tests(&self) -> bool {
        !self.test_blocks.is_empty() || !self.test_paths.is_empty()
    }
}

/// The set of all known languages, plus the lookup tables used to map a path to
/// one of them.
#[derive(Debug)]
pub struct Registry {
    languages: Vec<LanguageDef>,
    families: Vec<FamilyDef>,
    by_extension: HashMap<String, LanguageId>,
    by_filename: HashMap<String, LanguageId>,
    by_key: HashMap<String, LanguageId>,
    /// `(suffix, id)`, longest suffix first so the most specific wins.
    path_suffixes: Vec<(String, LanguageId)>,
    /// Path globs that mark a file as test code regardless of language.
    global_test_paths: GlobSet,
}

/// The process-wide registry, built on first use.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Registry::from_json(LANGUAGES_JSON, LANGUAGES_EXTRA_JSON, FAMILIES_JSON)
            .expect("embedded language definitions are valid")
    })
}

impl Registry {
    pub fn languages(&self) -> &[LanguageDef] {
        &self.languages
    }

    pub fn get(&self, id: LanguageId) -> &LanguageDef {
        &self.languages[id.0 as usize]
    }

    pub fn families(&self) -> &[FamilyDef] {
        &self.families
    }

    pub fn family(&self, id: FamilyId) -> &FamilyDef {
        &self.families[id.0 as usize]
    }

    /// The family `id` belongs to, or `None` when no family claims it.
    pub fn family_of(&self, id: LanguageId) -> Option<&FamilyDef> {
        self.get(id).family.map(|f| self.family(f))
    }

    /// The name of the family `id` belongs to, falling back to
    /// [`OTHER_FAMILY`]. `None` — a file of unknown language — lands there too.
    ///
    /// Takes `&'static self` because the only registry anyone has is the
    /// process-wide one, and the report wants a `&'static str` back.
    pub fn family_name(&'static self, id: Option<LanguageId>) -> &'static str {
        id.and_then(|id| self.family_of(id))
            .map(|f| f.name.as_str())
            .unwrap_or(OTHER_FAMILY)
    }

    /// Resolve a family by name, case-insensitively.
    pub fn resolve_family(&self, needle: &str) -> Option<FamilyId> {
        let needle = needle.to_ascii_lowercase();
        self.families
            .iter()
            .find(|f| f.name.to_ascii_lowercase() == needle)
            .map(|f| f.id)
    }

    pub fn by_key(&self, key: &str) -> Option<LanguageId> {
        self.by_key.get(key).copied()
    }

    /// Resolve a language by name, key or extension, case-insensitively. Used
    /// for CLI filters like `--language rust`.
    pub fn resolve(&self, needle: &str) -> Option<LanguageId> {
        let needle = needle.to_ascii_lowercase();
        self.languages
            .iter()
            .find(|l| l.key.to_ascii_lowercase() == needle || l.name.to_ascii_lowercase() == needle)
            .map(|l| l.id)
            .or_else(|| self.by_extension.get(needle.as_str()).copied())
    }

    /// Determine the language of a path from its filename, path suffix or
    /// extension. Does not read the file, so shebangs are not consulted.
    pub fn from_path(&self, path: impl AsRef<Path>) -> Option<LanguageId> {
        let path = path.as_ref();
        // tokei stores filenames and path suffixes lowercased, and matches
        // against a lowercased filename; do the same so `Makefile` resolves.
        let filename = path.file_name()?.to_str()?.to_ascii_lowercase();
        let filename = filename.as_str();

        if let Some(id) = self.by_filename.get(filename) {
            return Some(*id);
        }
        for (suffix, id) in &self.path_suffixes {
            if filename.ends_with(suffix.as_str()) {
                return Some(*id);
            }
        }
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.by_extension.get(ext.as_str()).copied()
    }

    /// Determine a language from the first line of a file, e.g.
    /// `#!/usr/bin/env python3`. Only consulted when the path is inconclusive.
    pub fn from_shebang(&self, first_line: &str) -> Option<LanguageId> {
        let mut words = first_line.split_whitespace();
        let first = words.next()?;
        if !first.starts_with("#!") {
            return None;
        }
        // `#!/usr/bin/env python3` — the interpreter is the second word.
        let interpreter = if first.ends_with("/env") {
            words.next()?
        } else {
            first.rsplit('/').next()?
        };
        let stem: String = interpreter
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        self.languages
            .iter()
            .find(|l| {
                l.key.eq_ignore_ascii_case(&stem)
                    || l.extensions.iter().any(|e| e.eq_ignore_ascii_case(&stem))
            })
            .map(|l| l.id)
    }

    /// Whether the whole file at `path` should be counted as test code.
    pub fn path_is_test(&self, path: impl AsRef<Path>, language: Option<LanguageId>) -> bool {
        let path = path.as_ref();
        if self.global_test_paths.is_match(path) {
            return true;
        }
        language.is_some_and(|id| self.get(id).test_paths.is_match(path))
    }

    /// Build a registry by deep-merging `extra` over `base` and interpreting the
    /// result. Exposed for tests; production code uses [`registry`].
    pub fn from_json(base: &str, extra: &str, families: &str) -> anyhow::Result<Self> {
        let base: Value = serde_json::from_str(base)?;
        let extra: Value = serde_json::from_str(extra)?;
        let families: Value = serde_json::from_str(families)?;

        let mut merged = base
            .get("languages")
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        if let Some(overlay) = extra.get("languages") {
            deep_merge(&mut merged, overlay);
        }
        let merged = merged
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("`languages` is not an object"))?;

        let global_test_paths = build_globset(extra.pointer("/global/test_paths"))?;

        let mut languages = Vec::with_capacity(merged.len());
        for (index, (key, value)) in merged.iter().enumerate() {
            let id = LanguageId(u16::try_from(index)?);
            languages.push(LanguageDef::from_json(id, key, value)?);
        }

        let mut by_extension = HashMap::new();
        let mut by_filename = HashMap::new();
        let mut by_key = HashMap::new();
        let mut path_suffixes = Vec::new();
        for lang in &languages {
            by_key.insert(lang.key.clone(), lang.id);
            for ext in &lang.extensions {
                // First definition wins, matching tokei's match-arm ordering.
                by_extension.entry(ext.clone()).or_insert(lang.id);
            }
            for name in &lang.filenames {
                by_filename.entry(name.clone()).or_insert(lang.id);
            }
            for suffix in &lang.path_suffixes {
                path_suffixes.push((suffix.clone(), lang.id));
            }
        }
        // Longest suffix first, so `.blade.php` beats `.php`.
        path_suffixes.sort_by_key(|(suffix, _)| std::cmp::Reverse(suffix.len()));

        let families = build_families(&families, &by_key, &mut languages)?;

        Ok(Self {
            languages,
            families,
            by_extension,
            by_filename,
            by_key,
            path_suffixes,
            global_test_paths,
        })
    }
}

impl LanguageDef {
    fn from_json(id: LanguageId, key: &str, v: &Value) -> anyhow::Result<Self> {
        // Delimiters are matched by trying each in turn, so the longest opener
        // has to be tried first: in a language with both `\"` and `\"\"\"`, a
        // triple quote must not be read as an empty string followed by a quote.
        let multi_line_comments = longest_first(pairs(v.get("multi_line_comments")));
        let nested_comments = longest_first(pairs(v.get("nested_comments")));
        let quotes = longest_first(pairs(v.get("quotes")));
        let doc_quotes = longest_first(pairs(v.get("doc_quotes")));
        let verbatim_quotes = longest_first(pairs(v.get("verbatim_quotes")));

        // Mirrors tokei: anything that could start a string, a doc string or a
        // comment, plus whatever the definition calls out explicitly.
        let mut important: Vec<String> = quotes
            .iter()
            .chain(&doc_quotes)
            .chain(&multi_line_comments)
            .chain(&nested_comments)
            .map(|(s, _)| s.clone())
            .collect();
        important.extend(strings(v.get("important_syntax")));
        important.sort();
        important.dedup();

        let any_multi_line_comments: Vec<(String, String)> = multi_line_comments
            .iter()
            .chain(&nested_comments)
            .cloned()
            .collect();

        let name = v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(key)
            .to_string();

        Ok(Self {
            id,
            key: key.to_string(),
            name,
            // Filled in by `Registry::from_json`, which owns the families.
            family: None,
            line_comments: {
                let mut c = strings(v.get("line_comment"));
                c.sort_by_key(|c| std::cmp::Reverse(c.len()));
                c
            },
            multi_line_comments,
            nested_comments,
            any_multi_line_comments,
            quotes,
            verbatim_quotes,
            doc_quotes,
            nested: v.get("nested").and_then(Value::as_bool).unwrap_or(false),
            literate: v.get("literate").and_then(Value::as_bool).unwrap_or(false),
            is_fortran: key == "FortranModern" || key == "FortranLegacy",
            important_syntax: build_corasick(&important),
            extensions: strings(v.get("extensions")),
            filenames: strings(v.get("filenames")),
            path_suffixes: strings(v.get("path_suffixes")),
            test_paths: build_globset(v.get("test_paths"))?,
            test_blocks: strings(v.get("test_blocks")),
            test_block_style: match v.get("test_block_style").and_then(Value::as_str) {
                Some("indent") => BlockStyle::Indent,
                _ => BlockStyle::Braces,
            },
        })
    }
}

/// Interpret `families.json` and stamp each language with the family that
/// claims it. A language may belong to at most one family, and every key named
/// has to exist, so a typo in the data file fails loudly instead of silently
/// dropping a language out of its group.
fn build_families(
    v: &Value,
    by_key: &HashMap<String, LanguageId>,
    languages: &mut [LanguageDef],
) -> anyhow::Result<Vec<FamilyDef>> {
    let Some(families) = v.get("families").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(families.len());
    for (index, (name, def)) in families.iter().enumerate() {
        let id = FamilyId(u16::try_from(index)?);
        let mut members = Vec::new();
        for key in strings(def.get("languages")) {
            let language = *by_key
                .get(key.as_str())
                .ok_or_else(|| anyhow::anyhow!("family {name:?} names unknown language {key:?}"))?;
            let slot = &mut languages[language.0 as usize].family;
            if let Some(existing) = *slot {
                anyhow::bail!(
                    "language {key:?} is in two families: {:?} and {name:?}",
                    families.keys().nth(existing.0 as usize).map(String::as_str),
                );
            }
            *slot = Some(id);
            members.push(language);
        }
        out.push(FamilyDef {
            id,
            name: name.clone(),
            languages: members,
        });
    }
    Ok(out)
}

/// Recursively merge `overlay` into `base`. Objects merge key-by-key; every
/// other value (including arrays) is replaced wholesale.
fn deep_merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (k, v) in overlay {
                match base.get_mut(k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        base.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

/// tokei's JSON is consumed by a code generator that pastes these values into
/// Rust string literals, so the file stores them pre-escaped (`\"` for a quote
/// character). Undo that: a backslash escapes whatever follows it.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(unescape)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Sort delimiter pairs so the longest opener is tried first. The sort is
/// stable, so delimiters of equal length keep their order from the database.
fn longest_first(mut pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs.sort_by_key(|(start, _)| std::cmp::Reverse(start.len()));
    pairs
}

fn pairs(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some((
                        unescape(p.first()?.as_str()?),
                        unescape(p.get(1)?.as_str()?),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn build_globset(v: Option<&Value>) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in strings(v) {
        builder.add(Glob::new(&pattern)?);
    }
    Ok(builder.build()?)
}

fn build_corasick(patterns: &[String]) -> AhoCorasick {
    AhoCorasick::builder()
        .match_kind(MatchKind::LeftmostLongest)
        .start_kind(StartKind::Unanchored)
        .prefilter(true)
        .kind(Some(AhoCorasickKind::DFA))
        .build(patterns)
        .expect("language patterns build a valid automaton")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_definitions_build() {
        let reg = registry();
        assert!(reg.languages().len() > 300, "got {}", reg.languages().len());
    }

    #[test]
    fn languages_are_stamped_with_their_family() {
        let reg = registry();
        let tsx = reg.by_key("Tsx").unwrap();
        assert_eq!(
            reg.family_of(tsx).map(|f| f.name.as_str()),
            Some("JavaScript")
        );
        assert_eq!(reg.family_name(Some(tsx)), "JavaScript");
        assert_eq!(reg.family_name(None), OTHER_FAMILY);
        assert_eq!(
            reg.resolve_family("javascript"),
            Some(reg.get(tsx).family.unwrap())
        );
    }

    #[test]
    fn a_family_lists_its_members() {
        let reg = registry();
        let family = reg.family(reg.resolve_family("Systems").unwrap());
        assert!(family.languages.contains(&reg.by_key("Rust").unwrap()));
        assert!(family
            .languages
            .iter()
            .all(|id| reg.family_of(*id).is_some_and(|f| f.id == family.id)));
    }

    #[test]
    fn a_family_naming_an_unknown_language_is_an_error() {
        let families = r#"{"families": {"Nope": {"languages": ["NotALanguage"]}}}"#;
        let err = Registry::from_json(LANGUAGES_JSON, LANGUAGES_EXTRA_JSON, families)
            .expect_err("unknown language should fail");
        assert!(err.to_string().contains("NotALanguage"), "{err}");
    }

    #[test]
    fn a_language_cannot_be_in_two_families() {
        let families =
            r#"{"families": {"A": {"languages": ["Rust"]}, "B": {"languages": ["Rust"]}}}"#;
        let err = Registry::from_json(LANGUAGES_JSON, LANGUAGES_EXTRA_JSON, families)
            .expect_err("a duplicate should fail");
        assert!(err.to_string().contains("two families"), "{err}");
    }

    #[test]
    fn escaped_quote_characters_are_unescaped() {
        // tokei stores these pre-escaped for its code generator.
        assert_eq!(unescape(r#"\""#), "\"");
        assert_eq!(unescape(r"\<"), "<");
        assert_eq!(unescape("plain"), "plain");
        // A trailing lone backslash is dropped rather than panicking.
        assert_eq!(unescape(r"a\"), "a");
    }

    #[test]
    fn quote_delimiters_survive_unescaping() {
        let rust = registry().get(registry().by_key("Rust").unwrap());
        assert!(rust.quotes.contains(&("\"".to_string(), "\"".to_string())));
        assert!(rust
            .verbatim_quotes
            .contains(&("r#\"".to_string(), "\"#".to_string())));
    }

    #[test]
    fn comment_syntax_is_loaded() {
        let reg = registry();
        let rust = reg.get(reg.by_key("Rust").unwrap());
        assert_eq!(rust.line_comments, vec!["//"]);
        assert_eq!(
            rust.multi_line_comments,
            vec![("/*".to_string(), "*/".to_string())]
        );
        assert!(rust.nested);

        let python = reg.get(reg.by_key("Python").unwrap());
        assert_eq!(python.line_comments, vec!["#"]);
        assert!(!python.nested);
        assert_eq!(python.doc_quotes.len(), 2);
    }

    #[test]
    fn display_names_override_keys() {
        let reg = registry();
        assert_eq!(reg.get(reg.by_key("Cpp").unwrap()).name, "C++");
        assert_eq!(reg.get(reg.by_key("Rust").unwrap()).name, "Rust");
    }

    // -- path resolution -----------------------------------------------------

    #[test]
    fn resolves_languages_by_extension() {
        let reg = registry();
        let expect = |path: &str, key: &str| {
            assert_eq!(
                reg.from_path(path),
                reg.by_key(key),
                "{path} should be {key}"
            );
        };
        expect("main.rs", "Rust");
        expect("src/deep/main.rs", "Rust");
        expect("a.py", "Python");
        expect("a.go", "Go");
        expect("a.tsx", "Tsx");
        expect("a.md", "Markdown");
    }

    #[test]
    fn extensions_are_matched_case_insensitively() {
        let reg = registry();
        assert_eq!(reg.from_path("MAIN.RS"), reg.by_key("Rust"));
    }

    #[test]
    fn resolves_languages_by_filename() {
        let reg = registry();
        assert_eq!(reg.from_path("Makefile"), reg.by_key("Makefile"));
        assert_eq!(reg.from_path("a/b/Dockerfile"), reg.by_key("Dockerfile"));
    }

    #[test]
    fn unknown_extensions_resolve_to_nothing() {
        let reg = registry();
        assert_eq!(reg.from_path("a.zzzznope"), None);
        assert_eq!(reg.from_path("noextension"), None);
    }

    #[test]
    fn resolves_languages_from_shebangs() {
        let reg = registry();
        assert_eq!(
            reg.from_shebang("#!/usr/bin/env python3"),
            reg.by_key("Python")
        );
        assert_eq!(reg.from_shebang("#!/bin/bash"), reg.by_key("Bash"));
        assert_eq!(reg.from_shebang("#!/usr/bin/env ruby"), reg.by_key("Ruby"));
        // Not a shebang at all.
        assert_eq!(reg.from_shebang("import os"), None);
        assert_eq!(reg.from_shebang(""), None);
    }

    #[test]
    fn resolve_accepts_names_keys_and_extensions() {
        let reg = registry();
        let rust = reg.by_key("Rust");
        assert_eq!(reg.resolve("Rust"), rust);
        assert_eq!(reg.resolve("rust"), rust);
        assert_eq!(reg.resolve("rs"), rust);
        assert_eq!(reg.resolve("c++"), reg.by_key("Cpp"));
        assert_eq!(reg.resolve("not-a-language"), None);
    }

    // -- the extras overlay --------------------------------------------------

    #[test]
    fn extras_are_merged_onto_the_base_definitions() {
        let reg = registry();
        let rust = reg.get(reg.by_key("Rust").unwrap());
        // From languages_extra.json...
        assert!(rust.test_blocks.iter().any(|m| m == "#[cfg(test)]"));
        // ...without losing anything from languages.json.
        assert_eq!(rust.line_comments, vec!["//"]);
        assert_eq!(rust.extensions, vec!["rs"]);
    }

    #[test]
    fn deep_merge_combines_objects_and_replaces_arrays() {
        let mut base = serde_json::json!({
            "keep": 1,
            "nested": {"a": 1, "b": 2},
            "list": [1, 2, 3],
        });
        deep_merge(
            &mut base,
            &serde_json::json!({
                "nested": {"b": 20, "c": 30},
                "list": [9],
                "added": true,
            }),
        );
        assert_eq!(
            base,
            serde_json::json!({
                "keep": 1,
                "nested": {"a": 1, "b": 20, "c": 30},
                "list": [9],
                "added": true,
            })
        );
    }

    #[test]
    fn test_path_globs_come_from_both_the_global_and_language_scopes() {
        let reg = registry();
        let go = reg.by_key("Go");
        // Language-scoped.
        assert!(reg.path_is_test("pkg/thing/widget_test.go", go));
        // Global, so it applies whatever the language.
        assert!(reg.path_is_test("src/tests/helpers.rs", reg.by_key("Rust")));
        assert!(reg.path_is_test("app/__tests__/a.js", reg.by_key("JavaScript")));
        // Neither.
        assert!(!reg.path_is_test("pkg/thing/widget.go", go));
        assert!(!reg.path_is_test("src/main.rs", reg.by_key("Rust")));
    }

    #[test]
    fn a_language_test_glob_does_not_apply_to_other_languages() {
        let reg = registry();
        // `*_test.go` is Go's rule; a Python file of that shape is not a test.
        assert!(!reg.path_is_test("a/b_test.go", reg.by_key("Python")));
    }

    #[test]
    fn python_test_paths_match_both_naming_conventions() {
        let reg = registry();
        let py = reg.by_key("Python");
        assert!(reg.path_is_test("pkg/test_thing.py", py));
        assert!(reg.path_is_test("pkg/thing_test.py", py));
        assert!(!reg.path_is_test("pkg/thing.py", py));
    }

    #[test]
    fn languages_with_test_support_are_the_ones_we_configured() {
        let reg = registry();
        for key in ["Rust", "Go", "Python", "TypeScript", "JavaScript", "Java"] {
            let lang = reg.get(reg.by_key(key).unwrap());
            assert!(lang.detects_tests(), "{key} should detect tests");
        }
    }

    #[test]
    fn block_styles_are_read_from_the_overlay() {
        let reg = registry();
        assert_eq!(
            reg.get(reg.by_key("Python").unwrap()).test_block_style,
            BlockStyle::Indent
        );
        assert_eq!(
            reg.get(reg.by_key("Rust").unwrap()).test_block_style,
            BlockStyle::Braces
        );
    }

    #[test]
    fn important_syntax_covers_every_delimiter_that_can_open_a_mode() {
        let reg = registry();
        let rust = reg.get(reg.by_key("Rust").unwrap());
        for needle in ["/*", "\"", "r#\""] {
            assert!(
                rust.important_syntax.is_match(needle),
                "{needle:?} should be important syntax"
            );
        }
    }
}
