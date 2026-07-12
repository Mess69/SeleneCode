//! `selene-db` — Graph store: the [`GraphStore`] trait + embedded SurrealDB backend + FTS.
//!
//! Everything DB-shaped lives behind [`GraphStore`]: nodes, edges, files,
//! unresolved references, metadata/stats, search candidates, and traversals
//! (callers/callees/impact-radius/path-finding/hierarchy/BFS). The rest of
//! the pipeline (`selene-graph`, `selene-context`, `selene-mcp`, `selene-cli`)
//! depends only on this trait, never on SurrealDB directly — that indirection
//! is the whole point: it is the escape hatch to a permissive fallback
//! backend (IndraDB/redb + Tantivy) if SurrealDB's BSL license ever blocks
//! packaging/adoption (PRD §5.2).
//!
//! **Status:** the trait and its shared parameter/result types live in
//! `src/store.rs`; [`SurrealStore`] (`src/surreal.rs`) opens/initializes the
//! embedded SurrealDB datastore and applies the v1 schema (`src/schema.rs`).
//! Its `GraphStore` operation methods land in subsequent tasks — see
//! `docs/plans/2026-07-12-phase1-selene-db.md`.
//!
//! Target design: `docs/specs/2026-07-11-rust-graph-db-migration-design.md`
//! (PRD §4 data model, §5 storage). Ported-knowledge reference:
//! `docs/reference/from-codegraph/maps/db-graph-search.md`.

mod error;
mod schema;
mod store;
mod surreal;

pub use error::{Error, Result};
pub use schema::SCHEMA_VERSION;
pub use store::{
    Direction, FileRecord, GraphStats, GraphStore, NeighborEntry, RefStatus, SearchCandidate,
    Subgraph, TraversalOptions, UnresolvedRef,
};
pub use surreal::{DATABASE_DIRNAME, SurrealStore};
