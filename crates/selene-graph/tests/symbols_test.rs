#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 3 — symbol resolution, against a real store.
//!
//! The centerpiece is `the_two_lookups_diverge_on_a_qualified_miss`: the asymmetry between
//! `find_all_symbols` and `find_symbol_matches` is **deliberate** (#173 fixed node mode
//! only), and this file is what stops the next reader from "unifying" them and silently
//! changing callers/callees behavior.

mod common;

use common::{index_fixture, write_overload_fixture};
use selene_db::SurrealStore;
use selene_graph::QueryManager;

async fn manager() -> (QueryManager<SurrealStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    write_overload_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    (QueryManager::new(store, tmp.path().to_path_buf()), tmp)
}

/// FTS's relevance cut drops overloads. Enumeration does not — and three classes each
/// declaring `handle` is three answers, not one.
#[tokio::test(flavor = "multi_thread")]
async fn find_all_symbols_returns_every_overload() {
    let (qm, _tmp) = manager().await;

    let hits = qm.find_all_symbols("handle").await.unwrap();
    let files: std::collections::BTreeSet<&str> =
        hits.iter().map(|n| n.file_path.as_str()).collect();

    assert!(
        files.len() >= 3,
        "three classes declare `handle`; an agent shown ONE of them is being lied to. \
         Found: {files:?}"
    );
    assert!(hits.iter().all(|n| n.name == "handle"));
}

/// #764 — grouped by definition site, in **first-seen order** (the order is observable).
#[tokio::test(flavor = "multi_thread")]
async fn group_by_definition_keeps_first_seen_order() {
    let (qm, _tmp) = manager().await;

    let hits = qm.find_all_symbols("handle").await.unwrap();
    let groups = qm.group_by_definition(hits.clone()).await;

    assert_eq!(
        groups.len(),
        hits.len(),
        "each `handle` has its own definition site"
    );
    // First-seen order: the group order mirrors the node order it was built from.
    let group_files: Vec<&str> = groups.iter().map(|g| g.file_path.as_str()).collect();
    let node_files: Vec<&str> = hits.iter().map(|n| n.file_path.as_str()).collect();
    assert_eq!(
        group_files, node_files,
        "IndexMap, not HashMap — grouped output order reaches the agent"
    );
}

/// Node mode: a qualified name resolves to exactly its one owner.
#[tokio::test(flavor = "multi_thread")]
async fn find_symbol_matches_resolves_a_qualified_name_to_one_owner() {
    let (qm, _tmp) = manager().await;

    let hits = qm.find_symbol_matches("Beta.handle").await.unwrap();
    assert_eq!(hits.len(), 1, "exactly Beta's: {hits:?}");
    assert_eq!(hits[0].file_path, "src/beta.ts");

    // …and `::` asks the same question as `.`.
    let colon = qm.find_symbol_matches("Beta::handle").await.unwrap();
    assert_eq!(colon.len(), 1);
    assert_eq!(colon[0].file_path, "src/beta.ts");
}

/// **THE PINNED DIVERGENCE.** Same query, two lookups, two different answers — on purpose.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_lookups_diverge_on_a_qualified_miss() {
    let (qm, _tmp) = manager().await;

    // The query: the agent inverted the qualification. Both TERMS exist (`handle`, `Beta`),
    // so FTS finds candidates — but nothing is actually *named* `handle.Beta`, and no
    // node's qualified name ends with it. That is precisely "a qualified query with no
    // exact match", and it is where the two lookups part company.
    let query = "handle.Beta";

    // Node mode (#173): a qualified miss is NOTHING. The agent asked for `handle.Beta`;
    // handing back `Beta::handle` — or worse, some other class's `handle` — is a wrong
    // answer wearing the right name, and node mode is where the agent READS CODE.
    let node_mode = qm.find_symbol_matches(query).await.unwrap();
    assert!(
        node_mode.is_empty(),
        "#173: node mode does NOT fall back — a confident wrong file is worse than \
         nothing: {node_mode:?}"
    );

    // callers/callees/impact: the SAME query DOES fall back to the best fuzzy hit. This is
    // the TS behavior; #173 did not touch it.
    let all_symbols = qm.find_all_symbols(query).await.unwrap();
    assert!(
        !all_symbols.is_empty(),
        "find_all_symbols falls back to the nearest thing FTS found. Do NOT 'fix' this to \
         match node mode: it changes callers/callees behavior, and the two tools would \
         silently swap failure modes. (This assertion is the pin.)"
    );

    // The positive control: node mode is not simply broken — it finds the real one.
    assert_eq!(
        qm.find_symbol_matches("Beta.handle").await.unwrap().len(),
        1,
        "the empty answer above is a DECISION, not a dead lookup"
    );
}

/// Generated symbols are real, but they are never what the agent meant.
#[tokio::test(flavor = "multi_thread")]
async fn generated_files_sort_last() {
    let (qm, _tmp) = manager().await;

    let hits = qm.find_symbol_matches("handle").await.unwrap();
    assert!(
        hits.len() >= 4,
        "including the generated one: {}",
        hits.len()
    );

    let last = hits.last().unwrap();
    assert!(
        last.file_path.contains("/generated/"),
        "the generated `handle` sorts LAST: {:?}",
        hits.iter().map(|n| &n.file_path).collect::<Vec<_>>()
    );
}
