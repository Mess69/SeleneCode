#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 2 — `QueryManager`: stats, files, project metadata.
//!
//! **Against a real store, every time.** The fixture is indexed and resolved by the
//! production pipeline (`tests/common/mod.rs`); nothing here hand-inserts a node. A test
//! that builds its own graph proves the assertion, not the product.

mod common;

use common::{assert_rig_resolved, index_fixture, write_3_file_fixture};
use selene_db::SurrealStore;
use selene_graph::{QueryManager, normalize_path, tokenize_project_name};

async fn manager() -> (QueryManager<SurrealStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    assert_rig_resolved(&store).await; // the positive control
    (QueryManager::new(store, tmp.path().to_path_buf()), tmp)
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_and_file_count_read_the_real_graph() {
    let (qm, _tmp) = manager().await;

    let stats = qm.stats().await.unwrap();
    assert_eq!(stats.files, 3);
    assert!(stats.nodes > 0 && stats.edges > 0);
    assert!(
        stats.nodes_by_kind.contains_key("function"),
        "the per-kind histogram is populated: {:?}",
        stats.nodes_by_kind
    );

    assert_eq!(
        qm.file_count().await.unwrap(),
        3,
        "file_count drives the explore BUDGET TIERS (Task 8) — a wrong count here silently \
         resizes every explore response"
    );
}

/// `files()` — sorted, with languages, and **non-zero node counts**.
///
/// The node-count assertion is the one that matters: a zero would mean `FileRecord`'s
/// cached count is not populated, and `files()` would have to fan out one query per file
/// (the spike proved it does not).
#[tokio::test(flavor = "multi_thread")]
async fn files_are_sorted_with_languages_and_non_zero_node_counts() {
    let (qm, _tmp) = manager().await;

    let files = qm.files().await.unwrap();
    assert_eq!(
        files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["src/app.ts", "src/crypto.ts", "src/service.ts"],
        "sorted by path — the output is rendered, so the order is observable"
    );
    for f in &files {
        assert_eq!(f.language, "typescript");
        assert!(f.node_count > 0, "{}: zero nodes", f.path);
    }
}

/// The rig's proof-of-resolution, at this layer: `crypto.ts` is imported by `service.ts`,
/// so it HAS a dependent. An empty answer here would mean the graph has no cross-file
/// edges and every later ranking/flow test is vacuous.
#[tokio::test(flavor = "multi_thread")]
async fn file_dependents_and_dependencies_are_non_empty_on_a_resolved_graph() {
    let (qm, _tmp) = manager().await;

    let dependents = qm.file_dependents("src/crypto.ts").await.unwrap();
    assert!(
        dependents.iter().any(|p| p == "src/service.ts"),
        "service.ts imports crypto.ts — an EMPTY answer here means the rig never resolved, \
         and every assertion downstream of it is vacuous: {dependents:?}"
    );

    let deps = qm.file_dependencies("src/service.ts").await.unwrap();
    assert!(
        deps.iter().any(|p| p == "src/crypto.ts"),
        "…and the same edge, read the other way: {deps:?}"
    );

    // The negative is paired with the positive above, so "empty" cannot pass vacuously.
    assert!(
        qm.file_dependents("src/app.ts").await.unwrap().is_empty(),
        "nothing imports app.ts — and we know the query WORKS, because crypto.ts's \
         dependents were found on this same graph"
    );
}

/// Every path-taking method funnels through the normalizer (#426).
#[tokio::test(flavor = "multi_thread")]
async fn path_taking_methods_normalize_the_agents_spellings() {
    let (qm, _tmp) = manager().await;

    let canonical = qm.file_dependents("src/crypto.ts").await.unwrap();
    assert!(!canonical.is_empty());

    for spelling in ["./src/crypto.ts", "src\\crypto.ts", "/src/crypto.ts"] {
        assert_eq!(
            qm.file_dependents(spelling).await.unwrap(),
            canonical,
            "{spelling:?} means the same file — the agent writes paths four ways and means \
             one thing (#426)"
        );
    }
}

/// The not-indexed seam: `Ok(false)`, **never** `Err`.
#[tokio::test(flavor = "multi_thread")]
async fn is_indexed_is_false_on_an_empty_store_and_true_after_indexing() {
    let empty = SurrealStore::in_memory().await.unwrap();
    empty.apply_schema().await.unwrap();
    let qm = QueryManager::new(empty, std::path::PathBuf::from("/nowhere"));

    assert!(
        !qm.is_indexed().await.expect(
            "an un-indexed project is the most common FIRST CONTACT an agent has with this \
             tool. Answering it with an Err is how the tool gets abandoned for the session."
        ),
        "empty store ⇒ not indexed"
    );

    let (qm, _tmp) = manager().await;
    assert!(qm.is_indexed().await.unwrap());
}

/// #720 — the project's own name has no discriminative value in ranking, so it is excluded.
#[tokio::test(flavor = "multi_thread")]
async fn project_name_tokens_come_from_the_root_directory_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("my-cool_App");
    std::fs::create_dir_all(&root).unwrap();
    write_3_file_fixture(&root);
    let store = index_fixture(&root).await;

    let qm = QueryManager::new(store, root);
    assert_eq!(
        qm.project_name_tokens().await.unwrap(),
        vec!["my", "cool", "app"],
        "derived from the ROOT DIR NAME — the one identifier every ecosystem has (spike, \
         Task 1). In a repo called `selene`, every query 'matches' the token `selene`, so \
         ranking must subtract it."
    );
}

#[test]
fn the_normalizer_and_tokenizer_are_public_because_selene_mcp_shares_them() {
    assert_eq!(normalize_path("./a"), "a");
    assert_eq!(tokenize_project_name("SeleneCode"), vec!["selene", "code"]);
}
