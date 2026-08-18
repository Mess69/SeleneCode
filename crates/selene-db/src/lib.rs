//! `selene-db` — Graph store: the [`GraphStore`] trait + embedded SurrealDB backend + FTS.
//!
//! Everything DB-shaped lives behind [`GraphStore`]: nodes, edges, files, the
//! single-file re-index protocol ([`GraphStore::replace_file_extraction`]),
//! unresolved references, metadata/stats, search-candidate fetch, bulk-load
//! mode, and traversals (callers/callees/impact-radius/path-finding/
//! hierarchy/BFS). The rest of the pipeline (`selene-graph`, `selene-context`,
//! `selene-mcp`, `selene-cli`) depends only on this trait, never on SurrealDB
//! types.
//!
//! ## Locked decision (2026-07-12): SurrealQL-max, **no fallback backend**
//!
//! Sole backend: **embedded SurrealDB** ([`SurrealStore`]). The PRD §5.4
//! spike is resolved as SurrealQL-max — query/traversal work is pushed into
//! the embedded engine wherever it wins (batched frontier expansion for the
//! walks whose TS-ported visited/cap/dedup semantics SurrealQL's recursion
//! cannot express — see `src/traverse.rs`) — and the permissive fallback
//! backend (IndraDB/redb + Tantivy, PRD §5.2) is **dropped**. [`GraphStore`]
//! is therefore a *seam* (the boundary layer crates code against and tests
//! mock), not a portability layer. The default on-disk engine is RocksDB
//! (`kv-rocksdb`), chosen by the §5.3 benchmark gate — results and the
//! backend decision: `docs/benchmarks/2026-07-phase1-db-gate.md`.
//!
//! ## Deliberate deferrals (vs the CodeGraph TS store surface)
//!
//! Checked against `docs/reference/from-codegraph/maps/db-graph-search.md`
//! §Public interface:
//!
//! - **Segment-vocab tables and the dominant/route-file aggregates**
//!   (`insertNameSegmentsBatch`, `getSegmentCoOccurrence`, …,
//!   `getDominantFile`, `getTopRouteFile`, `getRoutingManifest`) → **Phase 4**,
//!   when their consumers (prose→identifier lookup, the MCP routing manifest)
//!   arrive.
//! - **The WAL checkpoint valve** (`WalCheckpointValve`, `#1231`) → **dropped**,
//!   not deferred: it managed SQLite's WAL growth, and SurrealDB's engines
//!   manage their own storage (RocksDB: LSM compaction). Likewise the other
//!   SQLite connection mechanics (pragmas, journal mode, `-wal`/`-shm`
//!   sidecars, `isReplacedOnDisk`) — engine-operational surface, not product
//!   behavior.
//! - **TS ops whose consumer is a later phase's product logic** (`deleteNode`,
//!   `deleteEdgesBySource`, `getAllNodes`/the iterator variants,
//!   `getNodeNamesByFiles`, `getStaleFiles`, `getUnresolvedByName`,
//!   `getUnresolvedReferences()` (the unfiltered fetch-all;
//!   `unresolved_pending_batch` pages the pending set), `deleteUnresolvedByNode`
//!   (its per-file cascade is already inside `delete_file`), `getAllMetadata`,
//!   `iterateNodesByLanguageWithDecorator`) → added with their consuming phase
//!   (resolver/orchestrator, Phases 2–3); the pipeline's currently-ported
//!   operations all have trait methods. Final search *scoring* (kind/path/name
//!   bonuses, fuzzy, rescoring) is upstream product logic by design — see the
//!   trait's "search candidates are unranked" contract.
//! - **`traverseBFS`/`traverseDFS`**: the TS pair ships here as the single
//!   BFS-shaped [`GraphStore::traverse`]; the DFS variant's consumer arrives
//!   with `selene-graph`, where the depth-first replay walks
//!   (`impact_radius`, `type_hierarchy`) already cover today's DFS-order
//!   needs in-store.
//!
//! ## Layout
//!
//! `src/store.rs` — the [`GraphStore`] contract + shared parameter/result
//! types (no SurrealDB, no I/O). `src/surreal.rs` — open/init, schema apply,
//! bulk-load mode. `src/schema.rs` — the v1 SurrealQL DDL. The operation
//! groups are inherent-method modules (`src/nodes.rs`, `src/edges.rs`,
//! `src/files.rs`, `src/unresolved.rs`, `src/meta.rs`, `src/search.rs`,
//! `src/traverse.rs`), wired onto the trait by the pure-delegation
//! `src/store_impl.rs`. `bench_support` (behind the `bench-support` feature)
//! generates the §5.3 synthetic corpus.
//!
//! Target design: `docs/specs/2026-07-11-rust-graph-db-migration-design.md`
//! (PRD §4 data model, §5 storage). Ported-knowledge reference:
//! `docs/reference/from-codegraph/maps/db-graph-search.md`. Build plan:
//! `docs/plans/2026-07-12-phase1-selene-db.md`.

#[cfg(feature = "bench-support")]
pub mod bench_support;
mod edges;
mod error;
mod files;
mod meta;
mod nodes;
mod raw;
mod schema;
mod search;
mod semantic;
mod store;
mod store_impl;
mod surreal;
mod traverse;
mod unresolved;
mod util;

pub use error::{Error, Result};
pub use schema::SCHEMA_VERSION;
// The extraction row records live in `selene-core` (their producer is
// `selene-extract`, which must not depend on this crate); re-exported here
// so store-side code and tests keep the `selene_db::FileRecord` paths.
pub use selene_core::{FileRecord, RefStatus, UnresolvedRef};
pub use semantic::embedding_text;
pub use store::{
    Direction, GraphStats, GraphStore, NeighborEntry, ReplaceStats, SearchCandidate, Subgraph,
    TraversalOptions, UnresolvedKey,
};
pub use surreal::{DATABASE_DIRNAME, SurrealStore};
