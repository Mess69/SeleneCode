#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `SurrealStore` open/init + schema DDL contract tests (Task 3).
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
//! because `insert_nodes`/`insert_edges` (Tasks 4/5) do not exist yet; once they
//! land it can be rephrased against the typed API.

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
