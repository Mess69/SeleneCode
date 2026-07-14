#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 9 — the Flow section, against a real resolved graph.

mod common;

use common::{index_fixture, write_3_file_fixture};
use selene_context::{build_flow_from_named_symbols, render_flow};
use selene_graph::QueryManager;

/// The chain an agent asked for, rendered in order, with names it can trust.
#[tokio::test(flavor = "multi_thread")]
async fn the_flow_walks_the_real_cross_file_chain() {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    let qm = QueryManager::new(store, tmp.path().to_path_buf());

    let steps = build_flow_from_named_symbols(
        &qm,
        &["handleLogin".to_string(), "hashPassword".to_string()],
    )
    .await
    .unwrap()
    .expect("handleLogin → login → hashPassword is a REAL chain in this fixture");

    let names: Vec<&str> = steps.iter().map(|s| s.node.name.as_str()).collect();
    assert!(names.first() == Some(&"handleLogin"));
    assert!(names.last() == Some(&"hashPassword"));
    assert!(
        names.contains(&"login"),
        "the MIDDLE hop is the whole value — a flow of endpoints with a gap between them \
         sends the agent straight to Read: {names:?}"
    );

    let rendered = render_flow(&steps);
    assert!(rendered.starts_with("### Flow\n"));
    assert!(
        rendered.contains("1. `handleLogin` (src/app.ts:3)"),
        "numbered, named, and CITED — file:line, so the agent can point at it without \
         opening anything:\n{rendered}"
    );
    assert!(
        rendered.contains("   ↓ calls\n"),
        "the hop is named:\n{rendered}"
    );
}

/// A flow we cannot prove is a flow we must not draw. `None`, never a fabricated chain — and
/// never an `Err`.
#[tokio::test(flavor = "multi_thread")]
async fn an_unprovable_flow_is_none_not_a_guess_and_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    let qm = QueryManager::new(store, tmp.path().to_path_buf());

    // ⚠ **This case used to be `["hashPassword", "handleLogin"]` — "backwards, so no path".**
    // That premise is dead, and deliberately so: `build_flow_from_named_symbols` is now
    // **order-independent** (it seeds the BFS from *every* named symbol and keeps the longest
    // chain, rather than demanding a path from each name to the next one the caller happened to
    // list — see its doc comment for the three reasons, each measured). Under that contract the
    // old pair is perfectly provable, so the assertion had been failing ever since, asserting a
    // behaviour the code had correctly stopped having.
    //
    // The *intent* — never draw a chain you cannot prove — is what matters, so it is re-tested
    // on a pair that is genuinely unprovable: `login` → `hashPassword` is a real edge but only
    // **two** nodes, and a 2-node "flow" is just an edge (`limits::MIN_FLOW_NODES`). It tells the
    // agent nothing it did not already ask, so it must not be rendered as a spine.
    let none = build_flow_from_named_symbols(
        &qm,
        &["hashPassword".to_string(), "login".to_string()], // an edge, but never a 3-node chain
    )
    .await
    .expect("'there is no flow' is an ANSWER, not a malfunction");

    assert!(
        none.is_none(),
        "a chain we cannot prove must not be drawn — a fabricated flow is a confident wrong \
         answer, the one failure mode an agent cannot detect"
    );

    // The positive control: the same pipeline DOES draw the flow in the direction that exists.
    assert!(
        build_flow_from_named_symbols(
            &qm,
            &["handleLogin".to_string(), "hashPassword".to_string()]
        )
        .await
        .unwrap()
        .is_some(),
        "…so the None above is a DECISION, not a dead lookup"
    );
}
