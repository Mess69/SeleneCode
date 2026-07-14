//! The end-to-end pipeline harness for the framework tasks (Batch A).
//!
//! # Why a harness, and why it must be the WHOLE pipeline
//!
//! **Dynamic-dispatch coverage is end-to-end or not at all** (PRD §8.2). A
//! half-bridged flow is *worse* than no bridge: it advertises a hop the agent
//! then has to Read to finish, which is precisely the read-displacement the
//! product exists to prevent.
//!
//! So a framework test may not assert "the route node exists" or "an edge was
//! created". It must assert that a **path connects** from the entry point (the
//! route) to the terminal the agent would otherwise have opened a file to find
//! (the service function the handler body calls). A 3-of-4-hop resolution is a
//! FAILURE, not partial credit.
//!
//! [`index_and_resolve`] runs the real thing, in the real order:
//!
//! 1. `selene-extract` indexes the fixture directory (nodes, same-file edges,
//!    unresolved refs — **zero** cross-file edges, per Phase 2's contract);
//! 2. `run_framework_extract` emits the route nodes and their refs — **before**
//!    resolution, because a reference cannot bind to a route node that does not
//!    exist yet;
//! 3. the `ReferenceResolver` ladder binds every pending ref;
//! 4. the edges are persisted.
//!
//! Then [`Pipeline::assert_flow`] walks the graph and demands the whole chain.

#![allow(dead_code)] // each test file uses a subset

use std::path::Path;

use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance};
use selene_db::SurrealStore;
use selene_extract::Indexer;
use selene_resolve::{FrameworkResolver, ReferenceResolver, StoreContext, run_framework_extract};

/// The edge kinds a flow may traverse.
///
/// `Contains` is deliberately **excluded**: a path that walks file→class→method
/// containment is not a call flow, and allowing it would let a test go green
/// through pure structure while the dispatch bridge it is supposed to prove is
/// missing. That is the exact false-green this harness exists to prevent.
pub const FLOW_KINDS: &[EdgeKind] = &[EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports];

/// [`FLOW_KINDS`] **plus `Contains`** — for class-based dispatch only.
///
/// A Django CBV route (`path('x/', ArticleDetail.as_view())`) references the
/// **class**, and the code that answers the request lives in a *method* of that
/// class. The hop from the class to its handler method genuinely IS containment:
/// that is how a CBV dispatches, and an agent tracing the flow makes exactly the
/// same move.
///
/// Kept separate from [`FLOW_KINDS`] and used only where it is genuinely
/// warranted, because `Contains` is otherwise a false-green machine — with it, a
/// path can descend from a file node into any symbol in that file. Function-based
/// views (`path('legacy/', views.article_detail)`) are asserted with the STRICT
/// kinds, so both shapes are proven.
pub const CBV_FLOW_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Imports,
    EdgeKind::Contains,
];

/// A fixture indexed, framework-extracted, and fully resolved.
pub struct Pipeline {
    resolver: ReferenceResolver<StoreContext<SurrealStore>>,
    /// How many references the ladder bound.
    pub resolved: usize,
    /// How many edges the synthesizers inserted (0 unless
    /// `index_resolve_and_synthesize` was used).
    pub synthesized: u64,
}

impl Pipeline {
    /// The store, for queries.
    pub fn store(&self) -> &SurrealStore {
        self.resolver.ctx().store()
    }

    /// The resolution context (the synthesizers read source through it).
    pub fn ctx(&self) -> &StoreContext<SurrealStore> {
        self.resolver.ctx()
    }

    /// Every synthesized (`heuristic`) edge out of `from_id`, with its metadata.
    pub async fn synth_edges_from(&self, from_id: &str) -> Vec<Edge> {
        self.store()
            .outgoing(from_id, &[EdgeKind::Calls], None)
            .await
            .expect("outgoing")
            .into_iter()
            .map(|n| n.edge)
            .filter(|e| e.provenance == Some(Provenance::Heuristic))
            .collect()
    }

    /// Every edge INTO `id` (any kind), for precision assertions.
    pub async fn store_edges_into(&self, id: &str) -> Vec<Edge> {
        self.store()
            .incoming(id, &[EdgeKind::Calls, EdgeKind::References])
            .await
            .expect("incoming")
            .into_iter()
            .map(|n| n.edge)
            .collect()
    }

    /// Every edge OUT of `id`, for precision assertions.
    pub async fn store_edges_out_of(&self, id: &str) -> Vec<Edge> {
        self.store()
            .outgoing(id, &[EdgeKind::Calls, EdgeKind::References], None)
            .await
            .expect("outgoing")
            .into_iter()
            .map(|n| n.edge)
            .collect()
    }

    /// Assert the flow is **closed**: a path runs from `from_id` to the node
    /// named `to_name`, and every symbol in `via` lies on it, in order.
    ///
    /// A missing `via` symbol means the flow was bridged *around* a hop instead
    /// of *through* it — a silently wrong map, which fails here.
    pub async fn assert_flow(&self, from_id: &str, to_name: &str, via: &[&str], what: &str) {
        self.assert_flow_kinds(from_id, to_name, via, FLOW_KINDS, what)
            .await;
    }

    /// [`Pipeline::assert_flow`] over an explicit edge-kind set — see
    /// [`CBV_FLOW_KINDS`].
    pub async fn assert_flow_kinds(
        &self,
        from_id: &str,
        to_name: &str,
        via: &[&str],
        kinds: &[EdgeKind],
        what: &str,
    ) {
        let to = self.node_named(to_name).await;
        let path = self
            .store()
            .find_path(from_id, &to.id, kinds)
            .await
            .expect("find_path")
            .unwrap_or_else(|| {
                panic!(
                    "FLOW NOT CLOSED — {what}\n  no path from {from_id} to '{to_name}'.\n  \
                     A route that reaches nothing is worse than no route: it advertises a hop \
                     the agent must Read to finish (PRD §8.2)."
                )
            });

        let names: Vec<&str> = path.iter().map(|(n, _)| n.name.as_str()).collect();
        let mut cursor = 0usize;
        for want in via {
            match names[cursor..].iter().position(|n| n == want) {
                Some(at) => cursor += at + 1,
                None => panic!(
                    "FLOW BRIDGED AROUND A HOP — {what}\n  a path exists, but '{want}' is not \
                     on it (in order).\n  path: {names:?}\n  Bridging around a hop yields a \
                     silently wrong map."
                ),
            }
        }
    }

    /// Assert NO path connects — the 0-control. Used to prove a language gate or
    /// a guard actually holds.
    pub async fn assert_no_flow(&self, from_id: &str, to_name: &str, what: &str) {
        let to = self.node_named(to_name).await;
        let path = self
            .store()
            .find_path(from_id, &to.id, FLOW_KINDS)
            .await
            .expect("find_path");
        assert!(path.is_none(), "expected NO flow — {what}");
    }

    /// The single node with this name. Ambiguity is a broken fixture, not a
    /// result.
    pub async fn node_named(&self, name: &str) -> Node {
        let mut hits = self.store().get_nodes_by_name(name).await.expect("by name");
        assert_eq!(
            hits.len(),
            1,
            "fixture must hold exactly one symbol named '{name}' (found {})",
            hits.len()
        );
        hits.remove(0)
    }

    /// The one route with these semantics — via the **indexed** query, never by
    /// parsing an id.
    pub async fn route(&self, framework: &str, method: Option<&str>, path: &str) -> Node {
        let mut hits = self
            .store()
            .find_route(Some(framework), method, path)
            .await
            .expect("find_route");
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one {framework} route {method:?} '{path}', found {}: {:?}",
            hits.len(),
            hits.iter().map(|n| n.name.clone()).collect::<Vec<_>>()
        );
        hits.remove(0)
    }

    /// Every SYNTHESIZED edge in the graph (`provenance: heuristic`).
    ///
    /// The control fixtures assert this is EMPTY. That is the precision half of the
    /// coverage gate: every positive flow assertion is satisfied by a synthesizer that
    /// bridges everything in sight, and only a control catches one that guesses.
    pub async fn synthesized_edges(&self) -> Vec<Edge> {
        let mut nodes: Vec<Node> = Vec::new();
        for kind in NodeKind::ALL {
            nodes.extend(self.store().get_nodes_by_kind(kind).await.expect("by kind"));
        }
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let out = self
            .store()
            .outgoing_batch(&ids, &EdgeKind::ALL)
            .await
            .expect("outgoing");
        let mut edges: Vec<Edge> = out
            .into_values()
            .flatten()
            .map(|n| n.edge)
            .filter(|e| e.provenance == Some(Provenance::Heuristic))
            .collect();
        edges.sort_by(|a, b| (&a.source, &a.target).cmp(&(&b.source, &b.target)));
        edges.dedup_by(|a, b| a.source == b.source && a.target == b.target);
        edges
    }

    /// Every node of a kind — for asserting a *node* exists at all (the yaml config
    /// keys, whose absence makes every `@Value` dangle).
    pub async fn nodes_of_kind(&self, kind: NodeKind) -> Vec<Node> {
        let mut ns = self.store().get_nodes_by_kind(kind).await.expect("by kind");
        ns.sort_by(|a, b| a.id.cmp(&b.id));
        ns
    }

    /// The nodes pointing AT this one — the direction that proves a bridge closed
    /// from the far side (does anything actually reach this config key?).
    pub async fn sources_of(&self, id: &str) -> Vec<Node> {
        let map = self
            .store()
            .incoming_batch(&[id.to_string()], FLOW_KINDS)
            .await
            .expect("incoming");
        let mut nodes: Vec<Node> = map
            .get(id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.node)
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        nodes
    }

    /// The nodes this node points at along [`FLOW_KINDS`].
    ///
    /// For the frameworks whose reference is a **precise claim**
    /// (`Controller@method`, `controller#action`), "a path exists" is not the
    /// assertion that matters — *which* node it bound to is. Two controllers both
    /// declaring `index()` is the normal case, and a bare-name bind produces a path
    /// to the WRONG one that `assert_flow` would happily accept.
    pub async fn targets_of(&self, id: &str) -> Vec<Node> {
        let map = self
            .store()
            .outgoing_batch(&[id.to_string()], FLOW_KINDS)
            .await
            .expect("outgoing");
        let mut nodes: Vec<Node> = map
            .get(id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.node)
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        nodes
    }

    /// Every route node, in a deterministic order.
    pub async fn routes(&self) -> Vec<Node> {
        let mut rs = self
            .store()
            .get_nodes_by_kind(NodeKind::Route)
            .await
            .expect("routes");
        rs.sort_by(|a, b| {
            (&a.file_path, a.start_line, &a.name).cmp(&(&b.file_path, b.start_line, &b.name))
        });
        rs
    }

    /// The route names, sorted — the cheapest way to pin "exactly these routes".
    pub async fn route_names(&self) -> Vec<String> {
        self.routes().await.into_iter().map(|n| n.name).collect()
    }
}

/// Index, then run **THE PRODUCTION DRIVER** (`resolve_and_persist_batched`).
///
/// This is what the two GATES use, and the distinction is the whole point: a gate that
/// composes the pipeline itself cannot see a driver that does not exist — and this crate
/// spent a phase with four seams whose only callers were tests. Driving production code
/// is the difference between "the library works" and "the product runs".
pub async fn index_and_drive(dir: &Path) -> Pipeline {
    let store = SurrealStore::in_memory().await.expect("in-memory store");
    store.apply_schema().await.expect("schema");
    let indexer = Indexer::new(dir.to_path_buf(), store);
    let __ix = indexer.index_all(None).await;
    let result = &__ix;
    assert!(
        result.files_indexed > 0,
        "{dir:?} indexed ZERO files — the gate would be comparing nothing"
    );
    let store = indexer.into_store();

    // Detection, emission, the ladder, the conformance passes, the caches, synthesis —
    // all of it, in the order the DRIVER declares, not the order a test remembered.
    let stats =
        selene_resolve::resolve_and_persist_in_memory(&store, dir, __ix.unresolved.clone(), None)
            .await
            .expect("the driver must never fail an index");

    let ctx = StoreContext::new(store, dir.to_path_buf())
        .await
        .expect("store context (post-drive, for queries)");
    Pipeline {
        resolver: ReferenceResolver::new(ctx),
        resolved: stats.resolved,
        synthesized: stats
            .by_method
            .get("callback-synthesis")
            .copied()
            .unwrap_or(0) as u64,
    }
}

/// [`index_and_resolve`] with the frameworks **detected** rather than injected.
///
/// The coverage gate uses this: injecting the resolver list would test the framework
/// while bypassing its `detect()`, and a framework that cannot detect itself in its own
/// fixture is a framework that emits nothing in production.
pub async fn index_and_resolve_detected(dir: &Path) -> Pipeline {
    detected_pipeline(dir, true).await
}

/// [`index_and_resolve_detected`] for a fixture that has **no framework** — the
/// synthesizer projects and the controls are plain code by construction, and demanding
/// a `detect()` hit there would be demanding the opposite of what they exist to be.
pub async fn index_and_synthesize_detected(dir: &Path) -> Pipeline {
    detected_pipeline(dir, false).await
}

async fn detected_pipeline(dir: &Path, require_framework: bool) -> Pipeline {
    let store = SurrealStore::in_memory().await.expect("in-memory store");
    store.apply_schema().await.expect("schema");
    let indexer = Indexer::new(dir.to_path_buf(), store);
    let __ix = indexer.index_all(None).await;
    let ctx = StoreContext::new(indexer.into_store(), dir.to_path_buf())
        .await
        .expect("store context");
    let detected: Vec<&'static dyn FrameworkResolver> =
        selene_resolve::frameworks::detect_frameworks(&ctx);
    if require_framework {
        assert!(
            !detected.is_empty(),
            "{dir:?}: NO framework detected in its own fixture — detect() is the first \
             hop of every flow, and it emits nothing"
        );
    }
    let store = ctx.into_store();
    drop(store);
    // …and SYNTHESIZE. The coverage gate asserts synthesizer flows, so the driver it
    // uses must run the passes — a gate whose driver skips the very thing it asserts
    // would pass by asserting nothing.
    index_resolve_and_synthesize(dir, &detected).await
}

/// Index `dir`, emit framework nodes, resolve every reference, persist the edges.
pub async fn index_and_resolve(
    dir: &Path,
    detected: &[&'static dyn FrameworkResolver],
) -> Pipeline {
    let store = SurrealStore::in_memory().await.expect("in-memory store");
    store.apply_schema().await.expect("schema");

    // (1) Real extraction — nodes, same-file edges, unresolved refs.
    let indexer = Indexer::new(dir.to_path_buf(), store);
    let __ix = indexer.index_all(None).await;
    let result = &__ix;
    assert!(
        result.files_indexed > 0,
        "the fixture indexed ZERO files — the harness would be testing nothing"
    );
    let store = indexer.into_store();

    // (2) Framework emission — BEFORE resolution.
    let ctx = StoreContext::new(store, dir.to_path_buf())
        .await
        .expect("store context");
    let stats = run_framework_extract(ctx.store(), &ctx, detected)
        .await
        .expect("framework extract must never fail an index");
    assert!(
        stats.warnings.is_empty(),
        "framework extract warned: {:?}",
        stats.warnings
    );

    // The route nodes just landed, so the context's warm caches (known_names,
    // nodes-by-name) predate them. Rebuild it, or every ref the frameworks just
    // emitted would be pre-filtered out as "names nothing".
    let store = ctx.into_store();
    let ctx = StoreContext::new(store, dir.to_path_buf())
        .await
        .expect("store context (post-emission)");

    // (3) Resolve. The resolver is sync and drives the async store through
    // `block_on`, so it must run off the runtime's worker — exactly as
    // production does under `spawn_blocking`.
    // **The references come from TWO places, and forgetting either one silently guts a flow.**
    //
    // - Extraction's refs now come home in the `IndexResult` (`index_all` no longer stages them in
    //   the store — they were a hand-off buffer between two phases of the same process).
    // - The FRAMEWORK pass writes its own, INTO THE STORE, mid-run: route→handler links cannot
    //   exist until the route nodes do.
    //
    // Taking only the store's set left every ordinary call unresolved; taking only the in-memory
    // set left every ROUTE unresolved. The dispatch gate caught the second one
    // (`flow_bare_attribute_route_to_action_to_service`: the route reached the controller and the
    // controller reached nothing), which is exactly what that gate is for — a route that reaches
    // nothing is worse than no route.
    let mut pending = __ix.unresolved.clone();
    pending.extend(
        ctx.store()
            .unresolved_pending_batch(0, 10_000)
            .await
            .expect("pending refs"),
    );

    let (resolver, edges, resolved) = tokio::task::block_in_place(move || {
        let mut resolver = ReferenceResolver::new(ctx);
        let mut hits = Vec::new();
        for r in &pending {
            if let Some(hit) = resolver.resolve_one(r) {
                hits.push(hit);
            }
        }
        let edges = resolver.create_edges(&hits);
        (resolver, edges, hits.len())
    });

    // (4) Persist.
    resolver
        .ctx()
        .store()
        .insert_edges(&edges)
        .await
        .expect("insert edges");

    Pipeline {
        resolver,
        resolved,
        synthesized: 0,
    }
}

/// [`index_and_resolve`], then **run the synthesizers**.
///
/// Synthesis runs LAST, after base edges are persisted — and it must: the
/// callback pass finds registration sites by reading the `calls` edges INTO the
/// registrar, and those edges do not exist until resolution has run. A pass
/// ordered before persistence would see an empty graph and synthesize nothing,
/// silently.
pub async fn index_resolve_and_synthesize(
    dir: &Path,
    detected: &[&'static dyn FrameworkResolver],
) -> Pipeline {
    let p = index_and_resolve(dir, detected).await;
    let count = selene_resolve::synth::run_synthesis(p.store(), p.ctx())
        .await
        .expect("synthesis must never fail an index");
    Pipeline {
        synthesized: count,
        ..p
    }
}
