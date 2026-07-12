//! Edge CRUD + adjacency + file projections: inherent methods on
//! [`SurrealStore`] mirroring the edge section of [`crate::GraphStore`]
//! (Task 5). `impl GraphStore for SurrealStore` is wired later (Task 10);
//! until then these are plain inherent `async fn`s with identical signatures.
//!
//! ## Every `EdgeKind` is its own relation table
//!
//! Unlike nodes (one `node` table with a `kind` field), each of the 12
//! [`EdgeKind`]s is a separate `TYPE RELATION` table named exactly
//! `EdgeKind::as_str()` (`src/schema.rs`). Table names in every query below
//! come **only** from `EdgeKind::as_str()` — an exhaustive Rust enum, never
//! caller-supplied text — so there is no SQL-injection surface despite the
//! queries being assembled with `format!`.
//!
//! A query that spans several kinds does **not** loop one round trip per
//! kind. SurrealQL supports a multi-table `FROM t1, t2, ...` clause as a real
//! union in a single statement (verified against the embedded engine before
//! relying on it), and `record::tb(id)` recovers which table a given row came
//! from — exactly the [`EdgeKind`] wire string, since table names are the
//! wire strings. So `outgoing`/`incoming`/`edges_between`/the file-projection
//! queries are each **one** SurrealQL statement regardless of how many kinds
//! they span, satisfying the "not a per-id loop" batching contract with room
//! to spare.
//!
//! ## Record id ↔ edge endpoint mapping
//!
//! An edge's `in`/`out` fields are SurrealDB-reserved `record<node>` links,
//! populated automatically by `RELATE $from->kind->$to`, never sent as
//! content. Reading them back uses the same `record::id(..)` bridge
//! `src/nodes.rs` uses for `Node.id`: `record::id(in) AS source,
//! record::id(out) AS target` yields the raw `Node.id` strings, not
//! SurrealDB's backtick-escaped display form.
//!
//! `selene_core::Edge` has no id of its own — an edge's SurrealDB record id
//! (table-generated, e.g. `calls:8ok96p…`) is never read back; identity is
//! `(source, target, kind, line, col)`, enforced by the schema's unique index
//! (`src/schema.rs`), not by the record key.
//!
//! ## `RELATE ... CONTENT`, not `SET`
//!
//! Endpoints bind as `$from`/`$to` **`RecordId`** parameters directly in the
//! relate-arrow position (`RELATE $from->calls->$to`) — a bare `$param` is a
//! valid relate-expr, but `type::record('node', $id)` is **not** (verified: a
//! parse error, "Unexpected token `::`, expected :"; the relate-expr grammar
//! only accepts `$param`, an array literal, a few statement keywords, or a
//! literal record id, never a general function-call expression).
//!
//! The edge's own fields are written via `CONTENT $content`, not `SET
//! field = $val, ...`: [`edge_content`] serializes `Edge` and *omits* every
//! `None` field (mirrors `src/nodes.rs`'s `node_content` — SCHEMAFULL
//! `option<T>` columns accept only absent/`NONE`, never JSON `null`), which
//! sidesteps binding an `Option<T>` through the driver entirely. `Edge`
//! serializes its position field as `column`; the schema's edge tables store
//! it as `col` (`src/schema.rs`'s field-naming note) — `edge_content` renames
//! `column` → `col` after serializing.
//!
//! ## Duplicate insert = skip, not error
//!
//! `insert_edges`' contract: a duplicate per the storage identity
//! `(source, target, kind, line ?? -1, col ?? -1)` is **silently skipped**,
//! not an error, and does not count toward the returned insert count. This is
//! implemented by issuing **one `RELATE ... CONTENT` statement per surviving
//! edge**, all statements combined into a single multi-statement query per
//! [`CHUNK`], and inspecting each statement's result individually via
//! `Response::take(idx)` (per the Task 1 spike: a unique-index violation
//! resolves the outer `query().await` as `Ok`, the violation only surfaces at
//! `take`). This was verified against the real embedded engine before relying
//! on it: a failing statement does **not** abort later statements in the same
//! multi-statement query (`take` on a later index still succeeds), and two
//! RELATEs that duplicate each other **within the same query** correctly see
//! each other (the second is rejected) — sequential-visibility, not
//! per-statement snapshot isolation. [`is_unique_violation`] recognizes the
//! violation by matching the error text (`"already contains"`): the public
//! `surrealdb::Error` wire type does not carry a structured
//! `IndexExists`-shaped variant for this case (verified against the crate
//! source — it falls into the generic `Internal` catch-all), only a message
//! string, same as the Task 1/3 spikes found. A `FOR $item IN $batch { RELATE
//! ... }` single-statement loop (the pattern `insert_nodes` uses for `UPSERT`)
//! was deliberately **not** used here: `UPSERT` never conflicts, so per-item
//! success/failure isn't observable that way, and this store needs exactly
//! that per-edge observability to skip duplicates without losing the rest of
//! the chunk.
//!
//! ## `SELECT DISTINCT in.field` / `out.field` does not parse
//!
//! `in`/`out` are reserved tokens (also the relation comparison operators).
//! Dot-dereferencing them in a **plain** projection or a `WHERE` clause works
//! fine (verified: `SELECT in.filePath AS fp FROM ... WHERE out.filePath = ..`
//! parses and runs correctly) — but `SELECT DISTINCT in.filePath ...` and
//! `SELECT DISTINCT out.filePath ...` both fail to parse ("expected FROM"
//! right after the leading `in`/`out` token), a DISTINCT-clause-specific
//! grammar quirk. [`Self::dependent_file_paths`] /
//! [`Self::dependency_file_paths`] therefore never use SQL `DISTINCT`: they
//! fetch the (possibly duplicated) raw path list and sort+dedup in Rust,
//! which the trait contract requires anyway.
//!
//! ## Chunking
//!
//! `insert_edges` batches the *validated* edges at [`CHUNK`] per round trip,
//! mirroring `src/nodes.rs`'s `insert_nodes`. Endpoint pre-validation
//! ([`SurrealStore::existing_node_ids`]) is chunked independently at the same
//! size for the `IN`-list lookup.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use selene_core::{Edge, EdgeKind, NodeKind, Provenance};
use surrealdb::types::RecordId;

use crate::{NeighborEntry, Result, SurrealStore};

/// Edges written per [`SurrealStore::insert_edges`] round trip, and node ids
/// checked per [`SurrealStore::existing_node_ids`] round trip. Mirrors
/// `src/nodes.rs`'s `CHUNK`.
const CHUNK: usize = 500;

/// Shared projection for every edge read: bridges the reserved `in`/`out`
/// record-link fields to raw `Node.id` strings via `record::id(..)` (same
/// pattern `src/nodes.rs` uses for `Node.id` itself), and recovers the
/// [`EdgeKind`] wire string from the table the row came from via
/// `record::tb(id)` — required because a query can span a multi-table `FROM`
/// union (see the module docs).
const EDGE_FIELDS: &str = "record::id(in) AS source, record::id(out) AS target, record::tb(id) AS kind, \
     line, col, provenance, metadata";

/// A decoded [`EDGE_FIELDS`] row, pre-`Edge`-reconstruction. Field names
/// match the SQL aliases verbatim (`col`, not `column` — see the module docs'
/// field-naming note), so no `#[serde(rename)]` is needed.
#[derive(serde::Deserialize)]
struct EdgeRow {
    source: String,
    target: String,
    kind: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    col: Option<u32>,
    #[serde(default)]
    provenance: Option<Provenance>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

impl EdgeRow {
    fn into_edge(self) -> Result<Edge> {
        Ok(Edge {
            source: self.source,
            target: self.target,
            kind: EdgeKind::from_str(&self.kind)?,
            metadata: self.metadata,
            line: self.line,
            column: self.col,
            provenance: self.provenance,
        })
    }
}

/// [`EdgeRow`] plus the target node's `(name, kind)`, decoded from the extra
/// `targetName`/`targetKind` columns [`SurrealStore::cross_file_incoming_with_target`]
/// projects via dot-dereference (`out.name`, `out.kind`).
#[derive(serde::Deserialize)]
struct CrossFileEdgeRow {
    source: String,
    target: String,
    kind: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    col: Option<u32>,
    #[serde(default)]
    provenance: Option<Provenance>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(rename = "targetName")]
    target_name: String,
    #[serde(rename = "targetKind")]
    target_kind: String,
}

impl CrossFileEdgeRow {
    fn into_parts(self) -> Result<(Edge, String, NodeKind)> {
        let edge = Edge {
            source: self.source,
            target: self.target,
            kind: EdgeKind::from_str(&self.kind)?,
            metadata: self.metadata,
            line: self.line,
            column: self.col,
            provenance: self.provenance,
        };
        let target_kind = NodeKind::from_str(&self.target_kind)?;
        Ok((edge, self.target_name, target_kind))
    }
}

/// A single-column `{ fp: String }` row, the shape of
/// [`SurrealStore::dependent_file_paths`]/[`SurrealStore::dependency_file_paths`]'s
/// projection.
#[derive(serde::Deserialize)]
struct FilePathRow {
    fp: String,
}

/// Serializes `edge` into its stored content object for `RELATE ... CONTENT`:
/// `Edge`'s own camelCase JSON shape minus `source`/`target`/`kind` (not
/// stored fields — they become the RELATE endpoints and the table name), with
/// `column` renamed to `col` (see the module docs' field-naming note).
/// `#[serde(skip_serializing_if = "Option::is_none")]` on `Edge`'s optional
/// fields means an absent `Option` is omitted entirely, never sent as JSON
/// `null` (SCHEMAFULL `option<T>` columns accept only absent/`NONE`).
fn edge_content(edge: &Edge) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(edge)?;
    if let serde_json::Value::Object(map) = &mut value {
        map.remove("source");
        map.remove("target");
        map.remove("kind");
        if let Some(col) = map.remove("column") {
            map.insert("col".to_string(), col);
        }
    }
    Ok(value)
}

/// True if `err` is the edge identity unique-index violation (`"already
/// contains"` — see the module docs for why this is a text match rather than
/// a structured error variant). Any other error is a genuine store
/// malfunction and must propagate.
fn is_unique_violation(err: &surrealdb::Error) -> bool {
    err.to_string().contains("already contains")
}

/// The `FROM` table list for `kinds`: the kinds verbatim if non-empty, else
/// every [`EdgeKind`] (empty means "no filter", matching every other
/// `kinds: &[EdgeKind]` parameter on the trait).
fn table_list(kinds: &[EdgeKind]) -> Vec<&'static str> {
    if kinds.is_empty() {
        EdgeKind::ALL.iter().map(EdgeKind::as_str).collect()
    } else {
        kinds.iter().map(EdgeKind::as_str).collect()
    }
}

/// `table_list(kinds)` joined into a `FROM`-clause-ready string.
fn from_clause(kinds: &[EdgeKind]) -> String {
    table_list(kinds).join(", ")
}

/// The `FROM` clause for every [`EdgeKind`] except `contains` — the fixed
/// kind-set the three file-projection methods use (see their docs).
fn non_contains_from_clause() -> String {
    EdgeKind::ALL
        .iter()
        .filter(|k| **k != EdgeKind::Contains)
        .map(EdgeKind::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

impl SurrealStore {
    /// Insert `edges`. See the module docs for the CONTENT-write shape, the
    /// per-statement duplicate-as-skip mechanism, and the chunking. Endpoints
    /// are pre-validated ([`Self::existing_node_ids`]): an edge whose source
    /// or target is not a known node id is silently skipped, not an error.
    /// Returns the number of edges actually inserted (excludes skipped
    /// missing-endpoint edges and deduped/duplicate edges).
    pub async fn insert_edges(&self, edges: &[Edge]) -> Result<u64> {
        if edges.is_empty() {
            return Ok(0);
        }

        let mut referenced_ids: Vec<String> = Vec::with_capacity(edges.len() * 2);
        for e in edges {
            referenced_ids.push(e.source.clone());
            referenced_ids.push(e.target.clone());
        }
        referenced_ids.sort_unstable();
        referenced_ids.dedup();
        let existing = self.existing_node_ids(&referenced_ids).await?;

        let valid: Vec<&Edge> = edges
            .iter()
            .filter(|e| existing.contains(&e.source) && existing.contains(&e.target))
            .collect();

        let mut inserted: u64 = 0;
        for chunk in valid.chunks(CHUNK) {
            inserted += self.relate_chunk(chunk).await?;
        }
        Ok(inserted)
    }

    /// One chunk of [`Self::insert_edges`]: a multi-statement query, one
    /// `RELATE ... CONTENT` per edge, each statement's result inspected
    /// individually so a unique-index violation skips only that edge.
    async fn relate_chunk(&self, chunk: &[&Edge]) -> Result<u64> {
        if chunk.is_empty() {
            return Ok(0);
        }

        let mut sql = String::with_capacity(chunk.len() * 48);
        for (i, edge) in chunk.iter().enumerate() {
            sql.push_str(&format!(
                "RELATE $from{i}->{}->$to{i} CONTENT $content{i};",
                edge.kind.as_str()
            ));
        }

        let mut q = self.db().query(sql);
        for (i, edge) in chunk.iter().enumerate() {
            q = q
                .bind((
                    format!("from{i}"),
                    RecordId::new("node", edge.source.as_str()),
                ))
                .bind((
                    format!("to{i}"),
                    RecordId::new("node", edge.target.as_str()),
                ))
                .bind((format!("content{i}"), edge_content(edge)?));
        }

        let mut resp = q.await?;
        let mut inserted: u64 = 0;
        for i in 0..chunk.len() {
            let result: std::result::Result<Vec<serde_json::Value>, surrealdb::Error> =
                resp.take(i);
            match result {
                Ok(_) => inserted += 1,
                Err(e) if is_unique_violation(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(inserted)
    }

    /// The subset of `ids` that are known node ids, chunked at [`CHUNK`] per
    /// round trip. Used by [`Self::insert_edges`] to pre-validate endpoints
    /// against the ENFORCED edge tables (which reject a `RELATE` to a missing
    /// endpoint outright).
    async fn existing_node_ids(&self, ids: &[String]) -> Result<HashSet<String>> {
        let mut out = HashSet::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            let rids: Vec<RecordId> = chunk
                .iter()
                .map(|id| RecordId::new("node", id.as_str()))
                .collect();
            let mut resp = self
                .db()
                .query("SELECT record::id(id) AS id FROM node WHERE id IN $ids")
                .bind(("ids", rids))
                .await?;
            let rows: Vec<serde_json::Value> = resp.take(0)?;
            for row in rows {
                if let Some(id) = row.get("id").and_then(serde_json::Value::as_str) {
                    out.insert(id.to_string());
                }
            }
        }
        Ok(out)
    }

    /// Outgoing neighbors of `id`. `kinds` empty means every edge kind;
    /// `provenance`, when `Some`, restricts to edges with exactly that
    /// provenance. `NeighborEntry.node` is the **target** of each edge.
    pub async fn outgoing(
        &self,
        id: &str,
        kinds: &[EdgeKind],
        provenance: Option<Provenance>,
    ) -> Result<Vec<NeighborEntry>> {
        let mut sql = format!(
            "SELECT {EDGE_FIELDS} FROM {} WHERE in = $rid",
            from_clause(kinds)
        );
        if provenance.is_some() {
            sql.push_str(" AND provenance = $prov");
        }

        let mut q = self
            .db()
            .query(sql)
            .bind(("rid", RecordId::new("node", id)));
        if let Some(prov) = provenance {
            q = q.bind(("prov", serde_json::to_value(prov)?));
        }
        let mut resp = q.await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        let edges = decode_edges(rows)?;
        self.attach_neighbors(edges, EdgeEndpoint::Target).await
    }

    /// Incoming neighbors of `id`. `kinds` empty means every edge kind. No
    /// provenance filter (deliberately asymmetric with [`Self::outgoing`] —
    /// carried over from the CodeGraph query surface). `NeighborEntry.node`
    /// is the **source** of each edge.
    pub async fn incoming(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>> {
        let sql = format!(
            "SELECT {EDGE_FIELDS} FROM {} WHERE out = $rid",
            from_clause(kinds)
        );
        let mut resp = self
            .db()
            .query(sql)
            .bind(("rid", RecordId::new("node", id)))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        let edges = decode_edges(rows)?;
        self.attach_neighbors(edges, EdgeEndpoint::Source).await
    }

    /// [`Self::outgoing`] batched over multiple ids (no provenance filter),
    /// keyed by the queried id, as **one** query for the whole batch (not a
    /// per-id loop) — this powers the BFS frontier expansion (`selene-graph`,
    /// Task 8). An id with no matching neighbors need not appear as an
    /// explicit empty entry.
    pub async fn outgoing_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let sql = format!(
            "SELECT {EDGE_FIELDS} FROM {} WHERE in IN $rids",
            from_clause(kinds)
        );
        let rids: Vec<RecordId> = ids
            .iter()
            .map(|id| RecordId::new("node", id.as_str()))
            .collect();
        let mut resp = self.db().query(sql).bind(("rids", rids)).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        let edges = decode_edges(rows)?;
        self.group_neighbors(edges, EdgeEndpoint::Target).await
    }

    /// [`Self::incoming`] batched over multiple ids, keyed by the queried id.
    pub async fn incoming_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let sql = format!(
            "SELECT {EDGE_FIELDS} FROM {} WHERE out IN $rids",
            from_clause(kinds)
        );
        let rids: Vec<RecordId> = ids
            .iter()
            .map(|id| RecordId::new("node", id.as_str()))
            .collect();
        let mut resp = self.db().query(sql).bind(("rids", rids)).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        let edges = decode_edges(rows)?;
        self.group_neighbors(edges, EdgeEndpoint::Source).await
    }

    /// Every edge of `kinds` (empty = all) with both endpoints in `ids`, as
    /// one query. Used to recover connectivity among an already-known node
    /// set (e.g. after a BFS visit set is fixed).
    pub async fn edges_between(&self, ids: &[String], kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {EDGE_FIELDS} FROM {} WHERE in IN $rids AND out IN $rids",
            from_clause(kinds)
        );
        let rids: Vec<RecordId> = ids
            .iter()
            .map(|id| RecordId::new("node", id.as_str()))
            .collect();
        let mut resp = self.db().query(sql).bind(("rids", rids)).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        decode_edges(rows)
    }

    /// Cross-file incoming edges landing on any node under `path`: every edge
    /// of every kind **except** `contains` whose target is a node in this
    /// file and whose source is a node in a *different* file, paired with
    /// the target node's `(name, kind)` via dot-dereference (`out.name`,
    /// `out.kind`) — see the module docs for why `contains` is fixed rather
    /// than caller-supplied. Feeds the single-file re-index edge-preservation
    /// protocol (Task 6).
    pub async fn cross_file_incoming_with_target(
        &self,
        path: &str,
    ) -> Result<Vec<(Edge, String, NodeKind)>> {
        let sql = format!(
            "SELECT {EDGE_FIELDS}, out.name AS targetName, out.kind AS targetKind \
             FROM {} WHERE out.filePath = $path AND in.filePath != $path",
            non_contains_from_clause()
        );
        let mut resp = self
            .db()
            .query(sql)
            .bind(("path", path.to_string()))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter()
            .map(|row| {
                let parsed: CrossFileEdgeRow = serde_json::from_value(row)?;
                parsed.into_parts()
            })
            .collect()
    }

    /// Distinct file paths that depend on `path`: files containing a node
    /// with an outgoing non-`contains` edge whose target is a node in
    /// `path`, excluding `path` itself. Sorted, deduped in Rust (see the
    /// module docs for why not SQL `DISTINCT`).
    pub async fn dependent_file_paths(&self, path: &str) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT in.filePath AS fp FROM {} WHERE out.filePath = $path AND in.filePath != $path",
            non_contains_from_clause()
        );
        self.distinct_file_paths(sql, path).await
    }

    /// Distinct file paths that `path` depends on — the mirror of
    /// [`Self::dependent_file_paths`].
    pub async fn dependency_file_paths(&self, path: &str) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT out.filePath AS fp FROM {} WHERE in.filePath = $path AND out.filePath != $path",
            non_contains_from_clause()
        );
        self.distinct_file_paths(sql, path).await
    }

    /// Shared tail of [`Self::dependent_file_paths`]/[`Self::dependency_file_paths`]:
    /// run `sql` (a single `{fp}`-projecting statement bound to `$path`),
    /// then sort+dedup the raw (possibly repeated) path list in Rust.
    async fn distinct_file_paths(&self, sql: String, path: &str) -> Result<Vec<String>> {
        let mut resp = self
            .db()
            .query(sql)
            .bind(("path", path.to_string()))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        let mut paths: Vec<String> = rows
            .into_iter()
            .map(|row| Ok(serde_json::from_value::<FilePathRow>(row)?.fp))
            .collect::<Result<Vec<_>>>()?;
        paths.sort_unstable();
        paths.dedup();
        Ok(paths)
    }

    /// Batch-fetch each edge's neighbor node ([`EdgeEndpoint::Target`] for
    /// outgoing, [`EdgeEndpoint::Source`] for incoming) via
    /// [`Self::get_nodes`] (one round trip for the whole edge list, not a
    /// per-edge lookup) and pair each edge with its neighbor. An edge whose
    /// neighbor id is missing from the node table (never expected in normal
    /// operation — see the module docs' record-id-mapping section) is
    /// dropped rather than surfaced as a partial/placeholder `NeighborEntry`,
    /// consistent with the trait's success-shaped-miss contract.
    async fn attach_neighbors(
        &self,
        edges: Vec<Edge>,
        endpoint: EdgeEndpoint,
    ) -> Result<Vec<NeighborEntry>> {
        let neighbor_ids = dedup_sorted(edges.iter().map(|e| endpoint.of(e).clone()));
        let nodes = self.get_nodes(&neighbor_ids).await?;
        Ok(edges
            .into_iter()
            .filter_map(|edge| {
                nodes
                    .get(endpoint.of(&edge))
                    .cloned()
                    .map(|node| NeighborEntry { node, edge })
            })
            .collect())
    }

    /// [`Self::attach_neighbors`], grouped into a map keyed by the *other*
    /// endpoint (the queried id) — the shape [`Self::outgoing_batch`]/
    /// [`Self::incoming_batch`] need.
    async fn group_neighbors(
        &self,
        edges: Vec<Edge>,
        endpoint: EdgeEndpoint,
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        let neighbor_ids = dedup_sorted(edges.iter().map(|e| endpoint.of(e).clone()));
        let nodes = self.get_nodes(&neighbor_ids).await?;
        let mut out: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
        for edge in edges {
            if let Some(node) = nodes.get(endpoint.of(&edge)).cloned() {
                let key = endpoint.queried_of(&edge).clone();
                out.entry(key)
                    .or_default()
                    .push(NeighborEntry { node, edge });
            }
        }
        Ok(out)
    }
}

/// Which side of an [`Edge`] is the "neighbor" (the node to attach) versus
/// the "queried" side (the id the caller searched by) for a given traversal
/// direction.
#[derive(Clone, Copy)]
enum EdgeEndpoint {
    /// Outgoing: the neighbor is the edge's target, queried side is source.
    Target,
    /// Incoming: the neighbor is the edge's source, queried side is target.
    Source,
}

impl EdgeEndpoint {
    fn of(self, edge: &Edge) -> &String {
        match self {
            EdgeEndpoint::Target => &edge.target,
            EdgeEndpoint::Source => &edge.source,
        }
    }

    fn queried_of(self, edge: &Edge) -> &String {
        match self {
            EdgeEndpoint::Target => &edge.source,
            EdgeEndpoint::Source => &edge.target,
        }
    }
}

/// Decodes every row of an [`EDGE_FIELDS`] projection into an [`Edge`].
fn decode_edges(rows: Vec<serde_json::Value>) -> Result<Vec<Edge>> {
    rows.into_iter()
        .map(|row| {
            let parsed: EdgeRow = serde_json::from_value(row)?;
            parsed.into_edge()
        })
        .collect()
}

/// Sorted, deduplicated `Vec<String>` from an iterator of owned strings.
fn dedup_sorted(iter: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = iter.collect();
    v.sort_unstable();
    v.dedup();
    v
}
