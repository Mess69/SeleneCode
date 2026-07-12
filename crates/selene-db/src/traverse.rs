//! Graph traversals: inherent [`SurrealStore`] methods mirroring the
//! TRAVERSAL section of [`crate::GraphStore`] (Task 8). As with the other
//! operation modules, `impl GraphStore for SurrealStore` is wired later
//! (Task 10); until then these are plain inherent `async fn`s with identical
//! signatures.
//!
//! ## Frontier-batched hybrid, not SurrealQL recursion
//!
//! The traversal **loops live in Rust**: they carry the CodeGraph TS
//! traverser's subtle visited/enqueued/cap/dedup semantics (`#536`, `#774`,
//! `#1086`–`#1090` — see
//! `docs/reference/from-codegraph/maps/db-graph-search.md`), which SurrealQL's
//! recursive `.{1..n}` syntax cannot express (separate visited-on-dequeue vs
//! enqueued-on-enqueue sets, per-add node caps, container children joining at
//! the *same* depth, unconditional dependency-edge recording). But **every
//! adjacency expansion is a batched store query over the whole frontier**
//! (the edges-only [`SurrealStore::outgoing_edges`]/
//! [`SurrealStore::incoming_edges`] plus a walk-long node-payload cache —
//! see [`SurrealStore::level_entries`]) — never a per-node query in a loop —
//! so a traversal costs O(depth) round-trip pairs regardless of frontier
//! width, and each distinct node's payload crosses the wire at most once per
//! walk (Task 9d: cross-level payload re-fetch was nearly half the node
//! traffic of a hub-rooted depth-3 callers prefetch). Pure-SurrealQL
//! one-shots remain a future optimization behind these same method
//! signatures.
//!
//! ## No recursion
//!
//! The TS originals (`getCallersRecursive`, `getImpactRecursive`,
//! `getTypeAncestors`/`getTypeDescendants`, `dfsRecursive`) are
//! stack-recursive; deep graphs could overflow. Every depth-first-shaped walk
//! here uses an explicit frame stack (the map's "Recursion → iteration" port
//! note), replaying the exact TS visit order over adjacency that was
//! prefetched level-by-level. The prefetch is a superset cover: a node's
//! shortest-hop (BFS) depth never exceeds the depth the DFS replay first
//! enters it at, so prefetching levels `0..max_depth` covers every node the
//! replay can expand.
//!
//! ## Deterministic adjacency order
//!
//! TS sorted BFS adjacency `contains(0) < calls(1) < other(2)` (stable within
//! a rank on SQLite scan order, which is not portable). Here **every**
//! adjacency list is ordered by `(kind rank, edge line ?? -1, neighbor id,
//! kind wire string, edge col ?? -1)`. The rank is the TS contract; the
//! remaining keys are implementation-defined-but-stable tiebreakers so
//! results are reproducible across runs and backends.
//!
//! ## Deliberate divergence: `type_hierarchy` descendants
//!
//! In the TS source, `getTypeAncestors(root)` marks the root visited, so the
//! subsequent `getTypeDescendants(root)` call hits its own
//! `if (visited.has(nodeId)) return` entry guard immediately — descendants
//! were **never collected** (latent dead code). Both the trait doc (binding)
//! and the subsystem map ("descendants via incoming") specify the
//! ancestors + descendants union, so [`SurrealStore::type_hierarchy`] lets
//! the descendants pass expand the root despite the shared visited set.
//! Everything else (the single shared visited set, the `!nodes.has`
//! collection guard) is ported verbatim. Flagged in
//! `.superpowers/sdd/task-8-report.md`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use selene_core::{Edge, EdgeKind, Node, NodeKind};

use crate::{Direction, NeighborEntry, Result, Subgraph, SurrealStore, TraversalOptions};

/// The callers/callees edge-kind whitelist (`#774`: instantiation counts as a
/// call). A load-bearing constant — see the [`crate::GraphStore`] trait docs.
const CALL_EDGE_KINDS: [EdgeKind; 4] = [
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Imports,
    EdgeKind::Instantiates,
];

/// The type-hierarchy edge kinds: outgoing = ancestors, incoming = descendants.
const HIERARCHY_EDGE_KINDS: [EdgeKind; 2] = [EdgeKind::Extends, EdgeKind::Implements];

/// Container kinds whose `contains` children join the impact radius at the
/// **same** depth as the container itself (`#536`).
fn is_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Module
            | NodeKind::Enum
    )
}

/// The TS BFS adjacency priority: structural edges first so a walk discovers
/// internal structure before fanning out to references.
fn kind_rank(kind: EdgeKind) -> u8 {
    match kind {
        EdgeKind::Contains => 0,
        EdgeKind::Calls => 1,
        _ => 2,
    }
}

/// Total order for one node's adjacency list. `(rank, line)` is the TS
/// contract surface; `(neighbor id, kind, col)` are the stable tiebreakers
/// (see the module docs).
fn entry_key(e: &NeighborEntry) -> (u8, i64, &str, &'static str, i64) {
    (
        kind_rank(e.edge.kind),
        e.edge.line.map_or(-1, i64::from),
        e.node.id.as_str(),
        e.edge.kind.as_str(),
        e.edge.column.map_or(-1, i64::from),
    )
}

fn sort_entries(entries: &mut [NeighborEntry]) {
    entries.sort_by(|a, b| entry_key(a).cmp(&entry_key(b)));
}

/// The edge identity used for result-set dedup — the same
/// `(source, target, kind, line ?? -1, col ?? -1)` key the storage layer's
/// unique index folds on, so a `Direction::Both` scan that encounters an edge
/// from both endpoints records it once while parallel edges survive (`#1090`).
fn edge_identity(e: &Edge) -> (String, String, EdgeKind, i64, i64) {
    (
        e.source.clone(),
        e.target.clone(),
        e.kind,
        e.line.map_or(-1, i64::from),
        e.column.map_or(-1, i64::from),
    )
}

fn empty_subgraph() -> Subgraph {
    Subgraph {
        nodes: IndexMap::new(),
        edges: Vec::new(),
        roots: Vec::new(),
    }
}

/// Which adjacency direction a prefetch walks.
#[derive(Clone, Copy)]
enum Adj {
    Outgoing,
    Incoming,
}

/// One frame of the callers/callees depth-first replay.
struct CallFrame {
    entries: Vec<NeighborEntry>,
    idx: usize,
    depth: u32,
}

/// One frame of a type-hierarchy (ancestors or descendants) replay.
struct WalkFrame {
    entries: Vec<NeighborEntry>,
    idx: usize,
}

/// One frame of the impact-radius replay: the `contains` children are drained
/// first (at the owner's depth), then the non-`contains` incoming edges.
struct ImpactFrame {
    contains: Vec<NeighborEntry>,
    ci: usize,
    incoming: Vec<NeighborEntry>,
    ii: usize,
    depth: u32,
}

/// The exact TS `getCallersRecursive`/`getCalleesRecursive` walk, replayed
/// over prefetched adjacency with an explicit stack. Each reachable node
/// appears in the result **once**, paired with the first edge that reached it
/// in deterministic adjacency order; a node is marked visited *before* its
/// depth is checked, so a node collected at the depth boundary cannot be
/// re-collected through a parallel edge (`#1086`).
fn replay_call_walk(
    root: &str,
    max_depth: u32,
    adjacency: &mut HashMap<String, Vec<NeighborEntry>>,
) -> Vec<NeighborEntry> {
    let mut result: Vec<NeighborEntry> = Vec::new();
    let mut visited: HashSet<String> = HashSet::from([root.to_string()]);
    if max_depth == 0 {
        return result;
    }
    let mut stack = vec![CallFrame {
        entries: adjacency.remove(root).unwrap_or_default(),
        idx: 0,
        depth: 0,
    }];
    while let Some(frame) = stack.last_mut() {
        if frame.idx >= frame.entries.len() {
            stack.pop();
            continue;
        }
        let entry = frame.entries[frame.idx].clone();
        frame.idx += 1;
        let child_depth = frame.depth + 1;
        if visited.insert(entry.node.id.clone()) {
            let child_id = entry.node.id.clone();
            result.push(entry);
            if child_depth < max_depth {
                stack.push(CallFrame {
                    entries: adjacency.remove(&child_id).unwrap_or_default(),
                    idx: 0,
                    depth: child_depth,
                });
            }
        }
    }
    result
}

/// The exact TS `getTypeAncestors`/`getTypeDescendants` walk (they are
/// symmetric given the adjacency direction), replayed with an explicit
/// stack. Collection is gated on `nodes` membership, recursion on the shared
/// `visited` set — ported verbatim, except that the *root* is always
/// expanded (see the module docs' divergence note).
fn replay_hierarchy_walk(
    root: &str,
    adjacency: &mut HashMap<String, Vec<NeighborEntry>>,
    nodes: &mut IndexMap<String, Node>,
    edges: &mut Vec<Edge>,
    visited: &mut HashSet<String>,
) {
    visited.insert(root.to_string());
    let mut stack = vec![WalkFrame {
        entries: adjacency.remove(root).unwrap_or_default(),
        idx: 0,
    }];
    while let Some(frame) = stack.last_mut() {
        if frame.idx >= frame.entries.len() {
            stack.pop();
            continue;
        }
        let entry = frame.entries[frame.idx].clone();
        frame.idx += 1;
        if !nodes.contains_key(&entry.node.id) {
            let child_id = entry.node.id.clone();
            nodes.insert(child_id.clone(), entry.node);
            edges.push(entry.edge);
            if visited.insert(child_id.clone()) {
                stack.push(WalkFrame {
                    entries: adjacency.remove(&child_id).unwrap_or_default(),
                    idx: 0,
                });
            }
        }
    }
}

impl SurrealStore {
    /// One prefetch level's adjacency over `ids`: the edges-only batch fetch
    /// ([`SurrealStore::outgoing_edges`]/[`SurrealStore::incoming_edges`])
    /// with neighbor payloads attached from — and the misses fetched into —
    /// `node_cache`, grouped by the queried id.
    ///
    /// Task 9d frontier pruning: attaching from a per-traversal cache means a
    /// node's payload crosses the wire **once per traversal**, not once per
    /// level that reaches it — the §5.3 probe measured 6,834 cross-level
    /// payload re-fetches (of 17,074 distinct nodes) in a single depth-3
    /// hub-rooted callers prefetch on the 20k-node bench graph. An edge whose
    /// neighbor id is missing from the node table is dropped, exactly like
    /// `edges.rs`' `attach_neighbors` (success-shaped-miss contract).
    async fn level_entries(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
        dir: Adj,
        node_cache: &mut HashMap<String, Node>,
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        let edges = match dir {
            Adj::Outgoing => self.outgoing_edges(ids, kinds).await?,
            Adj::Incoming => self.incoming_edges(ids, kinds).await?,
        };
        // Outgoing: the neighbor is the edge's target, the queried id its
        // source; incoming is the mirror (same mapping as `edges.rs`'
        // `EdgeEndpoint`).
        let neighbor_of = |e: &Edge| match dir {
            Adj::Outgoing => e.target.clone(),
            Adj::Incoming => e.source.clone(),
        };
        let mut missing: Vec<String> = edges
            .iter()
            .map(&neighbor_of)
            .filter(|id| !node_cache.contains_key(id))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        if !missing.is_empty() {
            node_cache.extend(self.get_nodes(&missing).await?);
        }
        let mut out: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
        for edge in edges {
            let Some(node) = node_cache.get(&neighbor_of(&edge)).cloned() else {
                continue;
            };
            let key = match dir {
                Adj::Outgoing => edge.source.clone(),
                Adj::Incoming => edge.target.clone(),
            };
            out.entry(key)
                .or_default()
                .push(NeighborEntry { node, edge });
        }
        Ok(out)
    }

    /// Level-batched adjacency prefetch from `root` over `kinds` in `dir`:
    /// one edges-batch (+ cache-miss node fetch) round trip pair per level
    /// ([`Self::level_entries`]), up to `max_levels` levels (pass `u32::MAX`
    /// for "until the frontier empties"). Each node's entry list is sorted
    /// per [`entry_key`].
    ///
    /// This covers every node whose shortest-hop depth is `< max_levels` — a
    /// superset of the nodes a depth-first replay can expand, since a DFS
    /// first *enters* a node at a depth ≥ its shortest-hop depth. The
    /// frontier itself is pruned exactly (`fetched` gates on first sight), so
    /// no node is ever re-sent in a later batch and duplicate targets within
    /// a level expand once.
    async fn prefetch_adjacency(
        &self,
        root: &str,
        kinds: &[EdgeKind],
        dir: Adj,
        max_levels: u32,
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        let mut adjacency: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
        let mut node_cache: HashMap<String, Node> = HashMap::new();
        let mut fetched: HashSet<String> = HashSet::from([root.to_string()]);
        let mut frontier: Vec<String> = vec![root.to_string()];
        let mut level: u32 = 0;
        while !frontier.is_empty() && level < max_levels {
            let mut batch = self
                .level_entries(&frontier, kinds, dir, &mut node_cache)
                .await?;
            let mut next: Vec<String> = Vec::new();
            for id in &frontier {
                let mut entries = batch.remove(id).unwrap_or_default();
                sort_entries(&mut entries);
                for entry in &entries {
                    if fetched.insert(entry.node.id.clone()) {
                        next.push(entry.node.id.clone());
                    }
                }
                adjacency.insert(id.clone(), entries);
            }
            frontier = next;
            level += 1;
        }
        Ok(adjacency)
    }

    /// One BFS level's adjacency for [`Self::traverse`], honoring the
    /// direction and edge-kind filter: one edges-batch per direction
    /// (`Direction::Both` = two), payloads attached via the walk-long
    /// `node_cache` ([`Self::level_entries`]), each node's merged list sorted
    /// per [`entry_key`].
    async fn level_adjacency(
        &self,
        ids: &[String],
        opts: &TraversalOptions,
        node_cache: &mut HashMap<String, Node>,
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        let kinds = opts.edge_kinds.as_slice();
        let mut merged = match opts.direction {
            Direction::Outgoing => {
                self.level_entries(ids, kinds, Adj::Outgoing, node_cache)
                    .await?
            }
            Direction::Incoming => {
                self.level_entries(ids, kinds, Adj::Incoming, node_cache)
                    .await?
            }
            Direction::Both => {
                let mut out = self
                    .level_entries(ids, kinds, Adj::Outgoing, node_cache)
                    .await?;
                for (id, entries) in self
                    .level_entries(ids, kinds, Adj::Incoming, node_cache)
                    .await?
                {
                    out.entry(id).or_default().extend(entries);
                }
                out
            }
        };
        for entries in merged.values_mut() {
            sort_entries(entries);
        }
        Ok(merged)
    }

    /// Batched adjacency prefetch for [`Self::impact_radius`]. Per depth
    /// level: the same-depth `contains` closure is expanded first (containers
    /// pull their children — transitively, for nested containers — into the
    /// level, one edges-batch per nesting round), then **one** incoming
    /// edges-batch over the whole closure fetches the non-`contains`
    /// dependency edges whose sources form the next level. One `node_cache`
    /// spans both phases and every level (the Task 8 ledger's "impact
    /// prefetch over-fetch on fan-in heavy graphs" — closed by the same
    /// Task 9d pruning as the callers prefetch, see [`Self::level_entries`]).
    #[allow(clippy::type_complexity)] // two adjacency maps: (contains, incoming)
    async fn prefetch_impact_adjacency(
        &self,
        focal: &Node,
        max_depth: u32,
    ) -> Result<(
        HashMap<String, Vec<NeighborEntry>>,
        HashMap<String, Vec<NeighborEntry>>,
    )> {
        let non_contains: Vec<EdgeKind> = EdgeKind::ALL
            .into_iter()
            .filter(|k| *k != EdgeKind::Contains)
            .collect();
        let mut contains_adj: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
        let mut incoming_adj: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
        let mut node_cache: HashMap<String, Node> =
            HashMap::from([(focal.id.clone(), focal.clone())]);
        let mut fetched: HashSet<String> = HashSet::from([focal.id.clone()]);
        let mut frontier: Vec<(String, NodeKind)> = vec![(focal.id.clone(), focal.kind)];
        let mut depth: u32 = 0;

        while !frontier.is_empty() && depth < max_depth {
            let mut closure: Vec<String> = frontier.iter().map(|(id, _)| id.clone()).collect();
            let mut pending: Vec<String> = frontier
                .iter()
                .filter(|(_, kind)| is_container(*kind))
                .map(|(id, _)| id.clone())
                .collect();
            while !pending.is_empty() {
                let mut batch = self
                    .level_entries(
                        &pending,
                        &[EdgeKind::Contains],
                        Adj::Outgoing,
                        &mut node_cache,
                    )
                    .await?;
                let mut next_pending: Vec<String> = Vec::new();
                for id in &pending {
                    let mut entries = batch.remove(id).unwrap_or_default();
                    sort_entries(&mut entries);
                    for entry in &entries {
                        if fetched.insert(entry.node.id.clone()) {
                            closure.push(entry.node.id.clone());
                            if is_container(entry.node.kind) {
                                next_pending.push(entry.node.id.clone());
                            }
                        }
                    }
                    contains_adj.insert(id.clone(), entries);
                }
                pending = next_pending;
            }

            let mut batch = self
                .level_entries(&closure, &non_contains, Adj::Incoming, &mut node_cache)
                .await?;
            let mut next: Vec<(String, NodeKind)> = Vec::new();
            for id in &closure {
                let mut entries = batch.remove(id).unwrap_or_default();
                sort_entries(&mut entries);
                for entry in &entries {
                    if fetched.insert(entry.node.id.clone()) {
                        next.push((entry.node.id.clone(), entry.node.kind));
                    }
                }
                incoming_adj.insert(id.clone(), entries);
            }
            frontier = next;
            depth += 1;
        }
        Ok((contains_adj, incoming_adj))
    }

    /// Nodes that (transitively, up to `max_depth` hops) call/reference/
    /// import/instantiate `id`, paired with the edge that reaches them.
    /// `max_depth = 1` is direct callers only; depth counts from 0; each
    /// caller appears once even when reached via multiple edges (`#1086`);
    /// instantiation counts as a call (`#774`). Empty `Vec` for an unknown id.
    pub async fn callers(&self, id: &str, max_depth: u32) -> Result<Vec<NeighborEntry>> {
        let mut adjacency = self
            .prefetch_adjacency(id, &CALL_EDGE_KINDS, Adj::Incoming, max_depth)
            .await?;
        Ok(replay_call_walk(id, max_depth, &mut adjacency))
    }

    /// Symmetric to [`Self::callers`]: what `id` (transitively) calls, so
    /// callers/callees stay inverses and a trace can cross the instantiation
    /// boundary (`#774`).
    pub async fn callees(&self, id: &str, max_depth: u32) -> Result<Vec<NeighborEntry>> {
        let mut adjacency = self
            .prefetch_adjacency(id, &CALL_EDGE_KINDS, Adj::Outgoing, max_depth)
            .await?;
        Ok(replay_call_walk(id, max_depth, &mut adjacency))
    }

    /// Everything that would need re-checking if `id` changed, up to
    /// `max_depth` hops. A node is marked visited **before** its depth check
    /// (`#1089` dedup-at-boundary); a container's `contains` children join at
    /// the container's **own** depth; every other incoming edge kind is a
    /// dependency edge, recorded unconditionally (`#1089`), recursing
    /// `depth + 1` into its unvisited source. `contains` is never followed
    /// *upward* (`#536`: a container does not depend on its members). Empty
    /// `Subgraph` for an unknown id.
    pub async fn impact_radius(&self, id: &str, max_depth: u32) -> Result<Subgraph> {
        let Some(focal) = self.get_node(id).await? else {
            return Ok(empty_subgraph());
        };
        let (mut contains_adj, mut incoming_adj) =
            self.prefetch_impact_adjacency(&focal, max_depth).await?;

        let mut nodes: IndexMap<String, Node> = IndexMap::new();
        nodes.insert(focal.id.clone(), focal.clone());
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::from([focal.id.clone()]);

        let make_frame =
            |id: &str,
             kind: NodeKind,
             depth: u32,
             contains_adj: &mut HashMap<String, Vec<NeighborEntry>>,
             incoming_adj: &mut HashMap<String, Vec<NeighborEntry>>| {
                let contains = if is_container(kind) {
                    contains_adj.remove(id).unwrap_or_default()
                } else {
                    Vec::new()
                };
                ImpactFrame {
                    contains,
                    ci: 0,
                    incoming: incoming_adj.remove(id).unwrap_or_default(),
                    ii: 0,
                    depth,
                }
            };

        let mut stack: Vec<ImpactFrame> = Vec::new();
        if max_depth > 0 {
            stack.push(make_frame(
                &focal.id,
                focal.kind,
                0,
                &mut contains_adj,
                &mut incoming_adj,
            ));
        }
        while let Some(frame) = stack.last_mut() {
            if frame.ci < frame.contains.len() {
                let entry = frame.contains[frame.ci].clone();
                frame.ci += 1;
                let depth = frame.depth;
                if visited.insert(entry.node.id.clone()) {
                    nodes.insert(entry.node.id.clone(), entry.node.clone());
                    edges.push(entry.edge);
                    // Same-depth recursion: the parent passed the depth
                    // check, so its same-depth child passes it too.
                    stack.push(make_frame(
                        &entry.node.id,
                        entry.node.kind,
                        depth,
                        &mut contains_adj,
                        &mut incoming_adj,
                    ));
                }
            } else if frame.ii < frame.incoming.len() {
                let entry = frame.incoming[frame.ii].clone();
                frame.ii += 1;
                let child_depth = frame.depth + 1;
                // Record the dependency edge unconditionally: a second edge
                // into a node already collected via another path is still a
                // real dependency (#1089).
                edges.push(entry.edge.clone());
                if visited.insert(entry.node.id.clone()) {
                    nodes.insert(entry.node.id.clone(), entry.node.clone());
                    if child_depth < max_depth {
                        stack.push(make_frame(
                            &entry.node.id,
                            entry.node.kind,
                            child_depth,
                            &mut contains_adj,
                            &mut incoming_adj,
                        ));
                    }
                }
            } else {
                stack.pop();
            }
        }

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![focal.id],
        })
    }

    /// Shortest path from `from` to `to` over **outgoing** edges only
    /// (optionally kind-filtered), as `(node, edge_that_reached_it)` pairs —
    /// the first pair's edge is `None`. Plain BFS whose queue entries carry
    /// their full path; the first entry (in queue order) to reach `to` is the
    /// returned path. `Ok(None)` for disconnected or unknown endpoints. No
    /// depth cap (the visited set bounds the walk).
    #[allow(clippy::type_complexity)] // the trait's binding contract shape
    pub async fn find_path(
        &self,
        from: &str,
        to: &str,
        kinds: &[EdgeKind],
    ) -> Result<Option<Vec<(Node, Option<Edge>)>>> {
        let endpoints = self.get_nodes(&[from.to_string(), to.to_string()]).await?;
        if !endpoints.contains_key(to) {
            return Ok(None);
        }
        let Some(from_node) = endpoints.get(from) else {
            return Ok(None);
        };

        let mut visited: HashSet<String> = HashSet::new();
        let mut level: Vec<(String, Vec<(Node, Option<Edge>)>)> =
            vec![(from.to_string(), vec![(from_node.clone(), None)])];

        while !level.is_empty() {
            // Every entry in a level has the same path length, so the first
            // (in queue order) to be at `to` is the first shortest path the
            // sequential BFS would have returned.
            for (node_id, path) in &level {
                if node_id == to {
                    return Ok(Some(path.clone()));
                }
            }

            // Batch the level's expansion: unique not-yet-visited ids, in
            // queue order.
            let mut expand_ids: Vec<String> = Vec::new();
            let mut queued: HashSet<&str> = HashSet::new();
            for (node_id, _) in &level {
                if !visited.contains(node_id) && queued.insert(node_id.as_str()) {
                    expand_ids.push(node_id.clone());
                }
            }
            let mut batch = self.outgoing_batch(&expand_ids, kinds).await?;
            let mut adjacency: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
            for id in &expand_ids {
                let mut entries = batch.remove(id).unwrap_or_default();
                sort_entries(&mut entries);
                adjacency.insert(id.clone(), entries);
            }

            let mut next: Vec<(String, Vec<(Node, Option<Edge>)>)> = Vec::new();
            for (node_id, path) in level {
                if !visited.insert(node_id.clone()) {
                    continue;
                }
                let Some(entries) = adjacency.remove(&node_id) else {
                    continue;
                };
                for entry in entries {
                    if !visited.contains(&entry.node.id) {
                        let next_id = entry.node.id.clone();
                        let mut next_path = path.clone();
                        next_path.push((entry.node, Some(entry.edge)));
                        next.push((next_id, next_path));
                    }
                }
            }
            level = next;
        }
        Ok(None)
    }

    /// Ancestors (outgoing `extends`/`implements`, transitively) and
    /// descendants (the same kinds, incoming) of `id`, unioned into one
    /// `Subgraph` rooted at `id`, with one shared visited set across both
    /// passes (see the module docs for the deliberate root-expansion
    /// divergence from the TS source). Empty `Subgraph` for an unknown id.
    pub async fn type_hierarchy(&self, id: &str) -> Result<Subgraph> {
        let Some(focal) = self.get_node(id).await? else {
            return Ok(empty_subgraph());
        };

        let mut nodes: IndexMap<String, Node> = IndexMap::new();
        nodes.insert(focal.id.clone(), focal.clone());
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        let mut ancestors_adj = self
            .prefetch_adjacency(id, &HIERARCHY_EDGE_KINDS, Adj::Outgoing, u32::MAX)
            .await?;
        replay_hierarchy_walk(id, &mut ancestors_adj, &mut nodes, &mut edges, &mut visited);

        let mut descendants_adj = self
            .prefetch_adjacency(id, &HIERARCHY_EDGE_KINDS, Adj::Incoming, u32::MAX)
            .await?;
        replay_hierarchy_walk(
            id,
            &mut descendants_adj,
            &mut nodes,
            &mut edges,
            &mut visited,
        );

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![focal.id],
        })
    }

    /// General-purpose BFS walk from `start` per `opts` — the port of the TS
    /// `traverseBFS`. Load-bearing mechanics, all pinned by tests:
    ///
    /// - **Separate `visited` (set on dequeue) and `enqueued` (set on
    ///   enqueue) sets** (`#1090`): each node is queued exactly once, and
    ///   parallel edges to an already-kept node are still recorded.
    /// - **Per-add node cap** (`#1087`/`#1088`): `opts.limit` is checked on
    ///   every individual add, so one high-degree node cannot overshoot the
    ///   budget. `include_start` inserts the root *before* the cap is
    ///   consulted (TS parity: a `limit: 0` walk still returns the root).
    /// - **Adjacency order** `contains < calls < other` (then the module
    ///   docs' stable tiebreakers), which makes cap truncation deterministic.
    /// - **Edge-identity dedup** so a `Direction::Both` scan records each
    ///   physical edge once while keeping parallel edges distinct.
    ///
    /// Empty `Subgraph` for an unknown `start`.
    pub async fn traverse(&self, start: &str, opts: &TraversalOptions) -> Result<Subgraph> {
        let Some(start_node) = self.get_node(start).await? else {
            return Ok(empty_subgraph());
        };

        let mut nodes: IndexMap<String, Node> = IndexMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut seen_edges: HashSet<(String, String, EdgeKind, i64, i64)> = HashSet::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut enqueued: HashSet<String> = HashSet::from([start_node.id.clone()]);
        let mut node_cache: HashMap<String, Node> =
            HashMap::from([(start_node.id.clone(), start_node.clone())]);

        if opts.include_start {
            nodes.insert(start_node.id.clone(), start_node.clone());
        }

        let mut level: Vec<String> = vec![start_node.id.clone()];
        let mut depth: u32 = 0;

        'levels: while !level.is_empty() {
            if nodes.len() >= opts.limit {
                break;
            }
            // At the depth cap nothing can be expanded; the TS loop dequeues
            // the remainder purely to mark it visited, with no observable
            // effect on the result.
            if opts.max_depth.is_some_and(|m| depth >= m) {
                break;
            }

            let mut adjacency = self.level_adjacency(&level, opts, &mut node_cache).await?;
            let mut next: Vec<String> = Vec::new();

            for node_id in level {
                // TS: `while (queue.length > 0 && nodes.size < opts.limit)` —
                // the cap also stops the walk between dequeues.
                if nodes.len() >= opts.limit {
                    break 'levels;
                }
                if !visited.insert(node_id.clone()) {
                    continue;
                }
                let Some(entries) = adjacency.remove(&node_id) else {
                    continue;
                };
                for entry in entries {
                    let neighbor = entry.node;
                    let edge = entry.edge;
                    let is_new =
                        !visited.contains(&neighbor.id) && !enqueued.contains(&neighbor.id);
                    if !is_new && !nodes.contains_key(&neighbor.id) {
                        // TS `neighborNodes.get(id) ?? nodes.get(id)` came up
                        // empty: an already-enqueued neighbor that is not in
                        // the kept set (the `include_start: false` start
                        // node). Its edge is not recorded.
                        continue;
                    }
                    if !opts.node_kinds.is_empty() && !opts.node_kinds.contains(&neighbor.kind) {
                        continue;
                    }
                    if is_new {
                        if nodes.len() >= opts.limit {
                            // Per-add cap: the rejected neighbor's edge is not
                            // recorded either — it is not in the kept set.
                            continue;
                        }
                        enqueued.insert(neighbor.id.clone());
                        next.push(neighbor.id.clone());
                        nodes.insert(neighbor.id.clone(), neighbor);
                    }
                    // Record every distinct edge among kept nodes; collecting
                    // on the adjacency scan (not once per dequeue) preserves
                    // parallel edges (#1090).
                    if seen_edges.insert(edge_identity(&edge)) {
                        edges.push(edge);
                    }
                }
            }
            level = next;
            depth += 1;
        }

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![start_node.id],
        })
    }

    /// Containment ancestors of `id`: repeatedly follow the first (in
    /// deterministic adjacency order) incoming `contains` edge to the
    /// container, then its container, etc., with a cycle set. The chain is
    /// inherently sequential — each hop depends on the previous parent — so
    /// this is one single-id adjacency query per hop (the frontier *is* one
    /// node), not an unbatched loop over a frontier.
    pub async fn ancestors(&self, id: &str) -> Result<Vec<Node>> {
        let mut ancestors: Vec<Node> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut current = id.to_string();
        loop {
            if !visited.insert(current.clone()) {
                break;
            }
            let mut entries = self.incoming(&current, &[EdgeKind::Contains]).await?;
            sort_entries(&mut entries);
            let Some(first) = entries.into_iter().next() else {
                break;
            };
            current = first.node.id.clone();
            ancestors.push(first.node);
        }
        Ok(ancestors)
    }

    /// Direct `contains` children of `id`, in deterministic adjacency order
    /// (same kind rank, so effectively by source line). Empty for an unknown
    /// id or a leaf.
    pub async fn children(&self, id: &str) -> Result<Vec<Node>> {
        let mut entries = self.outgoing(id, &[EdgeKind::Contains], None).await?;
        sort_entries(&mut entries);
        Ok(entries.into_iter().map(|e| e.node).collect())
    }
}
