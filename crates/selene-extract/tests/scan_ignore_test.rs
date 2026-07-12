#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ScopeIgnore` contract tests (Task 16): built-in default ignores, the
//! defensive `.gitignore` reader, gitignore negation semantics, embedded-repo
//! scoping (#514), and the `{include, exclude}` overrides (#999). Ports the
//! ScopeIgnore-level cases of CodeGraph's `android-res-exclusion.test.ts`
//! (#1047) and `exclude-config.test.ts`.

use std::fs;
use std::path::Path;

use selene_extract::{ScopeIgnore, ScopeOverrides};

/// Write `body` to `root/rel`, creating parents.
fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// `ScopeIgnore` over `root` with no embedded repos and no overrides.
fn plain_scope(root: &Path) -> ScopeIgnore {
    ScopeIgnore::build(root, &[], &ScopeOverrides::default())
}

// =============================================================================
// Built-in defaults
// =============================================================================

#[test]
fn defaults_exclude_dependency_and_build_dirs_at_any_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let ig = plain_scope(tmp.path());

    // Directory form (trailing slash) and files beneath, at root and nested.
    for rel in [
        "node_modules/",
        "node_modules/pkg/index.js",
        "sub/dir/node_modules/x.js",
        "dist/bundle.js",
        "build/a.o",
        "out/gen.ts",
        ".output/x.mjs",
        "target/debug/selene",
        ".gradle/cache.bin",
        "__pycache__/m.pyc",
        ".venv/lib/site.py",
        "venv/bin/python",
        "coverage/lcov.info",
        ".next/app.js",
        "obj/Program.dll",
        "vendor/composer/autoload.php",
        "Pods/Alamofire/A.swift",
        ".dart_tool/pkg.dill",
        "__history/Unit1.pas",
        ".cache/x.tmp",
        // Glob defaults.
        "proj.egg-info/PKG-INFO",
        "cmake-build-debug/CMakeCache.txt",
        "bazel-out/k8/gen.cc",
    ] {
        assert!(ig.ignores(rel), "default must ignore {rel}");
    }

    // Similar-but-different names stay in.
    for rel in [
        "src/main.rs",
        "vendored/x.js",
        "distX/y.js",
        "outline/z.ts",
        "building/a.rs",
    ] {
        assert!(!ig.ignores(rel), "must keep {rel}");
    }
}

/// The ScopeIgnore-level port of `android-res-exclusion.test.ts` (#1047):
/// every typed `res/` subdir (with qualifier variants) is excluded at any
/// depth; code, plain XML, MyBatis mappers, and `res/raw/` stay in.
#[test]
fn android_res_dirs_excluded_but_raw_and_mappers_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let ig = plain_scope(tmp.path());

    for rel in [
        "app/src/main/res/layout/activity_main.xml",
        "app/src/main/res/values/strings.xml",
        "app/src/main/res/values-es/strings.xml",
        "app/src/main/res/drawable/ic_foo.xml",
        "app/src/main/res/drawable-hdpi/ic_foo.xml",
        "app/src/main/res/menu/main_menu.xml",
        "app/src/main/res/layout-v21/activity_main.xml",
    ] {
        assert!(ig.ignores(rel), "android res must be excluded: {rel}");
    }

    for rel in [
        "app/src/main/java/com/example/Main.java",
        "pom.xml",
        "app/src/main/res/raw/payload.xml", // deliberately spared (code-ish assets)
        "src/main/resources/FooMapper.xml", // MyBatis mappers never live under res/
        "results/values/data.xml",          // not a res/ dir
    ] {
        assert!(!ig.ignores(rel), "must keep {rel}");
    }
}

// =============================================================================
// Root .gitignore merge + negation semantics
// =============================================================================

#[test]
fn root_gitignore_rules_apply_and_negation_overrides_a_default() {
    let tmp = tempfile::tempdir().unwrap();
    // `!vendor/` is the documented opt-in that resurrects a committed
    // dependency dir (#407); `secret/` is an ordinary ignore rule.
    write(tmp.path(), ".gitignore", b"secret/\n!vendor/\n");
    let ig = plain_scope(tmp.path());

    assert!(ig.ignores("secret/keys.ts"));
    assert!(ig.ignores("secret/"));
    assert!(
        !ig.ignores("vendor/lib.php"),
        "a root .gitignore negation must override the built-in default"
    );
    assert!(!ig.ignores("vendor/"));
    // Unrelated defaults are untouched by the negation.
    assert!(ig.ignores("node_modules/x.js"));
}

// =============================================================================
// Defensive .gitignore reader (#682)
// =============================================================================

#[test]
fn gitignore_with_nul_bytes_is_rejected_whole() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), ".gitignore", b"droppable/\n\0ciphertext\n");
    let ig = plain_scope(tmp.path());
    assert!(
        !ig.ignores("droppable/x.ts"),
        "a NUL byte means the file is not gitignore text at all — skip it whole"
    );
}

#[test]
fn gitignore_with_invalid_utf8_is_rejected_whole() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), ".gitignore", b"droppable/\n\xFF\xFE garbage\n");
    let ig = plain_scope(tmp.path());
    assert!(!ig.ignores("droppable/x.ts"));
}

#[test]
fn gitignore_uncompilable_line_is_dropped_but_the_rest_kept() {
    let tmp = tempfile::tempdir().unwrap();
    // `a[` is an unterminated character class — uncompilable as a glob. The
    // surrounding good lines must survive (per-line compile probe, #682).
    write(tmp.path(), ".gitignore", b"gooddir/\na[\nlogs/\n");
    let ig = plain_scope(tmp.path());
    assert!(ig.ignores("gooddir/x.ts"));
    assert!(ig.ignores("logs/app.log"));
    assert!(
        !ig.ignores("ab/file.ts"),
        "the bad line must not half-apply"
    );
}

// =============================================================================
// Overrides: exclude wins over everything; include never revives defaults
// =============================================================================

#[test]
fn exclude_override_wins_over_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let overrides = ScopeOverrides {
        include: vec!["static/**".to_string()],
        exclude: vec!["static/".to_string(), "child/gen/".to_string()],
    };
    let ig = ScopeIgnore::build(tmp.path(), &["child/".to_string()], &overrides);

    // Nothing else ignores static/, and include even names it — exclude wins.
    assert!(ig.ignores("static/theme.css"));
    assert!(ig.ignores("static/"));
    // Exclude is matched on the FULL root-relative path, embedded repos included.
    assert!(ig.ignores("child/gen/api.js"));
    // Non-excluded neighbors stay in.
    assert!(!ig.ignores("statics/other.css"));
}

#[test]
fn include_forces_gitignored_files_back_unless_default_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), ".gitignore", b"generated/\n");
    let overrides = ScopeOverrides {
        include: vec!["generated/**".to_string(), "node_modules/**".to_string()],
        exclude: vec![],
    };
    let ig = ScopeIgnore::build(tmp.path(), &[], &overrides);

    assert!(
        !ig.ignores("generated/api.ts"),
        "include must override .gitignore"
    );
    assert!(
        !ig.ignores("generated/"),
        "a directory on an included subtree stays walkable"
    );
    assert!(
        ig.ignores("node_modules/x.js"),
        "include never resurrects a built-in default-ignored dir"
    );
    // Paths outside the include patterns are untouched by them.
    assert!(!ig.ignores("src/app.ts"));
}

// =============================================================================
// Embedded-repo scoping (#514)
// =============================================================================

#[test]
fn embedded_repo_files_are_judged_by_their_own_rules_not_the_parents() {
    let tmp = tempfile::tempdir().unwrap();
    // The super-repo gitignores its child repos to keep `git status` quiet —
    // that hides them from git, not from the index (#514).
    write(tmp.path(), ".gitignore", b"libs/\n");
    write(tmp.path(), "libs/child/.gitignore", b"dist2/\n");
    let ig = ScopeIgnore::build(
        tmp.path(),
        &["libs/child/".to_string()],
        &ScopeOverrides::default(),
    );

    // The embedded root itself and its ancestor dirs are never pruned.
    assert!(!ig.ignores("libs/child/"));
    assert!(!ig.ignores("libs/"));
    // Inside the embedded repo, only ITS rules apply — the parent's `libs/`
    // rule does not reach in.
    assert!(!ig.ignores("libs/child/src/a.js"));
    assert!(ig.ignores("libs/child/dist2/x.js"));
    // Built-in defaults apply to the FULL path uniformly (#407).
    assert!(ig.ignores("libs/child/node_modules/y.js"));
    // A gitignored path that is NOT inside the embedded repo follows the root
    // matcher as usual.
    assert!(ig.ignores("libs/other/b.js"));
}

#[test]
fn nested_embedded_repos_hit_the_innermost_matcher_first() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a/.gitignore", b"skip/\n");
    write(tmp.path(), "a/b/.gitignore", b"keep-not/\n");
    let ig = ScopeIgnore::build(
        tmp.path(),
        &["a/".to_string(), "a/b/".to_string()],
        &ScopeOverrides::default(),
    );

    // "a/b/keep-not/f.js" must be judged by a/b/'s matcher (inner path
    // "keep-not/f.js"), not a/'s (inner path "b/keep-not/f.js" — no match).
    assert!(ig.ignores("a/b/keep-not/f.js"));
    assert!(
        ig.ignores("a/skip/f.js"),
        "a/'s own rules still apply to a/*"
    );
    assert!(!ig.ignores("a/b/src/f.js"));
}
