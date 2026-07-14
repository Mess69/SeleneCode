//! `impl GraphStore for SurrealStore` — the trait wiring (Task 10).
//!
//! Pure delegation, zero logic: every trait method forwards to the
//! identically-signatured inherent method that carries the implementation and
//! its documentation (`src/nodes.rs`, `src/edges.rs`, `src/files.rs`,
//! `src/unresolved.rs`, `src/meta.rs`, `src/search.rs`, `src/traverse.rs`,
//! and `src/surreal.rs` for the bulk-load pair). The operations stay inherent
//! — and the operation modules stay split by section — because a trait is
//! implemented in a single `impl` block: folding ~50 method bodies into one
//! file would destroy the per-module narrative docs.
//!
//! Two mechanical notes:
//!
//! - The delegating bodies are plain `async fn`s: an `async fn` in an impl
//!   satisfies a desugared `-> impl Future<Output = …> + Send` trait method,
//!   and the `Send` bound is *proven here*, at the impl site — which is
//!   exactly why the trait is written desugared (see the [`GraphStore`] trait
//!   docs on `Send` futures).
//! - Each body calls `SurrealStore::method(self, …)`. That path resolves to
//!   the **inherent** method (inherent impls take precedence over trait
//!   methods), so the forwarding cannot recurse into itself; for the same
//!   reason, existing call sites on a concrete `SurrealStore` keep resolving
//!   to the inherent methods — this impl is additive, not a behavior change.

use std::collections::{BTreeSet, HashMap};

use selene_core::{Edge, EdgeKind, Node, NodeKind, Provenance};

use crate::{
    FileRecord, GraphStats, GraphStore, NeighborEntry, ReplaceStats, Result, SearchCandidate,
    Subgraph, SurrealStore, TraversalOptions, UnresolvedRef,
};

impl GraphStore for SurrealStore {
    // -------------------------------------------------------------------
    // Nodes (src/nodes.rs)
    // -------------------------------------------------------------------

    async fn insert_nodes(&self, nodes: &[Node]) -> Result<()> {
        SurrealStore::insert_nodes(self, nodes).await
    }

    async fn get_node(&self, id: &str) -> Result<Option<Node>> {
        SurrealStore::get_node(self, id).await
    }

    async fn get_nodes(&self, ids: &[String]) -> Result<HashMap<String, Node>> {
        SurrealStore::get_nodes(self, ids).await
    }

    async fn get_nodes_by_file(&self, path: &str) -> Result<Vec<Node>> {
        SurrealStore::get_nodes_by_file(self, path).await
    }

    async fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        SurrealStore::get_nodes_by_kind(self, kind).await
    }

    async fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        SurrealStore::get_nodes_by_name(self, name).await
    }

    async fn get_nodes_by_name_ci(&self, lower: &str) -> Result<Vec<Node>> {
        SurrealStore::get_nodes_by_name_ci(self, lower).await
    }

    async fn get_nodes_by_name_prefix(&self, prefix: &str, limit: usize) -> Result<Vec<Node>> {
        SurrealStore::get_nodes_by_name_prefix(self, prefix, limit).await
    }

    async fn get_nodes_by_qualified_name(&self, qn: &str) -> Result<Vec<Node>> {
        SurrealStore::get_nodes_by_qualified_name(self, qn).await
    }

    async fn count_nodes_matching_name_in_files(&self, name: &str) -> Result<u64> {
        SurrealStore::count_nodes_matching_name_in_files(self, name).await
    }

    async fn count_nodes_named(&self, name: &str) -> Result<u64> {
        SurrealStore::count_nodes_named(self, name).await
    }

    async fn nodes_by_kind_page(
        &self,
        kind: NodeKind,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Node>> {
        SurrealStore::nodes_by_kind_page(self, kind, after, limit).await
    }

    async fn find_route(
        &self,
        framework: Option<&str>,
        method: Option<&str>,
        path: &str,
    ) -> Result<Vec<Node>> {
        SurrealStore::find_route(self, framework, method, path).await
    }

    // -------------------------------------------------------------------
    // Edges (src/edges.rs)
    // -------------------------------------------------------------------

    async fn insert_edges(&self, edges: &[Edge]) -> Result<u64> {
        SurrealStore::insert_edges(self, edges).await
    }

    async fn outgoing(
        &self,
        id: &str,
        kinds: &[EdgeKind],
        provenance: Option<Provenance>,
    ) -> Result<Vec<NeighborEntry>> {
        SurrealStore::outgoing(self, id, kinds, provenance).await
    }

    async fn incoming(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>> {
        SurrealStore::incoming(self, id, kinds).await
    }

    async fn outgoing_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        SurrealStore::outgoing_batch(self, ids, kinds).await
    }

    async fn incoming_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        SurrealStore::incoming_batch(self, ids, kinds).await
    }

    async fn edges_between(&self, ids: &[String], kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        SurrealStore::edges_between(self, ids, kinds).await
    }

    async fn cross_file_incoming_with_target(
        &self,
        path: &str,
    ) -> Result<Vec<(Edge, String, NodeKind)>> {
        SurrealStore::cross_file_incoming_with_target(self, path).await
    }

    async fn dependent_file_paths(&self, path: &str) -> Result<Vec<String>> {
        SurrealStore::dependent_file_paths(self, path).await
    }

    async fn dependency_file_paths(&self, path: &str) -> Result<Vec<String>> {
        SurrealStore::dependency_file_paths(self, path).await
    }

    // -------------------------------------------------------------------
    // Files + single-file re-index (src/files.rs)
    // -------------------------------------------------------------------

    async fn upsert_file(&self, f: &FileRecord) -> Result<()> {
        SurrealStore::upsert_file(self, f).await
    }

    async fn get_file(&self, path: &str) -> Result<Option<FileRecord>> {
        SurrealStore::get_file(self, path).await
    }

    async fn all_files(&self) -> Result<Vec<FileRecord>> {
        SurrealStore::all_files(self).await
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        SurrealStore::delete_file(self, path).await
    }

    async fn last_indexed_at(&self) -> Result<Option<i64>> {
        SurrealStore::last_indexed_at(self).await
    }

    async fn distinct_file_languages(&self) -> Result<BTreeSet<String>> {
        SurrealStore::distinct_file_languages(self).await
    }

    async fn replace_file_extraction(
        &self,
        path: &str,
        nodes: &[Node],
        edges: &[Edge],
        unresolved: &[UnresolvedRef],
        file_record: &FileRecord,
    ) -> Result<ReplaceStats> {
        SurrealStore::replace_file_extraction(self, path, nodes, edges, unresolved, file_record)
            .await
    }

    // -------------------------------------------------------------------
    // Unresolved references (src/unresolved.rs)
    // -------------------------------------------------------------------

    async fn insert_unresolved(&self, refs: &[UnresolvedRef]) -> Result<()> {
        SurrealStore::insert_unresolved(self, refs).await
    }

    async fn unresolved_pending_count(&self) -> Result<u64> {
        SurrealStore::unresolved_pending_count(self).await
    }

    async fn unresolved_pending_batch(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<UnresolvedRef>> {
        SurrealStore::unresolved_pending_batch(self, offset, limit).await
    }

    async fn unresolved_by_files(&self, paths: &[String]) -> Result<Vec<UnresolvedRef>> {
        SurrealStore::unresolved_by_files(self, paths).await
    }

    async fn delete_resolved(&self, keys: &[crate::UnresolvedKey]) -> Result<()> {
        SurrealStore::delete_resolved(self, keys).await
    }

    async fn mark_failed(&self, keys: &[crate::UnresolvedKey]) -> Result<()> {
        SurrealStore::mark_failed(self, keys).await
    }

    async fn retryable_failed(
        &self,
        names: &[String],
        per_name_ceiling: usize,
    ) -> Result<Vec<UnresolvedRef>> {
        SurrealStore::retryable_failed(self, names, per_name_ceiling).await
    }

    async fn clear_unresolved(&self) -> Result<()> {
        SurrealStore::clear_unresolved(self).await
    }

    // -------------------------------------------------------------------
    // Metadata + stats (src/meta.rs)
    // -------------------------------------------------------------------

    async fn get_meta(&self, key: &str) -> Result<Option<String>> {
        SurrealStore::get_meta(self, key).await
    }

    async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        SurrealStore::set_meta(self, key, value).await
    }

    async fn stats(&self) -> Result<GraphStats> {
        SurrealStore::stats(self).await
    }

    async fn node_edge_count(&self) -> Result<(u64, u64)> {
        SurrealStore::node_edge_count(self).await
    }

    async fn dominant_file(&self) -> Result<Option<(String, u64, u64)>> {
        SurrealStore::dominant_file(self).await
    }

    async fn clear(&self) -> Result<()> {
        SurrealStore::clear(self).await
    }

    // -------------------------------------------------------------------
    // Bulk load (src/surreal.rs)
    // -------------------------------------------------------------------

    async fn bulk_load_begin(&self) -> Result<()> {
        SurrealStore::bulk_load_begin(self).await
    }

    async fn bulk_load_finish(&self) -> Result<()> {
        SurrealStore::bulk_load_finish(self).await
    }

    // -------------------------------------------------------------------
    // Search candidates (src/search.rs)
    // -------------------------------------------------------------------

    async fn search_fts(
        &self,
        terms: &[String],
        kinds: &[NodeKind],
        languages: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchCandidate>> {
        SurrealStore::search_fts(self, terms, kinds, languages, limit, offset).await
    }

    async fn search_name_like(
        &self,
        q: &str,
        kinds: &[NodeKind],
        limit: usize,
    ) -> Result<Vec<SearchCandidate>> {
        SurrealStore::search_name_like(self, q, kinds, limit).await
    }

    async fn find_by_exact_names(
        &self,
        names: &[String],
        per_name_limit: usize,
    ) -> Result<Vec<Node>> {
        SurrealStore::find_by_exact_names(self, names, per_name_limit).await
    }

    async fn all_node_names(&self) -> Result<Vec<String>> {
        SurrealStore::all_node_names(self).await
    }

    // -------------------------------------------------------------------
    // Traversal (src/traverse.rs)
    // -------------------------------------------------------------------

    async fn callers(&self, id: &str, max_depth: u32) -> Result<Vec<NeighborEntry>> {
        SurrealStore::callers(self, id, max_depth).await
    }

    async fn callees(&self, id: &str, max_depth: u32) -> Result<Vec<NeighborEntry>> {
        SurrealStore::callees(self, id, max_depth).await
    }

    async fn impact_radius(&self, id: &str, max_depth: u32) -> Result<Subgraph> {
        SurrealStore::impact_radius(self, id, max_depth).await
    }

    // Same nested shape the trait declares (and allows) — see the trait's note.
    #[allow(clippy::type_complexity)]
    async fn find_path(
        &self,
        from: &str,
        to: &str,
        kinds: &[EdgeKind],
    ) -> Result<Option<Vec<(Node, Option<Edge>)>>> {
        SurrealStore::find_path(self, from, to, kinds).await
    }

    async fn type_hierarchy(&self, id: &str) -> Result<Subgraph> {
        SurrealStore::type_hierarchy(self, id).await
    }

    async fn traverse(&self, start: &str, opts: &TraversalOptions) -> Result<Subgraph> {
        SurrealStore::traverse(self, start, opts).await
    }

    async fn ancestors(&self, id: &str) -> Result<Vec<Node>> {
        SurrealStore::ancestors(self, id).await
    }

    async fn children(&self, id: &str) -> Result<Vec<Node>> {
        SurrealStore::children(self, id).await
    }
}
