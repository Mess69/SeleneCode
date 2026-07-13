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

    let none = build_flow_from_named_symbols(
        &qm,
        &["hashPassword".to_string(), "handleLogin".to_string()], // backwards — no such path
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
