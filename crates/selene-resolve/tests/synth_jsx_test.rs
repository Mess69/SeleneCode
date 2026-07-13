#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 25 — JSX child (`<Child/>` → component), and the ACTIVATION of the React
//! dispatch pair.
//!
//! # The flow — and it takes BOTH passes
//!
//! ```text
//! App.handleClick → [react-render] → render → [jsx-render] → StaticCanvas → renderStaticScene
//! ```
//!
//! This file owns the end-to-end fixture for Tasks 24 AND 25. Neither pass is a
//! flow on its own:
//!
//! - `react-render` alone stops at `render` — which advertises that something
//!   happens next and gives the agent nowhere to go. It measurably RAISED reads.
//! - `jsx-render` alone never gets entered: nothing connects the click to `render`.
//!
//! Together they answer "what does clicking actually repaint?" — which is the
//! question. That is why they are one mergeable unit.

use selene_core::Provenance;
use selene_resolve::FrameworkResolver;
use selene_resolve::synth::{SYNTH_PASS_ORDER, registered_synthesizers};

mod pipeline;

const NONE: &[&'static dyn FrameworkResolver] = &[];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/react-render")
}

// =============================================================================
// THE FLOW — the gate for Tasks 24 AND 25
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn flow_click_to_repaint_is_closed_through_both_react_passes() {
    let p = pipeline::index_resolve_and_synthesize(&fixture(), NONE).await;

    let click = p.node_named("handleClick").await;

    p.assert_flow(
        &click.id,
        "renderStaticScene",
        &["render", "StaticCanvas"],
        "react: handleClick →[react-render]→ render →[jsx-render]→ StaticCanvas → renderStaticScene",
    )
    .await;
}

/// The two bridging edges, and their (deliberately different) metadata shapes.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_bridges_carry_their_contracted_metadata() {
    let p = pipeline::index_resolve_and_synthesize(&fixture(), NONE).await;

    let click = p.node_named("handleClick").await;
    let render = p.node_named("render").await;
    let canvas = p.node_named("StaticCanvas").await;

    // --- hop 1: react-render -------------------------------------------------
    let from_click = p.synth_edges_from(&click.id).await;
    assert_eq!(from_click.len(), 1);
    let e = &from_click[0];
    assert_eq!(e.target, render.id);
    assert_eq!(e.provenance, Some(Provenance::Heuristic));
    let m = e.metadata.as_ref().unwrap();
    assert_eq!(m["synthesizedBy"], "react-render");
    assert_eq!(m["via"], "setState");
    assert!(
        m.get("registeredAt").is_some(),
        "react-render HAS a wiring site"
    );

    // --- hop 2: jsx-render ---------------------------------------------------
    let from_render = p.synth_edges_from(&render.id).await;
    assert_eq!(from_render.len(), 1, "one JSX child (the `<span>` is DOM)");
    let e = &from_render[0];
    assert_eq!(e.target, canvas.id);
    let m = e.metadata.as_ref().unwrap();
    assert_eq!(m["synthesizedBy"], "jsx-render");
    assert_eq!(m["via"], "StaticCanvas");
    assert!(
        m.get("registeredAt").is_none(),
        "jsx-render has NO registeredAt — there is no wiring site: the JSX element \
         IS the call. (The map is explicit about the asymmetry.)"
    );
}

/// Lowercase tags are DOM, not components. `<div>` and `<span>` must bridge
/// nothing — the capital initial is JSX's own discriminator, and honoring it is
/// what keeps this pass from linking every component to `div`.
#[tokio::test(flavor = "multi_thread")]
async fn lowercase_dom_tags_bridge_nothing() {
    let p = pipeline::index_resolve_and_synthesize(&fixture(), NONE).await;
    let render = p.node_named("render").await;

    let vias: Vec<String> = p
        .synth_edges_from(&render.id)
        .await
        .iter()
        .filter_map(|e| e.metadata.as_ref()?["via"].as_str().map(str::to_string))
        .collect();

    assert_eq!(
        vias,
        vec!["StaticCanvas".to_string()],
        "`<div>` and `<span>` are DOM tags — only the capitalized component bridges"
    );
}

// =============================================================================
// ACTIVATION — the pair is now registered, in order
// =============================================================================

/// Both passes are registered, and in the declared order. Task 21's
/// `registry_agrees_with_the_declared_order` keeps the table and the order in
/// step; this pins the ORDER itself, which is behavior (the cross-pass dedupe is
/// first-wins).
#[test]
fn the_react_pair_is_registered_together_and_in_order() {
    assert_eq!(
        SYNTH_PASS_ORDER,
        &["callback", "event-emitter", "react-render", "jsx-render"],
        "the v0 channel set, in the one declared order"
    );

    let reg = registered_synthesizers();
    let rr = reg.iter().position(|n| *n == "react-render");
    let jsx = reg.iter().position(|n| *n == "jsx-render");

    assert!(
        rr.is_some() && jsx.is_some(),
        "the React pair ships TOGETHER — react-render without jsx-render is the \
         half-bridged flow that measurably raised agent reads (PRD §8.2)"
    );
    assert!(rr < jsx, "react-render precedes jsx-render");
}
