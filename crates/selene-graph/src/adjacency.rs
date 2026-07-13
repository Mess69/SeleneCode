//! Adjacency — **thin wrappers**. The traversal is SurrealQL's.
//!
//! Every method here is one store call plus the ported clamps. That thinness is the
//! SurrealQL-max decision paying out: recursive walks, shortest-path and impact radius run
//! *in the database*, so this layer has no graph algorithm to get wrong.
//!
//! # Clamping, not rejecting
//!
//! `limit` clamps to **1–100**; impact `depth` clamps to **1–10**, default **2**. An
//! out-of-range value is **not an error** — the agent asked for depth 50 and gets depth 10,
//! with the answer it wanted. Erroring here would spend the one thing we cannot spend: an
//! `isError` early in a session.

use selene_core::{Edge, EdgeKind, Node};
use selene_db::{GraphStore, NeighborEntry, Subgraph};

use crate::error::Result;
use crate::query::QueryManager;

/// Ported verbatim.
pub const MIN_LIMIT: usize = 1;
/// Ported verbatim.
pub const MAX_LIMIT: usize = 100;
/// Ported verbatim.
pub const MIN_DEPTH: u32 = 1;
/// Ported verbatim.
pub const MAX_DEPTH: u32 = 10;
/// Ported verbatim.
pub const DEFAULT_IMPACT_DEPTH: u32 = 2;

/// 1–100. An out-of-range limit is clamped, never refused.
pub fn clamp_limit(limit: usize) -> usize {
    limit.clamp(MIN_LIMIT, MAX_LIMIT)
}

/// 1–10, defaulting to 2 when the caller says "0" (i.e. "unset").
pub fn clamp_depth(depth: u32) -> u32 {
    if depth == 0 {
        return DEFAULT_IMPACT_DEPTH;
    }
    depth.clamp(MIN_DEPTH, MAX_DEPTH)
}

impl<S: GraphStore> QueryManager<S> {
    /// Who calls this (transitively, to `depth`).
    pub async fn callers(&self, id: &str, depth: u32) -> Result<Vec<NeighborEntry>> {
        Ok(self.store().callers(id, clamp_depth(depth)).await?)
    }

    /// What this calls (transitively, to `depth`).
    pub async fn callees(&self, id: &str, depth: u32) -> Result<Vec<NeighborEntry>> {
        Ok(self.store().callees(id, clamp_depth(depth)).await?)
    }

    /// Everything that breaks if this changes.
    pub async fn impact(&self, id: &str, depth: u32) -> Result<Subgraph> {
        Ok(self.store().impact_radius(id, clamp_depth(depth)).await?)
    }

    /// The shortest path between two nodes along `kinds`, or `None`. **`None` is an answer**
    /// ("these are not connected"), never an error.
    pub async fn find_path(
        &self,
        from: &str,
        to: &str,
        kinds: &[EdgeKind],
    ) -> Result<Option<Vec<(Node, Option<Edge>)>>> {
        Ok(self.store().find_path(from, to, kinds).await?)
    }

    /// The supertypes/subtypes around a node.
    pub async fn type_hierarchy(&self, id: &str) -> Result<Subgraph> {
        Ok(self.store().type_hierarchy(id).await?)
    }

    /// What this node contains (a class's methods).
    pub async fn children(&self, id: &str) -> Result<Vec<Node>> {
        Ok(self.store().children(id).await?)
    }

    /// Edges out of a node, by kind.
    ///
    /// `provenance: None` = **every** provenance. Filtering to `TreeSitter` here would hide
    /// the synthesized dispatch edges (`provenance: heuristic`) — the very hops Phase 3
    /// exists to draw, and the ones an agent cannot get any other way.
    pub async fn outgoing(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>> {
        Ok(self.store().outgoing(id, kinds, None).await?)
    }

    /// Edges into a node, by kind.
    ///
    /// (Asymmetric with [`Self::outgoing`] by the store's own design: `incoming` takes no
    /// provenance filter. Noted rather than papered over — the two signatures differ.)
    pub async fn incoming(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>> {
        Ok(self.store().incoming(id, kinds).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent asks for depth 50 and gets depth 10 **with an answer** — not an error.
    #[test]
    fn clamps_never_reject() {
        assert_eq!(clamp_limit(0), 1);
        assert_eq!(clamp_limit(5), 5);
        assert_eq!(clamp_limit(10_000), 100);

        assert_eq!(clamp_depth(0), 2, "0 means unset ⇒ the default");
        assert_eq!(clamp_depth(1), 1);
        assert_eq!(clamp_depth(50), 10);
    }
}
