//! `selene-embed` — local, offline text embeddings for SeleneCode's optional semantic search.
//!
//! This crate is a thin wrapper over [`fastembed`], whose sole job is to **isolate the native ONNX
//! runtime** behind a single dependency the rest of the workspace pulls in only under the
//! `semantic-search` feature. A default SeleneCode build never compiles this — it stays a lean,
//! fully-offline static binary. When semantic search IS enabled, the model runs on CPU with no API
//! key and no network after a one-time download (cached under the fastembed cache dir).
//!
//! # Why embeddings at all
//!
//! Lexical BM25 search cannot bridge a **vocabulary gap**: on a large repo, a query for `keypress`
//! never reaches a symbol named `keybinding` because they share no token. A 384-d sentence embedding
//! places them near each other in vector space (measured cosine ≈ 0.80 vs ≈ 0.65 to `mouse`), so a
//! KNN search finds the semantic neighbour BM25 is blind to. SurrealDB fuses the two with
//! `search::rrf` (verified working in the embedded engine).

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// The embedding dimension for [`EmbeddingModel::BGESmallENV15`]. The HNSW index is defined with
/// exactly this, so the two must not drift — a mismatched vector is rejected at insert time.
pub const EMBED_DIM: usize = 384;

/// A loaded local embedding model. Holds the ONNX session; construct once and reuse (loading the
/// model is the expensive part — a few seconds, plus the one-time download).
pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Load BGE-small-en-v1.5 (384-d). Downloads the model once, then runs fully offline.
    pub fn load() -> Result<Self> {
        let model = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))
            .context("load the BGE-small embedding model")?;
        Ok(Self { model })
    }

    /// Embed a batch of documents (node texts) into 384-d vectors, in order. Batching lets the model
    /// amortise its overhead; fastembed parallelises the batch internally.
    pub fn embed_documents(&mut self, docs: &[String]) -> Result<Vec<Vec<f32>>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        // Borrow, don't clone the strings — `embed` accepts any `AsRef<str>`.
        let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
        self.model.embed(refs, None).context("embed documents")
    }

    /// Embed a single query string. (BGE does not require a special query prefix for retrieval.)
    pub fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let mut out = self.model.embed(vec![query.to_string()], None).context("embed query")?;
        out.pop().context("embedding produced no vector")
    }
}
