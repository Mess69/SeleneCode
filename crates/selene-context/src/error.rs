//! `selene-context`'s error type.
//!
//! # The set of things allowed to be an error is TINY, and that is the point
//!
//! "No relevant context", "symbol not found", "the query was all stopwords", "nothing is
//! indexed" — **none of these are errors.** They are ordinary answers, and every one of
//! them returns a success-shaped value that the tool layer renders as *guidance*.
//!
//! An `Err` out of this crate becomes an `isError` at the MCP layer, and one `isError`
//! early makes an agent abandon the tool for the rest of the session. So `Err` means one of
//! exactly two things: the graph store malfunctioned, or a path escaped the project root.

/// A `selene-context` failure — a malfunction, never an answer.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// The layer below malfunctioned (store error, or a #527 path refusal).
    #[error(transparent)]
    Graph(#[from] selene_graph::GraphError),
}

/// `selene-context`'s result type.
pub type Result<T> = std::result::Result<T, ContextError>;
