#![allow(clippy::unwrap_used, clippy::expect_used)]
//! git-hooks against a **real** git repo — exercises `hooks_dir` (the `git rev-parse` resolution)
//! which the unit tests can't reach, plus install/preserve/remove on the actual hooks directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use selene_sync::hooks;

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn bin() -> PathBuf {
    PathBuf::from("/abs/selene")
}

#[test]
fn install_preserves_a_users_hook_and_remove_takes_only_our_block() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);

    let dir = hooks::hooks_dir(root).expect("a git repo has a hooks dir");
    std::fs::create_dir_all(&dir).unwrap();
    // A user's pre-existing hook.
    std::fs::write(dir.join("post-commit"), "#!/bin/sh\necho mine\n").unwrap();

    // Install: 3 hooks, the user's line kept.
    let installed = hooks::install(root, &bin()).unwrap();
    assert_eq!(installed.len(), 3);
    let pc = std::fs::read_to_string(dir.join("post-commit")).unwrap();
    assert!(pc.contains("echo mine"), "user's line survives");
    assert!(hooks::has_block(&pc), "our block is present");
    assert!(
        std::fs::read_to_string(dir.join("post-merge"))
            .unwrap()
            .contains(hooks::MARKER_BEGIN)
    );

    // Reinstall is idempotent (all unchanged).
    let again = hooks::install(root, &bin()).unwrap();
    assert!(
        again.iter().all(|r| r.action == "unchanged"),
        "reinstall is a no-op: {again:?}"
    );

    // Remove: our block gone, the user's file kept (had other content); the two we created deleted.
    hooks::remove(root).unwrap();
    let pc = std::fs::read_to_string(dir.join("post-commit")).unwrap();
    assert!(pc.contains("echo mine"), "user's line kept after remove");
    assert!(!hooks::has_block(&pc), "our block stripped");
    assert!(
        !dir.join("post-merge").exists(),
        "a hook we created (only shebang+block) is deleted"
    );
}

#[test]
fn outside_a_git_repo_install_is_a_silent_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    // No `git init` — not a repo.
    let installed = hooks::install(tmp.path(), &bin()).unwrap();
    assert!(installed.is_empty(), "no hooks outside a git repo");
}
