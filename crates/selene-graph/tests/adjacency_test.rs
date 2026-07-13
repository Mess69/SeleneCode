#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 4 — adjacency, against a real resolved graph.
//!
//! Every assertion here is paired with a positive control: an empty `callers()` proves
//! nothing unless the same graph can produce a non-empty one.

mod common;

use common::{index_fixture, write_3_file_fixture};
use selene_core::EdgeKind;
use selene_db::SurrealStore;
use selene_graph::{QueryManager, clamp_depth, clamp_limit};

async fn manager() -> (QueryManager<SurrealStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    (QueryManager::new(store, tmp.path().to_path_buf()), tmp)
}

async fn id_of(qm: &QueryManager<SurrealStore>, name: &str) -> String {
    qm.store()
        .get_nodes_by_name(name)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{name} is indexed"))
        .id
}

/// `handleLogin` → `login` → `hashPassword`. Callers and callees, both directions, on the
/// real chain.
#[tokio::test(flavor = "multi_thread")]
async fn callers_and_callees_walk_the_real_cross_file_chain() {
    let (qm, _tmp) = manager().await;

    let login = id_of(&qm, "login").await;

    let callers = qm.callers(&login, 2).await.unwrap();
    assert!(
        callers.iter().any(|n| n.node.name == "handleLogin"),
        "handleLogin calls login, ACROSS FILES: {:?}",
        callers.iter().map(|n| &n.node.name).collect::<Vec<_>>()
    );

    let callees = qm.callees(&login, 2).await.unwrap();
    assert!(
        callees.iter().any(|n| n.node.name == "hashPassword"),
        "login calls hashPassword, across files: {:?}",
        callees.iter().map(|n| &n.node.name).collect::<Vec<_>>()
    );
}

/// The impact radius of the leaf reaches back up the chain.
#[tokio::test(flavor = "multi_thread")]
async fn impact_reaches_everything_that_breaks() {
    let (qm, _tmp) = manager().await;

    let hash = id_of(&qm, "hashPassword").await;
    let subgraph = qm.impact(&hash, 0).await.unwrap(); // 0 ⇒ the ported default (2)

    let names: Vec<&str> = subgraph.nodes.values().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"login"),
        "changing hashPassword breaks login: {names:?}"
    );
}

/// `find_path` — `None` is an ANSWER ("not connected"), never an error.
#[tokio::test(flavor = "multi_thread")]
async fn find_path_connects_the_chain_and_answers_none_when_it_cannot() {
    let (qm, _tmp) = manager().await;

    let from = id_of(&qm, "handleLogin").await;
    let to = id_of(&qm, "hashPassword").await;

    // The positive control FIRST: the path exists.
    assert!(
        qm.find_path(&from, &to, &[EdgeKind::Calls, EdgeKind::References])
            .await
            .unwrap()
            .is_some(),
        "handleLogin → login → hashPassword must connect, or every 'no path' assertion \
         below is vacuous"
    );

    // …and a path that genuinely does not exist is `None`, not `Err`.
    let nowhere = qm
        .find_path(&to, "function:does-not-exist", &[EdgeKind::Calls])
        .await
        .expect("'not connected' is an ANSWER — an Err here would be an isError at the tool layer");
    assert!(nowhere.is_none());
}

/// Clamps: the agent asks for depth 50 and gets an ANSWER at depth 10.
#[tokio::test(flavor = "multi_thread")]
async fn out_of_range_depth_is_clamped_not_refused() {
    let (qm, _tmp) = manager().await;
    let login = id_of(&qm, "login").await;

    let deep = qm.callers(&login, 9999).await.expect(
        "an out-of-range depth is CLAMPED, never refused — spending an isError on \
                 a value we can simply fix is how the tool gets abandoned",
    );
    assert!(!deep.is_empty());

    assert_eq!(clamp_depth(9999), 10);
    assert_eq!(clamp_limit(0), 1);
}
