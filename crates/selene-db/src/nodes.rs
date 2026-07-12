//! Node CRUD + lookups: inherent methods on [`SurrealStore`] mirroring the
//! node section of [`crate::GraphStore`] (Task 4). `impl GraphStore for
//! SurrealStore` is wired later (Task 10) once every section exists; until
//! then these are plain inherent `async fn`s with identical signatures.
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
//! backend uses SurrealDB's `UPSERT <record> CONTENT <object>` — `UPSERT`
//! creates the record if absent or **replaces its content wholesale** if
//! present (unlike `MERGE`, which only overwrites the fields given). Because
//! `insert_nodes` always sends every `Node` field (optional ones simply
//! omitted, never `null` — SCHEMAFULL `option<T>` columns reject `NULL` and
//! only accept absent/`NONE`; see [`node_content`]), CONTENT-replace and a
//! field-by-field merge would coincide here, but CONTENT is the semantically
//! correct choice for "insert or replace".
//!
//! ## Chunking
//!
//! `insert_nodes` batches input at [`CHUNK`] nodes per round trip (mirrors
//! the TS store's `SQLITE_PARAM_CHUNK_SIZE`, kept here to bound statement
//! size / bind-variable count rather than for a SQLite-specific limit). Each
//! chunk is ONE query: a `FOR $item IN $batch { UPSERT
//! type::record('node', $item.key) CONTENT $item.content; }` loop bound to a
//! single `$batch` array parameter, not one round trip per node.

use std::collections::HashMap;

use selene_core::{Node, NodeKind};
use surrealdb::types::RecordId;

use crate::{Error, Result, SurrealStore};

/// Nodes written per `insert_nodes` round trip. Mirrors the TS store's
/// `SQLITE_PARAM_CHUNK_SIZE`, kept here to bound single-statement/bind-array
/// size rather than for a SQLite-specific limit.
const CHUNK: usize = 500;

/// Column list shared by every node read query, in `Node`'s declared field
/// order, with the record id resolved to its raw stored key via
/// `record::id(id) AS id` (see the module docs' "Record id ↔ `Node.id`
/// mapping" section for why this replaces a naive `id` projection).
const NODE_FIELDS: &str = "\
kind, name, qualifiedName, filePath, language, startLine, endLine, startColumn, endColumn, \
docstring, signature, visibility, isExported, isAsync, isStatic, isAbstract, decorators, \
typeParameters, returnType, updatedAt, record::id(id) AS id";

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

/// One element of the `insert_nodes` chunk batch bound to `$batch`: the raw
/// record key plus the stored content, consumed by the `FOR` loop's
/// `UPSERT type::record('node', $item.key) CONTENT $item.content`.
fn node_to_batch_item(node: &Node) -> Result<serde_json::Value> {
    Ok(serde_json::json!({ "key": node.id, "content": node_content(node)? }))
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
    pub async fn insert_nodes(&self, nodes: &[Node]) -> Result<()> {
        for chunk in nodes.chunks(CHUNK) {
            let batch = chunk
                .iter()
                .map(node_to_batch_item)
                .collect::<Result<Vec<_>>>()?;
            self.db()
                .query(
                    "FOR $item IN $batch {\
                        UPSERT type::record('node', $item.key) CONTENT $item.content;\
                     };",
                )
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
    pub async fn get_nodes(&self, ids: &[String]) -> Result<HashMap<String, Node>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rids: Vec<RecordId> = ids
            .iter()
            .map(|id| RecordId::new("node", id.as_str()))
            .collect();
        let sql = format!("SELECT {NODE_FIELDS} FROM node WHERE id IN $ids");
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
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut resp = self
            .db()
            .query(sql)
            .bind(("prefix", prefix.to_string()))
            .bind(("limit", limit))
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
