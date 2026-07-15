#![allow(clippy::unwrap_used)]
//! `sync_project` — incremental re-index, against a real store on a tiny tree.

use std::path::Path;

use selene_db::{GraphStore, SurrealStore};
use selene_extract::Indexer;

async fn index(root: &Path) {
    let dir = root.join(".selene");
    std::fs::create_dir_all(&dir).unwrap();
    let store = SurrealStore::open(&dir).await.unwrap();
    store.apply_schema().await.unwrap();
    let indexer = Indexer::new(root.to_path_buf(), store);
    let r = indexer.index_all_deferring_fts(None).await;
    let store = indexer.into_store();
    let (stats, fts) = tokio::join!(
        selene_resolve::resolve_and_persist_in_memory(&store, root, r.unresolved, None),
        store.bulk_load_finish()
    );
    fts.unwrap();
    stats.unwrap();
}

async fn file_count(root: &Path) -> u64 {
    let store = SurrealStore::open(&root.join(".selene")).await.unwrap();
    store.stats().await.unwrap().files
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_reindexes_only_what_changed_and_handles_add_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/a.ts"),
        "export function alpha(){return beta()}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/b.ts"), "export function beta(){return 1}\n").unwrap();
    index(root).await;
    assert_eq!(file_count(root).await, 2);

    // No change → no-op.
    let s = selene_sync::sync_project(root).await.unwrap();
    assert!(s.is_noop(), "unchanged tree must be a no-op: {s:?}");
    assert_eq!(s.unchanged, 2);

    // Add a file + modify one. mtime must advance, so sleep past the resolution.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(
        root.join("src/a.ts"),
        "export function alpha(){return gamma()}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/c.ts"), "export function gamma(){return 2}\n").unwrap();
    let s = selene_sync::sync_project(root).await.unwrap();
    assert_eq!(s.changed, 2, "a.ts modified + c.ts added: {s:?}");
    assert_eq!(file_count(root).await, 3, "c.ts is now indexed");

    // The new symbol is in the graph.
    let store = SurrealStore::open(&root.join(".selene")).await.unwrap();
    assert!(
        !store.get_nodes_by_name("gamma").await.unwrap().is_empty(),
        "gamma is indexed"
    );
    drop(store);

    // Delete a file.
    std::fs::remove_file(root.join("src/b.ts")).unwrap();
    let s = selene_sync::sync_project(root).await.unwrap();
    assert_eq!(s.removed, 1, "b.ts removed: {s:?}");
    assert_eq!(file_count(root).await, 2);
}
