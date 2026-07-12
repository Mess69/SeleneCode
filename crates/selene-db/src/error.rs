//! The crate-wide error type for `selene-db`.

/// Errors surfaced by [`crate::GraphStore`] implementations.
///
/// These are **store-internal failures** — driver errors, decode errors, a
/// malformed record shape. They are distinct from the product-level "not
/// indexed" / "symbol not found" guidance an agent-facing surface returns (that
/// lives in [`selene_core::Error`] / `selene-mcp`); per PRD §8.2, `isError` is
/// reserved for genuine malfunctions, and *every* `GraphStore` trait method
/// documents which misses are expected (and therefore return `Ok` with an
/// empty/`None` result) versus which are a real store failure (and therefore
/// `Err`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A SurrealDB driver or query error.
    ///
    /// **Load-bearing spike finding** (Task 1, `tests/spike_surreal.rs`):
    /// SurrealDB per-statement errors — e.g. a unique-index violation — do
    /// **not** surface at `db.query(..).await` (that resolves `Ok` even when a
    /// later statement in the same multi-statement query failed). They surface
    /// only when the corresponding statement result is unwrapped with
    /// `Response::take(idx)`. Every `GraphStore` implementation method that
    /// runs a query MUST call `.take()` on every statement index it cares
    /// about — checking only the outer `query().await` result is not
    /// sufficient to detect a failure.
    #[error(transparent)]
    Surreal(#[from] surrealdb::Error),

    /// An error from `selene-core` (e.g. an unknown `NodeKind`/`EdgeKind` wire
    /// string encountered while decoding a stored record).
    #[error(transparent)]
    Core(#[from] selene_core::Error),

    /// A JSON value round-tripped through the store (edge `metadata`, file
    /// `errors`, unresolved-ref `candidates`) failed to (de)serialize.
    #[error("json (de)serialization failed: {0}")]
    Serde(#[from] serde_json::Error),

    /// A value returned by the store did not have the shape a `GraphStore`
    /// method expected to decode — e.g. a `RecordId` that isn't a
    /// `<kind>:<key>` pair, or a query row missing a field the schema
    /// guarantees. A store-internal consistency error; never expected in
    /// normal operation and never used for "not found" (see the module docs).
    #[error("unexpected record shape: {0}")]
    Decode(String),

    /// A deferred (`CONCURRENTLY`) index rebuild reported a build failure —
    /// `bulk_load_finish`'s readiness poll saw `building.status: "error"`. A
    /// genuine store malfunction (the index would otherwise silently serve
    /// partial search results), never an expected outcome.
    #[error("index build failed: {0}")]
    IndexBuild(String),

    /// The store on disk was written by a **newer** schema than this build
    /// supports (`meta:schema_version` > [`crate::SCHEMA_VERSION`]).
    /// `apply_schema` refuses to touch such a store: every DDL statement is
    /// `DEFINE ... IF NOT EXISTS`, so against a future schema they would all
    /// silently no-op and this build would then read/write shapes it does
    /// not understand. Upgrade the binary, or discard and re-index.
    #[error(
        "store schema version {stored} is newer than this build supports \
         ({supported}) — refusing to open; upgrade the binary or re-index"
    )]
    SchemaTooNew {
        /// The version stored in `meta:schema_version`.
        stored: u32,
        /// This build's [`crate::SCHEMA_VERSION`].
        supported: u32,
    },
}

/// Convenience result alias for `selene-db`.
pub type Result<T> = std::result::Result<T, Error>;
