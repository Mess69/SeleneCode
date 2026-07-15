//! Worktree/index mismatch detection — warn when the index is for a *different* checkout.
//!
//! Git worktrees share one `.git` but check out different branches into different directories. If
//! you index worktree A and then query from worktree B, the graph describes code that is not on
//! disk here. This detects that so `status` can warn and the read tools can prefix a notice — a
//! stale answer the agent can't see is worse than no answer.
//!
//! It is deliberately conservative: **any** doubt resolves to "no mismatch" (`None`). A false warning
//! trains the user to ignore warnings.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A detected mismatch: the index lives at `index_root`, but the caller is in `worktree_root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub worktree_root: PathBuf,
    pub index_root: PathBuf,
}

impl Mismatch {
    /// The multi-line warning `status` prints.
    pub fn status_warning(&self) -> String {
        format!(
            "⚠ worktree/index mismatch\n  \
             you are in the git worktree {}\n  \
             but the index was built for      {}\n  \
             Results describe the indexed checkout, not this one. Re-run `selene index` here to fix.",
            self.worktree_root.display(),
            self.index_root.display()
        )
    }

    /// The one-line notice a read tool prefixes to its answer.
    pub fn notice(&self) -> String {
        format!(
            "⚠ This index was built for {} — a different git worktree than the current one ({}). \
             Results may be stale; re-index here to be sure.",
            self.index_root.display(),
            self.worktree_root.display()
        )
    }
}

fn git(path: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(path).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn worktree_root(path: &Path) -> Option<PathBuf> {
    git(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn common_dir(path: &Path) -> Option<PathBuf> {
    let raw = git(path, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(&raw);
    let abs = if p.is_absolute() { p } else { path.join(p) };
    // Canonicalize so two worktrees of the same repo compare equal.
    abs.canonicalize().ok().or(Some(abs))
}

fn real(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Detect a mismatch between the caller's location (`start`) and the indexed root (`index_root`).
///
/// Returns `None` (no warning) when: `start` is not in git; the caller's worktree *is* the indexed
/// root; the indexed root is not itself a git worktree; or the two are in **different** repos
/// (a nested repo/submodule the parent index already covers). Only a genuine same-repo,
/// different-worktree situation returns `Some`.
pub fn detect(start: &Path, index_root: &Path) -> Option<Mismatch> {
    let wt = worktree_root(start)?; // start not in git → no mismatch
    let index_real = real(index_root);
    if real(&wt) == index_real {
        return None; // same tree
    }
    // The index must itself be a git worktree for a *worktree* mismatch to make sense.
    let index_wt = worktree_root(index_root)?;
    if real(&index_wt) != index_real {
        return None; // the index isn't a worktree root — not a worktree mismatch
    }
    // Same repository? Different common dirs ⇒ different repos (submodule/nested) ⇒ not a mismatch.
    match (common_dir(start), common_dir(index_root)) {
        (Some(a), Some(b)) if a != b => return None,
        _ => {}
    }
    Some(Mismatch { worktree_root: real(&wt), index_root: index_real })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_init(dir: &Path) {
        Command::new("git").arg("-C").arg(dir).args(["init", "-q"]).status().unwrap();
        Command::new("git").arg("-C").arg(dir).args(["config", "user.email", "t@t.co"]).status().unwrap();
        Command::new("git").arg("-C").arg(dir).args(["config", "user.name", "t"]).status().unwrap();
    }

    #[test]
    fn a_non_git_dir_is_never_a_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect(tmp.path(), tmp.path()).is_none());
    }

    #[test]
    fn the_same_worktree_is_not_a_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        assert!(detect(tmp.path(), tmp.path()).is_none());
    }

    #[test]
    fn a_second_worktree_of_the_same_repo_is_a_mismatch() {
        let main = tempfile::tempdir().unwrap();
        git_init(main.path());
        // A repo needs a commit before `git worktree add`.
        std::fs::write(main.path().join("f.txt"), "x").unwrap();
        Command::new("git").arg("-C").arg(main.path()).args(["add", "-A"]).status().unwrap();
        Command::new("git").arg("-C").arg(main.path()).args(["commit", "-qm", "init"]).status().unwrap();

        let wt = main.path().join("wt");
        let ok = Command::new("git")
            .arg("-C")
            .arg(main.path())
            .args(["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "branch2"])
            .status()
            .unwrap()
            .success();
        assert!(ok, "git worktree add");

        // Indexed the main tree, but querying from the second worktree → mismatch.
        let m = detect(&wt, main.path()).expect("a second worktree is a mismatch");
        assert_eq!(real(&m.index_root), real(main.path()));
    }
}
