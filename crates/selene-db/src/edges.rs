//! Edge CRUD + adjacency + file projections: inherent methods on
//! [`SurrealStore`] carrying the edge section of [`crate::GraphStore`]
//! (Task 5); the trait impl in `src/store_impl.rs` delegates here (Task 10).
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
//! kind: every adjacency read is **record-anchored** — one multi-statement
//! query that first collects the edge record ids by walking the graph
//! pointers *from the queried node records themselves* (`LET $eids =
//! array::flatten(array::flatten((SELECT VALUE [->kind1, ->kind2, ...]
//! FROM $rids)))` — a single pass over the queried records projecting every
//! kind's pointer field at once; the double flatten is load-bearing — see
//! [`anchored_adjacency_sql`]), then point-fetches those edge records
//! (`SELECT ... FROM $eids`).
//! `record::tb(id)` recovers which table a given edge came from — exactly
//! the [`EdgeKind`] wire string, since table names are the wire strings.
//!
//! This replaced a multi-table scan (`SELECT ... FROM t1, t2, ... WHERE in
//! IN $rids`) in the Task 9b perf pass: SurrealDB executes that shape as a
//! full scan of every listed edge table with a per-row linear `IN`-list
//! membership test — O(edges × frontier) per traversal level, measured at
//! ~1.5 s for a 150-id frontier expansion (and ~0.16 s for a single
//! 2.1k-edge hub's incoming lookup) on a 20k-node/102k-edge graph, and
//! catastrophically worse for deep traversals whose frontiers grow into
//! the thousands. The record-anchored form is O(frontier × degree) point
//! lookups — ~16 ms for the same raw hub lookup, ~25 ms end-to-end for the
//! same 150-id frontier batch (release, kv-mem; see
//! `docs/benchmarks/2026-07-phase1-db-gate.md` for the probe table). Graph pointers
//! are populated by `INSERT RELATION` exactly as by `RELATE`
//! (`surrealdb-core` 3.2.1 `doc/insert.rs` runs `store_edges_data` on both
//! paths), so no extra index is needed to serve these reads.
//!
//! ## Record id ↔ edge endpoint mapping
//!
//! An edge's `in`/`out` fields are SurrealDB-reserved `record<node>` links,
//! sent as explicit `RecordId` values on each `INSERT RELATION` batch item
//! ([`edge_item`]), never as string content. Reading them back uses the same
//! `record::id(..)` bridge
//! `src/nodes.rs` uses for `Node.id`: `record::id(in) AS source,
//! record::id(out) AS target` yields the raw `Node.id` strings, not
//! SurrealDB's backtick-escaped display form.
//!
//! `selene_core::Edge` has no id of its own — an edge's SurrealDB record id
//! is never read back through the trait surface; identity is
//! `(source, target, kind, line, col)`, enforced by the schema's unique index
//! (`src/schema.rs`). Since the Task 9b perf pass the record key *encodes*
//! that identity deterministically ([`edge_record_id`]): an array key
//! `[source, target, line ?? -1, col ?? -1]` in the kind's table. The key
//! being derived from the identity is what makes duplicate detection a pure
//! point lookup (below); the unique index stays as the schema-level backstop.
//!
//! ## Bulk `INSERT RELATION`, not per-edge `RELATE`
//!
//! Edges are written with SurrealDB's native bulk relation insert:
//! `INSERT RELATION INTO <kind> $batch RETURN VALUE 1`, one statement per
//! kind present in the chunk, each `$batch` element carrying `id` (the
//! deterministic record id), `in`/`out` (endpoint `RecordId`s) and the edge's
//! own fields ([`edge_item`]). Optional fields are *omitted* when `None`
//! (mirrors `src/nodes.rs`'s `node_content` — SCHEMAFULL `option<T>` columns
//! accept only absent/`NONE`, never JSON `null`). `Edge` serializes its
//! position field as `column`; the schema's edge tables store it as `col`
//! (`src/schema.rs`'s field-naming note) — [`edge_content`] renames
//! `column` → `col` after serializing.
//!
//! This replaced one-`RELATE`-statement-per-edge multi-statement queries in
//! the Task 9b perf pass (~1.4k edges/s in the pre-rewrite probe; the bulk
//! path measured ~10.5k edges/s end-to-end — endpoint validation and both
//! dedup layers included — loading 102k edges on the same corpus, release,
//! kv-mem — see `docs/benchmarks/2026-07-phase1-db-gate.md`). `INSERT
//! RELATION` populates the
//! same record graph pointers `RELATE` does (`surrealdb-core` 3.2.1
//! `doc/insert.rs` calls `store_edges_data` on this path), so every
//! traversal read is unaffected.
//!
//! ## Duplicate insert = skip, not error
//!
//! `insert_edges`' contract: a duplicate per the storage identity
//! `(source, target, kind, line ?? -1, col ?? -1)` is **silently skipped**,
//! not an error, and does not count toward the returned insert count.
//! Duplicates are eliminated *before* the bulk insert, in two steps:
//!
//! 1. **Within the call**: a Rust-side identity-key set keeps the first
//!    occurrence of each identity ([`edge_identity_key`]), matching the old
//!    per-statement behavior where the first `RELATE` won and the second hit
//!    the unique index.
//! 2. **Against the store**: one point-lookup statement per chunk
//!    (`SELECT VALUE id FROM $eids` over the chunk's deterministic record
//!    ids — misses simply yield no row) filters out edges that already
//!    exist. This is only possible because the record key *is* the identity
//!    key (see above).
//!
//! The bulk insert therefore only ever writes brand-new identities, and any
//! error it reports is a genuine store malfunction that must propagate —
//! unlike an `INSERT IGNORE`-based variant, which would silently swallow
//! *every* per-row failure (verified in `surrealdb-core` 3.2.1
//! `doc/insert.rs`: with `IGNORE` and no `ON DUPLICATE KEY UPDATE` clause,
//! **any** row error — not just conflicts — becomes a silent skip). That
//! error-masking is why `IGNORE` was rejected despite being one round trip
//! cheaper. Failure semantics: a malformed edge fails its kind's statement
//! within the chunk; earlier statements/chunks are already committed (same
//! contract as `insert_nodes`).
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
//! `insert_edges` batches the *validated, deduplicated* edges at [`CHUNK`]
//! per round trip, mirroring `src/nodes.rs`'s `insert_nodes`. Endpoint
//! pre-validation ([`SurrealStore::existing_node_ids`]) is chunked
//! independently at the same size for its `FROM $ids` point lookup.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use selene_core::{Edge, EdgeKind, NodeKind, Provenance};
use surrealdb::types::{
    Array as SqlArray, Number as SqlNumber, RecordId, RecordIdKey, SurrealValue, Value as SqlValue,
};

use crate::{Error, NeighborEntry, Result, SurrealStore};

/// Edges written per [`SurrealStore::insert_edges`] round trip, and node ids
/// checked per [`SurrealStore::existing_node_ids`] round trip. Mirrors
/// `src/nodes.rs`'s `CHUNK`.
const CHUNK: usize = 500;

/// Shared projection for every edge read: bridges the reserved `in`/`out`
/// record-link fields to raw `Node.id` strings via `record::id(..)` (same
/// pattern `src/nodes.rs` uses for `Node.id` itself), and recovers the
/// [`EdgeKind`] wire string from the table the row came from via
/// `record::tb(id)` — required because one query's rows can span several
/// kind tables, whether point-fetched from a mixed `$eids` array or read
/// through a multi-table `FROM` union (see the module docs).
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

/// Serializes `edge` into its stored content object — the field part of an
/// `INSERT RELATION` batch item ([`edge_item`]): `Edge`'s own camelCase JSON
/// shape minus `source`/`target`/`kind` (not stored fields — they become the
/// `in`/`out` endpoints and the table name), with `column` renamed to `col`
/// (see the module docs' field-naming note).
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

/// The deterministic SurrealDB record id of `edge`: an array key
/// `[source, target, line ?? -1, col ?? -1]` in the edge's kind table. The
/// key encodes the storage identity (the kind is the table), so an edge's
/// existence can be probed by a pure point lookup — see the module docs'
/// record-id section. The `-1` fold matches the schema's `lineKey`/`colKey`
/// computed columns (`src/schema.rs`).
fn edge_record_id(edge: &Edge) -> RecordId {
    let key = RecordIdKey::Array(SqlArray::from(vec![
        SqlValue::String(edge.source.clone()),
        SqlValue::String(edge.target.clone()),
        SqlValue::Number(SqlNumber::Int(edge.line.map_or(-1, i64::from))),
        SqlValue::Number(SqlNumber::Int(edge.column.map_or(-1, i64::from))),
    ]));
    RecordId::new(edge.kind.as_str(), key)
}

/// The storage-identity key of `edge` —
/// `(source, target, kind, line ?? -1, col ?? -1)` — used for the Rust-side
/// within-call dedup (first occurrence wins; see the module docs' duplicate
/// section). Same identity [`edge_record_id`] encodes, as a hashable tuple.
fn edge_identity_key(edge: &Edge) -> (String, String, EdgeKind, i64, i64) {
    (
        edge.source.clone(),
        edge.target.clone(),
        edge.kind,
        edge.line.map_or(-1, i64::from),
        edge.column.map_or(-1, i64::from),
    )
}

/// One element of the `insert_edges` bulk-`INSERT RELATION` batch bound to a
/// `$batch<i>` parameter: the [`edge_content`] fields plus `id` (the
/// deterministic [`edge_record_id`]) and the `in`/`out` endpoint `RecordId`s.
/// Same `serde_json::Value` → [`SqlValue`] bridge as `src/nodes.rs`'s
/// `node_item` — the record ids must be real [`RecordId`] values, which
/// serde-JSON content cannot carry.
fn edge_item(edge: &Edge) -> Result<SqlValue> {
    let SqlValue::Object(mut obj) = edge_content(edge)?.into_value() else {
        return Err(Error::Decode(format!(
            "edge '{}' -> '{}' did not serialize to an object",
            edge.source, edge.target
        )));
    };
    obj.insert("id".to_string(), SqlValue::RecordId(edge_record_id(edge)));
    obj.insert(
        "in".to_string(),
        SqlValue::RecordId(RecordId::new("node", edge.source.as_str())),
    );
    obj.insert(
        "out".to_string(),
        SqlValue::RecordId(RecordId::new("node", edge.target.as_str())),
    );
    Ok(SqlValue::Object(obj))
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

/// Builds the two-statement record-anchored adjacency query the module docs
/// describe: `LET $eids = array::flatten(array::flatten((SELECT VALUE
/// [{arrow}{kind}, ...] FROM $rids)))` — **one** pass over the queried
/// records projecting every kind's graph pointer at once (empty `kinds` =
/// all 12) — then `SELECT {EDGE_FIELDS} FROM $eids` point-fetching the
/// collected edge records, with an optional extra `WHERE` filter on that
/// (frontier-sized) row set. `arrow` is `"->"` for outgoing (the queried
/// records are the edges' `in`) or `"<-"` for incoming (the edges' `out`).
/// Callers bind `$rids` and take result index 1 — the `LET` occupies index 0.
///
/// The single-pass array projection replaced one subquery *per kind*
/// (`[(SELECT VALUE {arrow}{kind} FROM $rids), ...]`) in the Task 9d perf
/// pass: each per-kind subquery re-loaded every `$rids` record to read one
/// pointer field, so a k-kind query cost k record loads per frontier node —
/// the dominant term in hub-rooted deep traversals (a depth-3 callers
/// prefetch over a 6,183-node frontier paid 4 × 6,183 record loads in the
/// `LET` alone; see `docs/benchmarks/2026-07-phase1-db-gate.md`). The array
/// literal reads all k pointer fields in one load per record.
///
/// The **double** `array::flatten` is load-bearing (verified against the
/// embedded engine — `array::flatten` strips exactly one level per call):
/// the projection yields, per queried record, an array of per-kind edge-id
/// arrays, and `SELECT VALUE` wraps those per-record. One flatten pass
/// leaves nested (and, for edge-less kinds, empty `[]`) elements that make
/// the point-fetch's `record::tb(id)` projection throw ("Expected `record`
/// but found `[]`"); the second pass concatenates them into a flat edge-id
/// list, dropping the empties.
fn anchored_adjacency_sql(kinds: &[EdgeKind], arrow: &str, extra_where: Option<&str>) -> String {
    let pointers = table_list(kinds)
        .iter()
        .map(|k| format!("{arrow}{k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!(
        "LET $eids = array::flatten(array::flatten((SELECT VALUE [{pointers}] FROM $rids))); \
         SELECT {EDGE_FIELDS} FROM $eids"
    );
    if let Some(filter) = extra_where {
        sql.push_str(" WHERE ");
        sql.push_str(filter);
    }
    sql.push(';');
    sql
}

/// `ids` as deduplicated `node` [`RecordId`]s, ready to bind as `$rids`. The
/// dedup matters for the record-anchored reads: a repeated queried id would
/// repeat every one of its graph-pointer subquery rows, duplicating edges in
/// `$eids` (the old `WHERE ... IN $rids` scans were naturally immune).
fn node_rids(ids: &[String]) -> Vec<RecordId> {
    let mut rids: Vec<RecordId> = ids
        .iter()
        .map(|id| RecordId::new("node", id.as_str()))
        .collect();
    rids.sort_unstable();
    rids.dedup();
    rids
}

impl SurrealStore {
    /// Insert `edges`. See the module docs for the bulk `INSERT RELATION`
    /// write shape, the two-layer duplicate-as-skip mechanism, and the
    /// chunking. Endpoints are pre-validated (`existing_node_ids`):
    /// an edge whose source or target is not a known node id is silently
    /// skipped, not an error. Returns the number of edges actually inserted
    /// (excludes skipped missing-endpoint edges and deduped/duplicate edges).
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

        // Endpoint validation first, then within-call dedup: an
        // invalid-endpoint edge must not claim an identity slot away from a
        // later valid edge with the same identity.
        let mut seen = HashSet::new();
        let valid: Vec<&Edge> = edges
            .iter()
            .filter(|e| existing.contains(&e.source) && existing.contains(&e.target))
            .filter(|e| seen.insert(edge_identity_key(e)))
            .collect();

        let mut inserted: u64 = 0;
        for chunk in valid.chunks(CHUNK) {
            inserted += self.insert_edge_chunk(chunk).await?;
        }
        Ok(inserted)
    }

    /// One chunk of [`Self::insert_edges`]: (1) one `SELECT VALUE id FROM
    /// $eids` point lookup over the chunk's deterministic record ids filters
    /// out identities already in the store (misses simply yield no row), then
    /// (2) one `INSERT RELATION INTO <kind> $batch<i> RETURN VALUE 1`
    /// statement per kind present writes the brand-new edges in a single
    /// round trip. Any insert error is a genuine malfunction and propagates —
    /// duplicates were already eliminated (see the module docs).
    async fn insert_edge_chunk(&self, chunk: &[&Edge]) -> Result<u64> {
        if chunk.is_empty() {
            return Ok(0);
        }

        // Computed once per edge: bound (cloned) for the lookup, then zipped
        // back with the chunk for the membership check below.
        let eids: Vec<RecordId> = chunk.iter().map(|e| edge_record_id(e)).collect();
        let mut resp = self
            .db()
            .query("SELECT VALUE id FROM $eids")
            .bind(("eids", eids.clone()))
            .await?;
        let already: Vec<RecordId> = resp.take(0)?;
        // mutable_key_type false positive: RecordId transitively reaches a
        // Regex (interior mutability) through the Value enum, but our keys
        // are plain string/int array keys and are never mutated.
        #[allow(clippy::mutable_key_type)]
        let already: HashSet<RecordId> = already.into_iter().collect();

        let mut by_kind: HashMap<&'static str, Vec<SqlValue>> = HashMap::new();
        for (edge, eid) in chunk.iter().zip(&eids) {
            if already.contains(eid) {
                continue;
            }
            by_kind
                .entry(edge.kind.as_str())
                .or_default()
                .push(edge_item(edge)?);
        }
        if by_kind.is_empty() {
            return Ok(0);
        }

        // Iterate kinds in EdgeKind::ALL order so the statement layout is
        // deterministic (HashMap order is not).
        let mut sql = String::new();
        let mut batches: Vec<Vec<SqlValue>> = Vec::with_capacity(by_kind.len());
        for kind in EdgeKind::ALL {
            if let Some(batch) = by_kind.remove(kind.as_str()) {
                sql.push_str(&format!(
                    "INSERT RELATION INTO {} $batch{} RETURN VALUE 1;",
                    kind.as_str(),
                    batches.len()
                ));
                batches.push(batch);
            }
        }
        let statements = batches.len();
        let mut q = self.db().query(sql);
        for (i, batch) in batches.into_iter().enumerate() {
            q = q.bind((format!("batch{i}"), batch));
        }
        let mut resp = q.await?;
        let mut inserted: u64 = 0;
        for i in 0..statements {
            let rows: Vec<i64> = resp.take(i)?;
            inserted += rows.len() as u64;
        }
        Ok(inserted)
    }

    /// The subset of `ids` that are known node ids, chunked at [`CHUNK`] per
    /// round trip. Used by [`Self::insert_edges`] to pre-validate endpoints
    /// against the ENFORCED edge tables (which reject an insert relating a
    /// missing endpoint outright).
    ///
    /// Selects `FROM $ids` (bound record ids — direct point lookups; a
    /// missing record simply yields no row), **not** `FROM node WHERE id IN
    /// $ids`: the `IN`-list form is a full table scan with a per-row linear
    /// membership test (same trap `src/nodes.rs`'s `get_nodes` documents).
    /// Input ids are deduped so a repeated id cannot fetch twice.
    async fn existing_node_ids(&self, ids: &[String]) -> Result<HashSet<String>> {
        let rids = node_rids(ids);
        let mut out = HashSet::with_capacity(rids.len());
        for chunk in rids.chunks(CHUNK) {
            let mut resp = self
                .db()
                .query("SELECT VALUE record::id(id) FROM $ids")
                .bind(("ids", chunk.to_vec()))
                .await?;
            let found: Vec<String> = resp.take(0)?;
            out.extend(found);
        }
        Ok(out)
    }

    /// Outgoing neighbors of `id`. `kinds` empty means every edge kind;
    /// `provenance`, when `Some`, restricts to edges with exactly that
    /// provenance (a `WHERE` on the record-anchored point-fetch, not a
    /// scan filter). `NeighborEntry.node` is the **target** of each edge.
    pub async fn outgoing(
        &self,
        id: &str,
        kinds: &[EdgeKind],
        provenance: Option<Provenance>,
    ) -> Result<Vec<NeighborEntry>> {
        let extra_where = provenance.is_some().then_some("provenance = $prov");
        let sql = anchored_adjacency_sql(kinds, "->", extra_where);
        let mut q = self
            .db()
            .query(sql)
            .bind(("rids", vec![RecordId::new("node", id)]));
        if let Some(prov) = provenance {
            q = q.bind(("prov", serde_json::to_value(prov)?));
        }
        let mut resp = q.await?;
        let rows: Vec<serde_json::Value> = resp.take(1)?;
        let edges = decode_edges(rows)?;
        self.attach_neighbors(edges, EdgeEndpoint::Target).await
    }

    /// Incoming neighbors of `id`. `kinds` empty means every edge kind. No
    /// provenance filter (deliberately asymmetric with [`Self::outgoing`] —
    /// carried over from the CodeGraph query surface). `NeighborEntry.node`
    /// is the **source** of each edge.
    pub async fn incoming(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>> {
        let sql = anchored_adjacency_sql(kinds, "<-", None);
        let mut resp = self
            .db()
            .query(sql)
            .bind(("rids", vec![RecordId::new("node", id)]))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(1)?;
        let edges = decode_edges(rows)?;
        self.attach_neighbors(edges, EdgeEndpoint::Source).await
    }

    /// The raw edge rows of [`Self::outgoing_batch`] — the anchored adjacency
    /// fetch *without* the neighbor-node attach. The traversal prefetch layer
    /// (`src/traverse.rs`) consumes this directly and maintains its own
    /// cross-level node-payload cache, so a payload is fetched at most once
    /// per traversal however many levels/edges reference it (Task 9d
    /// frontier pruning: the §5.3 probe measured 6,834 cross-level payload
    /// re-fetches in a single depth-3 hub-rooted callers prefetch on the
    /// 20k-node bench graph).
    pub(crate) async fn outgoing_edges(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = anchored_adjacency_sql(kinds, "->", None);
        let mut resp = self.db().query(sql).bind(("rids", node_rids(ids))).await?;
        let rows: Vec<serde_json::Value> = resp.take(1)?;
        decode_edges(rows)
    }

    /// The raw edge rows of [`Self::incoming_batch`] — see
    /// [`Self::outgoing_edges`].
    pub(crate) async fn incoming_edges(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = anchored_adjacency_sql(kinds, "<-", None);
        let mut resp = self.db().query(sql).bind(("rids", node_rids(ids))).await?;
        let rows: Vec<serde_json::Value> = resp.take(1)?;
        decode_edges(rows)
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
        let edges = self.outgoing_edges(ids, kinds).await?;
        self.group_neighbors(edges, EdgeEndpoint::Target).await
    }

    /// [`Self::incoming`] batched over multiple ids, keyed by the queried id.
    pub async fn incoming_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<HashMap<String, Vec<NeighborEntry>>> {
        let edges = self.incoming_edges(ids, kinds).await?;
        self.group_neighbors(edges, EdgeEndpoint::Source).await
    }

    /// Every edge of `kinds` (empty = all) with both endpoints in `ids`, as
    /// one query. Used to recover connectivity among an already-known node
    /// set (e.g. after a BFS visit set is fixed).
    ///
    /// Anchors on the outgoing pointers of `ids` and applies `out IN $rids`
    /// as a `WHERE` on that (frontier-sized) point-fetched row set — the
    /// `IN`-list test runs over the ids' own edges only, never as a
    /// whole-table scan predicate.
    pub async fn edges_between(&self, ids: &[String], kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = anchored_adjacency_sql(kinds, "->", Some("out IN $rids"));
        let mut resp = self.db().query(sql).bind(("rids", node_rids(ids))).await?;
        let rows: Vec<serde_json::Value> = resp.take(1)?;
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
