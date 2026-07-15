//! Crate-private helpers shared by the operation modules — the single
//! definition site for the batching constant and the `LIMIT`/`START` bind
//! clamp that `src/nodes.rs`, `src/edges.rs`, `src/search.rs`, and
//! `src/unresolved.rs` previously each carried a copy of.

use std::future::Future;

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

/// How many chunk writes may be in flight at once.
///
/// The store was talked to **one query at a time**: 39 sequential round trips to insert django's
/// 19 061 nodes (3.4 s), 94 more for its edges. SurrealDB's own benchmark reaches 300 k ops/s with
/// **128 clients issuing 48 concurrent queries each** — a single caller awaiting one query at a
/// time sees none of that. The engine is concurrent; the caller was not.
///
/// 16 rather than "unbounded": SurrealDB's RocksDB layer sizes its inline-blocking permit pool from
/// the tokio worker count (`surrealdb-core` `kvs/rocksdb/cnf.rs`), so past that width the futures
/// just queue on a semaphore inside the engine and we pay the memory for nothing.
pub(crate) const WRITE_CONCURRENCY: usize = 16;

/// Is this a **retryable optimistic-transaction conflict**? SurrealDB's embedded RocksDB uses
/// optimistic concurrency, so concurrent writers that touch overlapping state get back
/// `Transaction conflict: Resource busy. This transaction can be retried`. It is not a real failure
/// — the loser just re-runs once the winner commits. At small scale (django) the concurrent writes
/// rarely collide; at VS Code scale (257k nodes, ~1.2M edges) they do, and without a retry the whole
/// resolve aborts with an incomplete graph. Detected by message because the SDK surfaces it as an
/// opaque `surrealdb::Error`.
pub(crate) fn is_retryable_conflict(e: &crate::Error) -> bool {
    let s = e.to_string();
    s.contains("Resource busy") || s.contains("can be retried") || s.contains("Transaction conflict")
}

/// Run `op`, retrying on a retryable transaction conflict with a growing (dispersing) backoff.
/// `op` must be idempotent — every batched write here first filters out identities already present,
/// so a re-run after an aborted (nothing-committed) conflict cannot double-insert.
pub(crate) async fn with_conflict_retry<F, Fut, T>(mut op: F) -> crate::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = crate::Result<T>>,
{
    const MAX_ATTEMPTS: u32 = 16;
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < MAX_ATTEMPTS && is_retryable_conflict(&e) => {
                attempt += 1;
                // Grows with the attempt so a burst of conflicting writers disperses rather than
                // all retrying in lockstep. A few ms is nothing next to the write it guards.
                tokio::time::sleep(std::time::Duration::from_millis(2 * attempt as u64)).await;
            }
            Err(e) => return Err(e),
        }
    }
}
