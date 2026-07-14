//! Project metadata KV + aggregate stats + full clear: inherent methods on
//! [`SurrealStore`] carrying the metadata/stats section of
//! [`crate::GraphStore`] (Task 7); the trait impl in `src/store_impl.rs`
//! delegates here (Task 10).
//!
//! ## Placement: a dedicated module, not `src/surreal.rs`
//!
//! `src/surreal.rs` already owns `schema_version()` (a read over the same
//! `meta` table `apply_schema` seeds) — get/set/stats/clear could have lived
//! there. They get their own module instead, for the same reason
//! `src/nodes.rs`/`src/edges.rs`/`src/files.rs`/`src/unresolved.rs` are each
//! their own module: `src/surreal.rs` is open/init/schema-apply, not a
//! dumping ground for every `GraphStore` section. Reported per the task
//! brief's "your call, report it".
//!
//! ## `get_meta`/`set_meta` reuse the `meta` table `schema_version` lives in
//!
//! `src/schema.rs`'s `META_DDL` defines one `meta` table, `value: string`,
//! with the record **key** doubling as the KV key (`meta:schema_version` is
//! exactly `set_meta("schema_version", "1")` would produce). `set_meta`
//! therefore never touches any key but the one given — `schema_version` (or
//! any other previously-set key) survives every `set_meta` call to a
//! *different* key. Like `src/nodes.rs`'s `get_node`, `get_meta` uses
//! `RecordId::new` (not a hand-built literal record id like
//! `SurrealStore::schema_version`'s `meta:schema_version`) so an arbitrary
//! key string — including one containing `:` — round-trips without
//! reimplementing SurrealDB's record-id escaping.
//!
//! ## `stats().languages` is a **file** count per language, not a node count
//!
//! [`crate::GraphStats::languages`] is documented on the trait as "File count
//! per language" and CodeGraph TS's `getStats()` confirms it:
//! `filesByLanguage` is `SELECT language, COUNT(*) FROM files GROUP BY
//! language` — grouped over `files`, never `nodes`. This port mirrors that
//! exactly: `languages` here groups the `file` table's `language` column, not
//! `node.language`.
//!
//! ## `clear()` leaves every `meta` key untouched (not just `schema_version`)
//!
//! CodeGraph TS's `QueryBuilder.clear()` deletes `unresolved_refs`, `edges`,
//! `nodes`, `files` in one transaction — it never issues a `DELETE` against
//! `project_metadata` at all. This port matches that exactly: `clear()` drops
//! `node` (edges cascade away with their endpoints — proven in Task 6's
//! `delete_file_cascades_nodes_edges_and_unresolved`), `file`, and
//! `unresolved_ref`; the `meta` table (`schema_version` *and* any other key a
//! caller has `set_meta`'d) is left completely alone, matching
//! [`crate::GraphStore::clear`]'s trait doc ("Project metadata … is
//! untouched").

use std::collections::BTreeMap;

use selene_core::EdgeKind;
use surrealdb::types::RecordId;

use crate::{GraphStats, Result, SurrealStore};

impl SurrealStore {
    /// Read an opaque project metadata value by key. `None` if unset — not an
    /// error.
    pub async fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let rid = RecordId::new("meta", key);
        // `SELECT value FROM $rid` does not parse: a bare `value` right after
        // `SELECT` is grammatically ambiguous with the `SELECT VALUE <field>`
        // flattening form, and the parser rejects it ("expected FROM") before
        // ever seeing `value` as an ordinary field name. `SELECT VALUE value`
        // sidesteps the ambiguity *and* flattens the projection directly to
        // `Vec<String>`, one fewer decode step than the object-row form every
        // other read in this crate uses.
        let mut resp = self
            .db()
            .query("SELECT VALUE value FROM $rid")
            .bind(("rid", rid))
            .await?;
        let values: Vec<String> = resp.take(0)?;
        Ok(values.into_iter().next())
    }

    /// Write an opaque project metadata value. Touches only `key` — see the
    /// module docs for why `schema_version` (or any other key) is never
    /// affected by a `set_meta` call to a different key.
    pub async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.db()
            .query("UPSERT type::record('meta', $key) CONTENT { value: $value }")
            .bind(("key", key.to_string()))
            .bind(("value", value.to_string()))
            .await?
            .check()?;
        Ok(())
    }

    /// The busiest file and its runner-up, by edges leaving symbols defined in
    /// them — `(file_path, edge_count, next_edge_count)`, `None` on an empty
    /// graph. See [`GraphStore::dominant_file`](crate::GraphStore::dominant_file).
    ///
    /// The tie-break on equal counts is the **path**, so the "dominant" file
    /// cannot flip between runs on a corpus with two equally-busy files — this
    /// value feeds a scoring boost, and a boost that moves under a re-index is a
    /// ranking that moves under a re-index.
    pub async fn dominant_file(&self) -> Result<Option<(String, u64, u64)>> {
        let mut resp = self.db().query(edge_group_by_file_sql()).await?;

        let mut totals: BTreeMap<String, u64> = BTreeMap::new();
        for i in 0..EdgeKind::ALL.len() {
            let rows: Vec<serde_json::Value> = resp.take(i)?;
            for row in rows {
                if let (Some(f), Some(c)) = (row["f"].as_str(), row["count"].as_u64()) {
                    *totals.entry(f.to_string()).or_insert(0) += c;
                }
            }
        }

        let mut ranked: Vec<(String, u64)> = totals.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        Ok(match (ranked.first(), ranked.get(1)) {
            (Some((path, count)), next) => {
                Some((path.clone(), *count, next.map_or(0, |(_, n)| *n)))
            }
            (None, _) => None,
        })
    }

    /// Aggregate graph statistics (see [`GraphStats`]). Every count is a
    /// store-side `count()`/`GROUP BY` aggregate, never a full scan decoded
    /// in Rust.
    pub async fn stats(&self) -> Result<GraphStats> {
        let (nodes, edges) = self.node_edge_count().await?;
        let files = self.count_all("file").await?;

        let mut resp = self
            .db()
            .query("SELECT kind, count() FROM node GROUP BY kind")
            .await?;
        let nodes_by_kind = grouped_counts(resp.take(0)?, "kind")?;

        let mut edges_by_kind = BTreeMap::new();
        let mut resp = self.db().query(edge_group_all_sql()).await?;
        for (i, kind) in EdgeKind::ALL.iter().enumerate() {
            let rows: Vec<serde_json::Value> = resp.take(i)?;
            let count = row_count(&rows);
            if count > 0 {
                edges_by_kind.insert(kind.as_str().to_string(), count);
            }
        }

        // File count per language (NOT node.language — see the module docs).
        let mut resp = self
            .db()
            .query("SELECT language, count() FROM file GROUP BY language")
            .await?;
        let languages = grouped_counts(resp.take(0)?, "language")?;

        Ok(GraphStats {
            nodes,
            edges,
            files,
            nodes_by_kind,
            edges_by_kind,
            languages,
        })
    }

    /// `(node_count, edge_count)` — cheaper than [`Self::stats`] when only the
    /// totals are needed: one `GROUP ALL` on `node` plus one per edge table,
    /// all edge-table counts folded into a single multi-statement round trip.
    pub async fn node_edge_count(&self) -> Result<(u64, u64)> {
        let nodes = self.count_all("node").await?;

        let mut resp = self.db().query(edge_group_all_sql()).await?;
        let mut edges: u64 = 0;
        for i in 0..EdgeKind::ALL.len() {
            let rows: Vec<serde_json::Value> = resp.take(i)?;
            edges += row_count(&rows);
        }
        Ok((nodes, edges))
    }

    /// Drop every node, edge, file, and unresolved-ref row (full re-index
    /// discard). `node` deletes cascade their edges (Task 6's proven
    /// relation-delete behavior), so the 12 edge tables are never enumerated
    /// here. Every `meta` key — `schema_version` included — is untouched; see
    /// the module docs for the TS-parity rationale.
    pub async fn clear(&self) -> Result<()> {
        self.db()
            .query("DELETE unresolved_ref; DELETE node; DELETE file;")
            .await?
            .check()?;
        Ok(())
    }

    /// `SELECT count() FROM {table} GROUP ALL`, folded to `0` on an empty
    /// table (no `GROUP ALL` row at all, rather than a `count: 0` row).
    async fn count_all(&self, table: &str) -> Result<u64> {
        let sql = format!("SELECT count() FROM {table} GROUP ALL");
        let mut resp = self.db().query(sql).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        Ok(row_count(&rows))
    }
}

/// One `SELECT count() FROM {table} GROUP ALL;` statement per [`EdgeKind`],
/// in [`EdgeKind::ALL`] order, combined into a single multi-statement query —
/// mirrors `src/edges.rs`'s `insert_edge_chunk` per-kind statements/
/// `src/unresolved.rs`'s keyed batching: one round trip regardless of the
/// 12-table split.
/// One `SELECT in.filePath AS f, count() FROM {kind} GROUP BY f;` per edge-kind
/// relation table. Edges are stored one `TYPE RELATION` table per [`EdgeKind`],
/// so "how many edges leave this file" is a group-by per table, summed in Rust.
fn edge_group_by_file_sql() -> String {
    let mut sql = String::with_capacity(EdgeKind::ALL.len() * 64);
    for kind in EdgeKind::ALL {
        sql.push_str(&format!(
            "SELECT in.filePath AS f, count() FROM {} GROUP BY f;",
            kind.as_str()
        ));
    }
    sql
}

fn edge_group_all_sql() -> String {
    let mut sql = String::with_capacity(EdgeKind::ALL.len() * 40);
    for kind in EdgeKind::ALL {
        sql.push_str(&format!("SELECT count() FROM {} GROUP ALL;", kind.as_str()));
    }
    sql
}

/// The `count` field of a `GROUP ALL` result's single row, or `0` if the
/// aggregate produced no row (empty table).
fn row_count(rows: &[serde_json::Value]) -> u64 {
    rows.first().and_then(|r| r["count"].as_u64()).unwrap_or(0)
}

/// Decodes a `SELECT {key_field}, count() FROM t GROUP BY {key_field}` result
/// into a `key -> count` map. A row missing either field (never expected —
/// `key_field` is a required schema column, `count()` always projects
/// `count`) is skipped rather than failing the whole aggregate.
fn grouped_counts(rows: Vec<serde_json::Value>, key_field: &str) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    for row in rows {
        if let (Some(key), Some(count)) = (row[key_field].as_str(), row["count"].as_u64()) {
            out.insert(key.to_string(), count);
        }
    }
    Ok(out)
}
