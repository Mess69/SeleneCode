#![cfg(all(feature = "kv-mem", feature = "bench-support"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Determinism + shape-sanity tests for the `bench-support` synthetic graph
//! generator (`src/bench_support.rs`). These guard the two properties the
//! §5.3 benches depend on: same seed ⇒ identical output, and the generated
//! graph actually has the deep chain / hub / FTS overlap the benches target.

use std::collections::{HashMap, HashSet};

use selene_core::{Edge, EdgeKind, Node};
use selene_db::bench_support::{CHAIN_LEN, HUB_FAN_IN, LANGS, SyntheticGraph};

/// Same `(seed, nodes)` must produce byte-identical nodes AND edges.
#[test]
fn same_seed_same_output() {
    let (n1, e1) = SyntheticGraph::generate(42, 5_000);
    let (n2, e2) = SyntheticGraph::generate(42, 5_000);
    assert_eq!(n1, n2, "nodes must be identical for the same seed");
    assert_eq!(e1, e2, "edges must be identical for the same seed");
}

/// A different seed must produce a different graph (otherwise the seed is
/// dead and determinism above is trivially true).
#[test]
fn different_seed_different_output() {
    let (_, e1) = SyntheticGraph::generate(1, 5_000);
    let (_, e2) = SyntheticGraph::generate(2, 5_000);
    assert_ne!(e1, e2, "different seeds should diverge");
}

/// Node ids are unique (they are the store's primary key).
#[test]
fn node_ids_are_unique() {
    let (nodes, _) = SyntheticGraph::generate(7, 10_000);
    let unique: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(unique.len(), nodes.len(), "duplicate node id generated");
}

/// Every edge endpoint references a real node id (so `insert_edges` doesn't
/// silently drop the whole graph as missing-endpoint).
#[test]
fn edge_endpoints_all_exist() {
    let (nodes, edges) = SyntheticGraph::generate(9, 8_000);
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    for e in &edges {
        assert!(ids.contains(e.source.as_str()), "dangling source {}", e.source);
        assert!(ids.contains(e.target.as_str()), "dangling target {}", e.target);
    }
}

/// Rough size + edge-density sanity: ~5 edges/node, all four languages present,
/// ~30% docstrings, node count within range of the request.
#[test]
fn shape_counts_are_realistic() {
    let requested = 20_000;
    let (nodes, edges, landmarks) = SyntheticGraph::generate_with_landmarks(123, requested);

    // Node count tracks the request (files + symbols == requested).
    assert_eq!(nodes.len(), requested, "node count should match request");

    // ~5 edges/node (accept 3..8 to leave headroom for the RNG mix).
    let ratio = edges.len() as f64 / nodes.len() as f64;
    assert!(ratio >= 3.0 && ratio <= 9.0, "edges/node = {ratio:.2}, want ~5");

    // All four languages appear.
    let langs: HashSet<&str> = nodes.iter().map(|n| n.language.as_str()).collect();
    for l in LANGS {
        assert!(langs.contains(l), "language {l} missing");
    }

    // ~30% docstrings (accept a wide band).
    let with_doc = nodes.iter().filter(|n| n.docstring.is_some()).count();
    let doc_frac = with_doc as f64 / nodes.len() as f64;
    assert!(doc_frac >= 0.15 && doc_frac <= 0.45, "docstring frac = {doc_frac:.2}");

    // FTS term is present in many node names (analyzer splits camelCase).
    let term = landmarks.fts_term.to_lowercase();
    let hits = nodes
        .iter()
        .filter(|n| n.name.to_lowercase().contains(&term))
        .count();
    assert!(hits > 50, "FTS term '{term}' only in {hits} names, want broad overlap");
}

/// The reserved clean corridor is a genuine deep chain: head reaches tail in
/// exactly `CHAIN_LEN-1` `calls` hops, and nothing else points into the
/// corridor nodes (so it stays a clean deep path for find_path/impact).
#[test]
fn deep_chain_exists_and_is_clean() {
    let (nodes, edges, landmarks) = SyntheticGraph::generate_with_landmarks(55, 12_000);
    let by_id: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    assert!(by_id.contains_key(landmarks.deep_head_id.as_str()));
    assert!(by_id.contains_key(landmarks.deep_tail_id.as_str()));

    // Build a forward `calls` adjacency restricted to the corridor and walk it.
    let mut calls_out: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &edges {
        if e.kind == EdgeKind::Calls {
            calls_out.entry(e.source.as_str()).or_default().push(e.target.as_str());
        }
    }
    let hops = shortest_calls_hops(&calls_out, &landmarks.deep_head_id, &landmarks.deep_tail_id);
    assert_eq!(
        hops,
        Some((CHAIN_LEN - 1) as u32),
        "corridor head->tail should be exactly {} hops",
        CHAIN_LEN - 1
    );
}

/// The hub really does have >= HUB_FAN_IN direct callers (fan-out mix).
#[test]
fn hub_has_high_fan_in() {
    let (_, edges, landmarks) = SyntheticGraph::generate_with_landmarks(88, 20_000);
    let direct_callers = edges
        .iter()
        .filter(|e| e.target == landmarks.hub_id && call_like(e))
        .count();
    assert!(
        direct_callers >= HUB_FAN_IN,
        "hub has {direct_callers} direct callers, want >= {HUB_FAN_IN}"
    );
}

fn call_like(e: &Edge) -> bool {
    matches!(
        e.kind,
        EdgeKind::Calls | EdgeKind::References | EdgeKind::Imports | EdgeKind::Instantiates
    )
}

/// BFS shortest-hop over a `calls` adjacency (small, corridor-scoped).
fn shortest_calls_hops(
    calls_out: &HashMap<&str, Vec<&str>>,
    from: &str,
    to: &str,
) -> Option<u32> {
    let mut frontier = vec![from];
    let mut seen: HashSet<&str> = HashSet::from([from]);
    let mut depth = 0u32;
    while !frontier.is_empty() {
        if frontier.iter().any(|n| *n == to) {
            return Some(depth);
        }
        let mut next = Vec::new();
        for n in frontier {
            for &nb in calls_out.get(n).into_iter().flatten() {
                if seen.insert(nb) {
                    next.push(nb);
                }
            }
        }
        frontier = next;
        depth += 1;
        if depth > 1000 {
            return None;
        }
    }
    None
}
