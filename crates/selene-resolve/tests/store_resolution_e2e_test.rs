#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end resolution through a REAL store (Task 11 rider).
//!
//! # Why this exists
//!
//! Every ladder/import test in this crate runs against `FakeContext` — an
//! in-memory vec of nodes with hand-written lookups. That proves the resolution
//! *logic*, and nothing about the layer underneath it. It is exactly the failure
//! mode this project has already been bitten by twice: green tests pinning a
//! wrong assumption about the layer below (the Phase-1 `delete_resolved` 2-tuple
//! data loss, and the ambiguity ceiling wired to a FILE count — both had passing
//! test suites above them).
//!
//! So: one flow, driven the way production drives it — `StoreContext` over a
//! real in-memory `SurrealStore`, resolving through the real SurrealQL
//! qualified-name index rather than a `Vec::iter().find()`.
//!
//! The flow chosen is the JVM FQN import (`#314`), because it is the one that
//! *depends* on the store's qualified-name lookup to disambiguate: two classes
//! share the simple name `FooConverter`, and only the fully-qualified name tells
//! them apart. A `FakeContext` cannot prove that index exists.

use std::path::PathBuf;

use selene_core::{FileRecord, Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_db::SurrealStore;
use selene_resolve::{
    ReferenceResolver, ResolvedBy, StoreContext, resolve_jvm_import, resolve_via_import,
};

fn java_class(id: &str, name: &str, qn: &str, file: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Class,
        name: name.to_string(),
        qualified_name: qn.to_string(),
        file_path: file.to_string(),
        language: Language::Java.as_str().to_string(),
        start_line: 1,
        end_line: 40,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: Some(true),
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: vec![],
        type_parameters: vec![],
        return_type: None,
        route_method: None,
        route_path: None,
        framework: None,
        updated_at: 0,
    }
}

fn file_record(path: &str) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: "deadbeef".to_string(),
        language: Language::Java.as_str().to_string(),
        size: 10,
        modified_at: 0,
        indexed_at: 0,
        node_count: 1,
        errors: vec![],
    }
}

fn import_ref(fqn: &str, from_file: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "class:Main".to_string(),
        reference_name: fqn.to_string(),
        reference_kind: "imports".to_string(),
        line: Some(3),
        column: Some(0),
        candidates: vec![],
        file_path: from_file.to_string(),
        language: Language::Java.as_str().to_string(),
        status: RefStatus::Pending,
        name_tail: fqn.rsplit('.').next().unwrap_or(fqn).to_string(),
    }
}

/// Two `FooConverter` classes in different packages; the import names one by its
/// FQN. Resolution must pick the right one — through the store's real
/// qualified-name index, from a `StoreContext`, on a blocking thread, exactly as
/// Part C's driver will run it.
#[tokio::test(flavor = "multi_thread")]
async fn jvm_fqn_import_resolves_through_a_real_store_context() {
    const DAO: &str = "src/main/java/com/example/dao/FooConverter.java";
    const WEB: &str = "src/main/java/com/example/web/FooConverter.java";
    const MAIN: &str = "src/main/java/com/example/Main.java";

    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store
        .insert_nodes(&[
            java_class(
                "class:dao_FooConverter",
                "FooConverter",
                "com.example.dao::FooConverter",
                DAO,
            ),
            java_class(
                "class:web_FooConverter",
                "FooConverter",
                "com.example.web::FooConverter",
                WEB,
            ),
            java_class("class:Main", "Main", "com.example::Main", MAIN),
        ])
        .await
        .unwrap();
    for p in [DAO, WEB, MAIN] {
        store.upsert_file(&file_record(p)).await.unwrap();
    }

    let ctx = StoreContext::new(store, PathBuf::from("/tmp/jvm-e2e"))
        .await
        .unwrap();

    // The resolver is sync and drives the async store through `block_on`, which
    // is only legal off the runtime's worker threads — so run it exactly where
    // production runs it.
    tokio::task::spawn_blocking(move || {
        let r = import_ref("com.example.dao.FooConverter", MAIN);

        // (a) The import strategy in isolation.
        let hit = resolve_jvm_import(&r, &ctx).expect(
            "the FQN must resolve through the store's qualified-name index — if this is \
             None, the index or its query is wrong, and every FakeContext test above it \
             would still be green",
        );
        assert_eq!(
            hit.target_node_id, "class:dao_FooConverter",
            "the FQN disambiguates: the `dao` class, NOT the same-named `web` one"
        );
        assert_eq!(hit.confidence, 0.95, "the JVM-FQN confidence constant");
        assert_eq!(hit.resolved_by, ResolvedBy::Import);
        assert_eq!(
            hit.original.reference_name, "com.example.dao.FooConverter",
            "ResolvedRef.original is the stored row, unmutated"
        );

        // (b) `resolve_via_import` is deliberately NOT the JVM path — it binds
        // through *import mappings* (ES modules, Go, …), and a Java file declares
        // none. The ladder routes JVM FQNs to `resolve_jvm_import` at step 5
        // instead, ahead of the frameworks and the name matcher. Pinning that
        // separation here so a future refactor cannot quietly merge the two.
        assert!(
            resolve_via_import(&r, &ctx).is_none(),
            "the JVM FQN must NOT resolve through the import-mapping strategy — it \
             has its own step, and conflating them would change precedence"
        );

        // (c) And through the whole ladder, which is what actually ships.
        let mut resolver = ReferenceResolver::new(ctx);
        let resolved = resolver
            .resolve_one(&r)
            .expect("the ladder binds the FQN import end-to-end over a real store");
        assert_eq!(resolved.target_node_id, "class:dao_FooConverter");
        assert_eq!(resolved.confidence, 0.95);
        assert_eq!(resolved.resolved_by, ResolvedBy::Import);
    })
    .await
    .expect("the sync resolver must drive the async store from a blocking thread");
}

/// The negative half: an FQN that names no indexed class resolves to nothing —
/// over a real store, not a fake one. (Silent beats wrong: a miss is `None`, not
/// a guess at the same-named class in the other package.)
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_jvm_fqn_resolves_to_nothing_over_a_real_store() {
    const MAIN: &str = "src/main/java/com/example/Main.java";

    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store
        .insert_nodes(&[java_class(
            "class:web_FooConverter",
            "FooConverter",
            "com.example.web::FooConverter",
            "src/main/java/com/example/web/FooConverter.java",
        )])
        .await
        .unwrap();
    store.upsert_file(&file_record(MAIN)).await.unwrap();

    let ctx = StoreContext::new(store, PathBuf::from("/tmp/jvm-e2e-miss"))
        .await
        .unwrap();

    tokio::task::spawn_blocking(move || {
        // `com.example.dao.FooConverter` is NOT indexed — only the `web` one is.
        let r = import_ref("com.example.dao.FooConverter", MAIN);
        assert!(
            resolve_jvm_import(&r, &ctx).is_none(),
            "an FQN naming no indexed class must resolve to NOTHING — it must not \
             fall back to the same-named class in a different package"
        );
    })
    .await
    .unwrap();
}
