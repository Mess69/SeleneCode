//! `selene-graph` — the query surface over the knowledge graph.
//!
//! Target design: `docs/specs/2026-07-11-rust-graph-db-migration-design.md` (PRD §3);
//! build plan: `docs/plans/2026-07-13-phase45-graph-context-mcp.md`; the TS parity source
//! is mapped in `docs/reference/from-codegraph/maps/db-graph-search.md`.
//!
//! # What this crate is
//!
//! [`QueryManager`] turns a `GraphStore` into the vocabulary the layers above speak. It is
//! **deliberately thin**: traversal already lives in SurrealQL (the locked SurrealQL-max
//! decision), so this is a seam, not a graph engine.
//!
//! ```text
//! selene-db (GraphStore) → selene-graph (QueryManager) → selene-context → selene-mcp
//! ```
//!
//! **No reverse edge, ever.** A `selene-graph` that reaches up into `selene-context` for a
//! budget is the cycle that ends with budget logic smeared across three crates.
//!
//! # The two rules that shape every signature here
//!
//! - **`S: GraphStore`, never `dyn`.** The trait uses RPITIT, so it is not object-safe.
//! - **"Nothing found" is not an error.** A missing symbol, an empty file list, an
//!   un-indexed project — all `Ok`. The only error this crate *chooses* to return is
//!   [`GraphError::PathRefusal`] (#527); everything else is a genuine malfunction. One
//!   `isError` early and an agent abandons the tool for the session.

mod error;
mod query;

pub use error::{GraphError, Result};
pub use query::{FileInfo, QueryManager, normalize_path, tokenize_project_name};
