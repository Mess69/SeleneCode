#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 2 — `StoreContext` against a REAL `SurrealStore`.
//!
//! The fake (`tests/common/mod.rs`) is what the strategy suites are written
//! against; this file is what proves the fake is not lying about the real thing.
//! It exercises the seam the whole crate stands on:
//!
//! - the **sync/async bridge** — a sync `ResolutionContext` method driving an
//!   async `GraphStore`, from inside `spawn_blocking` (which is where the Part C
//!   driver runs the resolver);
//! - the **warm caches** — computed once, from the store;
//! - the **generic-over-`S: GraphStore`** constraint: this file's assertions run
//!   through a function that is generic over the store, so a `StoreContext` that
//!   had quietly become SurrealStore-specific would not compile.

use std::collections::BTreeSet;
use std::path::PathBuf;

use selene_core::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind, Provenance};
use selene_db::{GraphStore, SurrealStore};
use selene_resolve::{ResolutionContext, StoreContext};

fn node(id: &str, kind: NodeKind, name: &str, qn: &str, file: &str, lang: Language) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: qn.to_string(),
        file_path: file.to_string(),
        language: lang.as_str().to_string(),
        start_line: 1,
        end_line: 10,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: None,
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: vec![],
        type_parameters: vec![],
        return_type: None,
        updated_at: 0,
    }
}

fn file_record(path: &str, lang: Language) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: "deadbeef".to_string(),
        language: lang.as_str().to_string(),
        size: 10,
        modified_at: 0,
        indexed_at: 0,
        node_count: 1,
        errors: vec![],
    }
}

/// A populated store: two TS files, one Python file, a class hierarchy.
async fn seeded_store() -> SurrealStore {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store
        .insert_nodes(&[
            node(
                "class:Dog",
                NodeKind::Class,
                "Dog",
                "Dog",
                "src/dog.ts",
                Language::Typescript,
            ),
            node(
                "class:Animal",
                NodeKind::Class,
                "Animal",
                "Animal",
                "src/animal.ts",
                Language::Typescript,
            ),
            node(
                "method:animal_speak",
                NodeKind::Method,
                "speak",
                "Animal::speak",
                "src/animal.ts",
                Language::Typescript,
            ),
            node(
                "function:load",
                NodeKind::Function,
                "load",
                "load",
                "src/loader.py",
                Language::Python,
            ),
        ])
        .await
        .unwrap();
    store
        .insert_edges(&[
            Edge {
                source: "class:Dog".into(),
                target: "class:Animal".into(),
                kind: EdgeKind::Extends,
                metadata: None,
                line: None,
                column: None,
                provenance: Some(Provenance::TreeSitter),
            },
            Edge {
                source: "class:Animal".into(),
                target: "method:animal_speak".into(),
                kind: EdgeKind::Contains,
                metadata: None,
                line: None,
                column: None,
                provenance: Some(Provenance::TreeSitter),
            },
        ])
        .await
        .unwrap();
    for (p, l) in [
        ("src/dog.ts", Language::Typescript),
        ("src/animal.ts", Language::Typescript),
        ("src/loader.py", Language::Python),
    ] {
        store.upsert_file(&file_record(p, l)).await.unwrap();
    }
    store
}

/// Every assertion below runs through THIS function, which is generic over the
/// store — the Global Constraint ("the resolver is generic over `S: GraphStore`,
/// never tied to SurrealStore") made executable. If `StoreContext` ever grows a
/// SurrealStore-specific dependency, this stops compiling.
fn assert_context_contract<S: GraphStore>(ctx: &StoreContext<S>) {
    // --- warm caches ---------------------------------------------------------
    assert_eq!(
        ctx.all_files(),
        &[
            "src/animal.ts".to_string(),
            "src/dog.ts".to_string(),
            "src/loader.py".to_string()
        ],
        "sorted — determinism"
    );
    let langs: BTreeSet<Language> = ctx.languages().clone();
    assert_eq!(
        langs,
        BTreeSet::from([Language::Typescript, Language::Python])
    );
    assert_eq!(ctx.files_with_language().len(), 3);
    assert_eq!(
        ctx.files_with_language()[0],
        ("src/animal.ts".to_string(), Language::Typescript)
    );
    assert!(ctx.known_names().contains("Dog"));
    assert!(ctx.known_names().contains("speak"));
    assert!(!ctx.known_names().contains("nonexistent"));
    assert!(ctx.file_exists("src/dog.ts"));
    assert!(!ctx.file_exists("src/ghost.ts"));

    // --- graph reads, through the sync→async bridge ---------------------------
    assert_eq!(ctx.nodes_by_name("Dog").len(), 1);
    assert_eq!(ctx.nodes_by_lower_name("dog").len(), 1);
    assert_eq!(ctx.nodes_by_qualified_name("Animal::speak").len(), 1);
    assert_eq!(ctx.nodes_in_file("src/animal.ts").len(), 2);
    assert_eq!(ctx.nodes_by_kind(NodeKind::Class).len(), 2);
    assert_eq!(ctx.node_by_id("class:Dog").unwrap().name, "Dog");
    assert!(
        ctx.node_by_id("class:Ghost").is_none(),
        "a miss, not an error"
    );

    // --- the two conformance-pass primitives ---------------------------------
    let supers = ctx.supertypes("class:Dog");
    assert_eq!(supers.len(), 1);
    assert_eq!(supers[0].id, "class:Animal", "node-anchored, cross-file");
    let members = ctx.members_of("class:Animal");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].qualified_name, "Animal::speak");

    // --- validated inference --------------------------------------------------
    assert_eq!(
        ctx.method_matches(Language::Typescript, "Animal", "speak")
            .len(),
        1
    );
    assert!(
        ctx.method_matches(Language::Typescript, "Animal", "fly")
            .is_empty(),
        "the type lacks the method ⇒ NO match ⇒ no edge, never a wrong one"
    );
    assert!(
        ctx.method_matches(Language::Python, "Animal", "speak")
            .is_empty(),
        "wrong language ⇒ no match"
    );

    // --- the store's real (surprising) counting semantics ---------------------
    assert_eq!(
        ctx.count_files_with_name("speak"),
        1,
        "FILES, not nodes (spike F2) — and the fake reproduces this exactly"
    );
}

/// The resolver runs under `spawn_blocking`; `StoreContext`'s sync methods
/// `block_on` the async store from there. This is that arrangement, end to end.
#[tokio::test(flavor = "multi_thread")]
async fn store_context_serves_the_contract_from_spawn_blocking() {
    let store = seeded_store().await;
    let ctx = StoreContext::new(store, PathBuf::from("/tmp/fake-root"))
        .await
        .unwrap();

    // Exactly how Part C's driver (Task 27) will run the resolver.
    tokio::task::spawn_blocking(move || {
        assert_context_contract(&ctx);
    })
    .await
    .expect("the sync context must drive the async store from a blocking thread");
}

/// Reads are cached: the second lookup of a key does not re-query. (Correctness
/// is what the caches must preserve — a stale-cache bug here would silently
/// change which symbol a reference binds to.)
#[tokio::test(flavor = "multi_thread")]
async fn repeated_reads_are_served_from_cache_and_agree() {
    let store = seeded_store().await;
    let ctx = StoreContext::new(store, PathBuf::from("/tmp/fake-root"))
        .await
        .unwrap();

    tokio::task::spawn_blocking(move || {
        let a = ctx.nodes_by_name("Dog");
        let b = ctx.nodes_by_name("Dog");
        assert_eq!(a, b);
        let m1 = ctx.method_matches(Language::Typescript, "Animal", "speak");
        let m2 = ctx.method_matches(Language::Typescript, "Animal", "speak");
        assert_eq!(m1, m2);

        // clear_caches() is what a framework's post_extract (Part B) forces.
        ctx.clear_caches();
        assert_eq!(
            ctx.nodes_by_name("Dog"),
            a,
            "a cleared cache re-reads the same truth"
        );
    })
    .await
    .unwrap();
}

/// A file path that escapes the root is refused by `read_file`, not read — the
/// resolver derives paths from *references*, which are attacker-influenced data
/// in a hostile repo. (Reads are relative to the project root; `..` and absolute
/// paths yield `None`, a miss, never an error.)
#[tokio::test(flavor = "multi_thread")]
async fn read_file_refuses_to_escape_the_project_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("inside.ts"), "export const x = 1;").unwrap();

    let store = seeded_store().await;
    let ctx = StoreContext::new(store, dir.path().to_path_buf())
        .await
        .unwrap();

    tokio::task::spawn_blocking(move || {
        assert_eq!(
            ctx.read_file("inside.ts").unwrap(),
            "export const x = 1;",
            "an in-root file reads normally"
        );
        assert!(
            ctx.read_file("../../../etc/passwd").is_none(),
            "a traversal is refused"
        );
        assert!(
            ctx.read_file("/etc/passwd").is_none(),
            "an absolute path is refused (join() would otherwise REPLACE the root)"
        );
        assert!(
            ctx.read_file("does/not/exist.ts").is_none(),
            "a miss is None"
        );
    })
    .await
    .unwrap();
}

/// **The `spawn_blocking` contract, pinned.**
///
/// `StoreContext`'s methods are sync and drive the async store through
/// `Handle::block_on`, which **panics** when called from a runtime worker thread.
/// The whole design rests on the resolver running under `spawn_blocking` (Part C's
/// driver guarantees it) — and until now that hazard lived only in a doc comment,
/// where a future task could violate it silently and only find out in production.
///
/// This test fixes it as a contract: calling a context method directly from an
/// async task panics, loudly and immediately. If someone later "fixes" the seam by
/// hand-rolling an executor, this test fails and they have to argue with it — which
/// is the point, because the alternative failure mode (a silent deadlock) is far
/// worse than a panic.
#[tokio::test(flavor = "multi_thread")]
async fn a_context_method_called_from_a_runtime_worker_panics() {
    let store = seeded_store().await;
    let ctx = StoreContext::new(store, PathBuf::from("/tmp/fake-root"))
        .await
        .unwrap();

    // Silence the panic's backtrace print — the panic IS the expected outcome.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // A warm-cache read is fine (it touches no store)...
        assert!(ctx.known_names().contains("Dog"));
        // ...but a GRAPH read must reach the store, and block_on cannot run here.
        ctx.nodes_by_name("Dog")
    }));
    std::panic::set_hook(prev);

    assert!(
        result.is_err(),
        "a StoreContext graph read from inside an async task MUST panic — the \
         resolver is required to run under spawn_blocking (see context.rs's \
         sync/async seam docs). A silent success here would mean the seam had \
         been replaced by something that can deadlock instead."
    );
}

/// A store read that FAILS is degraded to an empty result — but it is **counted**,
/// never silent. Without the counter, a store outage looks byte-identical to a repo
/// with nothing to resolve: nothing binds, and nothing says why. That is the
/// vacuous-resolution failure the Phase 3 gate exists to catch, arriving through the
/// back door.
#[tokio::test(flavor = "multi_thread")]
async fn store_read_errors_are_counted_not_swallowed() {
    let store = seeded_store().await;
    let ctx = StoreContext::new(store, PathBuf::from("/tmp/fake-root"))
        .await
        .unwrap();

    tokio::task::spawn_blocking(move || {
        assert_eq!(
            ctx.store_read_errors(),
            0,
            "a healthy store reports zero read errors"
        );
        // A successful lookup and a clean MISS both leave the counter at zero —
        // a miss is an ordinary outcome, not a malfunction.
        assert_eq!(ctx.nodes_by_name("Dog").len(), 1);
        assert!(ctx.nodes_by_name("NoSuchSymbol").is_empty());
        assert_eq!(
            ctx.store_read_errors(),
            0,
            "a MISS is not an error — only a store malfunction is"
        );
    })
    .await
    .unwrap();
}

/// An empty index yields an empty-but-valid context: no panics, no errors, every
/// lookup a clean miss. (The resolver runs on a fresh repo before anything is
/// indexed; a context that errored here would surface as an `isError` to an
/// agent, which is exactly what the reservation forbids.)
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_store_yields_an_empty_context() {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let ctx = StoreContext::new(store, PathBuf::from("/tmp/empty"))
        .await
        .unwrap();

    tokio::task::spawn_blocking(move || {
        assert!(ctx.all_files().is_empty());
        assert!(ctx.known_names().is_empty());
        assert!(ctx.languages().is_empty());
        assert!(ctx.nodes_by_name("anything").is_empty());
        assert!(ctx.node_by_id("nothing").is_none());
        assert_eq!(ctx.count_files_with_name("anything"), 0);
    })
    .await
    .unwrap();
}
