//! Node CRUD + lookups: inherent methods on [`SurrealStore`] carrying the
//! node section of [`crate::GraphStore`] (Task 4); the trait impl in
//! `src/store_impl.rs` delegates here (Task 10).
//!
//! ## Record id ↔ `Node.id` mapping
//!
//! `selene_core::Node::id` is an opaque string (e.g. `"function:3fa2…"`) that
//! *is* the `node` table's record key — `node:⟨that string⟩`. SurrealDB never
//! stores `id` as an ordinary field (the schema does not declare one; see
//! `src/schema.rs`), and a raw `SELECT`'s `id` projects as the driver's
//! *display* form (backtick-quoted when the key contains special characters
//! like `:` — Task 1 spike finding), not the bare key. Round-tripping through
//! that display string would mean re-implementing SurrealDB's record-id
//! escaping by hand.
//!
//! Instead every read query explicitly projects
//! `record::id(id) AS id` (proven in this task's exploration probes) — a
//! built-in that returns the record key's *raw* value, so for our
//! string-keyed `node` table it comes back as the exact original
//! `Node.id`, no parsing required. Every write path (`CREATE`/`UPSERT`)
//! addresses the record directly by its `RecordId`/`type::record(...)` key
//! instead of sending `id` as a content field.
//!
//! ## Upsert semantics
//!
//! `insert_nodes` must implement "insert or replace" (the TS store's INSERT
//! OR REPLACE): re-submitting an existing id fully replaces its record. This
//! backend uses SurrealDB's **native bulk insert** with a full-field update
//! clause: `INSERT INTO node $batch ON DUPLICATE KEY UPDATE <every content
//! field> = $input.<field>`. Each `$batch` element carries its record id as
//! an `id: RecordId` value ([`node_item`]), so a fresh id takes the plain
//! bulk-create path and an existing id is retried as an update (verified in
//! `surrealdb-core` 3.2.1 `doc/insert.rs`: a `RecordExists` conflict falls
//! through to `insert_update` when an `ON DUPLICATE KEY UPDATE` clause is
//! present). Because the update clause assigns **every** content field from
//! `$input` — and an omitted optional field reads back from `$input` as
//! `NONE`, which clears the stored column — the update is a wholesale
//! replace, not a merge, preserving the previous `UPSERT ... CONTENT`
//! semantics exactly (the Some→None clearing direction is pinned by
//! `insert_nodes_upsert_clears_omitted_optionals`, the None→Some update
//! direction by `insert_nodes_upsert_replaces_same_id`, both in
//! `tests/store_test.rs`).
//!
//! This replaced a server-side `FOR $item IN $batch { UPSERT ... }` loop in
//! the Task 9b perf pass: the loop paid per-item statement overhead (~2.3k
//! nodes/s in the pre-rewrite probe), which native bulk `INSERT` removes.
//! Re-measured post-rewrite on the 20k-node corpus (release, kv-mem): the
//! bulk path loads ~4.9k nodes/s with the schema's four FULLTEXT indexes
//! removed and ~0.8k nodes/s with them in place — FTS incremental indexing,
//! not statement shape, now dominates node write cost — see
//! `docs/benchmarks/2026-07-phase1-db-gate.md` for the probe table.
//!
//! ## Chunking
//!
//! `insert_nodes` batches input at [`CHUNK`] nodes per round trip (mirrors
//! the TS store's `SQLITE_PARAM_CHUNK_SIZE`, kept here to bound statement
//! size / bind-variable count rather than for a SQLite-specific limit). Each
//! chunk is ONE bulk `INSERT` statement bound to a single `$batch` array
//! parameter, not one round trip (or one statement) per node.

use std::collections::HashMap;

use selene_core::{Node, NodeKind};
use surrealdb::types::{RecordId, SurrealValue, Value as SqlValue};

use crate::util::{CHUNK, clamp_i64};
use crate::{Error, Result, SurrealStore};

/// Column list shared by every node read query, in `Node`'s declared field
/// order, with the record id resolved to its raw stored key via
/// `record::id(id) AS id` (see the module docs' "Record id ↔ `Node.id`
/// mapping" section for why this replaces a naive `id` projection).
///
/// `pub(crate)`: `src/search.rs` (Task 7) reuses this verbatim for its
/// candidate-fetch projections instead of duplicating the 20-field list,
/// which would otherwise be a drift risk against `Node`'s wire shape.
pub(crate) const NODE_FIELDS: &str = "\
kind, name, qualifiedName, filePath, language, startLine, endLine, startColumn, endColumn, \
docstring, signature, visibility, isExported, isAsync, isStatic, isAbstract, decorators, \
typeParameters, returnType, routeMethod, routePath, framework, updatedAt, \
record::id(id) AS id";

/// Every *stored content* column of the `node` table (i.e. [`NODE_FIELDS`]
/// minus the `record::id(id)` projection), in the same order. Drives the
/// `ON DUPLICATE KEY UPDATE <field> = $input.<field>` clause of
/// [`SurrealStore::insert_nodes`]'s bulk insert: assigning **all** of them
/// from `$input` is what turns the duplicate-key update into a wholesale
/// content replace (an omitted optional field is `NONE` in `$input`, so the
/// stored column is cleared). Must stay in sync with `Node`'s serde shape
/// and the schema (`src/schema.rs`).
const NODE_CONTENT_FIELDS: [&str; 23] = [
    "kind",
    "name",
    "qualifiedName",
    "filePath",
    "language",
    "startLine",
    "endLine",
    "startColumn",
    "endColumn",
    "docstring",
    "signature",
    "visibility",
    "isExported",
    "isAsync",
    "isStatic",
    "isAbstract",
    "decorators",
    "typeParameters",
    "returnType",
    // Route fields — set only on `NodeKind::Route` nodes emitted by the
    // framework registry (`selene-resolve`); `None`/absent on every other node.
    "routeMethod",
    "routePath",
    "framework",
    "updatedAt",
];

/// Serializes `node` into its stored content object: `Node`'s own camelCase
/// JSON shape minus `id` (the record key, not a stored field — see the
/// module docs). `#[serde(skip_serializing_if = "Option::is_none")]` on
/// `Node`'s optional fields means an absent `Option` is omitted entirely
/// here, never sent as JSON `null` — SCHEMAFULL `option<T>` columns accept
/// only absent/`NONE`, not `NULL` (verified in this task's exploration).
///
/// `decorators`/`typeParameters` need special-casing: they are plain
/// `array<string> DEFAULT []` schema columns (not `option<...>`), and `Node`
/// also skips serializing them when empty
/// (`#[serde(skip_serializing_if = "Vec::is_empty")]`). `DEFAULT` only fills
/// an omitted field on record *creation* — a `CONTENT`-replace of an
/// *existing* record (this store's upsert path, see the module docs) leaves
/// an omitted field genuinely absent, which fails the non-optional `array`
/// coercion (found via this task's TDD: `insert_nodes_upsert_replaces_same_id`
/// red on a two-round upsert before this fix). Re-adding them as `[]` when
/// missing keeps every write self-sufficient, independent of `DEFAULT`.
fn node_content(node: &Node) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(node)?;
    if let serde_json::Value::Object(map) = &mut value {
        map.remove("id");
        for array_field in ["decorators", "typeParameters"] {
            map.entry(array_field)
                .or_insert_with(|| serde_json::Value::Array(vec![]));
        }
    }
    Ok(value)
}

/// One element of the `insert_nodes` bulk-`INSERT` batch bound to `$batch`:
/// the [`node_content`] object plus the record id as a real
/// [`RecordId`] value under `id`. The id must be a `RecordId` (not a plain
/// string): it is bound via `surrealdb::types::Value`, which serde-JSON
/// content cannot carry, hence the `serde_json::Value` →
/// [`SqlValue`] bridge through [`SurrealValue::into_value`].
fn node_item(node: &Node) -> Result<SqlValue> {
    let SqlValue::Object(mut obj) = node_content(node)?.into_value() else {
        return Err(Error::Decode(format!(
            "node '{}' did not serialize to an object",
            node.id
        )));
    };
    obj.insert(
        "id".to_string(),
        SqlValue::RecordId(RecordId::new("node", node.id.as_str())),
    );
    Ok(SqlValue::Object(obj))
}

/// Reconstructs a `Node` from a row shaped by [`NODE_FIELDS`]: every `Node`
/// field verbatim, with `id` already the raw stored key (not the SurrealDB
/// display form) thanks to the `record::id(id) AS id` projection.
fn row_to_node(row: serde_json::Value) -> Result<Node> {
    serde_json::from_value(row).map_err(Error::from)
}

impl SurrealStore {
    /// Insert or replace `nodes` (same id ⇒ replace in place). See the
    /// module docs for the chunking and upsert-semantics rationale.
    ///
    /// Failure semantics: a malformed node fails its whole `CHUNK`-sized
    /// batch atomically, but earlier chunks of the same call are already
    /// committed — there is no cross-chunk rollback.
    pub async fn insert_nodes(&self, nodes: &[Node]) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        let update_clause = NODE_CONTENT_FIELDS
            .iter()
            .map(|f| format!("{f} = $input.{f}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql =
            format!("INSERT INTO node $batch ON DUPLICATE KEY UPDATE {update_clause} RETURN NONE");
        for chunk in nodes.chunks(CHUNK) {
            let batch = chunk.iter().map(node_item).collect::<Result<Vec<_>>>()?;
            self.db()
                .query(sql.as_str())
                .bind(("batch", batch))
                .await?
                .check()?;
        }
        Ok(())
    }

    /// Point lookup by id. `None` if `id` is unknown — not an error.
    pub async fn get_node(&self, id: &str) -> Result<Option<Node>> {
        let sql = format!("SELECT {NODE_FIELDS} FROM $rid");
        let rid = RecordId::new("node", id);
        let mut resp = self.db().query(sql).bind(("rid", rid)).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter().next().map(row_to_node).transpose()
    }

    /// Batch lookup by id. The returned map contains only the ids that were
    /// found; unknown ids are simply absent, never an error.
    ///
    /// The query selects `FROM $ids` (a bound array of record ids — direct
    /// point lookups; a missing record simply yields no row), **not**
    /// `FROM node WHERE id IN $ids`: the `IN`-list form is a full table scan
    /// with a per-row linear membership test, measured at ~43 ms for 500 ids
    /// on a 20k-node graph vs ~3 ms for the `FROM $ids` form (~5 ms for the
    /// whole method; Task 9b probe, release, kv-mem). Input ids are deduped
    /// so a repeated id cannot fetch (or emit) twice.
    pub async fn get_nodes(&self, ids: &[String]) -> Result<HashMap<String, Node>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut rids: Vec<RecordId> = ids
            .iter()
            .map(|id| RecordId::new("node", id.as_str()))
            .collect();
        rids.sort_unstable();
        rids.dedup();
        let sql = format!("SELECT {NODE_FIELDS} FROM $ids");
        let mut resp = self.db().query(sql).bind(("ids", rids)).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            let node = row_to_node(row)?;
            out.insert(node.id.clone(), node);
        }
        Ok(out)
    }

    /// Every node whose `file_path` equals `path`.
    /// **Every node, in one scan.** The resolver's context looks nodes up lazily — by name, by id,
    /// by qualified name — and every miss is a BLOCKING round trip on one thread. Measured on
    /// django: **32 524 blocking reads, 4.8 s**, to interrogate a table of 19 061 rows that fits in
    /// ~8 MB of RAM. `get_node` alone (a point lookup by primary key) fired **14 674** times.
    ///
    /// This is the query that replaces all of them.
    pub async fn all_nodes(&self) -> Result<Vec<Node>> {
        let sql = format!("SELECT {NODE_FIELDS} FROM node");
        let mut resp = self.db().query(sql).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter()
            .map(|row| serde_json::from_value(row).map_err(crate::Error::from))
            .collect()
    }

    pub async fn get_nodes_by_file(&self, path: &str) -> Result<Vec<Node>> {
        self.select_nodes_where("filePath = $v", "v", path.to_string())
            .await
    }

    /// Every node of exactly `kind`.
    pub async fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        self.select_nodes_where("kind = $v", "v", kind.as_str().to_string())
            .await
    }

    /// Every node whose `name` matches exactly (case-sensitive).
    pub async fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        self.select_nodes_where("name = $v", "v", name.to_string())
            .await
    }

    /// Every node whose lower-cased `name` equals `lower`. `lower` is
    /// expected pre-lowercased by the caller — matches the stored
    /// `nameLower` computed field this queries against.
    pub async fn get_nodes_by_name_ci(&self, lower: &str) -> Result<Vec<Node>> {
        self.select_nodes_where("nameLower = $v", "v", lower.to_string())
            .await
    }

    /// Every node whose `qualified_name` matches exactly (more than one is
    /// possible, e.g. overloads sharing a qualified name).
    pub async fn get_nodes_by_qualified_name(&self, qn: &str) -> Result<Vec<Node>> {
        self.select_nodes_where("qualifiedName = $v", "v", qn.to_string())
            .await
    }

    /// Every node whose `name` starts with `prefix`, capped at `limit`. Uses
    /// SurrealDB's native `string::starts_with`, not a `\u{FFFF}` successor
    /// hack.
    pub async fn get_nodes_by_name_prefix(&self, prefix: &str, limit: usize) -> Result<Vec<Node>> {
        let sql = format!(
            "SELECT {NODE_FIELDS} FROM node WHERE string::starts_with(name, $prefix) LIMIT $limit"
        );
        let mut resp = self
            .db()
            .query(sql)
            .bind(("prefix", prefix.to_string()))
            .bind(("limit", clamp_i64(limit)))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter().map(row_to_node).collect()
    }

    /// Count of nodes named exactly `name`, across every file. Used upstream
    /// to decide whether a name is "distinctive" enough to boost in search.
    ///
    /// Counts **distinct files**, not nodes: two same-named nodes in one file
    /// contribute one to the count, matching the "distinctive name < N files"
    /// upstream scoring contract.
    pub async fn count_nodes_matching_name_in_files(&self, name: &str) -> Result<u64> {
        let mut resp = self
            .db()
            .query("SELECT filePath FROM node WHERE name = $name GROUP BY filePath")
            .bind(("name", name.to_string()))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        Ok(rows.len() as u64)
    }

    /// Route nodes matching the given semantics, via the `node_route` index.
    ///
    /// **This is the ONE way to look a route up.** A route's id is the ordinary
    /// hashed node id (see `selene_core::Node`'s route-field docs) — it does NOT
    /// encode the method or path, so downstream code must never parse or
    /// string-build one. `framework`/`method` are optional filters; `path` is
    /// required (it is the discriminator a caller always knows).
    ///
    /// Results are ordered by `(filePath, startLine, name)` so a caller that
    /// gets several routes back gets them deterministically.
    pub async fn find_route(
        &self,
        framework: Option<&str>,
        method: Option<&str>,
        path: &str,
    ) -> Result<Vec<Node>> {
        let mut sql = format!(
            "SELECT {NODE_FIELDS} FROM node \
             WHERE kind = $kind AND routePath = $path"
        );
        if framework.is_some() {
            sql.push_str(" AND framework = $framework");
        }
        if method.is_some() {
            sql.push_str(" AND routeMethod = $method");
        }
        sql.push_str(" ORDER BY filePath, startLine, name");

        let mut query = self
            .db()
            .query(sql)
            .bind(("kind", NodeKind::Route.as_str()))
            .bind(("path", path.to_string()));
        if let Some(f) = framework {
            query = query.bind(("framework", f.to_string()));
        }
        if let Some(m) = method {
            query = query.bind(("method", m.to_string()));
        }
        let mut resp = query.await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter().map(row_to_node).collect()
    }

    /// Count of **nodes** named exactly `name` — the counterpart to
    /// [`Self::count_nodes_matching_name_in_files`]'s *file* count (three
    /// `helper`s in two files: this answers 3, that answers 2).
    ///
    /// `count()` + `GROUP ALL` aggregates in the database over the existing
    /// `node_name` index, so a ubiquitous name is counted **without
    /// materializing its rows** — which is the entire reason an ambiguity
    /// ceiling (`#999`) uses a counter instead of `get_nodes_by_name(..).len()`.
    /// One page of nodes of `kind`, in **id order**, after `after`.
    ///
    /// The comparison is on the record id, which for this table IS the node id
    /// (`record::id(id)`), so `id > $after` pages stably: no row is dropped at a
    /// boundary and none is returned twice. See the trait docs for why the
    /// synthesizers must page rather than materialize (#610).
    pub async fn nodes_by_kind_page(
        &self,
        kind: NodeKind,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Node>> {
        // Compare and order on `record::id(id)` — the RAW stored key, which for
        // this table IS `Node.id`. Ordering on the RecordId itself would sort on
        // the driver's *display* form (backtick-quoted when the key contains a
        // `:`, which every node id does), so the paging key would not be the id
        // the caller passes back in as `after` — the loop would never advance.
        let sql = match after {
            Some(_) => format!(
                "SELECT {NODE_FIELDS} FROM node \
                 WHERE kind = $kind AND record::id(id) > $after \
                 ORDER BY id LIMIT $limit"
            ),
            None => format!(
                "SELECT {NODE_FIELDS} FROM node WHERE kind = $kind ORDER BY id LIMIT $limit"
            ),
        };
        let mut query = self
            .db()
            .query(sql)
            .bind(("kind", kind.as_str()))
            .bind(("limit", clamp_i64(limit)));
        if let Some(a) = after {
            query = query.bind(("after", a.to_string()));
        }
        let mut resp = query.await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter().map(row_to_node).collect()
    }

    pub async fn count_nodes_named(&self, name: &str) -> Result<u64> {
        let mut resp = self
            .db()
            .query("SELECT count() FROM node WHERE name = $name GROUP ALL")
            .bind(("name", name.to_string()))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        // No rows at all == no node carries that name.
        Ok(rows.first().and_then(|r| r["count"].as_u64()).unwrap_or(0))
    }

    /// Shared helper for the single-predicate `WHERE field = $v` lookups
    /// above.
    async fn select_nodes_where(
        &self,
        predicate: &str,
        var: &str,
        value: String,
    ) -> Result<Vec<Node>> {
        let sql = format!("SELECT {NODE_FIELDS} FROM node WHERE {predicate}");
        let mut resp = self.db().query(sql).bind((var, value)).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter().map(row_to_node).collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeSet;

    use selene_core::Visibility;

    use super::*;

    /// [`NODE_FIELDS`] and [`NODE_CONTENT_FIELDS`] must stay in lockstep:
    /// the read projection is exactly the stored content columns plus the
    /// `record::id(id) AS id` bridge, in the same order. A field added to
    /// one list but not the other (silent read/write drift against `Node`'s
    /// wire shape) fails here loudly.
    #[test]
    fn node_fields_is_content_fields_plus_id_projection() {
        assert_eq!(
            NODE_FIELDS,
            format!("{}, record::id(id) AS id", NODE_CONTENT_FIELDS.join(", "))
        );
    }

    /// The field lists must also stay in lockstep with `Node`'s serde shape
    /// itself: a fully-populated `Node` (every optional `Some`, every `Vec`
    /// non-empty, so no `skip_serializing_if` fires) must serialize to
    /// exactly `NODE_CONTENT_FIELDS ∪ {id}` — this catches a new `Node`
    /// field missing from BOTH lists, which the projection-vs-content check
    /// above cannot see.
    #[test]
    fn node_serde_key_set_matches_the_field_lists() {
        let full = Node {
            id: "method:full".to_string(),
            kind: NodeKind::Method,
            name: "full".to_string(),
            qualified_name: "src/full.rs::Widget.full".to_string(),
            file_path: "src/full.rs".to_string(),
            language: "rust".to_string(),
            start_line: 10,
            end_line: 20,
            start_column: 2,
            end_column: 3,
            docstring: Some("does a thing".to_string()),
            signature: Some("fn full(&self) -> bool".to_string()),
            visibility: Some(Visibility::Public),
            is_exported: Some(true),
            is_async: Some(true),
            is_static: Some(false),
            is_abstract: Some(false),
            decorators: vec!["#[inline]".to_string()],
            type_parameters: vec!["T".to_string()],
            return_type: Some("bool".to_string()),
            // Route fields Some(..) so no `skip_serializing_if` fires — this
            // test's contract is "every field of Node appears in the DB field
            // lists", which only holds on a node where nothing is skipped.
            route_method: Some("GET".to_string()),
            route_path: Some("/full".to_string()),
            framework: Some("express".to_string()),
            updated_at: 42,
        };

        let value = serde_json::to_value(&full).unwrap();
        let serde_keys: BTreeSet<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<&str> = NODE_CONTENT_FIELDS
            .iter()
            .copied()
            .chain(std::iter::once("id"))
            .collect();
        assert_eq!(
            serde_keys, expected,
            "Node's serde key set must equal NODE_CONTENT_FIELDS ∪ {{id}} — \
             a mismatch means a Node field is missing from the read/write \
             field lists (or a list names a field Node no longer has)"
        );
    }
}
