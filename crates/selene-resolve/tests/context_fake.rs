#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 2 — the `FakeContext` test rig itself.
//!
//! Every strategy test in Tasks 3–10 is written against this fake, so the fake
//! is worth its own tests: a rig that lies (a `nodes_by_name` that ignores case,
//! a `method_matches` that skips the qualified-name filter) would make the whole
//! matcher suite green over a resolver that does not work.
//!
//! The rig deliberately reproduces the **real store's** semantics, including the
//! ones that surprised us: `count_files_with_name` counts DISTINCT FILES, not
//! nodes (spike F2), and `method_matches` applies the exact
//! `"{ty}::{method}"` / `"::{ty}::{method}"` filter that makes a wrong type
//! inference yield *no* match rather than a wrong one.

mod common;

use common::{FakeContext, node, ts_fn, ts_method};
use selene_core::{Language, NodeKind};
use selene_resolve::ResolutionContext;

#[test]
fn nodes_are_queryable_by_name_file_kind_and_qualified_name() {
    let ctx = FakeContext::new()
        .with_node(ts_fn("function:a", "save", "src/a.ts"))
        .with_node(ts_fn("function:b", "save", "src/b.ts"))
        .with_node(ts_method("method:repo_save", "Repo", "save", "src/repo.ts"));

    assert_eq!(ctx.nodes_by_name("save").len(), 3);
    assert_eq!(ctx.nodes_by_name("missing").len(), 0);
    assert_eq!(ctx.nodes_in_file("src/a.ts").len(), 1);
    assert_eq!(ctx.nodes_by_kind(NodeKind::Method).len(), 1);
    assert_eq!(ctx.nodes_by_kind(NodeKind::Function).len(), 2);
    assert_eq!(ctx.nodes_by_qualified_name("Repo::save").len(), 1);
    assert_eq!(ctx.node_by_id("function:a").unwrap().name, "save");
    assert!(ctx.node_by_id("nope").is_none());
}

#[test]
fn lower_name_lookup_is_case_folding() {
    let ctx = FakeContext::new().with_node(ts_fn("function:a", "SaveUser", "src/a.ts"));
    assert_eq!(ctx.nodes_by_lower_name("saveuser").len(), 1);
    // The caller pre-lowercases (the store's contract) — an unfolded query misses.
    assert_eq!(ctx.nodes_by_lower_name("SaveUser").len(), 0);
}

/// The fake reproduces the store's REAL (surprising) semantics: this counts
/// distinct FILES, not nodes. See spike F2 — the plan's Task 7 assumed nodes,
/// and a fake that "helpfully" counted nodes would hide the discrepancy from
/// every test written against it.
#[test]
fn count_files_with_name_counts_files_not_nodes() {
    let ctx = FakeContext::new()
        .with_node(ts_fn("function:a1", "get", "src/a.ts"))
        .with_node(ts_fn("function:a2", "get", "src/a.ts")) // same file
        .with_node(ts_fn("function:b1", "get", "src/b.ts"));

    assert_eq!(ctx.nodes_by_name("get").len(), 3, "three NODES named `get`");
    assert_eq!(
        ctx.count_files_with_name("get"),
        2,
        "but only two FILES contain one — the store's actual semantics"
    );
}

/// `method_matches` is the validated-inference mechanism: a method that does not
/// exist on the inferred type yields NO match, which is what makes a wrong type
/// guess produce no edge rather than a wrong one.
#[test]
fn method_matches_validates_the_type_and_the_language() {
    let ctx = FakeContext::new()
        .with_node(ts_method("method:repo_get", "Repo", "get", "src/repo.ts"))
        .with_node(ts_method(
            "method:cache_get",
            "Cache",
            "get",
            "src/cache.ts",
        ))
        .with_node(node(
            "method:pkg_repo_get",
            NodeKind::Method,
            "get",
            "pkg::Repo::get", // a package-qualified Repo — the SUFFIX form
            "src/pkg/repo.ts",
            Language::Typescript,
        ))
        .with_node(node(
            "method:go_repo_get",
            NodeKind::Method,
            "get",
            "Repo::get",
            "src/repo.go",
            Language::Go, // right shape, wrong language
        ));

    let hits = ctx.method_matches(Language::Typescript, "Repo", "get");
    let ids: Vec<&str> = hits.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["method:repo_get", "method:pkg_repo_get"],
        "exact `Repo::get` AND the `::Repo::get` suffix form; NOT Cache::get, \
         and NOT the Go node"
    );

    // The absent-method case — the whole safety property in one assertion.
    assert!(
        ctx.method_matches(Language::Typescript, "Repo", "nonexistent")
            .is_empty(),
        "a method the type does not have yields NO match ⇒ no edge, never a wrong one"
    );
    assert!(
        ctx.method_matches(Language::Typescript, "NotAType", "get")
            .is_empty(),
        "a type that does not exist yields NO match"
    );
}

/// Node-anchored supertypes + `contains`-anchored members: the mechanism behind
/// both conformance passes (`#750`, `#808`). Cross-file, because that is the
/// only case that matters.
#[test]
fn supertypes_and_members_are_node_anchored() {
    let ctx = FakeContext::new()
        .with_node(node(
            "class:Dog",
            NodeKind::Class,
            "Dog",
            "Dog",
            "src/dog.ts",
            Language::Typescript,
        ))
        .with_node(node(
            "class:Animal",
            NodeKind::Class,
            "Animal",
            "Animal",
            "src/animal.ts",
            Language::Typescript,
        ))
        .with_node(ts_method(
            "method:animal_speak",
            "Animal",
            "speak",
            "src/animal.ts",
        ))
        .with_supertype("class:Dog", "class:Animal")
        .with_member("class:Animal", "method:animal_speak");

    let supers = ctx.supertypes("class:Dog");
    assert_eq!(supers.len(), 1);
    assert_eq!(supers[0].id, "class:Animal");

    let members = ctx.members_of("class:Animal");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].qualified_name, "Animal::speak");

    // A node with no supertype edge has no supertypes — not an error.
    assert!(ctx.supertypes("class:Animal").is_empty());
}

#[test]
fn warm_caches_reflect_the_nodes_and_files() {
    let ctx = FakeContext::new()
        .with_node(ts_fn("function:a", "save", "src/a.ts"))
        .with_node(node(
            "function:p",
            NodeKind::Function,
            "load",
            "load",
            "src/p.py",
            Language::Python,
        ))
        .with_file("src/a.ts", "export function save() {}");

    assert_eq!(
        ctx.all_files(),
        &["src/a.ts".to_string(), "src/p.py".to_string()]
    );
    assert!(ctx.known_names().contains("save"));
    assert!(ctx.known_names().contains("load"));
    assert!(!ctx.known_names().contains("missing"));
    assert!(ctx.file_exists("src/a.ts"));
    assert!(!ctx.file_exists("src/nope.ts"));

    let langs: Vec<Language> = ctx.languages().iter().copied().collect();
    assert_eq!(
        langs,
        vec![Language::Typescript, Language::Python],
        "BTreeSet ⇒ deterministic order (declaration order, not insertion order)"
    );

    assert_eq!(
        ctx.files_with_language(),
        &[
            ("src/a.ts".to_string(), Language::Typescript),
            ("src/p.py".to_string(), Language::Python),
        ]
    );

    assert_eq!(
        ctx.read_file("src/a.ts").unwrap(),
        "export function save() {}"
    );
    assert!(ctx.read_file("src/gone.ts").is_none());
    assert_eq!(ctx.file_lines("src/a.ts").unwrap().len(), 1);
}

/// The read counter — Task 3's pre-filter test asserts a reference with no
/// possible match short-circuits BEFORE any strategy queries the graph, and this
/// is the instrument that proves it.
#[test]
fn the_read_counter_tracks_graph_reads() {
    let ctx = FakeContext::new().with_node(ts_fn("function:a", "save", "src/a.ts"));
    assert_eq!(ctx.read_count(), 0);
    ctx.nodes_by_name("save");
    ctx.nodes_by_name("save");
    assert_eq!(ctx.read_count(), 2);
    // The warm caches are NOT graph reads — they are already in memory.
    ctx.known_names().contains("save");
    ctx.file_exists("src/a.ts");
    assert_eq!(ctx.read_count(), 2);
}
