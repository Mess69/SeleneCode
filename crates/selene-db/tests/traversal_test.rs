#![cfg(feature = "kv-mem")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Graph traversal contract tests (Task 8): the traversal-relevant block of
//! CodeGraph's `__tests__/graph.test.ts` ported test-by-test onto
//! `SurrealStore`'s inherent traversal methods (`src/traverse.rs`).
//!
//! Fixtures are small hand-built graphs (the same approach the TS
//! `#1086`–`#1090` regression block uses) rather than extraction round-trips,
//! so the exact parallel-edge / high-degree / container topologies are
//! deterministic. Tests that belong to `GraphQueryManager` (getContext, file
//! deps, cycles, dead code, metrics) are out of scope here (Phase 4).

use selene_core::{Edge, EdgeKind, Language, Node, NodeKind};
use selene_db::{Direction, SurrealStore, TraversalOptions};

// =============================================================================
// Fixture helpers
// =============================================================================

/// A minimal `Node` named `name` with id `"<kind>:<name>"` (ids keep the
/// load-bearing colon shape real extraction produces).
fn tnode(name: &str, kind: NodeKind) -> Node {
    Node {
        id: format!("{}:{name}", kind.as_str()),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: format!("src/{name}.ts"),
        language: Language::Typescript,
        start_line: 1,
        end_line: 10,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: None,
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: vec![],
        type_parameters: vec![],
        return_type: None,
        route_method: None,
        route_path: None,
        framework: None,
        updated_at: 0,
    }
}

fn func(name: &str) -> Node {
    tnode(name, NodeKind::Function)
}

fn edge(source: &Node, target: &Node, kind: EdgeKind) -> Edge {
    Edge {
        source: source.id.clone(),
        target: target.id.clone(),
        kind,
        metadata: None,
        line: None,
        column: None,
        provenance: None,
    }
}

fn edge_at(source: &Node, target: &Node, kind: EdgeKind, line: u32) -> Edge {
    Edge {
        line: Some(line),
        ..edge(source, target, kind)
    }
}

/// Fresh in-memory store seeded with `nodes` and `edges`.
async fn store_with(nodes: &[Node], edges: &[Edge]) -> SurrealStore {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    store.insert_nodes(nodes).await.unwrap();
    let inserted = store.insert_edges(edges).await.unwrap();
    assert_eq!(
        inserted as usize,
        edges.len(),
        "fixture edges must all insert"
    );
    store
}

/// The TS `DEFAULT_OPTIONS`: `maxDepth=Infinity, edgeKinds=[], nodeKinds=[],
/// direction='outgoing', limit=1000, includeStart=true`.
fn topts() -> TraversalOptions {
    TraversalOptions {
        max_depth: None,
        edge_kinds: vec![],
        node_kinds: vec![],
        direction: Direction::Outgoing,
        limit: 1000,
        include_start: true,
    }
}

/// Node ids of a subgraph, in visit (insertion) order.
fn node_ids(sub: &selene_db::Subgraph) -> Vec<&str> {
    sub.nodes.keys().map(String::as_str).collect()
}

// =============================================================================
// traverse() — BFS walk (graph.test.ts "traverse()" block)
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn traverse_from_start_node() {
    let main = func("main");
    let process = func("processValue");
    let format = func("formatValue");
    let store = store_with(
        &[main.clone(), process.clone(), format.clone()],
        &[
            edge_at(&main, &process, EdgeKind::Calls, 3),
            edge_at(&process, &format, EdgeKind::Calls, 8),
        ],
    )
    .await;

    let sub = store
        .traverse(
            &main.id,
            &TraversalOptions {
                max_depth: Some(2),
                ..topts()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        node_ids(&sub),
        vec![main.id.as_str(), process.id.as_str(), format.id.as_str()]
    );
    assert_eq!(sub.roots, vec![main.id.clone()]);
    assert_eq!(sub.edges.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_respects_max_depth() {
    let a = func("a");
    let b = func("b");
    let c = func("c");
    let d = func("d");
    let store = store_with(
        &[a.clone(), b.clone(), c.clone(), d.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 1),
            edge_at(&b, &c, EdgeKind::Calls, 1),
            edge_at(&c, &d, EdgeKind::Calls, 1),
        ],
    )
    .await;

    let shallow = store
        .traverse(
            &a.id,
            &TraversalOptions {
                max_depth: Some(1),
                ..topts()
            },
        )
        .await
        .unwrap();
    let deep = store
        .traverse(
            &a.id,
            &TraversalOptions {
                max_depth: Some(3),
                ..topts()
            },
        )
        .await
        .unwrap();

    assert_eq!(shallow.nodes.len(), 2, "depth 1: start + direct callee");
    assert_eq!(deep.nodes.len(), 4, "depth 3: the whole chain");
    assert!(deep.nodes.len() >= shallow.nodes.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_supports_incoming_direction() {
    let a = func("a");
    let b = func("b");
    let c = func("c");
    let store = store_with(
        &[a.clone(), b.clone(), c.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 1),
            edge_at(&b, &c, EdgeKind::Calls, 1),
        ],
    )
    .await;

    let sub = store
        .traverse(
            &c.id,
            &TraversalOptions {
                max_depth: Some(2),
                direction: Direction::Incoming,
                ..topts()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        node_ids(&sub),
        vec![c.id.as_str(), b.id.as_str(), a.id.as_str()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_unknown_start_returns_empty_subgraph() {
    let store = store_with(&[], &[]).await;
    let sub = store.traverse("function:nope", &topts()).await.unwrap();
    assert!(sub.nodes.is_empty());
    assert!(sub.edges.is_empty());
    assert!(
        sub.roots.is_empty(),
        "TS returns roots: [] for an unknown start"
    );
}

// =============================================================================
// traverse() — edge-completeness & limits (#1086–#1090 regression block)
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn traverse_keeps_parallel_edges_to_same_target_1090() {
    // A reaches B via both `calls` and `references` — two distinct edges.
    let a = func("A");
    let b = func("B");
    let store = store_with(
        &[a.clone(), b.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 1),
            edge_at(&a, &b, EdgeKind::References, 2),
        ],
    )
    .await;

    let sub = store.traverse(&a.id, &topts()).await.unwrap();

    let mut ab_kinds: Vec<&str> = sub
        .edges
        .iter()
        .filter(|e| e.source == a.id && e.target == b.id)
        .map(|e| e.kind.as_str())
        .collect();
    ab_kinds.sort_unstable();
    // Pre-fix: only the higher-priority `calls` edge survived (#1090).
    assert_eq!(ab_kinds, vec!["calls", "references"]);
    assert!(sub.nodes.contains_key(&b.id));
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_keeps_same_kind_edges_on_different_lines_1090() {
    let a = func("A");
    let b = func("B");
    let store = store_with(
        &[a.clone(), b.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 3),
            edge_at(&a, &b, EdgeKind::Calls, 7),
        ],
    )
    .await;

    let sub = store.traverse(&a.id, &topts()).await.unwrap();
    let ab: Vec<&Edge> = sub
        .edges
        .iter()
        .filter(|e| e.source == a.id && e.target == b.id)
        .collect();
    assert_eq!(ab.len(), 2, "two call sites are two distinct edges");
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_limit_never_overshoots_high_degree_1087_1088() {
    // Star: A → {B..F}. Pre-fix, all 5 neighbors were added in one adjacency
    // pass → 6 nodes despite limit 3 (#1087 BFS / #1088 DFS — the trait
    // exposes a single BFS-shaped `traverse`, so the DFS per-add-cap intent is
    // pinned through the same method).
    let a = func("A");
    let neighbors: Vec<Node> = ["B", "C", "D", "E", "F"].iter().map(|n| func(n)).collect();
    let mut nodes = vec![a.clone()];
    nodes.extend(neighbors.iter().cloned());
    let edges: Vec<Edge> = neighbors
        .iter()
        .map(|n| edge_at(&a, n, EdgeKind::Calls, 1))
        .collect();
    let store = store_with(&nodes, &edges).await;

    let sub3 = store
        .traverse(
            &a.id,
            &TraversalOptions {
                limit: 3,
                ..topts()
            },
        )
        .await
        .unwrap();
    assert!(sub3.nodes.len() <= 3, "limit 3 must never be overshot");

    let sub2 = store
        .traverse(
            &a.id,
            &TraversalOptions {
                limit: 2,
                ..topts()
            },
        )
        .await
        .unwrap();
    assert!(sub2.nodes.len() <= 2, "limit 2 must never be overshot");
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_both_direction_records_shared_edge_once() {
    // The A→B edge is encountered from both endpoints in a `both` scan; the
    // edge-identity dedup (source|target|kind|line|col) must record it once.
    let a = func("A");
    let b = func("B");
    let store = store_with(
        &[a.clone(), b.clone()],
        &[edge_at(&a, &b, EdgeKind::Calls, 1)],
    )
    .await;

    let sub = store
        .traverse(
            &a.id,
            &TraversalOptions {
                direction: Direction::Both,
                ..topts()
            },
        )
        .await
        .unwrap();

    assert_eq!(sub.nodes.len(), 2);
    assert_eq!(
        sub.edges.len(),
        1,
        "one physical edge, seen twice, recorded once"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_include_start_false_excludes_root() {
    let a = func("A");
    let b = func("B");
    let store = store_with(
        &[a.clone(), b.clone()],
        &[edge_at(&a, &b, EdgeKind::Calls, 1)],
    )
    .await;

    let sub = store
        .traverse(
            &a.id,
            &TraversalOptions {
                include_start: false,
                ..topts()
            },
        )
        .await
        .unwrap();

    assert!(!sub.nodes.contains_key(&a.id), "start excluded");
    assert!(sub.nodes.contains_key(&b.id), "neighbors still collected");
    assert_eq!(sub.roots, vec![a.id.clone()], "roots still name the start");
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_visits_contains_before_calls_under_cap() {
    // R --calls--> X (line 1) and R --contains--> Y (line 9). Adjacency order
    // is contains(0) < calls(1) < other(2), so with limit 2 the single
    // admitted neighbor must be the structural child Y despite its later line.
    let r = tnode("R", NodeKind::Class);
    let x = func("X");
    let y = tnode("Y", NodeKind::Method);
    let store = store_with(
        &[r.clone(), x.clone(), y.clone()],
        &[
            edge_at(&r, &x, EdgeKind::Calls, 1),
            edge_at(&r, &y, EdgeKind::Contains, 9),
        ],
    )
    .await;

    let sub = store
        .traverse(
            &r.id,
            &TraversalOptions {
                limit: 2,
                ..topts()
            },
        )
        .await
        .unwrap();

    assert_eq!(node_ids(&sub), vec![r.id.as_str(), y.id.as_str()]);
    assert_eq!(sub.edges.len(), 1);
    assert_eq!(sub.edges[0].kind, EdgeKind::Contains);
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_edge_kind_filter() {
    let a = func("A");
    let b = func("B");
    let c = func("C");
    let store = store_with(
        &[a.clone(), b.clone(), c.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 1),
            edge_at(&a, &c, EdgeKind::References, 2),
        ],
    )
    .await;

    let sub = store
        .traverse(
            &a.id,
            &TraversalOptions {
                edge_kinds: vec![EdgeKind::Calls],
                ..topts()
            },
        )
        .await
        .unwrap();

    assert_eq!(node_ids(&sub), vec![a.id.as_str(), b.id.as_str()]);
    assert_eq!(sub.edges.len(), 1);
    assert_eq!(sub.edges[0].kind, EdgeKind::Calls);
}

#[tokio::test(flavor = "multi_thread")]
async fn traverse_node_kind_filter() {
    let a = func("A");
    let b = func("B");
    let c = tnode("C", NodeKind::Class);
    let store = store_with(
        &[a.clone(), b.clone(), c.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 1),
            edge_at(&a, &c, EdgeKind::Calls, 2),
        ],
    )
    .await;

    let sub = store
        .traverse(
            &a.id,
            &TraversalOptions {
                node_kinds: vec![NodeKind::Function],
                ..topts()
            },
        )
        .await
        .unwrap();

    assert_eq!(node_ids(&sub), vec![a.id.as_str(), b.id.as_str()]);
    assert_eq!(
        sub.edges.len(),
        1,
        "the edge to the filtered-out class is not recorded"
    );
}

// =============================================================================
// callers() / callees() (graph.test.ts "getCallers() and getCallees()")
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn callers_returns_direct_callers() {
    let process = func("processValue");
    let format = func("formatValue");
    let store = store_with(
        &[process.clone(), format.clone()],
        &[edge_at(&process, &format, EdgeKind::Calls, 4)],
    )
    .await;

    let callers = store.callers(&format.id, 1).await.unwrap();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].node.id, process.id);
    assert_eq!(callers[0].edge.kind, EdgeKind::Calls);
}

#[tokio::test(flavor = "multi_thread")]
async fn callees_returns_direct_callees() {
    let process = func("processValue");
    let format = func("formatValue");
    let store = store_with(
        &[process.clone(), format.clone()],
        &[edge_at(&process, &format, EdgeKind::Calls, 4)],
    )
    .await;

    let callees = store.callees(&process.id, 1).await.unwrap();
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].node.id, format.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn callers_transitive_depth_two_in_dfs_order() {
    // a → b → c: callers(c, 2) walks depth-first — direct caller first, then
    // its caller — exactly the TS recursion's result order.
    let a = func("a");
    let b = func("b");
    let c = func("c");
    let store = store_with(
        &[a.clone(), b.clone(), c.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 1),
            edge_at(&b, &c, EdgeKind::Calls, 1),
        ],
    )
    .await;

    let two = store.callers(&c.id, 2).await.unwrap();
    let ids: Vec<&str> = two.iter().map(|e| e.node.id.as_str()).collect();
    assert_eq!(ids, vec![b.id.as_str(), a.id.as_str()]);

    let one = store.callers(&c.id, 1).await.unwrap();
    let ids: Vec<&str> = one.iter().map(|e| e.node.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![b.id.as_str()],
        "maxDepth=1 is direct callers only"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn instantiation_counts_as_caller_and_callee_774() {
    // main() does `new DerivedClass(...)`: constructing a class is calling its
    // constructor, so main is a caller of DerivedClass and DerivedClass is a
    // callee of main (#774 — `instantiates` is in the traversal whitelist).
    let main = func("main");
    let derived = tnode("DerivedClass", NodeKind::Class);
    let store = store_with(
        &[main.clone(), derived.clone()],
        &[edge_at(&main, &derived, EdgeKind::Instantiates, 6)],
    )
    .await;

    let callers = store.callers(&derived.id, 1).await.unwrap();
    let caller_names: Vec<&str> = callers.iter().map(|e| e.node.name.as_str()).collect();
    assert!(caller_names.contains(&"main"));

    let callees = store.callees(&main.id, 1).await.unwrap();
    let callee_names: Vec<&str> = callees.iter().map(|e| e.node.name.as_str()).collect();
    assert!(callee_names.contains(&"DerivedClass"));
}

#[tokio::test(flavor = "multi_thread")]
async fn callers_dedup_at_depth_boundary_1086() {
    // Y calls X at two sites and also references it — three incoming edges.
    // Pre-fix, Y appeared three times (the depth guard returned before
    // visited.add). Each caller appears once, paired with the first edge in
    // deterministic adjacency order (calls < references, then line).
    let x = func("X");
    let y = func("Y");
    let store = store_with(
        &[x.clone(), y.clone()],
        &[
            edge_at(&y, &x, EdgeKind::Calls, 1),
            edge_at(&y, &x, EdgeKind::Calls, 2),
            edge_at(&y, &x, EdgeKind::References, 3),
        ],
    )
    .await;

    let callers = store.callers(&x.id, 1).await.unwrap();
    let ids: Vec<&str> = callers.iter().map(|e| e.node.id.as_str()).collect();
    assert_eq!(ids, vec![y.id.as_str()], "Y must appear exactly once");
    assert_eq!(callers[0].edge.kind, EdgeKind::Calls);
    assert_eq!(
        callers[0].edge.line,
        Some(1),
        "paired with the first edge deterministically"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn callees_dedup_at_depth_boundary_1086() {
    let x = func("X");
    let y = func("Y");
    let store = store_with(
        &[x.clone(), y.clone()],
        &[
            edge_at(&x, &y, EdgeKind::Calls, 1),
            edge_at(&x, &y, EdgeKind::Calls, 2),
        ],
    )
    .await;

    let callees = store.callees(&x.id, 1).await.unwrap();
    let ids: Vec<&str> = callees.iter().map(|e| e.node.id.as_str()).collect();
    assert_eq!(ids, vec![y.id.as_str()], "Y must appear exactly once");
}

#[tokio::test(flavor = "multi_thread")]
async fn callers_unknown_id_is_empty_not_error() {
    let store = store_with(&[], &[]).await;
    assert!(store.callers("function:nope", 3).await.unwrap().is_empty());
    assert!(store.callees("function:nope", 3).await.unwrap().is_empty());
}

// =============================================================================
// impact_radius() (graph.test.ts "getImpactRadius()")
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn impact_radius_includes_focal_and_transitive_dependents() {
    let main = func("main");
    let process = func("processValue");
    let format = func("formatValue");
    let store = store_with(
        &[main.clone(), process.clone(), format.clone()],
        &[
            edge_at(&main, &process, EdgeKind::Calls, 3),
            edge_at(&process, &format, EdgeKind::Calls, 8),
        ],
    )
    .await;

    let impact = store.impact_radius(&format.id, 3).await.unwrap();
    assert!(
        impact.nodes.contains_key(&format.id),
        "focal always present"
    );
    assert!(impact.nodes.contains_key(&process.id), "direct caller");
    assert!(impact.nodes.contains_key(&main.id), "transitive dependent");
    assert_eq!(impact.roots, vec![format.id.clone()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn impact_does_not_drag_in_siblings_via_contains_536() {
    // DerivedClass contains getName and print; print calls getName. The
    // containing class must NOT be pulled into impact just because it
    // *contains* getName — climbing that edge would re-expand every sibling.
    let derived = tnode("DerivedClass", NodeKind::Class);
    let get_name = tnode("getName", NodeKind::Method);
    let print = tnode("print", NodeKind::Method);
    let store = store_with(
        &[derived.clone(), get_name.clone(), print.clone()],
        &[
            edge(&derived, &get_name, EdgeKind::Contains),
            edge(&derived, &print, EdgeKind::Contains),
            edge_at(&print, &get_name, EdgeKind::Calls, 12),
        ],
    )
    .await;

    let impact = store.impact_radius(&get_name.id, 3).await.unwrap();
    assert!(
        !impact.nodes.contains_key(&derived.id),
        "container must not enter impact via its contains edge (#536)"
    );
    assert!(impact.nodes.contains_key(&print.id), "a real caller does");
}

#[tokio::test(flavor = "multi_thread")]
async fn impact_keeps_direct_edge_into_already_collected_node_1089() {
    // Class P contains method M. Q calls both M and P. Reaching M first
    // collects Q; the pre-fix `!nodes.has()` gate then dropped the direct
    // Q→P edge from `edges` even though it's a real dependency.
    let p = tnode("P", NodeKind::Class);
    let m = tnode("M", NodeKind::Method);
    let q = func("Q");
    let store = store_with(
        &[p.clone(), m.clone(), q.clone()],
        &[
            edge(&p, &m, EdgeKind::Contains),
            edge_at(&q, &m, EdgeKind::Calls, 1),
            edge_at(&q, &p, EdgeKind::Calls, 2),
        ],
    )
    .await;

    let impact = store.impact_radius(&p.id, 2).await.unwrap();

    assert!(impact.nodes.contains_key(&q.id));
    assert!(
        impact
            .edges
            .iter()
            .any(|e| e.source == q.id && e.target == m.id && e.kind == EdgeKind::Calls)
    );
    // The regression: this direct dependency edge used to vanish.
    assert!(
        impact
            .edges
            .iter()
            .any(|e| e.source == q.id && e.target == p.id && e.kind == EdgeKind::Calls)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn impact_container_children_join_at_the_same_depth() {
    // C contains M; X calls M. At maxDepth=1 the contains child M is expanded
    // at the container's own depth (not one hop deeper), so M's caller X is
    // still within radius 1.
    let c = tnode("C", NodeKind::Class);
    let m = tnode("M", NodeKind::Method);
    let x = func("X");
    let store = store_with(
        &[c.clone(), m.clone(), x.clone()],
        &[
            edge(&c, &m, EdgeKind::Contains),
            edge_at(&x, &m, EdgeKind::Calls, 5),
        ],
    )
    .await;

    let impact = store.impact_radius(&c.id, 1).await.unwrap();
    assert_eq!(
        node_ids(&impact),
        vec![c.id.as_str(), m.id.as_str(), x.id.as_str()]
    );
    assert_eq!(
        impact.edges.len(),
        2,
        "the contains edge and the calls edge"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn impact_unknown_id_is_empty_subgraph() {
    let store = store_with(&[], &[]).await;
    let impact = store.impact_radius("function:nope", 3).await.unwrap();
    assert!(impact.nodes.is_empty());
    assert!(impact.edges.is_empty());
    assert!(impact.roots.is_empty());
}

// =============================================================================
// find_path() (graph.test.ts "findPath()")
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn find_path_between_connected_nodes() {
    let process = func("processValue");
    let format = func("formatValue");
    let store = store_with(
        &[process.clone(), format.clone()],
        &[edge_at(&process, &format, EdgeKind::Calls, 4)],
    )
    .await;

    let path = store
        .find_path(&process.id, &format.id, &[])
        .await
        .unwrap()
        .expect("connected nodes must have a path");

    assert_eq!(path.len(), 2);
    assert_eq!(path[0].0.id, process.id);
    assert!(path[0].1.is_none(), "the first step has no inbound edge");
    assert_eq!(path[1].0.id, format.id);
    assert_eq!(path[1].1.as_ref().map(|e| e.kind), Some(EdgeKind::Calls));
}

#[tokio::test(flavor = "multi_thread")]
async fn find_path_returns_shortest_of_competing_paths() {
    // a→b→c (two hops) and a→c direct (one hop): BFS must return the direct
    // 2-node path even though the `references` edge sorts after `calls`.
    let a = func("a");
    let b = func("b");
    let c = func("c");
    let store = store_with(
        &[a.clone(), b.clone(), c.clone()],
        &[
            edge_at(&a, &b, EdgeKind::Calls, 1),
            edge_at(&b, &c, EdgeKind::Calls, 1),
            edge_at(&a, &c, EdgeKind::References, 9),
        ],
    )
    .await;

    let path = store
        .find_path(&a.id, &c.id, &[])
        .await
        .unwrap()
        .expect("path exists");
    let ids: Vec<&str> = path.iter().map(|(n, _)| n.id.as_str()).collect();
    assert_eq!(ids, vec![a.id.as_str(), c.id.as_str()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn find_path_returns_none_for_disconnected_or_unknown() {
    let a = func("a");
    let b = func("b");
    // No edges at all: a and b are disconnected.
    let store = store_with(&[a.clone(), b.clone()], &[]).await;

    assert!(store.find_path(&a.id, &b.id, &[]).await.unwrap().is_none());
    // TS: findPath('non-existent-1', 'non-existent-2') → null, not an error.
    assert!(
        store
            .find_path("non-existent-1", "non-existent-2", &[])
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn find_path_respects_edge_kind_filter() {
    let a = func("a");
    let b = func("b");
    let store = store_with(
        &[a.clone(), b.clone()],
        &[edge_at(&a, &b, EdgeKind::References, 1)],
    )
    .await;

    assert!(
        store
            .find_path(&a.id, &b.id, &[EdgeKind::Calls])
            .await
            .unwrap()
            .is_none(),
        "kind filter excludes the only connecting edge"
    );
    assert!(store.find_path(&a.id, &b.id, &[]).await.unwrap().is_some());
}

// =============================================================================
// type_hierarchy() (graph.test.ts "getTypeHierarchy()")
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn type_hierarchy_collects_ancestors_and_descendants() {
    // Root ← Base ← Derived (extends chain), Derived implements Printable,
    // Third extends Derived. The hierarchy of Derived is the union of its
    // transitive ancestors AND its descendants (the trait-doc contract; see
    // src/traverse.rs on the TS root-visited quirk this deliberately fixes).
    let root = tnode("Root", NodeKind::Class);
    let base = tnode("Base", NodeKind::Class);
    let derived = tnode("Derived", NodeKind::Class);
    let printable = tnode("Printable", NodeKind::Interface);
    let third = tnode("Third", NodeKind::Class);
    let store = store_with(
        &[
            root.clone(),
            base.clone(),
            derived.clone(),
            printable.clone(),
            third.clone(),
        ],
        &[
            edge(&base, &root, EdgeKind::Extends),
            edge(&derived, &base, EdgeKind::Extends),
            edge(&derived, &printable, EdgeKind::Implements),
            edge(&third, &derived, EdgeKind::Extends),
        ],
    )
    .await;

    let sub = store.type_hierarchy(&derived.id).await.unwrap();

    assert!(sub.nodes.contains_key(&derived.id), "focal");
    assert!(sub.nodes.contains_key(&base.id), "direct ancestor");
    assert!(sub.nodes.contains_key(&root.id), "transitive ancestor");
    assert!(
        sub.nodes.contains_key(&printable.id),
        "implemented interface"
    );
    assert!(sub.nodes.contains_key(&third.id), "descendant");
    assert_eq!(sub.nodes.len(), 5);
    assert_eq!(sub.edges.len(), 4);
    assert_eq!(sub.roots, vec![derived.id.clone()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn type_hierarchy_missing_node_returns_empty_subgraph() {
    let store = store_with(&[], &[]).await;
    let sub = store.type_hierarchy("class:non-existent").await.unwrap();
    assert_eq!(sub.nodes.len(), 0);
    assert_eq!(sub.edges.len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn type_hierarchy_terminates_on_extends_cycle() {
    // A extends B, B extends A: the shared visited set must stop the walk;
    // the back-edge into an already-collected node is not re-collected.
    let a = tnode("A", NodeKind::Class);
    let b = tnode("B", NodeKind::Class);
    let store = store_with(
        &[a.clone(), b.clone()],
        &[
            edge(&a, &b, EdgeKind::Extends),
            edge(&b, &a, EdgeKind::Extends),
        ],
    )
    .await;

    let sub = store.type_hierarchy(&a.id).await.unwrap();
    assert_eq!(sub.nodes.len(), 2);
    assert_eq!(sub.edges.len(), 1, "only the A→B ancestor edge is recorded");
}

// =============================================================================
// ancestors() / children() (graph.test.ts "getAncestors() and getChildren()")
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn ancestors_climbs_the_contains_chain() {
    // file contains class contains method: ancestors(method) goes from the
    // immediate parent up to the root.
    let file = tnode("mod", NodeKind::File);
    let class = tnode("Widget", NodeKind::Class);
    let method = tnode("render", NodeKind::Method);
    let store = store_with(
        &[file.clone(), class.clone(), method.clone()],
        &[
            edge(&file, &class, EdgeKind::Contains),
            edge(&class, &method, EdgeKind::Contains),
        ],
    )
    .await;

    let ancestors = store.ancestors(&method.id).await.unwrap();
    let ids: Vec<&str> = ancestors.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec![class.id.as_str(), file.id.as_str()]);

    assert!(store.ancestors("function:nope").await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ancestors_terminates_on_contains_cycle() {
    // A contains B, B contains A. The TS cycle set breaks the climb after the
    // walk re-reaches A: ancestors(A) = [B, A].
    let a = tnode("A", NodeKind::Class);
    let b = tnode("B", NodeKind::Class);
    let store = store_with(
        &[a.clone(), b.clone()],
        &[
            edge(&a, &b, EdgeKind::Contains),
            edge(&b, &a, EdgeKind::Contains),
        ],
    )
    .await;

    let ancestors = store.ancestors(&a.id).await.unwrap();
    let ids: Vec<&str> = ancestors.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec![b.id.as_str(), a.id.as_str()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn children_returns_direct_contains_targets_only() {
    let class = tnode("Widget", NodeKind::Class);
    let m1 = tnode("later", NodeKind::Method);
    let m2 = tnode("earlier", NodeKind::Method);
    let helper = func("helper");
    let store = store_with(
        &[class.clone(), m1.clone(), m2.clone(), helper.clone()],
        &[
            edge_at(&class, &m1, EdgeKind::Contains, 5),
            edge_at(&class, &m2, EdgeKind::Contains, 3),
            edge_at(&class, &helper, EdgeKind::Calls, 4),
        ],
    )
    .await;

    let children = store.children(&class.id).await.unwrap();
    let ids: Vec<&str> = children.iter().map(|n| n.id.as_str()).collect();
    // Deterministic adjacency order: same kind rank → by line.
    assert_eq!(ids, vec![m2.id.as_str(), m1.id.as_str()]);

    assert!(store.children(&m1.id).await.unwrap().is_empty());
    assert!(store.children("function:nope").await.unwrap().is_empty());
}

// =============================================================================
// Task 9d — frontier-pruning regression fence (dense fan-in)
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn callers_dense_fan_in_output_correct() {
    // A dense fan-in shape where naive frontier re-expansion would be
    // quadratic: 20 callers (A*) that each call all 15 intermediate targets
    // (B*), which each call the hub. This test pins the *observable output*
    // on that shape — the exact TS DFS replay order (B01's entire caller
    // subtree A01..A20, by edge line, before B02..B15, which by then
    // contribute nothing new) and each node exactly once (#1086 dedup) —
    // so any future frontier/prefetch change is fenced by the worst-case
    // topology. (That the frontier sends each node once is a fetch-level
    // property, evidenced by the §5.3 probe instrumentation in
    // docs/benchmarks/2026-07-phase1-db-gate.md, not asserted here.)
    let hub = func("hub");
    let bs: Vec<Node> = (1..=15).map(|j| func(&format!("B{j:02}"))).collect();
    let callers_a: Vec<Node> = (1..=20).map(|i| func(&format!("A{i:02}"))).collect();

    let mut nodes = vec![hub.clone()];
    nodes.extend(bs.iter().cloned());
    nodes.extend(callers_a.iter().cloned());

    let mut edges = Vec::new();
    for (j, b) in bs.iter().enumerate() {
        edges.push(edge_at(
            b,
            &hub,
            EdgeKind::Calls,
            u32::try_from(j + 1).unwrap(),
        ));
    }
    for (i, a) in callers_a.iter().enumerate() {
        for b in &bs {
            edges.push(edge_at(
                a,
                b,
                EdgeKind::Calls,
                u32::try_from(i + 1).unwrap(),
            ));
        }
    }
    let store = store_with(&nodes, &edges).await;

    let d3 = store.callers(&hub.id, 3).await.unwrap();
    assert_eq!(d3.len(), 35, "15 B + 20 A, each exactly once");
    let ids: Vec<&str> = d3.iter().map(|e| e.node.id.as_str()).collect();
    let mut expected: Vec<&str> = vec![bs[0].id.as_str()];
    expected.extend(callers_a.iter().map(|a| a.id.as_str()));
    expected.extend(bs[1..].iter().map(|b| b.id.as_str()));
    assert_eq!(ids, expected, "exact DFS replay order");

    // Each entry is paired with the first edge that reached it.
    assert_eq!(d3[0].edge.source, bs[0].id);
    assert_eq!(d3[0].edge.target, hub.id);
    assert_eq!(d3[1].edge.source, callers_a[0].id);
    assert_eq!(d3[1].edge.target, bs[0].id);

    let d1 = store.callers(&hub.id, 1).await.unwrap();
    assert_eq!(d1.len(), 15, "depth 1 is the direct callers only");
}
