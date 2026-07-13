#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 22 — callback / field-observer channels.
//!
//! # The flow this must close
//!
//! ```text
//! mutateElement → triggerUpdate → [SYNTHESIZED] → triggerRender → renderScene
//! ```
//!
//! The synthesized edge is `triggerUpdate → triggerRender`. But that edge alone
//! is not the point: a test that asserts only the edge would pass on a bridge
//! that goes nowhere. The flow is closed **only if a path runs from the mutation
//! entry point all the way to the render body** — which is the question an agent
//! actually asks ("how does an update reach the screen?").

use selene_core::Provenance;
use selene_resolve::FrameworkResolver;

mod pipeline;

/// No framework — this is a pure language-level dispatch shape.
const NONE: &[&'static dyn FrameworkResolver] = &[];

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dispatch")
        .join(name)
}

// =============================================================================
// THE FLOW
// =============================================================================

/// The excalidraw shape, end to end.
#[tokio::test(flavor = "multi_thread")]
async fn flow_mutate_to_render_is_closed_through_the_synthesized_callback_edge() {
    let p = pipeline::index_resolve_and_synthesize(&fixture("callback"), NONE).await;

    assert_eq!(
        p.synthesized, 1,
        "exactly ONE channel in this fixture — precision matters as much as recall"
    );

    let mutate = p.node_named("mutateElement").await;
    p.assert_flow(
        &mutate.id,
        "renderScene",
        &["triggerUpdate", "triggerRender"],
        "callback: mutateElement → triggerUpdate →[synth]→ triggerRender → renderScene",
    )
    .await;
}

/// The bridging edge carries the full wiring provenance — this is what the MCP
/// layer surfaces so the agent can SEE where the callback was registered without
/// opening the file.
#[tokio::test(flavor = "multi_thread")]
async fn the_synthesized_edge_carries_via_field_and_the_registration_site() {
    let p = pipeline::index_resolve_and_synthesize(&fixture("callback"), NONE).await;

    let dispatcher = p.node_named("triggerUpdate").await;
    let callback = p.node_named("triggerRender").await;

    let edges = p.synth_edges_from(&dispatcher.id).await;
    assert_eq!(edges.len(), 1, "one synthesized edge out of the dispatcher");
    let e = &edges[0];

    assert_eq!(
        e.target, callback.id,
        "dispatcher → the registered callback"
    );
    assert_eq!(e.provenance, Some(Provenance::Heuristic));

    let m = e.metadata.as_ref().unwrap();
    assert_eq!(m["synthesizedBy"], "callback");
    assert_eq!(m["via"], "onUpdate", "the registrar that took the callback");
    assert_eq!(m["field"], "callbacks", "the field it was stored in");
    assert_eq!(
        m["registeredAt"], "src/app.ts:9",
        "the WIRING SITE — the line the agent would otherwise have to find by reading"
    );
}

// =============================================================================
// Precision — the 0-control
// =============================================================================

/// A repo with a registrar-shaped method but **no dispatcher**, and a
/// dispatcher-shaped method but **no registrar**, synthesizes **nothing**.
///
/// This is the precision guard that makes the channel trustworthy: a pass that
/// fires on half a shape would sprinkle wrong edges across every codebase with an
/// `onX` method in it.
#[tokio::test(flavor = "multi_thread")]
async fn a_half_shape_synthesizes_nothing() {
    let p = pipeline::index_resolve_and_synthesize(&fixture("callback-control"), NONE).await;
    assert_eq!(
        p.synthesized, 0,
        "registrar without dispatcher (and vice versa) is NOT a channel — \
         0 edges, or the pass is over-linking"
    );
}
