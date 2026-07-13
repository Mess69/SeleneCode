//! [`ResolveError`] — the crate's error type.
//!
//! # `Err` is for malfunctions, not for misses
//!
//! Resolution's *expected* outcomes are all successes: a reference with no
//! candidate, an ambiguous name that declines rather than guesses, a path-shaped
//! ref that binds to nothing — every one of those is `Ok(None)`, and the batch
//! pass records it in `stats.unresolved`. `Err` is reserved for a genuine
//! malfunction (the store failed), mirroring `selene-db`'s own contract and,
//! one layer up, the `isError` reservation of PRD §8.2: an agent that sees one
//! or two errors early abandons the tool.
//!
//! Per-reference failures are therefore **collected, never thrown** — a
//! resolver that panics on one odd reference would take down an index of a
//! million.

use thiserror::Error;

/// A resolution malfunction. Not a miss — see the module docs.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// The graph store failed (the only genuine malfunction in this crate).
    #[error("graph store error: {0}")]
    Store(#[from] selene_db::Error),

    /// The resolver was built without a tokio runtime handle to bridge the
    /// (async) store from its (sync) strategies. This is a wiring bug in the
    /// caller, not a data condition: see [`crate::StoreContext`]'s docs on the
    /// sync/async seam.
    #[error("no tokio runtime available to drive the graph store: {0}")]
    NoRuntime(String),
}

/// `Result` with [`ResolveError`] as the error.
pub type Result<T> = std::result::Result<T, ResolveError>;
