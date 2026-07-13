#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 24 — React re-render (`setState` → `render`).
//!
//! # ⛔ This pass is DORMANT, on purpose
//!
//! It is **not** in `SYNTH_PASS_ORDER` yet. Task 25 (JSX child) registers both.
//!
//! Shipping `react-render` alone measurably **RAISED** agent reads in the TS
//! build: the half-bridged flow ends at `render`, which advertises that something
//! happens next and gives the agent nowhere to go — so it opens the file. A bridge
//! to nowhere is worse than no bridge (PRD §8.2). `render` is not an answer;
//! `renderStaticScene` is, and only the JSX hop reaches it.
//!
//! So these are UNIT tests, calling the pass directly. The end-to-end flow lives
//! in `synth_jsx_test.rs`, which owns the fixture for both.

use selene_core::Provenance;
use selene_resolve::FrameworkResolver;
use selene_resolve::synth::react::run_react_render;

mod pipeline;

const NONE: &[&'static dyn FrameworkResolver] = &[];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/react-render")
}

/// Exactly one edge: `handleClick → render`. Not `helper` (no setState), and
/// nothing at all from `NoRender` (no `render` method).
#[tokio::test(flavor = "multi_thread")]
async fn setstate_siblings_link_to_render_and_nothing_else_does() {
    let p = pipeline::index_and_resolve(&fixture(), NONE).await;

    let edges = run_react_render(p.store(), p.ctx()).await.unwrap();

    let handle_click = p.node_named("handleClick").await;
    let render = p.node_named("render").await;

    assert_eq!(
        edges.len(),
        1,
        "one setState sibling in the fixture: {:?}",
        edges
            .iter()
            .map(|e| (&e.source, &e.target))
            .collect::<Vec<_>>()
    );
    let e = &edges[0];
    assert_eq!(e.source, handle_click.id, "the setState sibling");
    assert_eq!(e.target, render.id, "→ render");
    assert_eq!(e.provenance, Some(Provenance::Heuristic));

    let m = e.metadata.as_ref().unwrap();
    assert_eq!(m["synthesizedBy"], "react-render");
    assert_eq!(m["via"], "setState");
    assert!(
        m["registeredAt"]
            .as_str()
            .unwrap()
            .starts_with("src/App.tsx:"),
        "registeredAt points at the render method"
    );

    // `helper` has no setState → no edge.
    let helper = p.node_named("helper").await;
    assert!(
        !edges.iter().any(|e| e.source == helper.id),
        "a sibling WITHOUT setState must not link to render"
    );

    // `NoRender.bump` calls setState, but its class has NO `render` method → the
    // whole class is skipped. This is what keeps the pass inert on every ordinary
    // OO class that happens to have a `setState`-shaped method.
    let bump = p.node_named("bump").await;
    assert!(
        !edges.iter().any(|e| e.source == bump.id),
        "a class with no `render` method yields NO edges"
    );
}

/// The shipping gate, asserted: this pass is **not registered** until Task 25.
#[test]
fn react_render_is_not_registered_alone() {
    assert!(
        !selene_resolve::synth::registered_synthesizers().contains(&"react-render")
            || selene_resolve::synth::registered_synthesizers().contains(&"jsx-render"),
        "`react-render` must never be registered WITHOUT `jsx-render` — the \
         half-bridged flow (handleClick → render → nothing) measurably raised agent \
         reads in the TS build. They ship as one unit."
    );
}
