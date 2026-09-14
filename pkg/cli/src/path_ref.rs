//! How a source is named on the command line.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// Something slopcount can count: a place on disk, or a commit in git.
///
/// The notation is a bare path, or a `git:` prefix followed by anything git
/// can resolve to a commit:
///
/// ```text
/// .                       the current directory
/// ../other/project        a directory
/// git:origin/main         a branch
/// git:fefefefe            a commit
/// git:HEAD~2, git:v1.0    anything `git rev-parse` accepts
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRef {
    /// A directory on the filesystem.
    Fs(PathBuf),
    /// A commit, resolved against the local object database. slopcount never
    /// fetches, so the revision has to be one git already knows about.
    GitCommit(String),
}

/// The prefix that marks a revision rather than a path.
const GIT_PREFIX: &str = "git:";

impl FromStr for PathRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_prefix(GIT_PREFIX) {
            Some("") => {
                Err("`git:` needs a revision after it, such as `git:origin/main`".to_string())
            }
            Some(revision) => Ok(PathRef::GitCommit(revision.to_string())),
            None if s.is_empty() => Err("expected a path or a `git:` revision".to_string()),
            None => Ok(PathRef::Fs(PathBuf::from(s))),
        }
    }
}

impl fmt::Display for PathRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathRef::Fs(path) => write!(f, "{}", path.display()),
            PathRef::GitCommit(revision) => write!(f, "{GIT_PREFIX}{revision}"),
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
        PathRef::GitCommit(revision.to_string())
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
        for text in [".", "src/deep", "git:origin/main", "git:HEAD~2"] {
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
