//! Semantic (vector) search — the DB half of SeleneCode's optional hybrid search.
//!
//! This module stores per-node embeddings, defines the HNSW vector index, and runs a **hybrid**
//! search that fuses the lexical BM25 result with a vector KNN result. It holds **no** embedding
//! model — the caller supplies the vectors (generation lives behind the `semantic-search` feature
//! in `selene-embed`, so a default build never pulls the native ONNX runtime). Everything here is
//! plain SurrealQL over `f32` arrays, so it compiles in every build; it is simply inert until a
//! `selene embed` run populates the `embedding` field and defines the index.
//!
//! # Why fuse instead of replace
//!
//! Lexical BM25 is precise when the query shares tokens with a symbol; vector KNN bridges the
//! **vocabulary gap** (`keypress` → `keybinding`) where they share none. Reciprocal Rank Fusion
//! (RRF) keeps the best of both: a result ranked high by *either* signal surfaces, and one ranked
//! high by *both* wins. RRF is done here in Rust (`sum 1/(k+rank)`) rather than SurrealDB's
//! `search::rrf` so the fused candidates carry full node data, not just ids.

use serde_json::Value;

use selene_core::{Node, NodeKind};

use crate::Result;
use crate::nodes::NODE_FIELDS;
use crate::store::SearchCandidate;
use crate::surreal::SurrealStore;

/// RRF smoothing constant. Standard value; a larger `k` flattens the contribution of top ranks.
const RRF_K: f64 = 60.0;
/// How many candidates to pull from each signal before fusing.
const PER_SIGNAL: usize = 50;

/// The text embedded for a node: its names + signature + docstring, the fields a human would read to
/// know what it is. Kept here so the `embed` command and any future re-embed agree on the input.
pub fn embedding_text(node: &Node) -> String {
    let mut parts = vec![node.name.clone(), node.qualified_name.clone()];
    if let Some(s) = &node.signature {
        parts.push(s.clone());
    }
    if let Some(d) = &node.docstring {
        parts.push(d.clone());
    }
    parts.retain(|s| !s.trim().is_empty());
    parts.join(" ")
}

impl SurrealStore {
    /// Define the nullable `embedding` field. The node table is SCHEMAFULL, so this MUST run before
    /// any `store_embeddings`. Idempotent; never part of the default `apply_schema`, so a
    /// lexical-only index never carries it.
    pub async fn define_embedding_field(&self) -> Result<()> {
        self.db()
            .query("DEFINE FIELD IF NOT EXISTS embedding ON node TYPE option<array<float>>;")
            .await?
            .check()?;
        Ok(())
    }

    /// Build the HNSW index over the (now populated) `embedding` field. Run AFTER `store_embeddings`
    /// so the index builds once in bulk rather than being maintained per UPDATE. `dim` must match the
    /// model's output size — a mismatched vector is rejected.
    pub async fn define_embedding_index(&self, dim: usize) -> Result<()> {
        let ddl = format!(
            "DEFINE INDEX IF NOT EXISTS node_embedding_hnsw ON node FIELDS embedding \
             HNSW DIMENSION {dim} DIST COSINE;"
        );
        self.db().query(ddl).await?.check()?;
        Ok(())
    }

    /// Store embeddings onto existing nodes, addressed by their record id. Batched per call.
    pub async fn store_embeddings(&self, rows: &[(String, Vec<f32>)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let payload: Vec<Value> = rows
            .iter()
            .map(|(id, vec)| serde_json::json!({ "id": id, "embedding": vec }))
            .collect();
        // One round trip per chunk; `FOR` is fine here — embedding is an opt-in, index-time pass,
        // never the hot query path.
        self.db()
            .query(
                "FOR $row IN $rows { UPDATE type::record('node', $row.id) SET embedding = $row.embedding; }",
            )
            .bind(("rows", payload))
            .await?
            .check()?;
        Ok(())
    }

    /// Whether this index has any embeddings — i.e. whether `selene embed` has run. Search uses this
    /// to pick hybrid vs. lexical without the caller having to know.
    pub async fn has_embeddings(&self) -> Result<bool> {
        let mut resp = self
            .db()
            .query("SELECT count() FROM node WHERE embedding != NONE GROUP ALL")
            .await?;
        let n: Option<Value> = resp.take(0)?;
        Ok(n.and_then(|v| v.get("count").and_then(|c| c.as_u64())).unwrap_or(0) > 0)
    }

    /// K nearest nodes to `query_vec` by cosine distance, via the HNSW index. `raw_score` is a
    /// descending relevance (`1/(1+dist)`), so it composes with the lexical score's polarity.
    pub async fn vector_search(
        &self,
        query_vec: &[f32],
        kinds: &[NodeKind],
        languages: &[String],
        limit: usize,
    ) -> Result<Vec<SearchCandidate>> {
        if query_vec.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = format!(
            "SELECT {NODE_FIELDS}, vector::distance::knn() AS dist FROM node \
             WHERE embedding <|{limit},COSINE|> $qvec"
        );
        if !kinds.is_empty() {
            sql.push_str(" AND kind IN $kinds");
        }
        if !languages.is_empty() {
            sql.push_str(" AND language IN $languages");
        }
        sql.push_str(" ORDER BY dist ASC LIMIT $limit");

        let mut query = self.db().query(sql).bind(("qvec", query_vec.to_vec()));
        if !kinds.is_empty() {
            query = query.bind(("kinds", crate::search::kind_strings(kinds)));
        }
        if !languages.is_empty() {
            query = query.bind(("languages", languages.to_vec()));
        }
        query = query.bind(("limit", limit as i64));

        let mut resp = match query.await {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        let rows: std::result::Result<Vec<Value>, surrealdb::Error> = resp.take(0);
        let rows = match rows {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        rows.into_iter()
            .map(|mut row| {
                let dist = row.get("dist").and_then(|v| v.as_f64()).unwrap_or(1.0);
                if let Some(obj) = row.as_object_mut() {
                    obj.remove("dist");
                }
                let node: Node = serde_json::from_value(row)?;
                Ok(SearchCandidate { node, raw_score: 1.0 / (1.0 + dist) })
            })
            .collect()
    }

    /// Hybrid lexical + semantic search, fused with RRF. Falls back to pure lexical when there are
    /// no embeddings (so it is always safe to call). `raw_score` on the result is the RRF score.
    pub async fn hybrid_search(
        &self,
        query_text: &str,
        query_vec: &[f32],
        kinds: &[NodeKind],
        languages: &[String],
        limit: usize,
    ) -> Result<Vec<SearchCandidate>> {
        let lexical = self
            .search_fts(&[query_text.to_string()], kinds, languages, PER_SIGNAL, 0)
            .await?;
        let semantic = self.vector_search(query_vec, kinds, languages, PER_SIGNAL).await?;

        if semantic.is_empty() {
            let mut out = lexical;
            out.truncate(limit);
            return Ok(out);
        }

        // RRF: each list contributes `1/(k + rank)` (rank 0-based) to a node's fused score. Identity
        // is the node's record id. First occurrence keeps the (richer) node payload.
        use std::collections::HashMap;
        let mut score: HashMap<String, f64> = HashMap::new();
        let mut node_of: HashMap<String, Node> = HashMap::new();
        for list in [&lexical, &semantic] {
            for (rank, cand) in list.iter().enumerate() {
                *score.entry(cand.node.id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f64);
                node_of.entry(cand.node.id.clone()).or_insert_with(|| cand.node.clone());
            }
        }
        let mut fused: Vec<SearchCandidate> = score
            .into_iter()
            .filter_map(|(id, s)| node_of.remove(&id).map(|node| SearchCandidate { node, raw_score: s }))
            .collect();
        fused.sort_by(|a, b| {
            b.raw_score
                .partial_cmp(&a.raw_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.node.id.cmp(&b.node.id)) // stable tiebreak
        });
        fused.truncate(limit);
        Ok(fused)
    }
}
