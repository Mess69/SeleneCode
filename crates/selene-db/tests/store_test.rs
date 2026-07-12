#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `SurrealStore` open/init + schema DDL contract tests (Task 3), node CRUD +
//! lookups (Task 4), plus edge operations + file projections (Task 5, see the
//! `edge_*`/`cross_file_*`/`dependent_*`/`dependency_*` tests below).
//!
//! Proves the four load-bearing guarantees of the schema layer:
//! 1. a fresh in-memory store applies the schema and reports version 1;
//! 2. `apply_schema` is idempotent (the whole schema is `IF NOT EXISTS`);
//! 3. an on-disk store (`open`, default `kv-surrealkv`) persists the schema
//!    version across a close/reopen of the same directory;
//! 4. the edge unique index folds a missing `line`/`col` to `-1`, so two edges
//!    with no source position between the same endpoints are duplicates.
//!
//! Test 4 reaches for the raw handle via the `#[doc(hidden)]` `db()` accessor
//! because `insert_edges` (Task 5) does not exist yet; once it lands it can be
//! rephrased against the typed API.

#[cfg(feature = "kv-mem")]
use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance, Visibility};
#[cfg(feature = "kv-mem")]
use selene_db::FileRecord;
use selene_db::SurrealStore;

/// A minimal SCHEMAFULL-valid `node` record body for a given key, used to give
/// the ENFORCED edge tables real endpoints to relate.
#[cfg(feature = "kv-mem")]
fn node_content(name: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "function",
        "name": name,
        "qualifiedName": name,
        "filePath": "src/a.rs",
        "language": "rust",
        "startLine": 1,
        "endLine": 1,
        "startColumn": 0,
        "endColumn": 0,
        "updatedAt": 0,
    })
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn in_memory_apply_schema_reports_version_one() {
    let store = SurrealStore::in_memory().await.unwrap();
    assert_eq!(
        store.schema_version().await.unwrap(),
        None,
        "no version before apply"
    );
    store.apply_schema().await.unwrap();
    assert_eq!(store.schema_version().await.unwrap(), Some(1));
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn apply_schema_is_idempotent() {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    // Applying again must not error (every statement is IF NOT EXISTS) and must
    // not bump or duplicate the seeded version.
    store.apply_schema().await.unwrap();
    store.apply_schema().await.unwrap();
    assert_eq!(store.schema_version().await.unwrap(), Some(1));
}

#[cfg(any(feature = "kv-surrealkv", feature = "kv-rocksdb"))]
#[tokio::test(flavor = "multi_thread")]
async fn on_disk_schema_version_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(selene_db::DATABASE_DIRNAME);

    {
        let store = SurrealStore::open(&dir).await.unwrap();
        store.apply_schema().await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), Some(1));
    } // drop closes the on-disk engine

    // Reopen the same directory: the persisted schema version must survive.
    let reopened = SurrealStore::open(&dir).await.unwrap();
    assert_eq!(
        reopened.schema_version().await.unwrap(),
        Some(1),
        "schema version must persist across reopen"
    );
    // A second apply on the reopened store stays a no-op.
    reopened.apply_schema().await.unwrap();
    assert_eq!(reopened.schema_version().await.unwrap(), Some(1));
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn edge_unique_index_folds_null_line_col() {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let db = store.db();

    // Two SCHEMAFULL-valid endpoint nodes (edge tables are ENFORCED).
    let _: Option<serde_json::Value> = db
        .create(("node", "function:a"))
        .content(node_content("a"))
        .await
        .unwrap();
    let _: Option<serde_json::Value> = db
        .create(("node", "function:b"))
        .content(node_content("b"))
        .await
        .unwrap();

    // First edge with NO line/col: accepted.
    let mut resp = db
        .query("RELATE node:`function:a`->calls->node:`function:b`")
        .await
        .unwrap();
    let first: Vec<serde_json::Value> = resp.take(0).unwrap();
    assert_eq!(first.len(), 1, "first null-position edge is inserted");

    // Second identical edge, still NO line/col: must collide as a duplicate.
    // Under a raw (in,out,line,col) index NONE would read as distinct; the
    // materialized lineKey/colKey (VALUE line ?? -1) fold both to -1 so this is
    // a real duplicate. The violation surfaces at take(0), not query().await.
    let mut resp = db
        .query("RELATE node:`function:a`->calls->node:`function:b`")
        .await
        .unwrap();
    let dup: Result<Vec<serde_json::Value>, surrealdb::Error> = resp.take(0);
    assert!(
        dup.is_err(),
        "second null-position edge must violate the unique index (null line/col folded to -1)"
    );

    // Exactly one edge persisted.
    let mut resp = db
        .query("SELECT count() FROM calls GROUP ALL")
        .await
        .unwrap();
    let counted: Vec<serde_json::Value> = resp.take(0).unwrap();
    assert_eq!(counted[0]["count"], 1);
}

// =============================================================================
// Node CRUD + lookups (Task 4)
// =============================================================================

/// A minimal but valid `Node`: every `#[serde(skip_serializing_if)]` optional
/// field is `None`/empty. `id` is `"function:<name>"` so it always contains
/// the load-bearing colon.
#[cfg(feature = "kv-mem")]
fn node(name: &str, file_path: &str) -> Node {
    Node {
        id: format!("function:{name}"),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: format!("{file_path}::{name}"),
        file_path: file_path.to_string(),
        language: "rust".to_string(),
        start_line: 1,
        end_line: 2,
        start_column: 0,
        end_column: 1,
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

/// A maximal `Node`: every optional field `Some`/non-empty, decorators and
/// type parameters populated, a non-default kind.
#[cfg(feature = "kv-mem")]
fn maximal_node(name: &str, file_path: &str) -> Node {
    Node {
        id: format!("method:{name}"),
        kind: NodeKind::Method,
        name: name.to_string(),
        qualified_name: format!("{file_path}::Widget.{name}"),
        file_path: file_path.to_string(),
        language: "rust".to_string(),
        start_line: 10,
        end_line: 20,
        start_column: 2,
        end_column: 3,
        docstring: Some("does a thing".to_string()),
        signature: Some(format!("fn {name}(&self) -> bool")),
        visibility: Some(Visibility::Public),
        is_exported: Some(true),
        is_async: Some(true),
        is_static: Some(false),
        is_abstract: Some(false),
        decorators: vec!["#[inline]".to_string()],
        type_parameters: vec!["T".to_string()],
        return_type: Some("bool".to_string()),
        updated_at: 42,
    }
}

/// A fresh in-memory, schema-applied store — the common setup for every node
/// test below.
#[cfg(feature = "kv-mem")]
async fn fresh_store() -> SurrealStore {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn minimal_and_maximal_node_round_trip_exactly() {
    let store = fresh_store().await;

    let min = node("min", "src/a.rs");
    let max = maximal_node("calculateTotal", "src/b.rs");
    store
        .insert_nodes(&[min.clone(), max.clone()])
        .await
        .unwrap();

    assert_eq!(store.get_node(&min.id).await.unwrap(), Some(min));
    assert_eq!(store.get_node(&max.id).await.unwrap(), Some(max));
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_node_unknown_id_is_none_not_error() {
    let store = fresh_store().await;
    assert_eq!(store.get_node("function:nope").await.unwrap(), None);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn insert_nodes_upsert_replaces_same_id() {
    let store = fresh_store().await;

    let v1 = node("x", "src/a.rs");
    let mut v2 = node("x", "src/a.rs");
    v2.docstring = Some("v2".to_string());
    v2.updated_at = 999;

    store.insert_nodes(&[v1]).await.unwrap();
    store.insert_nodes(&[v2.clone()]).await.unwrap();

    assert_eq!(
        store.get_node("function:x").await.unwrap(),
        Some(v2),
        "newer fields must win"
    );

    let by_file = store.get_nodes_by_file("src/a.rs").await.unwrap();
    assert_eq!(
        by_file.len(),
        1,
        "re-insert of the same id must replace, not duplicate"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_batch_keeps_only_found_ids() {
    let store = fresh_store().await;

    let a = node("a", "src/a.rs");
    let b = node("b", "src/a.rs");
    store.insert_nodes(&[a.clone(), b.clone()]).await.unwrap();

    let ids = vec![
        "function:a".to_string(),
        "function:missing".to_string(),
        "function:b".to_string(),
    ];
    let found = store.get_nodes(&ids).await.unwrap();

    assert_eq!(
        found.len(),
        2,
        "unknown id must be absent, not an error entry"
    );
    assert_eq!(found.get("function:a"), Some(&a));
    assert_eq!(found.get("function:b"), Some(&b));
    assert!(!found.contains_key("function:missing"));
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_by_file_returns_only_that_file() {
    let store = fresh_store().await;

    let a = node("a", "src/a.rs");
    let b = node("b", "src/b.rs");
    store.insert_nodes(&[a.clone(), b.clone()]).await.unwrap();

    assert_eq!(store.get_nodes_by_file("src/a.rs").await.unwrap(), vec![a]);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_by_kind_filters_exact_kind() {
    let store = fresh_store().await;

    let f = node("f", "src/a.rs");
    let mut c = node("c", "src/a.rs");
    c.id = "class:c".to_string();
    c.kind = NodeKind::Class;
    store.insert_nodes(&[f.clone(), c.clone()]).await.unwrap();

    assert_eq!(
        store.get_nodes_by_kind(NodeKind::Class).await.unwrap(),
        vec![c]
    );
    assert_eq!(
        store.get_nodes_by_kind(NodeKind::Function).await.unwrap(),
        vec![f]
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_by_name_is_case_sensitive_exact() {
    let store = fresh_store().await;

    let lower = node("calculateTotal", "src/a.rs");
    let mut upper = node("CALCULATETOTAL", "src/b.rs");
    upper.id = "function:CALCULATETOTAL".to_string();
    store
        .insert_nodes(&[lower.clone(), upper.clone()])
        .await
        .unwrap();

    assert_eq!(
        store.get_nodes_by_name("calculateTotal").await.unwrap(),
        vec![lower]
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_by_name_ci_matches_case_insensitively() {
    let store = fresh_store().await;

    let lower = node("calculateTotal", "src/a.rs");
    let mut upper = node("CALCULATETOTAL", "src/b.rs");
    upper.id = "function:CALCULATETOTAL".to_string();
    store
        .insert_nodes(&[lower.clone(), upper.clone()])
        .await
        .unwrap();

    let mut ci = store.get_nodes_by_name_ci("calculatetotal").await.unwrap();
    ci.sort_by(|a, b| a.id.cmp(&b.id));
    let mut expected = vec![lower, upper];
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(ci, expected);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_by_name_prefix_respects_boundary_and_limit() {
    let store = fresh_store().await;

    // "fop" is NOT a prefix match for "foo" — a naive successor-string hack
    // (incrementing the last byte) is exactly the kind of bug this pins.
    let foo = node("foo", "src/a.rs");
    let foo_bar = node("fooBar", "src/a.rs");
    let fop = node("fop", "src/a.rs");
    store
        .insert_nodes(&[foo.clone(), foo_bar.clone(), fop.clone()])
        .await
        .unwrap();

    let mut prefixed = store.get_nodes_by_name_prefix("foo", 10).await.unwrap();
    prefixed.sort_by(|a, b| a.id.cmp(&b.id));
    let mut expected = vec![foo, foo_bar];
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        prefixed, expected,
        "fop must be excluded by the prefix boundary"
    );

    let limited = store.get_nodes_by_name_prefix("foo", 1).await.unwrap();
    assert_eq!(limited.len(), 1, "limit must be respected");
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_by_qualified_name_can_return_multiple_overloads() {
    let store = fresh_store().await;

    let mut a = node("run", "src/a.rs");
    a.qualified_name = "src/a.rs::run".to_string();
    let mut b = node("run", "src/b.rs");
    b.id = "function:run2".to_string();
    b.qualified_name = "src/a.rs::run".to_string();
    store.insert_nodes(&[a.clone(), b.clone()]).await.unwrap();

    let mut found = store
        .get_nodes_by_qualified_name("src/a.rs::run")
        .await
        .unwrap();
    found.sort_by(|x, y| x.id.cmp(&y.id));
    let mut expected = vec![a, b];
    expected.sort_by(|x, y| x.id.cmp(&y.id));
    assert_eq!(found, expected);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn count_nodes_matching_name_in_files_counts_distinct_files_not_nodes() {
    let store = fresh_store().await;

    let a1 = node("helper", "src/a.rs");
    let mut a2 = node("helper", "src/a.rs");
    a2.id = "function:helper2".to_string(); // same file, same name: still one file
    let mut b1 = node("helper", "src/b.rs");
    b1.id = "function:helper3".to_string();
    store.insert_nodes(&[a1, a2, b1]).await.unwrap();

    let count = store
        .count_nodes_matching_name_in_files("helper")
        .await
        .unwrap();
    assert_eq!(
        count, 2,
        "two distinct files contain 'helper', despite three matching nodes"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn insert_nodes_chunks_over_500() {
    let store = fresh_store().await;

    let nodes: Vec<Node> = (0..1200)
        .map(|i| node(&format!("n{i}"), "src/big.rs"))
        .collect();
    store.insert_nodes(&nodes).await.unwrap();

    let by_kind = store.get_nodes_by_kind(NodeKind::Function).await.unwrap();
    assert_eq!(
        by_kind.len(),
        1200,
        "all 1200 nodes across 3 chunks must be present"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn get_nodes_empty_ids_short_circuits_to_empty_map() {
    let store = fresh_store().await;
    let found = store.get_nodes(&[]).await.unwrap();
    assert!(
        found.is_empty(),
        "empty input must yield an empty map (short-circuit, no query)"
    );
}

// =============================================================================
// Edge operations + file projections (Task 5)
// =============================================================================

/// A minimal `Edge`: `kind` given, every optional field `None`.
#[cfg(feature = "kv-mem")]
fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: None,
        line: None,
        column: None,
        provenance: None,
    }
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn insert_edges_byte_identical_dedups_same_call_and_across_calls() {
    let store = fresh_store().await;
    store
        .insert_nodes(&[node("a", "src/a.rs"), node("b", "src/a.rs")])
        .await
        .unwrap();

    let e = Edge {
        line: Some(10),
        column: Some(2),
        ..edge("function:a", "function:b", EdgeKind::Calls)
    };

    // Same call: two byte-identical edges collapse to one insert.
    let inserted = store.insert_edges(&[e.clone(), e.clone()]).await.unwrap();
    assert_eq!(
        inserted, 1,
        "byte-identical edges in one call must dedup to 1"
    );

    // Across calls: re-submitting the same edge inserts zero more.
    let inserted_again = store.insert_edges(std::slice::from_ref(&e)).await.unwrap();
    assert_eq!(
        inserted_again, 0,
        "re-submitting the same edge must insert 0"
    );

    let out = store.outgoing("function:a", &[], None).await.unwrap();
    assert_eq!(out.len(), 1, "exactly one edge must persist");
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn insert_edges_metadata_difference_does_not_break_dedup() {
    let store = fresh_store().await;
    store
        .insert_nodes(&[node("a", "src/a.rs"), node("b", "src/a.rs")])
        .await
        .unwrap();

    let base = Edge {
        line: Some(5),
        column: Some(1),
        ..edge("function:a", "function:b", EdgeKind::References)
    };
    let e1 = Edge {
        metadata: Some(serde_json::json!({ "resolvedBy": "exact-match" })),
        ..base.clone()
    };
    let e2 = Edge {
        metadata: Some(serde_json::json!({ "resolvedBy": "import" })),
        ..base
    };

    let inserted = store.insert_edges(&[e1, e2]).await.unwrap();
    assert_eq!(
        inserted, 1,
        "metadata is not part of the identity key — still dedups"
    );

    let out = store.outgoing("function:a", &[], None).await.unwrap();
    assert_eq!(out.len(), 1);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn insert_edges_distinct_line_keeps_both() {
    let store = fresh_store().await;
    store
        .insert_nodes(&[node("a", "src/a.rs"), node("b", "src/a.rs")])
        .await
        .unwrap();

    let e1 = Edge {
        line: Some(1),
        column: Some(1),
        ..edge("function:a", "function:b", EdgeKind::Calls)
    };
    let e2 = Edge {
        line: Some(2),
        column: Some(1),
        ..edge("function:a", "function:b", EdgeKind::Calls)
    };

    let inserted = store.insert_edges(&[e1, e2]).await.unwrap();
    assert_eq!(inserted, 2, "distinct call sites are not duplicates");
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn insert_edges_no_line_col_dedups_via_null_fold() {
    let store = fresh_store().await;
    store
        .insert_nodes(&[node("a", "src/a.rs"), node("b", "src/a.rs")])
        .await
        .unwrap();

    let e = edge("function:a", "function:b", EdgeKind::Imports);
    let inserted = store.insert_edges(&[e.clone(), e]).await.unwrap();
    assert_eq!(
        inserted, 1,
        "two positionless edges between the same endpoints must fold to one"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn insert_edges_skips_missing_endpoints_but_keeps_valid_ones() {
    let store = fresh_store().await;
    store
        .insert_nodes(&[
            node("source", "src/a.rs"),
            node("target", "src/a.rs"),
            node("other", "src/a.rs"),
        ])
        .await
        .unwrap();

    let inserted = store
        .insert_edges(&[
            edge("function:source", "function:target", EdgeKind::Calls),
            edge(
                "function:source",
                "function:missing-target",
                EdgeKind::Calls,
            ),
            edge(
                "function:missing-source",
                "function:other",
                EdgeKind::References,
            ),
        ])
        .await
        .unwrap();
    assert_eq!(inserted, 1, "only the valid edge must be counted");

    let out = store.outgoing("function:source", &[], None).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].edge.target, "function:target");
    assert_eq!(out[0].node.id, "function:target");
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn outgoing_incoming_round_trip_kinds_and_provenance() {
    let store = fresh_store().await;
    let a = node("a", "src/a.rs");
    let b = node("b", "src/a.rs");
    let c = node("c", "src/a.rs");
    store
        .insert_nodes(&[a.clone(), b.clone(), c.clone()])
        .await
        .unwrap();

    let calls_ab = Edge {
        line: Some(1),
        column: Some(2),
        provenance: Some(Provenance::TreeSitter),
        ..edge(&a.id, &b.id, EdgeKind::Calls)
    };
    let refs_ab = Edge {
        provenance: Some(Provenance::Heuristic),
        metadata: Some(serde_json::json!({ "synthesizedBy": "callback" })),
        ..edge(&a.id, &b.id, EdgeKind::References)
    };
    let imports_ac = edge(&a.id, &c.id, EdgeKind::Imports);

    let inserted = store
        .insert_edges(&[calls_ab.clone(), refs_ab.clone(), imports_ac.clone()])
        .await
        .unwrap();
    assert_eq!(inserted, 3);

    // empty kinds = all kinds.
    let mut all_out = store.outgoing(&a.id, &[], None).await.unwrap();
    assert_eq!(all_out.len(), 3);
    all_out.sort_by(|x, y| {
        x.edge
            .target
            .cmp(&y.edge.target)
            .then(x.edge.kind.as_str().cmp(y.edge.kind.as_str()))
    });

    // subset kinds filter.
    let calls_only = store
        .outgoing(&a.id, &[EdgeKind::Calls], None)
        .await
        .unwrap();
    assert_eq!(calls_only.len(), 1);
    assert_eq!(calls_only[0].edge, calls_ab);
    assert_eq!(
        calls_only[0].node, b,
        "outgoing neighbor node is the TARGET"
    );

    // provenance filter, outgoing only.
    let heuristic_only = store
        .outgoing(&a.id, &[], Some(Provenance::Heuristic))
        .await
        .unwrap();
    assert_eq!(heuristic_only.len(), 1);
    assert_eq!(heuristic_only[0].edge, refs_ab);

    // incoming has no provenance param; b receives both calls_ab and refs_ab.
    let mut into_b = store.incoming(&b.id, &[]).await.unwrap();
    assert_eq!(into_b.len(), 2);
    into_b.sort_by(|x, y| x.edge.kind.as_str().cmp(y.edge.kind.as_str()));
    assert!(
        into_b.iter().all(|n| n.node == a),
        "incoming neighbor node is the SOURCE"
    );
    assert!(into_b.iter().any(|n| n.edge == calls_ab));
    assert!(into_b.iter().any(|n| n.edge == refs_ab));

    let into_c = store.incoming(&c.id, &[EdgeKind::Imports]).await.unwrap();
    assert_eq!(into_c.len(), 1);
    assert_eq!(into_c[0].edge, imports_ac);
    assert_eq!(into_c[0].node, a);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn outgoing_batch_and_incoming_batch_match_single_id_calls() {
    let store = fresh_store().await;
    let a = node("a", "src/a.rs");
    let b = node("b", "src/a.rs");
    let c = node("c", "src/a.rs");
    store
        .insert_nodes(&[a.clone(), b.clone(), c.clone()])
        .await
        .unwrap();

    let ab = edge(&a.id, &b.id, EdgeKind::Calls);
    let bc = edge(&b.id, &c.id, EdgeKind::Calls);
    store.insert_edges(&[ab.clone(), bc.clone()]).await.unwrap();

    let batch = store
        .outgoing_batch(&[a.id.clone(), b.id.clone()], &[])
        .await
        .unwrap();
    assert_eq!(batch.len(), 2, "one entry per queried id with results");
    assert_eq!(
        batch.get(&a.id).unwrap(),
        &store.outgoing(&a.id, &[], None).await.unwrap()
    );
    assert_eq!(
        batch.get(&b.id).unwrap(),
        &store.outgoing(&b.id, &[], None).await.unwrap()
    );

    let in_batch = store
        .incoming_batch(&[b.id.clone(), c.id.clone()], &[])
        .await
        .unwrap();
    assert_eq!(in_batch.len(), 2);
    assert_eq!(
        in_batch.get(&b.id).unwrap(),
        &store.incoming(&b.id, &[]).await.unwrap()
    );
    assert_eq!(
        in_batch.get(&c.id).unwrap(),
        &store.incoming(&c.id, &[]).await.unwrap()
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn edges_between_only_returns_edges_with_both_endpoints_in_set() {
    let store = fresh_store().await;
    let a = node("a", "src/a.rs");
    let b = node("b", "src/a.rs");
    let c = node("c", "src/a.rs");
    let d = node("d", "src/a.rs");
    store
        .insert_nodes(&[a.clone(), b.clone(), c.clone(), d.clone()])
        .await
        .unwrap();

    let ab = edge(&a.id, &b.id, EdgeKind::Calls);
    let bc = edge(&b.id, &c.id, EdgeKind::Calls);
    let cd = edge(&c.id, &d.id, EdgeKind::Calls);
    let ad = edge(&a.id, &d.id, EdgeKind::References);
    store
        .insert_edges(&[ab.clone(), bc.clone(), cd.clone(), ad.clone()])
        .await
        .unwrap();

    let mut between = store
        .edges_between(&[a.id.clone(), b.id.clone(), c.id.clone()], &[])
        .await
        .unwrap();
    between.sort_by(|x, y| x.source.cmp(&y.source));
    assert_eq!(
        between,
        vec![ab, bc],
        "only edges fully inside {{a,b,c}} qualify"
    );
}

/// Shared 2-file fixture for the cross-file/dependency-projection tests:
/// f1 has a container `f1Parent` containing method `f1fn`, plus a same-file
/// `f1Other` function that calls `f1fn` (same-file, must be excluded from
/// cross-file results even though its kind isn't `contains`). f2 has two
/// functions that each reach into f1: `f2fn1` --calls--> `f1fn` and `f2fn2`
/// --references--> `f1fn` (both cross-file).
#[cfg(feature = "kv-mem")]
async fn cross_file_fixture(store: &SurrealStore) -> (Node, Node) {
    let mut f1_parent = node("f1Parent", "src/f1.rs");
    f1_parent.id = "class:f1Parent".to_string();
    f1_parent.kind = NodeKind::Class;
    let mut f1_fn = node("f1fn", "src/f1.rs");
    f1_fn.id = "method:f1fn".to_string();
    f1_fn.kind = NodeKind::Method;
    let mut f1_other = node("f1Other", "src/f1.rs");
    f1_other.id = "function:f1Other".to_string();
    let mut f2_fn1 = node("f2fn1", "src/f2.rs");
    f2_fn1.id = "function:f2fn1".to_string();
    let mut f2_fn2 = node("f2fn2", "src/f2.rs");
    f2_fn2.id = "function:f2fn2".to_string();

    store
        .insert_nodes(&[
            f1_parent.clone(),
            f1_fn.clone(),
            f1_other.clone(),
            f2_fn1.clone(),
            f2_fn2.clone(),
        ])
        .await
        .unwrap();

    store
        .insert_edges(&[
            edge(&f1_parent.id, &f1_fn.id, EdgeKind::Contains), // same-file, contains
            edge(&f1_other.id, &f1_fn.id, EdgeKind::Calls),     // same-file, non-contains
            edge(&f2_fn1.id, &f1_fn.id, EdgeKind::Calls),       // cross-file
            edge(&f2_fn2.id, &f1_fn.id, EdgeKind::References),  // cross-file
        ])
        .await
        .unwrap();

    (f1_fn, f2_fn1)
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn cross_file_incoming_with_target_excludes_same_file_and_contains() {
    let store = fresh_store().await;
    let (f1_fn, _) = cross_file_fixture(&store).await;

    let mut result = store
        .cross_file_incoming_with_target("src/f1.rs")
        .await
        .unwrap();
    assert_eq!(
        result.len(),
        2,
        "only the two cross-file, non-contains edges qualify"
    );
    result.sort_by(|x, y| x.0.source.cmp(&y.0.source));

    assert_eq!(result[0].0.source, "function:f2fn1");
    assert_eq!(result[0].0.kind, EdgeKind::Calls);
    assert_eq!(result[0].1, f1_fn.name);
    assert_eq!(result[0].2, f1_fn.kind);

    assert_eq!(result[1].0.source, "function:f2fn2");
    assert_eq!(result[1].0.kind, EdgeKind::References);
    assert_eq!(result[1].1, f1_fn.name);
    assert_eq!(result[1].2, f1_fn.kind);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn dependent_and_dependency_file_paths_are_sorted_deduped_and_never_self() {
    let store = fresh_store().await;
    cross_file_fixture(&store).await;

    let dependents = store.dependent_file_paths("src/f1.rs").await.unwrap();
    assert_eq!(
        dependents,
        vec!["src/f2.rs".to_string()],
        "two edges from src/f2.rs collapse to one deduped path; src/f1.rs itself is excluded"
    );

    let dependencies = store.dependency_file_paths("src/f2.rs").await.unwrap();
    assert_eq!(dependencies, vec!["src/f1.rs".to_string()]);

    // A file with no cross-file relationships yields an empty (not missing) list.
    assert!(
        store
            .dependent_file_paths("src/nope.rs")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .dependency_file_paths("src/f1.rs")
            .await
            .unwrap()
            .is_empty()
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn relate_smoke_over_every_edge_kind() {
    let store = fresh_store().await;
    let a = node("smokeA", "src/a.rs");
    let b = node("smokeB", "src/a.rs");
    store.insert_nodes(&[a.clone(), b.clone()]).await.unwrap();

    for kind in EdgeKind::ALL {
        let inserted = store
            .insert_edges(&[edge(&a.id, &b.id, kind)])
            .await
            .unwrap();
        assert_eq!(inserted, 1, "RELATE into the {kind:?} table must succeed");

        let out = store.outgoing(&a.id, &[kind], None).await.unwrap();
        assert_eq!(out.len(), 1, "{kind:?} must round-trip via outgoing");
        assert_eq!(out[0].edge.kind, kind);
        assert_eq!(out[0].node.id, b.id);
    }
}

// =============================================================================
// File records + single-file re-index protocol (Task 6)
// =============================================================================

/// A `FileRecord` with the given path, hash, timestamp, node count and
/// language; `size`/`modified_at` are fixed and `errors` empty.
#[cfg(feature = "kv-mem")]
fn file_record(
    path: &str,
    hash: &str,
    language: &str,
    indexed_at: i64,
    node_count: u32,
) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: hash.to_string(),
        language: language.to_string(),
        size: 128,
        modified_at: indexed_at,
        indexed_at,
        node_count,
        errors: vec![],
    }
}

/// Raw row count of one edge table via the `db()` handle. The typed adjacency
/// reads (`outgoing`/`incoming`) DROP any edge whose neighbor node is missing
/// (`attach_neighbors`), so they cannot distinguish "edge cascaded away" from
/// "edge left dangling" — cascade regression guards must count the table raw.
#[cfg(feature = "kv-mem")]
async fn edge_table_count(store: &SurrealStore, table: &str) -> i64 {
    let sql = format!("SELECT count() FROM {table} GROUP ALL");
    let mut resp = store.db().query(sql).await.unwrap();
    let rows: Vec<serde_json::Value> = resp.take(0).unwrap();
    rows.first().and_then(|r| r["count"].as_i64()).unwrap_or(0)
}

/// Every `unresolved_ref` row, projected to its fields (bypasses the not-yet-
/// built Task 7 read API via the raw `db()` handle).
#[cfg(feature = "kv-mem")]
async fn read_unresolved(store: &SurrealStore) -> Vec<serde_json::Value> {
    let mut resp = store
        .db()
        .query(
            "SELECT fromNodeId, referenceName, referenceKind, line, column, \
             filePath, language, status, nameTail FROM unresolved_ref",
        )
        .await
        .unwrap();
    resp.take(0).unwrap()
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn upsert_file_round_trips_and_replaces_same_path() {
    let store = fresh_store().await;

    // A non-empty `errors` array exercises the FLEXIBLE `array<object>` column
    // as part of the whole-struct round trip.
    let mut f = file_record("src/a.rs", "hash-v1", "rust", 1000, 3);
    f.errors = vec![serde_json::json!({ "line": 4, "message": "unterminated string" })];
    store.upsert_file(&f).await.unwrap();
    assert_eq!(
        store.get_file("src/a.rs").await.unwrap(),
        Some(f),
        "whole-struct round trip, including the FLEXIBLE errors column"
    );

    // Upsert the same path with a newer hash/timestamp → replace, not duplicate.
    let f2 = file_record("src/a.rs", "hash-v2", "rust", 2000, 5);
    store.upsert_file(&f2).await.unwrap();
    assert_eq!(
        store.get_file("src/a.rs").await.unwrap(),
        Some(f2),
        "newer content must win"
    );
    assert_eq!(
        store.all_files().await.unwrap().len(),
        1,
        "same-path upsert replaces, never duplicates"
    );

    assert_eq!(store.get_file("src/missing.rs").await.unwrap(), None);
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn all_files_last_indexed_and_distinct_languages() {
    let store = fresh_store().await;

    // Empty store: no rows, no last-indexed, no languages.
    assert!(store.all_files().await.unwrap().is_empty());
    assert_eq!(store.last_indexed_at().await.unwrap(), None);
    assert!(store.distinct_file_languages().await.unwrap().is_empty());

    store
        .upsert_file(&file_record("src/a.rs", "h1", "rust", 1000, 1))
        .await
        .unwrap();
    store
        .upsert_file(&file_record("src/b.ts", "h2", "typescript", 3000, 1))
        .await
        .unwrap();
    store
        .upsert_file(&file_record("src/c.rs", "h3", "rust", 2000, 1))
        .await
        .unwrap();

    assert_eq!(store.all_files().await.unwrap().len(), 3);
    assert_eq!(
        store.last_indexed_at().await.unwrap(),
        Some(3000),
        "the max indexedAt across files"
    );

    let langs = store.distinct_file_languages().await.unwrap();
    let expected: std::collections::BTreeSet<String> =
        ["rust".to_string(), "typescript".to_string()].into();
    assert_eq!(langs, expected, "distinct, sorted language set");
}

/// Builds the delete-cascade fixture: file `src/f1.rs` with two nodes
/// (`class:P` contains `method:M`), a same-file internal `contains` edge, a
/// cross-file `calls` edge from `src/f2.rs`'s `function:C` into `method:M`,
/// and an `unresolved_ref` rooted at `method:M`. Returns nothing; the caller
/// re-derives ids by their literal strings.
#[cfg(feature = "kv-mem")]
async fn delete_cascade_fixture(store: &SurrealStore) {
    let mut parent = node("P", "src/f1.rs");
    parent.id = "class:P".to_string();
    parent.kind = NodeKind::Class;
    let mut method = node("M", "src/f1.rs");
    method.id = "method:M".to_string();
    method.kind = NodeKind::Method;
    let mut caller = node("C", "src/f2.rs");
    caller.id = "function:C".to_string();

    store
        .insert_nodes(&[parent.clone(), method.clone(), caller.clone()])
        .await
        .unwrap();
    store
        .insert_edges(&[
            edge("class:P", "method:M", EdgeKind::Contains), // internal
            edge("function:C", "method:M", EdgeKind::Calls), // cross-file incoming
        ])
        .await
        .unwrap();
    store
        .db()
        .query(
            "CREATE unresolved_ref CONTENT { fromNodeId: 'method:M', \
             referenceName: 'Foo.bar', referenceKind: 'call', candidates: [], \
             filePath: 'src/f1.rs', language: 'rust', status: 'pending', nameTail: 'bar' }",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
    store
        .upsert_file(&file_record("src/f1.rs", "h", "rust", 1, 2))
        .await
        .unwrap();
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn delete_file_cascades_nodes_edges_and_unresolved() {
    let store = fresh_store().await;
    delete_cascade_fixture(&store).await;

    // Baseline raw counts: prove the fixture actually wrote both edge rows, so
    // the zero-after-delete assertions below cannot pass vacuously.
    assert_eq!(edge_table_count(&store, "calls").await, 1);
    assert_eq!(edge_table_count(&store, "contains").await, 1);

    store.delete_file("src/f1.rs").await.unwrap();

    // f1's nodes are gone; f2's node survives.
    assert!(store.get_node("class:P").await.unwrap().is_none());
    assert!(store.get_node("method:M").await.unwrap().is_none());
    assert!(store.get_node("function:C").await.unwrap().is_some());

    // RAW edge-table counts — the cascade regression guard. `outgoing`/
    // `incoming` drop edges whose neighbor node is missing, so only a raw
    // count can distinguish "SurrealDB cascaded the RELATE rows" (the probed
    // 3.2 behavior delete_file RELIES on) from "rows left dangling". If a
    // SurrealDB upgrade ever stops cascading, these fail loudly.
    assert_eq!(
        edge_table_count(&store, "calls").await,
        0,
        "cross-file calls edge row must be cascaded away with its target node"
    );
    assert_eq!(
        edge_table_count(&store, "contains").await,
        0,
        "internal contains edge row must be cascaded away with its endpoints"
    );

    // And the typed view over the surviving f2 node agrees.
    assert!(
        store
            .outgoing("function:C", &[], None)
            .await
            .unwrap()
            .is_empty(),
        "the cross-file calls edge must be cascaded away with its target"
    );

    // f1's unresolved ref is gone; the file row is gone.
    assert!(read_unresolved(&store).await.is_empty());
    assert!(store.get_file("src/f1.rs").await.unwrap().is_none());

    // Deleting a missing path is a no-op, not an error.
    store.delete_file("src/never.rs").await.unwrap();
}

/// Establishes an initial graph where `src/f2.rs`'s `function:C` reaches into
/// `src/f1.rs`'s `method:M` (id `method:M@10`, line 10) via a `calls` edge with
/// the given `metadata` (plus a fixed line/column/provenance, so tests can
/// assert their preservation through the re-attach path).
#[cfg(feature = "kv-mem")]
async fn reindex_fixture(store: &SurrealStore, edge_metadata: Option<serde_json::Value>) {
    let mut method = node("M", "src/f1.rs");
    method.id = "method:M@10".to_string();
    method.kind = NodeKind::Method;
    method.start_line = 10;
    let mut caller = node("C", "src/f2.rs");
    caller.id = "function:C".to_string();

    store
        .insert_nodes(&[method.clone(), caller.clone()])
        .await
        .unwrap();
    let cross = Edge {
        line: Some(7),
        column: Some(3),
        provenance: Some(Provenance::TreeSitter),
        metadata: edge_metadata,
        ..edge("function:C", "method:M@10", EdgeKind::Calls)
    };
    store.insert_edges(&[cross]).await.unwrap();
    store
        .upsert_file(&file_record("src/f1.rs", "old", "rust", 1, 1))
        .await
        .unwrap();
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn replace_file_extraction_reattaches_cross_file_incoming_to_new_id() {
    let store = fresh_store().await;
    let metadata = serde_json::json!({ "note": "kept-through-reattach" });
    reindex_fixture(&store, Some(metadata.clone())).await;

    // Re-extract f1: the same method M, but shifted to line 20 → a NEW node id.
    let mut method_v2 = node("M", "src/f1.rs");
    method_v2.id = "method:M@20".to_string();
    method_v2.kind = NodeKind::Method;
    method_v2.start_line = 20;

    let stats = store
        .replace_file_extraction(
            "src/f1.rs",
            &[method_v2],
            &[],
            &[],
            &file_record("src/f1.rs", "new", "rust", 2, 1),
        )
        .await
        .unwrap();

    assert_eq!(stats.incoming_reattached, 1);
    assert_eq!(stats.incoming_resurrected, 0);
    assert_eq!(stats.incoming_dropped, 0);
    assert_eq!(stats.nodes_inserted, 1);

    // Raw-count guard: exactly ONE row in the calls table — the old edge (to
    // method:M@10) is gone with its cascaded target, the re-attached edge (to
    // method:M@20) is present. A raw count is required because the typed reads
    // below drop dangling edges silently (see edge_table_count's docs).
    assert_eq!(
        edge_table_count(&store, "calls").await,
        1,
        "old edge cascaded away, exactly the re-attached edge remains"
    );

    // The cross-file edge now lands on the NEW node id.
    assert!(store.get_node("method:M@10").await.unwrap().is_none());
    let into_new = store.incoming("method:M@20", &[]).await.unwrap();
    assert_eq!(into_new.len(), 1, "edge re-attached to the new node id");
    assert_eq!(into_new[0].node.id, "function:C");

    // The re-attach rewrites ONLY the target: every other edge field —
    // kind, line, column, provenance, metadata — is the original's.
    let reattached = &into_new[0].edge;
    assert_eq!(reattached.kind, EdgeKind::Calls);
    assert_eq!(reattached.line, Some(7), "original line preserved");
    assert_eq!(reattached.column, Some(3), "original column preserved");
    assert_eq!(
        reattached.provenance,
        Some(Provenance::TreeSitter),
        "original provenance preserved"
    );
    assert_eq!(
        reattached.metadata,
        Some(metadata),
        "original metadata preserved"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn replace_file_extraction_resurrects_unmatched_stamped_edge() {
    let store = fresh_store().await;
    reindex_fixture(
        &store,
        Some(serde_json::json!({ "refName": "Helper.M", "refKind": "call" })),
    )
    .await;

    // Re-extract f1 WITHOUT method M → the incoming edge cannot re-attach.
    let mut other = node("Other", "src/f1.rs");
    other.id = "function:Other".to_string();

    let stats = store
        .replace_file_extraction(
            "src/f1.rs",
            &[other],
            &[],
            &[],
            &file_record("src/f1.rs", "new", "rust", 2, 1),
        )
        .await
        .unwrap();

    assert_eq!(stats.incoming_resurrected, 1);
    assert_eq!(stats.incoming_reattached, 0);
    assert_eq!(stats.incoming_dropped, 0);

    let refs = read_unresolved(&store).await;
    assert_eq!(refs.len(), 1, "one resurrected unresolved ref");
    let r = &refs[0];
    assert_eq!(r["fromNodeId"], "function:C");
    assert_eq!(r["referenceName"], "Helper.M");
    assert_eq!(r["referenceKind"], "call");
    assert_eq!(r["status"], "pending");
    assert_eq!(r["nameTail"], "M", "last '.'-separated segment");
    assert_eq!(r["line"], 7);
    assert_eq!(
        r["filePath"], "src/f2.rs",
        "denormalized from the source node's file"
    );
    assert_eq!(r["language"], "rust");
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn replace_file_extraction_drops_unmatched_unstamped_edge() {
    let store = fresh_store().await;
    // Edge carries metadata but NOT the refName/refKind stamp.
    reindex_fixture(
        &store,
        Some(serde_json::json!({ "synthesizedBy": "callback" })),
    )
    .await;

    let mut other = node("Other", "src/f1.rs");
    other.id = "function:Other".to_string();

    let stats = store
        .replace_file_extraction(
            "src/f1.rs",
            &[other],
            &[],
            &[],
            &file_record("src/f1.rs", "new", "rust", 2, 1),
        )
        .await
        .unwrap();

    assert_eq!(stats.incoming_dropped, 1);
    assert_eq!(stats.incoming_resurrected, 0);
    assert_eq!(stats.incoming_reattached, 0);
    assert!(
        read_unresolved(&store).await.is_empty(),
        "an unstamped unmatched edge is dropped, not resurrected"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn replace_file_extraction_writes_file_row_last_with_node_count() {
    let store = fresh_store().await;
    reindex_fixture(&store, None).await;

    let mut method_v2 = node("M", "src/f1.rs");
    method_v2.id = "method:M@20".to_string();
    method_v2.kind = NodeKind::Method;
    method_v2.start_line = 20;
    let mut helper = node("H", "src/f1.rs");
    helper.id = "function:H".to_string();

    let file = file_record("src/f1.rs", "new-hash", "rust", 5000, 2);
    let stats = store
        .replace_file_extraction("src/f1.rs", &[method_v2, helper], &[], &[], &file)
        .await
        .unwrap();
    assert_eq!(stats.nodes_inserted, 2);

    // The file row is present after the protocol, carrying the new metadata.
    let stored = store.get_file("src/f1.rs").await.unwrap().unwrap();
    assert_eq!(stored.content_hash, "new-hash");
    assert_eq!(stored.node_count, 2);
    assert_eq!(stored.indexed_at, 5000);
    assert_eq!(store.last_indexed_at().await.unwrap(), Some(5000));
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn replace_file_extraction_ambiguous_match_picks_earliest_start_line() {
    let store = fresh_store().await;
    reindex_fixture(&store, None).await;

    // Two re-extracted nodes share (name "M", kind Method) at different lines.
    // The re-attach must deterministically pick the EARLIER start line.
    let mut early = node("M", "src/f1.rs");
    early.id = "method:M@15".to_string();
    early.kind = NodeKind::Method;
    early.start_line = 15;
    let mut late = node("M", "src/f1.rs");
    late.id = "method:M@40".to_string();
    late.kind = NodeKind::Method;
    late.start_line = 40;

    // Insert in "late first" order to prove ordering isn't insertion order.
    let stats = store
        .replace_file_extraction(
            "src/f1.rs",
            &[late, early],
            &[],
            &[],
            &file_record("src/f1.rs", "new", "rust", 2, 2),
        )
        .await
        .unwrap();
    assert_eq!(stats.incoming_reattached, 1);

    let into_early = store.incoming("method:M@15", &[]).await.unwrap();
    assert_eq!(into_early.len(), 1, "attached to the earliest-line node");
    assert_eq!(into_early[0].node.id, "function:C");
    assert!(
        store.incoming("method:M@40", &[]).await.unwrap().is_empty(),
        "the later-line node receives nothing"
    );
}

#[cfg(feature = "kv-mem")]
#[tokio::test(flavor = "multi_thread")]
async fn replace_file_extraction_skips_required_field_invalid_nodes_silently() {
    let store = fresh_store().await;

    // Two valid nodes plus one invalid per required field (empty id / name /
    // filePath). The invalid ones are skipped silently — no error — and
    // nodes_inserted counts only the valid ones (per the ReplaceStats doc:
    // "valid, required-field-complete").
    let good_a = node("goodA", "src/f1.rs");
    let good_b = node("goodB", "src/f1.rs");
    let mut no_id = node("noId", "src/f1.rs");
    no_id.id = String::new();
    let mut no_name = node("noName", "src/f1.rs");
    no_name.name = String::new();
    let mut no_path = node("noPath", "src/f1.rs");
    no_path.file_path = String::new();

    let stats = store
        .replace_file_extraction(
            "src/f1.rs",
            &[
                no_id,
                good_a.clone(),
                no_name.clone(),
                good_b.clone(),
                no_path,
            ],
            &[],
            &[],
            &file_record("src/f1.rs", "h", "rust", 1, 2),
        )
        .await
        .unwrap();

    assert_eq!(
        stats.nodes_inserted, 2,
        "only the two required-field-complete nodes count"
    );
    assert_eq!(store.get_node(&good_a.id).await.unwrap(), Some(good_a));
    assert_eq!(store.get_node(&good_b.id).await.unwrap(), Some(good_b));
    assert_eq!(
        store.get_node(&no_name.id).await.unwrap(),
        None,
        "the empty-name node must not have been written"
    );
    assert_eq!(
        store.get_nodes_by_file("src/f1.rs").await.unwrap().len(),
        2,
        "exactly the valid nodes are attributed to the file"
    );
}
