//! Locating the repository, and deciding what to compare against by default.
//!
//! This is defaulting policy rather than counting, so it lives with the CLI:
//! the library has no opinion about which branch is "the" branch. Every lookup
//! reads refs already in the local object database — slopcount never fetches.

use std::path::{Path, PathBuf};

/// The symbolic ref `git clone` writes to record the remote's default branch.
const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";

/// Prefix stripped to turn a full ref name into a short one.
const REMOTES_PREFIX: &str = "refs/remotes/";

/// Conventional default branches, tried in order when `origin/HEAD` is absent.
/// A remote branch beats a local one: it is the shared history to compare with.
const FALLBACKS: [&str; 4] = ["origin/main", "origin/master", "main", "master"];

/// The working-tree root of the repository containing `path`.
///
/// `None` when `path` is not in a repository, or the repository is bare and so
/// has no working tree to compare against.
pub fn root(path: impl AsRef<Path>) -> Option<PathBuf> {
    let repo = gix::discover(path.as_ref()).ok()?;
    repo.workdir().map(Path::to_path_buf)
}

/// The default branch of the repository containing `path`, as a short name
/// such as `origin/main`.
pub fn default_branch(path: impl AsRef<Path>) -> Option<String> {
    let repo = gix::discover(path.as_ref()).ok()?;

    // `origin/HEAD` is authoritative when the clone has it: it names whatever
    // the remote's default branch was at clone time, whatever it is called.
    if let Ok(reference) = repo.find_reference(ORIGIN_HEAD) {
        if let gix::refs::TargetRef::Symbolic(name) = reference.target() {
            let full = name.as_bstr().to_string();
            if let Some(short) = full.strip_prefix(REMOTES_PREFIX) {
                return Some(short.to_string());
            }
        }
    }

    FALLBACKS
        .into_iter()
        .find(|candidate| repo.rev_parse_single(*candidate).is_ok())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    use tempfile::TempDir;

    struct Repo(TempDir);

    impl Repo {
        fn init() -> Self {
            let repo = Repo(tempfile::tempdir().unwrap());
            repo.git(&["init", "-q", "-b", "main"]);
            repo.git(&["config", "user.email", "t@example.com"]);
            repo.git(&["config", "user.name", "T"]);
            std::fs::write(repo.path().join("a.rs"), "fn a() {}\n").unwrap();
            repo.git(&["add", "-A"]);
            repo.git(&["commit", "-q", "-m", "first"]);
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

        /// Fabricate the remote-tracking refs a clone would have, with no
        /// remote to fetch from: everything must come from the local database.
        fn set_origin_head(&self, branch: &str) {
            let head = format!("refs/remotes/origin/{branch}");
            self.git(&["update-ref", &head, "HEAD"]);
            self.git(&["symbolic-ref", ORIGIN_HEAD, &head]);
        }
    }

    /// `TempDir` hands back a symlinked path on macOS (`/var` → `/private/var`),
    /// which git resolves; compare canonical paths.
    fn same_path(a: &Path, b: &Path) -> bool {
        a.canonicalize().ok() == b.canonicalize().ok()
    }

    // -- root ----------------------------------------------------------------

    #[test]
    fn root_finds_the_working_tree() {
        let repo = Repo::init();
        let found = root(repo.path()).expect("a repository");
        assert!(same_path(&found, repo.path()), "got {}", found.display());
    }

    #[test]
    fn root_is_found_from_a_subdirectory() {
        let repo = Repo::init();
        let deep = repo.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        let found = root(&deep).expect("a repository");
        assert!(same_path(&found, repo.path()), "got {}", found.display());
    }

    #[test]
    fn root_is_none_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(root(dir.path()), None);
    }

    #[test]
    fn root_is_none_for_a_bare_repository() {
        let dir = tempfile::tempdir().unwrap();
        let out = Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        // A bare repo has no working tree to compare against.
        assert_eq!(root(dir.path()), None);
    }

    // -- default_branch ------------------------------------------------------

    #[test]
    fn origin_head_is_authoritative() {
        let repo = Repo::init();
        repo.set_origin_head("main");
        assert_eq!(default_branch(repo.path()).as_deref(), Some("origin/main"));
    }

    #[test]
    fn origin_head_is_honoured_whatever_the_branch_is_called() {
        let repo = Repo::init();
        repo.set_origin_head("trunk");
        assert_eq!(default_branch(repo.path()).as_deref(), Some("origin/trunk"));
    }

    #[test]
    fn origin_head_beats_the_conventional_names() {
        let repo = Repo::init();
        // Both `origin/main` and `origin/trunk` exist; `origin/HEAD` decides.
        repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        repo.set_origin_head("trunk");
        assert_eq!(default_branch(repo.path()).as_deref(), Some("origin/trunk"));
    }

    #[test]
    fn a_remote_branch_is_preferred_over_a_local_one() {
        let repo = Repo::init();
        repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(default_branch(repo.path()).as_deref(), Some("origin/main"));
    }

    #[test]
    fn a_local_branch_is_the_last_resort() {
        // No remote at all, as in a repository that was never cloned.
        let repo = Repo::init();
        assert_eq!(default_branch(repo.path()).as_deref(), Some("main"));
    }

    #[test]
    fn master_is_recognised_as_well_as_main() {
        let repo = Repo::init();
        repo.git(&["branch", "-m", "main", "master"]);
        assert_eq!(default_branch(repo.path()).as_deref(), Some("master"));
    }

    #[test]
    fn it_is_found_from_a_subdirectory() {
        let repo = Repo::init();
        repo.set_origin_head("main");
        let deep = repo.path().join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(default_branch(&deep).as_deref(), Some("origin/main"));
    }

    #[test]
    fn there_is_no_default_branch_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(default_branch(dir.path()), None);
    }

    #[test]
    fn there_is_no_default_branch_without_a_conventional_name() {
        let repo = Repo::init();
        repo.git(&["branch", "-m", "main", "some-other-name"]);
        assert_eq!(default_branch(repo.path()), None);
    }
}
