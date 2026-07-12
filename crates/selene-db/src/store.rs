//! The [`GraphStore`] trait and the shared parameter/result types it uses.
//!
//! This module defines the **store contract** only — no SurrealDB, no I/O.
//! `SurrealStore` is the sole implementation, and the sole *planned* one: the
//! permissive fallback backend (PRD §5.2) was dropped when the §5.4 spike
//! resolved to SurrealQL-max (2026-07-12, see the crate docs). The trait
//! remains as the **seam** every other layer crate (`selene-graph`,
//! `selene-mcp`, `selene-cli`, …) is written against — and tests mock —
//! never against SurrealDB directly; it is not a portability layer.
//!
//! `Node`, `Edge`, `NodeKind`, `EdgeKind`, `Provenance` — and the extraction
//! row records `FileRecord`, `UnresolvedRef`, `RefStatus` — are re-used
//! verbatim from `selene_core`, not redefined here: `selene-extract`
//! (Phase 2) produces those rows and must not depend on this crate to name
//! its own output. This crate re-exports all three at its root, so
//! `crate::FileRecord` etc. remain the store-side paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;

use indexmap::IndexMap;
use selene_core::{Edge, EdgeKind, FileRecord, Node, NodeKind, Provenance, UnresolvedRef};
use serde::{Deserialize, Serialize};

use crate::Result;

// =============================================================================
// Stats
// =============================================================================

/// Aggregate counts over the whole graph, used for `status`/`stats`-style
/// surfaces.
///
/// `nodes_by_kind`/`edges_by_kind` are keyed by the canonical wire strings
/// ([`NodeKind::as_str`]/[`EdgeKind::as_str`]), not the Rust variant names.
/// `BTreeMap` gives deterministic iteration order for stable output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStats {
    /// Total node count.
    pub nodes: u64,
    /// Total edge count.
    pub edges: u64,
    /// Total tracked file count.
    pub files: u64,
    /// Node count per [`NodeKind`] wire string.
    pub nodes_by_kind: BTreeMap<String, u64>,
    /// Edge count per [`EdgeKind`] wire string.
    pub edges_by_kind: BTreeMap<String, u64>,
    /// File count per language.
    pub languages: BTreeMap<String, u64>,
}

/// Outcome counts from the single-file re-index protocol
/// ([`GraphStore::replace_file_extraction`], a port of CodeGraph's
/// `storeExtractionResult`).
///
/// `nodes_inserted`/`edges_inserted` are the fresh rows written for the
/// re-extracted file. The three `incoming_*` counters partition the
/// **cross-file incoming** edges that were snapshotted before the file's old
/// nodes were deleted — deleting those nodes cascades their edges away, so
/// each snapshotted edge is afterwards either:
/// - **reattached** — a re-extracted node with the same `(name, kind)` was
///   found and the edge was re-attached onto its new id;
/// - **resurrected** — no match, but the edge's `metadata` carried a stamped
///   `refName`/`refKind`, so it becomes a `pending` [`UnresolvedRef`] for the
///   cross-file resolver (`#899`/`#1240`); or
/// - **dropped** — no match and no stamp, so it is discarded.
///
/// `incoming_reattached + incoming_resurrected + incoming_dropped` equals the
/// snapshot size, except that a reattached edge which duplicates an existing
/// one is deduped away by `insert_edges` and therefore not counted in
/// `incoming_reattached`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceStats {
    /// Nodes written for the re-extracted file (valid, required-field-complete).
    pub nodes_inserted: u64,
    /// Edges written for the re-extracted file (after endpoint validation and
    /// identity dedup — see [`GraphStore::insert_edges`]).
    pub edges_inserted: u64,
    /// Snapshotted cross-file incoming edges re-attached to a re-extracted node.
    pub incoming_reattached: u64,
    /// Snapshotted cross-file incoming edges resurrected as pending
    /// [`UnresolvedRef`]s (unmatched, but carrying a stamped `refName`/`refKind`).
    pub incoming_resurrected: u64,
    /// Snapshotted cross-file incoming edges dropped (unmatched and unstamped).
    pub incoming_dropped: u64,
}

// =============================================================================
// Traversal results
// =============================================================================

/// A connected slice of the graph produced by a traversal
/// (`impact_radius`, `type_hierarchy`, `traverse`, …).
///
/// `nodes` is an [`indexmap::IndexMap`], not a `HashMap`: traversal order is
/// meaningful (it mirrors the CodeGraph TS traverser's reliance on `Map`
/// insertion order — see `docs/reference/from-codegraph/maps/db-graph-search.md`
/// "Rust port notes") and callers may render nodes in visit order. `edges` is
/// every edge among the visited nodes that the traversal recorded (parallel
/// edges are preserved, not deduped to one per pair). `roots` holds the id(s)
/// the traversal started from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subgraph {
    /// Visited nodes, keyed by id, in traversal (visit) order.
    pub nodes: IndexMap<String, Node>,
    /// Every edge recorded among the visited nodes.
    pub edges: Vec<Edge>,
    /// The id(s) the traversal started from.
    pub roots: Vec<String>,
}

/// One step of an adjacency result: a neighbor node paired with the edge that
/// reaches it from the query node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NeighborEntry {
    /// The neighboring node.
    pub node: Node,
    /// The edge connecting the query node to `node`.
    pub edge: Edge,
}

/// A node reached by a candidate-fetch search method, with the store's raw
/// (unranked, unnormalized) relevance signal.
///
/// `raw_score` is a positive magnitude from whatever ranking the store itself
/// can cheaply produce (e.g. BM25 for [`GraphStore::search_fts`], a
/// exact/prefix/contains tier for [`GraphStore::search_name_like`]). It is
/// **not** comparable across the two methods, and it is not the product's
/// final ranked score — blending kind/path/name-match bonuses on top of this
/// is upstream product logic (`selene-graph`/`selene-context`), deliberately
/// kept out of this crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchCandidate {
    /// The candidate node.
    pub node: Node,
    /// The store's raw relevance signal for this candidate (positive
    /// magnitude; see the struct docs for what it is and isn't comparable to).
    pub raw_score: f64,
}

/// Which direction(s) [`GraphStore::traverse`] follows edges in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow edges away from the start node (`start -> neighbor`).
    Outgoing,
    /// Follow edges into the start node (`neighbor -> start`).
    Incoming,
    /// Follow edges in both directions.
    Both,
}

/// Options for the general-purpose [`GraphStore::traverse`] walk.
///
/// Mirrors the CodeGraph TS traverser's `TraversalOptions` defaults
/// (`maxDepth=Infinity, edgeKinds=[], nodeKinds=[], direction='outgoing',
/// limit=1000, includeStart=true`): `max_depth: None` means unbounded, and an
/// empty `edge_kinds`/`node_kinds` means "no filter" (every kind), matching
/// every other `kinds: &[EdgeKind]` parameter on this trait.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraversalOptions {
    /// Maximum depth (hops) to traverse; `None` is unbounded.
    pub max_depth: Option<u32>,
    /// Edge kinds to follow; empty means every kind.
    pub edge_kinds: Vec<EdgeKind>,
    /// Node kinds to keep in the result; empty means every kind.
    pub node_kinds: Vec<NodeKind>,
    /// Which direction(s) to follow edges in.
    pub direction: Direction,
    /// Maximum number of nodes to visit (enforced per-add, not post-hoc
    /// truncation, so a capped traversal still reflects a real frontier walk).
    pub limit: usize,
    /// Whether the start node itself is included in the result.
    pub include_start: bool,
}

// =============================================================================
// GraphStore
// =============================================================================

/// The store contract. [`crate::SurrealStore`] (embedded SurrealDB) is the
/// sole backend — the permissive fallback (PRD §5.2) is dropped per the
/// locked SurrealQL-max decision (2026-07-12, see the crate docs) — but every
/// other layer crate still depends on this trait, never on the concrete
/// backend: it is the mocking/test seam and the layering boundary.
///
/// # Async / `Send` futures
///
/// Every method is written in **desugared return-position-`impl Trait`**
/// form — `fn m(&self, ..) -> impl Future<Output = Result<T>> + Send` —
/// instead of plain `async fn`. A plain `async fn` in a *public* trait yields
/// a future whose `Send`-ness a generic caller cannot prove (the concrete
/// future type is opaque across the trait boundary); that breaks a future MCP
/// server that needs to hold and schedule `GraphStore` work on a
/// multi-threaded tokio runtime. The desugared form pins the `Send` bound
/// into the trait signature itself, so every implementation is required to
/// prove it at the `impl` site.
///
/// Trade-off accepted: this trait is **not `dyn`-safe** (return-position
/// `impl Trait` in a trait method cannot appear in a vtable) — callers must be
/// generic over `S: GraphStore` or hold a concrete store type, not
/// `Box`/`Arc<dyn GraphStore>`. Nothing in the current design needs
/// `dyn GraphStore`; if that changes, prefer switching to the `trait-variant`
/// crate (`#[trait_variant::make(GraphStore: Send)]`, which generates a
/// `dyn`-safe `async fn`-shaped trait) over re-litigating plain `async fn`.
///
/// # Contract: traversals — and lookups generally — never error on a miss
///
/// An unknown id, an absent file, a query with zero matches: these are all
/// **expected** outcomes, not failures. Every method that can miss returns
/// `Ok` with an empty `Vec`/`HashMap`, a `None`, or (for the `Subgraph`-typed
/// traversals) an empty `Subgraph { nodes: {}, edges: vec![], roots }`. `Err`
/// is reserved for a genuine store malfunction (see [`crate::Error`] and PRD
/// §8.2's `isError` reservation, which this mirrors one layer down).
///
/// # Contract: the callers/callees edge-kind whitelist
///
/// [`GraphStore::callers`] and [`GraphStore::callees`] only ever follow edges
/// of kind `calls`, `references`, `imports`, `instantiates` — in the incoming
/// direction for `callers`, outgoing for `callees`. This whitelist is a
/// load-bearing constant carried over from CodeGraph (`#774`, instantiation
/// counts as a call); implementations must not drift it, and it is
/// intentionally **not** a caller-supplied parameter.
///
/// # Contract: impact-radius semantics
///
/// [`GraphStore::impact_radius`] answers "what would need re-checking if this
/// symbol changed". A node is marked visited **before** its depth is checked
/// (so a node reachable via both a short and a long path is recorded once, at
/// its shortest distance — the `#1086`/`#1089` dedup-at-boundary behavior).
/// For a *container* node (`class`, `interface`, `struct`, `trait`,
/// `protocol`, `module`, `enum`), its `contains` children are pulled in at
/// the **same** depth as the container itself — a class's methods share the
/// class's impact radius, they are not one hop deeper (`#536`: this prevents
/// sibling methods from "dragging in" the whole rest of the impact set at an
/// inflated depth). Every other incoming edge kind is a "depends on me" edge
/// and is recorded, recursing `depth + 1` into its unvisited source.
///
/// # Contract: search candidates are unranked
///
/// [`GraphStore::search_fts`] and [`GraphStore::search_name_like`] return
/// candidates with a raw store-level relevance signal, not a final ranking.
/// Blending in kind/path/name-match bonuses and merging the two candidate
/// sources is upstream product logic, deliberately kept out of this crate:
/// the store implements candidate *fetch* only, so the full CodeGraph scoring
/// pipeline lives in one place upstream (`selene-graph`/`selene-context`)
/// instead of being smeared across the storage layer.
pub trait GraphStore: Send + Sync {
    // -------------------------------------------------------------------
    // Nodes
    // -------------------------------------------------------------------

    /// Insert or replace `nodes` (same id ⇒ replace in place). Any derived
    /// search index must stay consistent with the replacement.
    fn insert_nodes(&self, nodes: &[Node]) -> impl Future<Output = Result<()>> + Send;

    /// Point lookup by id. `None` if `id` is unknown — not an error.
    fn get_node(&self, id: &str) -> impl Future<Output = Result<Option<Node>>> + Send;

    /// Batch lookup by id. The returned map contains only the ids that were
    /// found; unknown ids are simply absent, never an error.
    fn get_nodes(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Result<HashMap<String, Node>>> + Send;

    /// Every node whose `file_path` equals `path`.
    fn get_nodes_by_file(&self, path: &str) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Every node of exactly `kind`.
    fn get_nodes_by_kind(&self, kind: NodeKind) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Every node whose `name` matches exactly (case-sensitive).
    fn get_nodes_by_name(&self, name: &str) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Every node whose lower-cased `name` equals `lower`. `lower` is
    /// expected pre-lowercased by the caller (this method does not lowercase
    /// it for you — matches the store's `lower(name)` index shape).
    fn get_nodes_by_name_ci(&self, lower: &str) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Every node whose `name` starts with `prefix`, capped at `limit`.
    fn get_nodes_by_name_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Every node whose `qualified_name` matches exactly (more than one is
    /// possible, e.g. overloads sharing a qualified name).
    fn get_nodes_by_qualified_name(
        &self,
        qn: &str,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Count of nodes named exactly `name`, across every file. Used upstream
    /// to decide whether a name is "distinctive" enough to boost in search.
    fn count_nodes_matching_name_in_files(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<u64>> + Send;

    // -------------------------------------------------------------------
    // Edges
    // -------------------------------------------------------------------

    /// Insert `edges`. An edge whose source or target id is not a known node
    /// is silently skipped (not an error). Storage-level identity dedup
    /// applies on `(source, target, kind, line ?? -1, col ?? -1)` — a
    /// resubmitted identical edge does not create a duplicate. Returns the
    /// number of edges actually inserted after validation and dedup.
    fn insert_edges(&self, edges: &[Edge]) -> impl Future<Output = Result<u64>> + Send;

    /// Outgoing neighbors of `id`. `kinds` empty means every edge kind;
    /// `provenance`, when `Some`, restricts to edges with exactly that
    /// provenance.
    fn outgoing(
        &self,
        id: &str,
        kinds: &[EdgeKind],
        provenance: Option<Provenance>,
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send;

    /// Incoming neighbors of `id`. `kinds` empty means every edge kind. No
    /// provenance filter (deliberately asymmetric with [`Self::outgoing`] —
    /// carried over from the CodeGraph query surface, not a Rust omission).
    fn incoming(
        &self,
        id: &str,
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send;

    /// [`Self::outgoing`] batched over multiple ids (no provenance filter),
    /// keyed by the queried id. An id with no matching neighbors need not
    /// appear as an explicit empty entry.
    fn outgoing_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<HashMap<String, Vec<NeighborEntry>>>> + Send;

    /// [`Self::incoming`] batched over multiple ids, keyed by the queried id.
    fn incoming_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<HashMap<String, Vec<NeighborEntry>>>> + Send;

    /// Every edge with both endpoints in `ids` (optionally kind-filtered).
    /// Used to compute a filtered subgraph among an already-known node set.
    fn edges_between(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<Vec<Edge>>> + Send;

    /// Cross-file incoming edges landing on any node under `path`: every edge
    /// whose target is a node in this file and whose source is a node in a
    /// *different* file, kind `!= contains`, paired with the target node's
    /// `(name, kind)`. Feeds the single-file re-index edge-preservation
    /// protocol (Task 6): snapshot before delete, re-attach after re-insert
    /// by `(kind, name)` match.
    fn cross_file_incoming_with_target(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Vec<(Edge, String, NodeKind)>>> + Send;

    /// Distinct file paths that depend on `path`: files containing a node
    /// with an outgoing non-`contains` edge whose target is a node in
    /// `path`, excluding `path` itself. Note `imports` edges are same-file by
    /// construction (a file's import declarations live in that file), so
    /// this is driven by the resolved symbol graph, not by `imports` edges
    /// alone.
    fn dependent_file_paths(&self, path: &str) -> impl Future<Output = Result<Vec<String>>> + Send;

    /// Distinct file paths that `path` depends on — the mirror of
    /// [`Self::dependent_file_paths`].
    fn dependency_file_paths(&self, path: &str)
    -> impl Future<Output = Result<Vec<String>>> + Send;

    // -------------------------------------------------------------------
    // Files
    // -------------------------------------------------------------------

    /// Insert or replace the file record for `f.path`.
    fn upsert_file(&self, f: &FileRecord) -> impl Future<Output = Result<()>> + Send;

    /// Look up a file record by path. `None` if not tracked.
    fn get_file(&self, path: &str) -> impl Future<Output = Result<Option<FileRecord>>> + Send;

    /// Every tracked file record.
    fn all_files(&self) -> impl Future<Output = Result<Vec<FileRecord>>> + Send;

    /// Delete the file record for `path` and cascade: every node attributed
    /// to this file, every edge touching one of those nodes, and every
    /// unresolved reference sourced from one of those nodes.
    fn delete_file(&self, path: &str) -> impl Future<Output = Result<()>> + Send;

    /// The most recent `indexed_at` across all tracked files, or `None` if no
    /// file has been indexed yet.
    fn last_indexed_at(&self) -> impl Future<Output = Result<Option<i64>>> + Send;

    /// The distinct set of `language` values across all tracked files.
    fn distinct_file_languages(&self) -> impl Future<Output = Result<BTreeSet<String>>> + Send;

    /// Replace the whole stored extraction of `path` in one protocol pass —
    /// the single-file re-index write protocol (a port of CodeGraph's
    /// `storeExtractionResult` steps 2–8; step 1, the content-hash skip, is
    /// the caller's job). `nodes`/`edges`/`unresolved` are the freshly
    /// extracted rows for this file. In order:
    ///
    /// - **(a)** snapshot the cross-file **incoming** edges with their target
    ///   `(name, kind)` ([`Self::cross_file_incoming_with_target`]) — a
    ///   re-extraction churns node ids (any line shift changes an id), and
    ///   deleting the old nodes cascades these edges away;
    /// - **(b)** delete the old extraction ([`Self::delete_file`] cascade);
    /// - **(c)** insert the re-extracted `nodes`, silently skipping any with
    ///   an empty required field (`id`/`name`/`file_path`), mirroring TS;
    /// - **(d)** insert the re-extracted `edges` (endpoint validation + dedup
    ///   per [`Self::insert_edges`]);
    /// - **(e)** re-attach each snapshotted incoming edge to the re-extracted
    ///   node matching its old target's `(name, kind)`; resurrect an
    ///   unmatched edge as a `pending` [`UnresolvedRef`] if its `metadata`
    ///   carries a stamped `refName`/`refKind` (`#899`), else drop it;
    /// - **(f)** insert the caller-supplied `unresolved` refs;
    /// - **(g)** upsert `file_record` **last**.
    ///
    /// The step order is the crash-safety mechanism: there is no
    /// multi-statement transaction spanning the protocol (exactly as the
    /// TS/SQLite original had none), and the file row landing last means a
    /// crash mid-protocol leaves no `file` row — the next indexing run sees
    /// the file as un-indexed and re-runs the whole protocol, so a partial
    /// re-index never masquerades as complete. Implementations must not move
    /// the file-row write earlier. Full narrative: `src/files.rs`; returned
    /// counts: [`ReplaceStats`].
    fn replace_file_extraction(
        &self,
        path: &str,
        nodes: &[Node],
        edges: &[Edge],
        unresolved: &[UnresolvedRef],
        file_record: &FileRecord,
    ) -> impl Future<Output = Result<ReplaceStats>> + Send;

    // -------------------------------------------------------------------
    // Unresolved references
    // -------------------------------------------------------------------

    /// Insert `refs` (typically as `Pending`).
    fn insert_unresolved(&self, refs: &[UnresolvedRef]) -> impl Future<Output = Result<()>> + Send;

    /// Count of refs currently `Pending`.
    fn unresolved_pending_count(&self) -> impl Future<Output = Result<u64>> + Send;

    /// A page of `Pending` refs, ordered deterministically, for resolver
    /// batching.
    fn unresolved_pending_batch(
        &self,
        offset: usize,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<UnresolvedRef>>> + Send;

    /// Every `Pending` ref whose `file_path` is in `paths`.
    fn unresolved_by_files(
        &self,
        paths: &[String],
    ) -> impl Future<Output = Result<Vec<UnresolvedRef>>> + Send;

    /// Delete refs matching `(from_node_id, reference_name)` keys — call once
    /// a reference has been resolved into a real edge.
    fn delete_resolved(&self, keys: &[(String, String)])
    -> impl Future<Output = Result<()>> + Send;

    /// Flip refs matching `(from_node_id, reference_name)` keys to `Failed`
    /// (kept for the bounded retry pipeline rather than deleted; `#1240`).
    fn mark_failed(&self, keys: &[(String, String)]) -> impl Future<Output = Result<()>> + Send;

    /// `Failed` refs whose `reference_name` is in `names`, capped at
    /// `per_name_ceiling` entries per distinct name — the bounded retry
    /// pipeline's candidate fetch.
    fn retryable_failed(
        &self,
        names: &[String],
        per_name_ceiling: usize,
    ) -> impl Future<Output = Result<Vec<UnresolvedRef>>> + Send;

    /// Delete every unresolved ref (full re-index discard).
    fn clear_unresolved(&self) -> impl Future<Output = Result<()>> + Send;

    // -------------------------------------------------------------------
    // Metadata + stats
    // -------------------------------------------------------------------

    /// Read an opaque project metadata value by key. `None` if unset.
    fn get_meta(&self, key: &str) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Write an opaque project metadata value.
    fn set_meta(&self, key: &str, value: &str) -> impl Future<Output = Result<()>> + Send;

    /// Aggregate graph statistics (see [`GraphStats`]).
    fn stats(&self) -> impl Future<Output = Result<GraphStats>> + Send;

    /// `(node_count, edge_count)` — cheaper than [`Self::stats`] when only
    /// the totals are needed.
    fn node_edge_count(&self) -> impl Future<Output = Result<(u64, u64)>> + Send;

    /// Drop every node, edge, file, and unresolved-ref row (full re-index
    /// discard). Project metadata (e.g. schema version) is untouched.
    fn clear(&self) -> impl Future<Output = Result<()>> + Send;

    // -------------------------------------------------------------------
    // Bulk load (deferred search-index maintenance)
    // -------------------------------------------------------------------

    /// Enter bulk-load mode: initialize the store (idempotently — safe as the
    /// very first call on a fresh store) with any derived full-text search
    /// indexes *deferred*, so a large `insert_nodes` stream skips per-row
    /// index maintenance. Motivation (§5.3 gate remediation, measured on the
    /// SurrealDB backend): inline FTS maintenance capped node ingest at
    /// 803 nodes/s vs 4,703 nodes/s without it — the deferred pattern is a
    /// 2.15x total-load win on a 100k-node graph.
    ///
    /// Contract: idempotent (calling twice is a no-op). Between this call and
    /// [`Self::bulk_load_finish`], [`Self::search_fts`] returns `Ok` with an
    /// empty result — a success-shaped miss, **never** `Err` (the `isError`
    /// reservation, PRD §8.2, one layer down). Everything else (lookups,
    /// adjacency, traversals, `search_name_like`) works normally. A backend
    /// whose write path pays nothing for inline index maintenance may
    /// implement both methods as no-ops.
    fn bulk_load_begin(&self) -> impl Future<Output = Result<()>> + Send;

    /// Leave bulk-load mode: (re)build the deferred search indexes and block
    /// until the store is search-ready — when this returns `Ok`,
    /// [`Self::search_fts`] serves results identical to a store that was
    /// never in bulk-load mode. Idempotent, and a no-op on an *initialized*
    /// store that never entered bulk-load mode; on an uninitialized store
    /// (schema never applied) it may error on missing schema objects —
    /// [`Self::bulk_load_begin`] is the initializing entry point of the pair.
    /// A failed index build is a genuine store malfunction and returns `Err`.
    fn bulk_load_finish(&self) -> impl Future<Output = Result<()>> + Send;

    // -------------------------------------------------------------------
    // Search candidates (final scoring lives upstream — see trait docs)
    // -------------------------------------------------------------------

    /// Full-text candidate fetch. `terms` are pre-sanitized/tokenized by the
    /// caller (splitting/punctuation-stripping is upstream product logic,
    /// not this crate's job); `kinds`/`languages` empty means no filter.
    /// Malformed or empty `terms` yields an empty result, never an error
    /// (mirrors the "FTS syntax error ⇒ empty" contract).
    fn search_fts(
        &self,
        terms: &[String],
        kinds: &[NodeKind],
        languages: &[String],
        limit: usize,
        offset: usize,
    ) -> impl Future<Output = Result<Vec<SearchCandidate>>> + Send;

    /// LIKE-style fallback candidate fetch (exact/prefix/contains tiers) over
    /// `name`/`qualified_name`, for when [`Self::search_fts`] comes up empty.
    /// `kinds` empty means no filter.
    fn search_name_like(
        &self,
        q: &str,
        kinds: &[NodeKind],
        limit: usize,
    ) -> impl Future<Output = Result<Vec<SearchCandidate>>> + Send;

    /// Exact-name lookup across multiple names in one call, capped at
    /// `per_name_limit` results per name.
    fn find_by_exact_names(
        &self,
        names: &[String],
        per_name_limit: usize,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Every distinct node name in the graph — input to the upstream fuzzy
    /// (bounded edit-distance) match pipeline, which does not belong in this
    /// crate.
    fn all_node_names(&self) -> impl Future<Output = Result<Vec<String>>> + Send;

    // -------------------------------------------------------------------
    // Traversal (SurrealQL-max: implemented in-DB; semantics per trait docs)
    // -------------------------------------------------------------------

    /// Nodes that (transitively, up to `max_depth` hops) call/reference/
    /// import/instantiate `id`, paired with the edge that reaches them. See
    /// the trait docs for the edge-kind whitelist and visited-before-depth
    /// dedup contract. Empty `Vec` for an unknown id.
    fn callers(
        &self,
        id: &str,
        max_depth: u32,
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send;

    /// Symmetric to [`Self::callers`]: what `id` (transitively) calls.
    fn callees(
        &self,
        id: &str,
        max_depth: u32,
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send;

    /// Everything that would need re-checking if `id` changed, up to
    /// `max_depth` hops. See the trait docs for the container/`contains`
    /// same-depth rule and the visited-before-depth-check dedup. Empty
    /// `Subgraph` for an unknown id.
    fn impact_radius(
        &self,
        id: &str,
        max_depth: u32,
    ) -> impl Future<Output = Result<Subgraph>> + Send;

    /// Shortest path from `from` to `to` over **outgoing** edges only
    /// (optionally kind-filtered; empty `kinds` means no filter), as
    /// `(node, edge_that_reached_it)` pairs — the first pair's edge is
    /// `None`. `Ok(None)` means disconnected (or either id is unknown) — not
    /// an error. No depth cap.
    // The nested `Option<Vec<(Node, Option<Edge>)>>` is the brief's contract
    // shape (path-or-none, each step's inbound edge is nullable at the
    // start) — a type alias would just rename it, not simplify it.
    #[allow(clippy::type_complexity)]
    fn find_path(
        &self,
        from: &str,
        to: &str,
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<Option<Vec<(Node, Option<Edge>)>>>> + Send;

    /// Ancestors (outgoing `extends`/`implements`, transitively) and
    /// descendants (the same kinds, incoming) of `id`, unioned into one
    /// `Subgraph` rooted at `id`. Empty `Subgraph` for an unknown id.
    fn type_hierarchy(&self, id: &str) -> impl Future<Output = Result<Subgraph>> + Send;

    /// General-purpose BFS/DFS-shaped walk from `start`, per `opts` (see
    /// [`TraversalOptions`]). Implementations should preserve parallel edges
    /// and a deterministic edge-kind visitation order (`contains`, then
    /// `calls`, then everything else), and enforce `opts.limit` per-add
    /// rather than by post-hoc truncation. Empty `Subgraph` for an unknown
    /// `start`.
    fn traverse(
        &self,
        start: &str,
        opts: &TraversalOptions,
    ) -> impl Future<Output = Result<Subgraph>> + Send;

    /// Containment ancestors of `id`: repeatedly follow the (at most one)
    /// incoming `contains` edge to `id`'s container, then its container, etc.
    fn ancestors(&self, id: &str) -> impl Future<Output = Result<Vec<Node>>> + Send;

    /// Direct `contains` children of `id`.
    fn children(&self, id: &str) -> impl Future<Output = Result<Vec<Node>>> + Send;
}
