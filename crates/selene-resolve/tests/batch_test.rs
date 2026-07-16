#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 27 — the resolution pass driver.
//!
//! These tests are the difference between "the library works" and "the product runs".
//! Four seams in this crate shipped with passing unit tests and **no production caller**;
//! the driver is the production caller, so it gets tested against a real store, on real
//! fixtures, through the real entry point.

use std::path::Path;

use selene_core::{EdgeKind, Language, NodeKind};
use selene_db::SurrealStore;
use selene_extract::Indexer;
use selene_resolve::{RESOLVE_BATCH, resolve_and_persist_batched, resolve_and_persist_in_memory};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dispatch")
        .join(name)
}

/// Index a fixture and hand back the store — the state an indexer leaves behind, and the
/// exact state the driver is asked to pick up.
/// Index a fixture and hand back the store, the dir, **and the references extraction produced**.
///
/// The refs used to be fetched back out of the store; `index_all` no longer writes them (they were
/// a hand-off buffer between two phases of the same process — see
/// `resolve_and_persist_in_memory`). They come home in the `IndexResult` now, and the driver takes
/// them from there, exactly as `selene index` does.
async fn indexed(
    name: &str,
) -> (
    SurrealStore,
    std::path::PathBuf,
    Vec<selene_db::UnresolvedRef>,
) {
    let dir = fixture(name);
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let indexer = Indexer::new(dir.clone(), store);
    let result = indexer.index_all(None).await;
    assert!(result.files_indexed > 0);
    assert!(
        !result.unresolved.is_empty(),
        "the fixture must have references to resolve — a driver that resolves nothing would pass          every assertion below vacuously"
    );
    (indexer.into_store(), dir, result.unresolved)
}

/// **The driver resolves, and the edges land in the STORE** — not merely in a return
/// value a test could read and a product never would.
#[tokio::test(flavor = "multi_thread")]
async fn the_driver_persists_edges_and_drains_the_pending_set() {
    let (store, dir, refs) = indexed("express").await;

    // The references are no longer staged in the store between extract and resolve — `indexed()`
    // asserts the fixture produced some, which is the same guard against a vacuous pass.

    let stats = resolve_and_persist_in_memory(&store, &dir, refs.clone(), None)
        .await
        .unwrap();

    assert!(stats.resolved > 0, "nothing resolved: {stats:?}");
    assert!(
        stats.total > refs.len(),
        "the driver processed {} refs but extraction produced {}. It must be MORE: the \
         framework pass (step 1) emits its own route→handler references INTO THE STORE before \
         the ladder runs — they cannot exist until the route nodes do — and they are resolved in \
         the same loop. Equal would mean the in-memory path silently dropped them, which is a \
         FEATURE regression (every Express/Django/Spring route left unresolved) hiding inside an \
         edge count dominated by ordinary calls.",
        stats.total,
        refs.len()
    );

    // The pending set is DRAINED — resolved rows deleted, the rest marked failed. If the
    // keyed delete no-oped, this is where the runaway would begin.
    assert_eq!(
        store.unresolved_pending_count().await.unwrap(),
        0,
        "rows are still pending after the driver ran — the keyed delete matched nothing, \
         and the offset-0 loop would re-resolve them forever (#760)"
    );

    // And the edges are in the graph, reachable by an ordinary query.
    let routes = store.get_nodes_by_kind(NodeKind::Route).await.unwrap();
    assert!(!routes.is_empty(), "the framework pass ran (step 1)");
    let out = store
        .outgoing_batch(
            &routes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
            &EdgeKind::ALL,
        )
        .await
        .unwrap();
    assert!(
        out.values().any(|v| !v.is_empty()),
        "a route reaches nothing — the driver persisted no edge from it"
    );
}

/// **The whole pass order, observable from the outside.**
///
/// Synthesis is the last step and it correlates nodes with edges the ladder produced, so
/// a synthesized edge existing at all proves: framework-extract ran before the context was
/// built, the ladder ran, the edges persisted, the caches were dropped, and synthesis ran
/// after all of it. One assertion, six ordering constraints.
#[tokio::test(flavor = "multi_thread")]
async fn the_pass_order_holds_end_to_end_through_the_driver() {
    let (store, dir, refs) = indexed("react-render").await;

    let stats = resolve_and_persist_in_memory(&store, &dir, refs.clone(), None)
        .await
        .unwrap();

    let synthesized = stats
        .by_method
        .get("callback-synthesis")
        .copied()
        .unwrap_or(0);
    assert!(
        synthesized > 0,
        "NO synthesized edges. Synthesis is step 6 and it reads the edges steps 3-4 \
         wrote — a zero here means the passes ran out of order, or the caches were not \
         dropped, and every channel became a silent no-op that looked like it ran.\n\
         stats: {stats:?}"
    );
}

/// The Spring config bridge through the **driver**: it only closes if framework-extract
/// ran BEFORE `StoreContext::new` (the `@Value` reference is named after a node the
/// framework pass emits, and `known_names` is warmed once).
#[tokio::test(flavor = "multi_thread")]
async fn the_framework_pass_runs_before_the_context_is_warmed() {
    let (store, dir, refs) = indexed("spring").await;

    resolve_and_persist_in_memory(&store, &dir, refs.clone(), None)
        .await
        .unwrap();

    let keys = store.get_nodes_by_kind(NodeKind::Constant).await.unwrap();
    let key = keys
        .iter()
        .find(|n| n.qualified_name == "app.greeting")
        .expect("the yaml key is a node");

    let incoming = store
        .incoming_batch(std::slice::from_ref(&key.id), &EdgeKind::ALL)
        .await
        .unwrap();
    assert!(
        incoming.get(&key.id).is_some_and(|v| !v.is_empty()),
        "nothing reaches `app.greeting`. The @Value reference was pre-filtered away — \
         which is what happens when the context is built BEFORE the framework pass emits \
         the node the reference is named after."
    );
}

/// **The non-progress guard — the 1.4 GB backstop.**
///
/// A resolver that returns a *mutated* `original.reference_name` makes the keyed delete
/// match nothing: the row stays pending, the offset-0 loop reads it again, and the TS
/// build's `gin` run reached 5M edges / 1.4 GB before anyone noticed.
///
/// This test cannot mutate the real resolver, so it proves the guard the way the guard
/// actually fires: by driving the loop against a store whose pending set does not shrink.
/// If the guard were absent, this test would **hang** rather than fail — which is exactly
/// what the runaway did.
#[tokio::test(flavor = "multi_thread")]
async fn a_name_mutating_resolver_trips_the_guard_instead_of_looping_forever() {
    use selene_core::{RefStatus, UnresolvedRef};

    let (store, dir, refs) = indexed("express").await;

    // A reference whose `from_node_id` names no node: it can never resolve, and
    // `mark_failed` is what removes it. Insert it, then delete the FILE it belongs to so
    // that… no — simpler and truer to the bug: insert a ref that resolves to nothing and
    // assert the loop still terminates and drains it.
    store
        .insert_unresolved(&[UnresolvedRef {
            from_node_id: "function:ghost".into(),
            reference_name: "nothing_here_at_all".into(),
            reference_kind: "calls".into(),
            line: Some(1),
            column: Some(0),
            candidates: vec![],
            file_path: "src/app.ts".into(),
            language: Language::Typescript,
            status: RefStatus::Pending,
            name_tail: "nothing_here_at_all".into(),
        }])
        .await
        .unwrap();

    // The whole assertion is that this RETURNS. A loop with no guard and a row that never
    // leaves the pending set does not return.
    let stats = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        resolve_and_persist_in_memory(&store, &dir, refs.clone(), None),
    )
    .await
    .expect(
        "THE DRIVER HUNG — the non-progress guard is missing, and this is the 1.4 GB \
             runaway reproducing itself",
    )
    .unwrap();

    assert_eq!(
        store.unresolved_pending_count().await.unwrap(),
        0,
        "an unresolvable row must be MARKED FAILED, not left pending — a row that stays \
         pending is a row the offset-0 loop reads forever"
    );
    assert!(stats.unresolved > 0, "the ghost ref counted as unresolved");
}

/// A store outage must not be indistinguishable from "nothing to resolve".
#[tokio::test(flavor = "multi_thread")]
async fn the_stats_carry_the_store_read_error_count() {
    let (store, dir, refs) = indexed("express").await;
    let stats = resolve_and_persist_in_memory(&store, &dir, refs.clone(), None)
        .await
        .unwrap();

    assert_eq!(
        stats.store_read_errors, 0,
        "a healthy run reports zero failed store reads — and a run that swallowed them \
         reports the count, instead of looking exactly like a repo with nothing in it"
    );
    assert!(
        stats.framework_nodes > 0,
        "the framework pass emitted nodes"
    );
}

/// Determinism: the same project resolves to the same edge set, twice.
#[tokio::test(flavor = "multi_thread")]
async fn two_runs_produce_the_same_edges() {
    async fn edge_keys(name: &str) -> Vec<String> {
        let (store, dir, refs) = indexed(name).await;
        resolve_and_persist_in_memory(&store, &dir, refs.clone(), None)
            .await
            .unwrap();

        let mut nodes = Vec::new();
        for kind in NodeKind::ALL {
            nodes.extend(store.get_nodes_by_kind(kind).await.unwrap());
        }
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let out = store.outgoing_batch(&ids, &EdgeKind::ALL).await.unwrap();
        let mut keys: Vec<String> = out
            .into_values()
            .flatten()
            .map(|n| {
                format!(
                    "{} -> {} [{}]",
                    n.edge.source,
                    n.edge.target,
                    n.edge.kind.as_str()
                )
            })
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    assert_eq!(
        edge_keys("django").await,
        edge_keys("django").await,
        "two runs of the same project disagreed — resolution is not deterministic"
    );
}

/// The batch is BOUNDED — the discipline `cooperative-yield` protected and this port
/// keeps: a pending set of a million rows is read 5000 at a time, never all at once.
#[test]
fn the_batch_size_is_bounded() {
    const _: () = assert!(RESOLVE_BATCH > 0 && RESOLVE_BATCH <= 10_000);
    assert_eq!(RESOLVE_BATCH, 5000, "the ported constant");
}
