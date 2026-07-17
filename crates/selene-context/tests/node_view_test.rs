#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 12 — the node view + the whole-file rule.

mod common;

use common::{index_fixture, write_3_file_fixture};
use selene_context::{build_node_view, node_not_found, render_node_view};
use selene_graph::QueryManager;

#[tokio::test(flavor = "multi_thread")]
async fn the_node_view_carries_source_callers_and_callees() {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    let qm = QueryManager::new(store, tmp.path().to_path_buf());

    let view = build_node_view(&qm, "login")
        .await
        .unwrap()
        .expect("login is indexed");

    let rendered = render_node_view(&view);

    assert!(
        rendered.contains("### Called by"),
        "who calls it:\n{rendered}"
    );
    assert!(rendered.contains("handleLogin"));
    assert!(rendered.contains("### Calls"), "what it calls");
    assert!(rendered.contains("hashPassword"));
    assert!(
        rendered.contains("hashPassword(user)"),
        "THE SOURCE IS THERE — a node view without the body is a lookup, not an answer, and \
         the agent opens the file:\n{rendered}"
    );
    assert!(
        rendered.contains("\t"),
        "…and it is line-numbered, so the agent can cite it"
    );
    assert!(
        view.whole_file,
        "this file is tiny — the WHOLE FILE rule fires, because a fragment makes the agent \
         wonder what surrounds it, and wondering is one Read away from happening"
    );
}

/// #173 — a miss is a miss. Success-shaped, and it says what to do instead.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_symbol_is_none_and_the_guidance_never_says_read() {
    let tmp = tempfile::tempdir().unwrap();
    write_3_file_fixture(tmp.path());
    let store = index_fixture(tmp.path()).await;
    let qm = QueryManager::new(store, tmp.path().to_path_buf());

    assert!(
        build_node_view(&qm, "Nope.gone")
            .await
            .expect("a missing symbol is an ANSWER, not a malfunction")
            .is_none()
    );

    let guidance = node_not_found("Nope.gone").to_lowercase();
    // Surface-neutral spelling: "the `explore` tool" is `selene explore` on the
    // CLI and `selene_explore` over MCP — the guidance routes to it either way.
    assert!(guidance.contains("`explore` tool"), "it routes to a TOOL");
    assert!(
        !guidance.contains("read the file") && !guidance.contains("open the file"),
        "even the miss must not send the agent to Read: {guidance}"
    );
}
