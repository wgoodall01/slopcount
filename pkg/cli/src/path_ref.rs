//! How a source is named on the command line.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// Something slopcount can count: a place on disk, or a commit in git.
///
/// The notation is a bare path, a `git:` prefix followed by anything git can
/// resolve to a commit, or two such revisions joined by `git-merge:`:
///
/// ```text
/// .                           the current directory
/// ../other/project            a directory
/// git:origin/main             a branch
/// git:fefefefe                a commit
/// git:HEAD~2, git:v1.0        anything `git rev-parse` accepts
/// git-merge:origin/main:HEAD  where those two last agreed
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRef {
    /// A directory on the filesystem.
    Fs(PathBuf),
    /// A commit, resolved against the local object database. slopcount never
    /// fetches, so the revision has to be one git already knows about.
    GitCommitish(String),
    /// The merge base of two commit-ishes: the commit they last had in common,
    /// which is what a topic branch actually grew from.
    GitMergeBase(String, String),
}

/// The prefix that marks a revision rather than a path.
const GIT_PREFIX: &str = "git:";

/// The prefix that marks a merge base of the two revisions after it.
const MERGE_PREFIX: &str = "git-merge:";

impl FromStr for PathRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // `git-merge:` is checked first; neither prefix is a prefix of the
        // other, so the order only matters for which error gets reported.
        if let Some(pair) = s.strip_prefix(MERGE_PREFIX) {
            // Git forbids `:` in a ref name, so the first one separates the
            // two sides however exotic the revisions around it are.
            return match pair.split_once(':') {
                Some((one, two)) if !one.is_empty() && !two.is_empty() => {
                    Ok(PathRef::GitMergeBase(one.to_string(), two.to_string()))
                }
                _ => Err(format!(
                    "`{MERGE_PREFIX}` needs two revisions separated by `:`, \
                     such as `{MERGE_PREFIX}origin/main:HEAD`"
                )),
            };
        }

        match s.strip_prefix(GIT_PREFIX) {
            Some("") => {
                Err("`git:` needs a revision after it, such as `git:origin/main`".to_string())
            }
            Some(revision) => Ok(PathRef::GitCommitish(revision.to_string())),
            None if s.is_empty() => Err("expected a path or a `git:` revision".to_string()),
            None => Ok(PathRef::Fs(PathBuf::from(s))),
        }
    }
}

impl fmt::Display for PathRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathRef::Fs(path) => write!(f, "{}", path.display()),
            PathRef::GitCommitish(revision) => write!(f, "{GIT_PREFIX}{revision}"),
            PathRef::GitMergeBase(one, two) => write!(f, "{MERGE_PREFIX}{one}:{two}"),
        }
    }
}

impl PathRef {
    /// A reference to the current directory.
    pub fn here() -> Self {
        PathRef::Fs(PathBuf::from("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<PathRef, String> {
        s.parse()
    }

    fn git(revision: &str) -> PathRef {
        PathRef::GitCommitish(revision.to_string())
    }

    fn merge(one: &str, two: &str) -> PathRef {
        PathRef::GitMergeBase(one.to_string(), two.to_string())
    }

    fn fs(path: &str) -> PathRef {
        PathRef::Fs(PathBuf::from(path))
    }

    #[test]
    fn a_bare_word_is_a_filesystem_path() {
        assert_eq!(parse("."), Ok(fs(".")));
        assert_eq!(parse("./*"), Ok(fs("./*")));
        assert_eq!(parse("src"), Ok(fs("src")));
        assert_eq!(parse("../other/project"), Ok(fs("../other/project")));
        assert_eq!(parse("/abs/path"), Ok(fs("/abs/path")));
    }

    #[test]
    fn a_git_prefix_is_a_revision() {
        assert_eq!(parse("git:fefefefe"), Ok(git("fefefefe")));
        assert_eq!(parse("git:origin/main"), Ok(git("origin/main")));
        assert_eq!(parse("git:HEAD~2"), Ok(git("HEAD~2")));
        assert_eq!(parse("git:v1.0"), Ok(git("v1.0")));
    }

    #[test]
    fn the_prefix_is_only_stripped_once() {
        // A branch really named `git:thing` would be written `git:git:thing`.
        assert_eq!(parse("git:git:thing"), Ok(git("git:thing")));
    }

    #[test]
    fn a_path_that_merely_contains_the_prefix_is_still_a_path() {
        assert_eq!(parse("a/git:b"), Ok(fs("a/git:b")));
        assert_eq!(parse("mygit:thing"), Ok(fs("mygit:thing")));
        assert_eq!(parse("a/git-merge:b:c"), Ok(fs("a/git-merge:b:c")));
    }

    #[test]
    fn a_git_merge_prefix_is_a_pair_of_revisions() {
        assert_eq!(
            parse("git-merge:origin/main:HEAD"),
            Ok(merge("origin/main", "HEAD"))
        );
        assert_eq!(parse("git-merge:main:topic"), Ok(merge("main", "topic")));
        assert_eq!(parse("git-merge:HEAD~2:v1.0"), Ok(merge("HEAD~2", "v1.0")));
    }

    #[test]
    fn a_merge_base_splits_on_its_first_colon() {
        // Git forbids `:` in ref names, so anything after the second one
        // belongs to the right-hand revision.
        assert_eq!(parse("git-merge:a:b:c"), Ok(merge("a", "b:c")));
    }

    #[test]
    fn a_merge_base_needs_both_sides() {
        for text in [
            "git-merge:",
            "git-merge:main",
            "git-merge::main",
            "git-merge:main:",
        ] {
            let err = parse(text).unwrap_err();
            assert!(
                err.contains("git-merge:origin/main:HEAD"),
                "unhelpful error for {text:?}: {err}"
            );
        }
    }

    #[test]
    fn an_empty_revision_is_rejected() {
        let err = parse("git:").unwrap_err();
        assert!(err.contains("git:origin/main"), "unhelpful error: {err}");
    }

    #[test]
    fn an_empty_argument_is_rejected() {
        assert!(parse("").is_err());
    }

    #[test]
    fn display_round_trips_through_parsing() {
        for text in [
            ".",
            "src/deep",
            "git:origin/main",
            "git:HEAD~2",
            "git-merge:origin/main:HEAD",
        ] {
            let parsed = parse(text).unwrap();
            assert_eq!(parsed.to_string(), text);
            assert_eq!(parse(&parsed.to_string()), Ok(parsed));
        }
    }

    #[test]
    fn here_is_the_current_directory() {
        assert_eq!(PathRef::here(), fs("."));
    }
}
