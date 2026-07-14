//! Crate-private helpers shared by the operation modules — the single
//! definition site for the batching constant and the `LIMIT`/`START` bind
//! clamp that `src/nodes.rs`, `src/edges.rs`, `src/search.rs`, and
//! `src/unresolved.rs` previously each carried a copy of.

/// Rows/keys/ids processed per store round trip by every batched operation
/// (`insert_nodes`, `insert_edges`, `insert_unresolved`, `IN`-list reads,
/// keyed multi-statement writes, `existing_node_ids` point lookups).
///
/// Mirrors the TS store's `SQLITE_PARAM_CHUNK_SIZE`, kept here to bound
/// single-statement / bind-array size rather than for a SQLite-specific
/// limit.
pub(crate) const CHUNK: usize = 1000;

/// `usize` → `i64`, saturating at `i64::MAX` — SurrealDB's `LIMIT`/`START`
/// bind as signed integers, so an (absurd) beyond-`i64` count must clamp,
/// not error or wrap.
pub(crate) fn clamp_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}
