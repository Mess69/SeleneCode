#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 23 — EventEmitter (string-keyed) channels.
//!
//! # The flow this must close
//!
//! ```text
//! Application.use → bus.emit('mount') →[SYNTHESIZED]→ onmount → initApp
//! ```
//!
//! The correlation key is a string literal, invisible to the AST. The flow is
//! closed only if a path runs from the function that EMITS to what the registered
//! handler actually does — `initApp` — because that is the agent's question
//! ("what happens when the app mounts?").

use selene_core::Provenance;
use selene_resolve::FrameworkResolver;

mod pipeline;

const NONE: &[&'static dyn FrameworkResolver] = &[];

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dispatch")
        .join(name)
}

// =============================================================================
// THE FLOW
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn flow_emit_to_named_handler_body_is_closed() {
    let p = pipeline::index_resolve_and_synthesize(&fixture("event"), NONE).await;

    let emitter = p.node_named("use").await;
    p.assert_flow(
        &emitter.id,
        "initApp",
        &["onmount"],
        "event-emitter: use → emit('mount') →[synth]→ onmount → initApp",
    )
    .await;
}

/// The bridging edge names the event and points at the **`on(` site** — the
/// wiring location — not at the emit.
#[tokio::test(flavor = "multi_thread")]
async fn the_edge_carries_the_event_and_the_registration_site() {
    let p = pipeline::index_resolve_and_synthesize(&fixture("event"), NONE).await;

    let emitter = p.node_named("use").await;
    let handler = p.node_named("onmount").await;

    let edges = p.synth_edges_from(&emitter.id).await;
    assert_eq!(edges.len(), 1, "one synthesized edge");
    let e = &edges[0];

    assert_eq!(e.target, handler.id);
    assert_eq!(e.provenance, Some(Provenance::Heuristic));

    let m = e.metadata.as_ref().unwrap();
    assert_eq!(m["synthesizedBy"], "event-emitter");
    assert_eq!(m["event"], "mount");
    // app.ts:4 is `bus.on('mount', function onmount() {` — the ON site. The emit
    // lives at line 13, inside `use()`; pointing there would send the agent to
    // the place it already is.
    assert_eq!(
        m["registeredAt"], "src/app.ts:4",
        "the ON site is the wiring location — NOT the emit site"
    );
    assert!(
        e.line.is_none(),
        "this pass carries no `line` on the edge (the map is explicit)"
    );
}

// =============================================================================
// The deliberate frontier
// =============================================================================

/// `bus.on('tick', () => refresh())` bridges **nothing**.
///
/// The arrow is not a node, so there is nothing to point at. **Do not "fix" this
/// by linking the emitter to the enclosing function** — that would claim the
/// emitter calls the enclosing function, which it does not. A wrong edge poisons
/// the map; a missing one merely leaves it incomplete. Silent beats wrong.
#[tokio::test(flavor = "multi_thread")]
async fn an_anonymous_arrow_handler_bridges_nothing() {
    let p = pipeline::index_resolve_and_synthesize(&fixture("event"), NONE).await;

    // `refresh` is only reachable through the arrow handler on 'tick'.
    let refresh = p.node_named("refresh").await;
    let all: Vec<_> = p
        .store_edges_into(&refresh.id)
        .await
        .into_iter()
        .filter(|e| e.provenance == Some(Provenance::Heuristic))
        .collect();
    assert!(
        all.is_empty(),
        "an arrow handler is the known frontier — it must synthesize NOTHING, \
         not an edge to whatever function happens to enclose it"
    );

    // Exactly one channel bridged in this fixture: 'mount'. Not 'tick'.
    assert_eq!(p.synthesized, 1, "only the NAMED handler bridged");
}

// =============================================================================
// Precision — the 0-control
// =============================================================================

/// A repo with `.on(` but no `.emit(` bridges nothing: there is no dispatcher, so
/// there is no channel.
#[tokio::test(flavor = "multi_thread")]
async fn on_without_emit_bridges_nothing() {
    let p = pipeline::index_resolve_and_synthesize(&fixture("event-control"), NONE).await;
    assert_eq!(
        p.synthesized, 0,
        "a registration with nothing to fire it is not a channel"
    );
}
