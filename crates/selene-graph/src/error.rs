//! `selene-graph`'s error type.
//!
//! # `PathRefusal` is the ONE `isError` source in Phase 4
//!
//! Everything else this crate can fail at is either a genuine store malfunction
//! (`Store`) or a disk problem (`Io`). A *recoverable* condition — a symbol that does not
//! exist, a file not in the index, an empty result — is **never** an error here: it is an
//! `Ok` with an empty/`None` payload, and the tool layer turns it into success-shaped
//! guidance.
//!
//! That is not a style preference. One `isError` early and an agent abandons the tool for
//! the rest of the session (PRD §8.2), so the set of things allowed to *be* an error is
//! kept as small as it can possibly be — and it is enumerated here, in one place, rather
//! than being an emergent property of a hundred `?`s.

use std::path::PathBuf;

/// A `selene-graph` failure.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The graph store malfunctioned. A genuine error.
    #[error("graph store: {0}")]
    Store(#[from] selene_db::Error),

    /// **A path escaped the project root** (#527) — the security refusal, and the only
    /// error in this crate that is a *deliberate* answer rather than a malfunction.
    #[error("path refused: {path} is outside the project root")]
    PathRefusal {
        /// The offending path, as the caller wrote it.
        path: String,
    },

    /// Reading a source file off disk failed.
    #[error("io: {path}: {source}")]
    Io {
        /// The file we were reading.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// `selene-graph`'s result type.
pub type Result<T> = std::result::Result<T, GraphError>;
