#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 18 — the Cargo workspace crate map.
//!
//! # What breaks without it
//!
//! `use blog_core::Article` names a **sibling crate**, and nothing in the
//! reference says where that crate lives. Unresolved, the module arm either finds
//! a same-named local module (a wrong edge) or nothing at all. The map is what
//! turns `blog_core` into `crates/blog-core/src/lib.rs`.
//!
//! The two spellings are the trap: the manifest says `blog-core`, every `use` in
//! the code says `blog_core`, and a map that carries only one of them misses every
//! reference in the repo.

mod common;

use common::FakeContext;
use selene_resolve::frameworks::cargo::{
    cargo_workspace_crate_map, clear_cargo_workspace_cache, package_name, workspace_members,
};

/// A workspace with two members, reached through a `crates/*` glob.
fn workspace() -> FakeContext {
    FakeContext::new()
        .with_file(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        )
        .with_directory("", &["crates", "target", ".git"])
        .with_directory("crates", &["blog-core", "blog-api", "target", ".cache"])
        .with_file(
            "crates/blog-core/Cargo.toml",
            "[package]\nname = \"blog-core\"\nversion = \"0.1.0\"\n",
        )
        .with_file(
            "crates/blog-api/Cargo.toml",
            "[package]\nname = \"blog-api\"\nversion = \"0.1.0\"\n",
        )
}

#[test]
fn both_spellings_of_a_crate_name_map_to_its_directory() {
    clear_cargo_workspace_cache();
    let ctx = workspace();
    let map = cargo_workspace_crate_map(&ctx);

    assert_eq!(
        map.get("blog-core").map(String::as_str),
        Some("crates/blog-core"),
        "the manifest spelling"
    );
    assert_eq!(
        map.get("blog_core").map(String::as_str),
        Some("crates/blog-core"),
        "the UNDERSCORE spelling — this is the one every `use` in the repo writes, \
         and a map without it misses every workspace reference"
    );
    assert_eq!(
        map.get("blog_api").map(String::as_str),
        Some("crates/blog-api")
    );
}

#[test]
fn the_glob_walk_skips_target_and_dot_dirs() {
    clear_cargo_workspace_cache();
    let ctx = workspace();
    let map = cargo_workspace_crate_map(&ctx);

    // `crates/target` and `crates/.cache` were offered by the directory listing and
    // must never have been descended into. (`target/` alone holds thousands of
    // directories, and not one of them is a workspace member.)
    assert!(!map.keys().any(|k| k.contains("target")));
    assert!(!map.keys().any(|k| k.starts_with('.')));
    assert_eq!(map.len(), 4, "two crates × two spellings, and nothing else");
}

#[test]
fn a_member_deeper_than_the_walk_cap_is_not_walked() {
    clear_cargo_workspace_cache();
    let ctx = FakeContext::new()
        .with_file(
            "Cargo.toml",
            "[workspace]\nmembers = [\"a/b/c/d/e/f\"]\n", // 6 segments > MAX_GLOB_WALK_DEPTH
        )
        .with_file("a/b/c/d/e/f/Cargo.toml", "[package]\nname = \"too-deep\"\n");

    assert!(
        cargo_workspace_crate_map(&ctx).is_empty(),
        "MAX_GLOB_WALK_DEPTH = 5. The cap is what stops a pathological `**` from \
         walking a monorepo — the manifest is read once per index, but 'once' must \
         still be bounded."
    );
}

#[test]
fn no_workspace_section_is_an_empty_map_not_an_error() {
    clear_cargo_workspace_cache();
    let plain = FakeContext::new().with_file(
        "Cargo.toml",
        "[package]\nname = \"single\"\nversion = \"0.1.0\"\n",
    );
    assert!(cargo_workspace_crate_map(&plain).is_empty());

    // And a project with no manifest at all.
    assert!(cargo_workspace_crate_map(&FakeContext::new()).is_empty());
}

/// The parser is hand-rolled precisely so that a broken manifest degrades to "no
/// workspace" instead of failing the index. Errors are collected, never thrown.
#[test]
fn a_malformed_manifest_degrades_instead_of_failing() {
    clear_cargo_workspace_cache();
    let broken = FakeContext::new().with_file("Cargo.toml", "[workspace\nmembers = [\"crates/*\"");
    assert!(cargo_workspace_crate_map(&broken).is_empty());

    assert!(workspace_members("this is not toml at all").is_empty());
    assert_eq!(
        package_name("[package]\nname = \"ok\"\n").as_deref(),
        Some("ok")
    );
}

#[test]
fn a_member_whose_manifest_has_no_package_name_is_skipped() {
    clear_cargo_workspace_cache();
    let ctx = FakeContext::new()
        .with_file("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n")
        .with_directory("crates", &["ghost", "real"])
        // `ghost` has no [package] table — a stale directory, not a crate.
        .with_file("crates/ghost/Cargo.toml", "[dependencies]\nserde = \"1\"\n")
        .with_file("crates/real/Cargo.toml", "[package]\nname = \"real\"\n");

    let map = cargo_workspace_crate_map(&ctx);
    assert_eq!(
        map.len(),
        1,
        "only `real` — and `real` has no dash to alias"
    );
    assert_eq!(map.get("real").map(String::as_str), Some("crates/real"));
}
