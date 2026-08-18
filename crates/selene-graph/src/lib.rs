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
//! # ⚠ Inert-seam disclosure — this crate has NO production caller yet
//!
//! Every method here is consumed by `selene-context` (Tasks 5–12) and reaches production
//! through `selene serve --mcp` (Task 14). **Until then it is exercised only by tests.**
//!
//! That is legitimate sequencing, not an accident — but this project has shipped **four**
//! seams whose unit tests passed while nothing called them (`import_mappings`, the four
//! project singletons, all five synthesizers, and the batch driver itself), and every one was
//! found by a gate rather than a test, because *a test double injects what a stub fails to
//! load*. So it is written down rather than assumed:
//!
//! > **If any method in this crate is still uncalled after Task 13's gate, it is dead —
//! > delete it or wire it. Task 13's ledger pass checks this list against reality.**
//!
//! Two are already on watch:
//! - [`QueryManager::group_by_definition`] (#764) only means anything once Task 18 renders
//!   *grouped* callers/callees. If Task 18 ships a flat list, this method dies quietly — a
//!   fifth inert seam. **Verify at Task 18.**
//! - Pass 4's core-directory boost needs a `dominant_file()` primitive that does not exist
//!   (see `selene-context`'s `DominantFile`); until it does, that scoring pass is a no-op.
//!
//! # The two rules that shape every signature here
//!
//! - **`S: GraphStore`, never `dyn`.** The trait uses RPITIT, so it is not object-safe.
//! - **"Nothing found" is not an error.** A missing symbol, an empty file list, an
//!   un-indexed project — all `Ok`. The only error this crate *chooses* to return is
//!   [`GraphError::PathRefusal`] (#527); everything else is a genuine malfunction. One
//!   `isError` early and an agent abandons the tool for the session.

mod adjacency;
/// Whole-graph analysis (Louvain, Tarjan SCC, betweenness) — in-RAM by design.
/// The SurrealQL-max decision covers *traversal at query time*; plain-code
/// analytics over the full graph is the same species as the resolver's
/// in-memory symbol table. Moved here from `selene-cli` (2026-08-18) so the
/// MCP surface and the CLI share one implementation.
pub mod analysis;
mod error;
mod query;
mod source;
mod symbols;

pub use adjacency::{
    DEFAULT_IMPACT_DEPTH, MAX_DEPTH, MAX_LIMIT, MIN_DEPTH, MIN_LIMIT, clamp_depth, clamp_limit,
};
pub use error::{GraphError, Result};
pub use query::{FileInfo, QueryManager, normalize_path, tokenize_project_name};
pub use source::{
    CHAR_BUDGET, CONFIG_LEAF_LANGUAGES, DEFAULT_READ_LIMIT, FileSlice, number_lines,
    validate_path_within_root,
};
pub use symbols::{RUST_PATH_PREFIXES, SymbolGroup, matches_symbol};
