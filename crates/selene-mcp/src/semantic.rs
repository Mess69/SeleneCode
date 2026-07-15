//! Warm embedder + hybrid search — compiled ONLY under `semantic-search`.
//!
//! The MCP server (and the daemon it becomes) loads the local embedding model **once** and holds it
//! for the process's life, so a query costs ~8 ms to embed rather than the ~3.6 s the model takes to
//! load. That is the whole reason to hold it warm here rather than load per query the way the
//! one-shot CLI does. Query embedding is serialised behind a `Mutex` (the ONNX session is not built
//! for concurrent inference, and 8 ms under the lock is nothing next to the graph reads around it).

use std::sync::OnceLock;

use selene_core::Node;
use selene_db::SurrealStore;
use selene_embed::Embedder;
use selene_graph::QueryManager;
use tokio::sync::Mutex;

/// The one warm model for this process. `None` until the first query lazily loads it.
static EMBEDDER: OnceLock<Mutex<Option<Embedder>>> = OnceLock::new();

/// Embed `query` against the warm model, loading it once on first use. `None` if the model can't be
/// loaded (e.g. no cached weights and no network) — the caller then falls back to lexical search.
async fn embed_query(query: &str) -> Option<Vec<f32>> {
    let cell = EMBEDDER.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().await;
    if guard.is_none() {
        // Loading is seconds; do it off the async workers so it never stalls the runtime.
        let loaded = tokio::task::spawn_blocking(Embedder::load).await.ok()?.ok()?;
        *guard = Some(loaded);
    }
    guard.as_mut()?.embed_query(query).ok()
}

/// The query's embedding, but only when `qm`'s index actually has vectors — so `explore` skips the
/// (cheap, but pointless) embedding on a lexical-only index. `None` means "stay lexical".
pub async fn embed_query_for(qm: &QueryManager<SurrealStore>, query: &str) -> Option<Vec<f32>> {
    if !qm.store().has_embeddings().await.unwrap_or(false) {
        return None;
    }
    embed_query(query).await
}

/// Hybrid (lexical BM25 + vector KNN, RRF-fused) search, as a list of nodes. `None` when the index
/// carries no embeddings (no `selene embed` was run) or the query can't be embedded — the caller
/// then uses the lexical path, so this is always safe to try first.
pub async fn hybrid_nodes(
    qm: &QueryManager<SurrealStore>,
    query: &str,
    limit: usize,
) -> Option<Vec<Node>> {
    if !qm.store().has_embeddings().await.unwrap_or(false) {
        return None;
    }
    let qvec = embed_query(query).await?;
    let cands = qm
        .store()
        .hybrid_search(query, &qvec, &[], &[], limit)
        .await
        .ok()?;
    Some(cands.into_iter().map(|c| c.node).collect())
}
