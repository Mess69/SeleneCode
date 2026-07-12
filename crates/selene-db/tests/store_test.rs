#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `SurrealStore` open/init + schema DDL contract tests (Task 3), plus node
//! CRUD + lookups (Task 4, see the `node_*` tests below).
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
use selene_core::{Node, NodeKind, Visibility};
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
