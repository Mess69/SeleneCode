#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 26 — the Django ORM descriptor.
//!
//! # ⚠ This is a RESOLVER, not a synthesizer — and that is the point
//!
//! The roadmap files it under "the 5 synthesizers". It is a framework `resolve()`
//! branch, and it must stay one. The playbook's central mechanism lesson:
//!
//! | The reference is… | Mechanism | Provenance |
//! |---|---|---|
//! | **named** — `_iterable_class` IS an attribute name | `claims_reference` + `resolve()` | ordinary `tree-sitter` edge |
//! | **anonymous** — `cb()`, `emit('e')`, `<Child/>` | a whole-graph synth pass | `heuristic` + `synthesizedBy` |
//!
//! So this bridge emits **no heuristic edge and no `synthesizedBy`**, and there is
//! a test below that says so. A reviewer expecting one is expecting the wrong
//! contract.
//!
//! # The flow
//!
//! ```text
//! QuerySet._fetch_all → ModelIterable.__iter__ → SQLCompiler.execute_sql
//! ```
//!
//! Hop 1 is this task's bridge; hops 2+ are ordinary static calls inside
//! `__iter__`. Statically, `_fetch_all`'s only callee was
//! `_prefetch_related_objects` — the query→SQL flow did not exist at all.

use selene_core::Provenance;
use selene_resolve::FrameworkResolver;
use selene_resolve::frameworks::python::DjangoResolver;

mod pipeline;

const DJANGO: &[&'static dyn FrameworkResolver] = &[&DjangoResolver];

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dispatch")
        .join(name)
}

// =============================================================================
// THE FLOW
// =============================================================================

/// The whole chain — not just the bridging edge. A test that asserted only
/// `_fetch_all → __iter__` would pass on a bridge that leads nowhere.
#[tokio::test(flavor = "multi_thread")]
async fn flow_queryset_to_sql_compiler_is_closed() {
    let p = pipeline::index_and_resolve(&fixture("django-orm"), DJANGO).await;

    let fetch_all = p.node_named("_fetch_all").await;
    p.assert_flow(
        &fetch_all.id,
        "execute_sql",
        &["__iter__"],
        "django ORM: _fetch_all →[descriptor]→ ModelIterable.__iter__ → execute_sql",
    )
    .await;
}

// =============================================================================
// The MECHANISM contract — the asymmetry is deliberate
// =============================================================================

/// The bridging edge is an ORDINARY RESOLVED EDGE: `tree-sitter` provenance, and
/// **no `synthesizedBy`**.
///
/// A test that asserted `Heuristic` here would be asserting the wrong contract.
/// The reference is *named* (`_iterable_class` is an attribute name), so it goes
/// through `resolve()`, not through a synthesizer pass.
#[tokio::test(flavor = "multi_thread")]
async fn the_bridge_is_a_resolved_edge_not_a_synthesized_one() {
    let p = pipeline::index_and_resolve(&fixture("django-orm"), DJANGO).await;

    let fetch_all = p.node_named("_fetch_all").await;
    let iter = p.node_named("__iter__").await;

    let edge = p
        .store_edges_into(&iter.id)
        .await
        .into_iter()
        .find(|e| e.source == fetch_all.id)
        .expect("the descriptor bridge must exist");

    assert_ne!(
        edge.provenance,
        Some(Provenance::Heuristic),
        "NOT heuristic — a named ref resolves, it is not synthesized"
    );
    assert_eq!(edge.provenance, Some(Provenance::TreeSitter));

    if let Some(m) = &edge.metadata {
        assert!(
            m.get("synthesizedBy").is_none(),
            "a resolved edge carries NO synthesizedBy — that key belongs to the \
             whole-graph passes, and conflating the two mechanisms is the mistake \
             the playbook exists to prevent"
        );
    }
}

/// The pre-filter hook, asserted directly: `_iterable_class` names **no declared
/// symbol anywhere**, so without the claim `resolve()` is never even reached and
/// the bridge is silently inert. This is the same hook Rails and Laravel need.
#[test]
fn the_attribute_is_claimed_past_the_pre_filter() {
    assert!(
        DjangoResolver.claims_reference("_iterable_class"),
        "unclaimed, the reference is dropped BEFORE resolve() runs — and the ORM's \
         hottest flow silently does not exist"
    );
    // …and Task 14's claim still stands. Task 26 EXTENDS the hook, never replaces it.
    assert!(
        DjangoResolver.claims_reference("api.urls"),
        "the include() claim must survive — extending claims_reference, not replacing it"
    );
    assert!(!DjangoResolver.claims_reference("some_ordinary_name"));
}

// =============================================================================
// Precision — the 0-control
// =============================================================================

/// A Python project that calls `self._iterable_class(...)` but declares **no**
/// `ModelIterable` resolves to **nothing**.
///
/// Silent beats wrong: the class is chosen at runtime, and binding the ORM's
/// hottest flow to whatever iterator happened to be nearby would be worse than
/// leaving the hop honest and open.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_model_iterable_the_reference_stays_unresolved() {
    let p = pipeline::index_and_resolve(&fixture("django-orm-control"), DJANGO).await;

    let fetch_all = p.node_named("_fetch_all").await;
    let out = p.store_edges_out_of(&fetch_all.id).await;

    assert!(
        out.is_empty(),
        "no ModelIterable ⇒ no bridge. Found: {:?}",
        out.iter().map(|e| &e.target).collect::<Vec<_>>()
    );
}
