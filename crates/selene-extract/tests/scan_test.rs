#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `scan_directory` pipeline tests (Task 17): the git fast path (tracked +
//! untracked, gitignore respected, `-z` NUL parsing for CJK paths, embedded
//! repos recursed, gitlinks, worktrees skipped), the gitignored-root FS
//! fallback, the non-git walk (scoped `.gitignore`s + symlink-cycle guard),
//! and the embedded-repo discovery helpers. Uses real `git init` repos in
//! tempdirs, like CodeGraph's scan test suites.

use std::fs;
use std::path::Path;
use std::process::Command;

use selene_extract::{
    ScanOverrides, discover_embedded_repo_roots, find_unindexed_ignored_repos, scan_directory,
};

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        // Hermetic: a developer's global/system gitconfig (hooks,
        // fsmonitor, init.defaultBranch, ignore tweaks) must not leak into
        // these fixture repos.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {cwd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `git init` + identity config so commits work in a clean environment.
fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn scan(root: &Path) -> Vec<String> {
    scan_directory(root, &ScanOverrides::default()).expect("scan_directory")
}

// =============================================================================
// Git fast path
// =============================================================================

#[test]
fn git_path_keeps_tracked_and_untracked_drops_gitignored_and_default_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    write(root, ".gitignore", "ignored/\n");
    write(root, "src/main.rs", "fn main() {}\n");
    // A committed dependency dir stays out: defaults apply to tracked files
    // too (#407).
    write(root, "node_modules/dep/index.js", "module.exports = 1;\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    // Untracked but visible.
    write(root, "new.ts", "export const x = 1;\n");
    // Gitignored — excluded.
    write(root, "ignored/x.ts", "export const y = 2;\n");

    let files = scan(root);
    assert!(files.contains(&"src/main.rs".to_string()), "{files:?}");
    assert!(files.contains(&"new.ts".to_string()), "{files:?}");
    assert!(
        !files.iter().any(|f| f.starts_with("ignored/")),
        "{files:?}"
    );
    assert!(
        !files.iter().any(|f| f.starts_with("node_modules/")),
        "tracked default-ignored dir must stay out: {files:?}"
    );
    let mut sorted = files.clone();
    sorted.sort();
    assert_eq!(files, sorted, "output must be sorted");
}

/// #541: `-z` NUL-delimited output keeps non-ASCII paths verbatim (without it
/// git octal-escapes + quotes them and the paths never match disk).
#[test]
fn cjk_filename_survives_the_z_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    write(root, "日本語モジュール.ts", "export const 名前 = 1;\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "cjk"]);

    let files = scan(root);
    assert!(
        files.contains(&"日本語モジュール.ts".to_string()),
        "CJK path must survive: {files:?}"
    );
}

/// #193: an untracked embedded repo surfaces as an opaque `child/` entry the
/// parent's ls-files refuses to descend into — it is recursed as its own repo.
#[test]
fn untracked_embedded_repo_is_recursed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    write(root, "app.ts", "export {};\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);

    let child = root.join("child");
    fs::create_dir_all(&child).unwrap();
    init_repo(&child);
    write(&child, "lib.go", "package lib\n");
    git(&child, &["add", "."]);
    git(&child, &["commit", "-q", "-m", "child"]);

    let files = scan(root);
    assert!(files.contains(&"app.ts".to_string()), "{files:?}");
    assert!(
        files.contains(&"child/lib.go".to_string()),
        "embedded repo files must be indexed: {files:?}"
    );

    let roots = discover_embedded_repo_roots(root);
    assert_eq!(roots, vec!["child/".to_string()]);
}

/// A tracked mode-160000 gitlink (embedded clone `git add`ed without
/// `.gitmodules`) is invisible to both the tracked expansion and the
/// untracked listing — the gitlink pass must recurse it (#1031, #1033).
#[test]
fn tracked_gitlink_with_real_checkout_is_recursed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    write(root, "app.ts", "export {};\n");

    let child = root.join("tool");
    fs::create_dir_all(&child).unwrap();
    init_repo(&child);
    write(&child, "tool.py", "x = 1\n");
    git(&child, &["add", "."]);
    git(&child, &["commit", "-q", "-m", "tool"]);

    git(root, &["add", "."]); // records tool as a 160000 gitlink
    git(root, &["commit", "-q", "-m", "gitlink"]);

    let files = scan(root);
    assert!(
        files.contains(&"tool/tool.py".to_string()),
        "gitlink checkout must be indexed: {files:?}"
    );
}

/// #848/#945: a git worktree's `.git` FILE points into some repo's
/// `.git[/modules/<m>]/worktrees/` — a duplicate working view, skipped.
#[test]
fn worktree_git_file_is_skipped() {
    let outer = tempfile::tempdir().unwrap();
    // The "other" repo whose worktree lands inside our scan root.
    let other = outer.path().join("other");
    fs::create_dir_all(&other).unwrap();
    init_repo(&other);
    write(&other, "dup.ts", "export const d = 1;\n");
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "other"]);

    let root = outer.path().join("root");
    fs::create_dir_all(&root).unwrap();
    init_repo(&root);
    write(&root, "mine.ts", "export {};\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "mine"]);

    // Place a worktree of `other` INSIDE the scan root.
    let wt = root.join("wt");
    git(&other, &["worktree", "add", "-q", wt.to_str().unwrap()]);

    let files = scan(&root);
    assert!(files.contains(&"mine.ts".to_string()), "{files:?}");
    assert!(
        !files.iter().any(|f| f.starts_with("wt/")),
        "a worktree is a duplicate view and must be skipped: {files:?}"
    );
}

/// #514/#970/#976: a gitignored embedded repo is respected (not indexed) by
/// default; the `include_ignored` override opts it back in.
#[test]
fn gitignored_embedded_repo_needs_the_include_ignored_opt_in() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    write(root, ".gitignore", "repos/\n");
    write(root, "app.ts", "export {};\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);

    let child = root.join("repos/svc");
    fs::create_dir_all(&child).unwrap();
    init_repo(&child);
    write(&child, "svc.rb", "puts 1\n");
    git(&child, &["add", "."]);
    git(&child, &["commit", "-q", "-m", "svc"]);

    // Default: .gitignore is respected — the nested repo stays out.
    let files = scan(root);
    assert!(
        !files.iter().any(|f| f.starts_with("repos/")),
        "gitignored repos stay out by default: {files:?}"
    );

    // Opted in: indexed.
    let overrides = ScanOverrides {
        include_ignored: vec!["repos/".to_string()],
        ..ScanOverrides::default()
    };
    let files = scan_directory(root, &overrides).unwrap();
    assert!(
        files.contains(&"repos/svc/svc.rb".to_string()),
        "include_ignored must opt the nested repo in: {files:?}"
    );

    // The CLI hint enumerates exactly the skipped kind.
    let hints = find_unindexed_ignored_repos(root);
    assert_eq!(hints, vec!["repos/svc/".to_string()]);
}

// =============================================================================
// Fallbacks
// =============================================================================

/// A root that is itself gitignored by an enclosing repo gets the FS walk
/// (its parent's ls-files would return nothing).
#[test]
fn gitignored_root_inside_a_parent_repo_falls_back_to_the_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path();
    init_repo(parent);
    write(parent, ".gitignore", "sub/\n");
    write(parent, "up.ts", "export {};\n");
    git(parent, &["add", "."]);
    git(parent, &["commit", "-q", "-m", "parent"]);

    write(parent, "sub/inner.ts", "export const i = 1;\n");
    write(parent, "sub/skipme/.gitignore", ""); // just a dir marker
    write(parent, "sub/.gitignore", "gen/\n");
    write(parent, "sub/gen/out.ts", "export const o = 1;\n");

    let files = scan(&parent.join("sub"));
    assert!(files.contains(&"inner.ts".to_string()), "{files:?}");
    assert!(
        !files.iter().any(|f| f.starts_with("gen/")),
        "the walk must honor the root's own .gitignore: {files:?}"
    );
}

#[test]
fn non_git_walk_layers_scoped_gitignores_and_sorts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, ".gitignore", "dropped/\n");
    write(root, "src/a.rs", "fn a() {}\n");
    write(root, "nested/.gitignore", "gen/\n");
    write(root, "nested/keep.rs", "fn k() {}\n");
    write(root, "nested/gen/b.rs", "fn b() {}\n");
    write(root, "dropped/c.rs", "fn c() {}\n");
    write(root, "droppedX/d.rs", "fn d() {}\n");

    let files = scan(root);
    assert_eq!(
        files,
        vec![
            "droppedX/d.rs".to_string(),
            "nested/keep.rs".to_string(),
            "src/a.rs".to_string(),
        ],
        "scoped .gitignores + root rules + sorted output"
    );
}

#[cfg(unix)]
#[test]
fn symlink_cycle_terminates_the_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "a/f.ts", "export {};\n");
    // a/loop -> root: walking descends a/ -> loop/(=root) -> a/ -> already
    // visited (canonicalized) -> stop.
    std::os::unix::fs::symlink(root, root.join("a/loop")).unwrap();

    let files = scan(root);
    assert!(files.contains(&"a/f.ts".to_string()), "{files:?}");
    assert!(
        files.len() < 10,
        "cycle must terminate without duplicating the tree: {files:?}"
    );
}

#[test]
fn non_git_root_discovery_helpers_return_empty() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "x.ts", "export {};\n");
    assert!(discover_embedded_repo_roots(tmp.path()).is_empty());
    assert!(find_unindexed_ignored_repos(tmp.path()).is_empty());
}
