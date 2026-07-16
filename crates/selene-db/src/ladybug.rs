//! `LadybugStore` — the LadybugDB (`lbug`) backend, the migration target off SurrealDB.
//!
//! **Status: FOUNDATION.** This implements the perf-critical write path — schema, the bulk
//! `COPY` insert for nodes and edges, and counts — plus the async-over-sync plumbing every method
//! needs. It is NOT yet the full [`crate::GraphStore`] trait; the reads, traversal, unresolved
//! queue, and search follow (see `docs/plans/2026-07-16-ladybug-migration.md`). Off by default
//! behind `kv-ladybug` because `lbug` builds a C++ core.
//!
//! # Why `COPY`, never `CREATE`
//!
//! Measured in the spike: 5 000 per-row `CREATE` = **54 s**; the same via bulk `COPY FROM` a CSV =
//! **128 ms for 50 000**. Per-row insert is dead on arrival; every write here batches through `COPY`.
//!
//! # Async over a sync engine
//!
//! `lbug` is synchronous and its `Connection<'a>` borrows the `Database`. We hold an
//! `Arc<Database>` (which is `Send + Sync`) and, inside `spawn_blocking`, make a short-lived
//! `Connection` per call — so the blocking C++ work never sits on a tokio worker.

use crate::{Error, Result};
use selene_core::{Edge, Node};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The LadybugDB-backed graph store.
#[derive(Clone)]
pub struct LadybugStore {
    db: Arc<lbug::Database>,
}

fn lbug_err(e: impl std::fmt::Display) -> Error {
    Error::Ladybug(e.to_string())
}

/// Wire string for a node's visibility (`Visibility` has no `as_str`); empty for `None`.
fn visibility_str(v: Option<selene_core::Visibility>) -> &'static str {
    use selene_core::Visibility::*;
    match v {
        Some(Public) => "public",
        Some(Private) => "private",
        Some(Protected) => "protected",
        Some(Internal) => "internal",
        None => "",
    }
}

/// Quote a CSV field RFC4180-style (wrap in `"`, double embedded `"`). Applied to every STRING
/// column so names/paths/signatures carrying commas, quotes, or newlines round-trip through `COPY`.
fn csv_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// A unique scratch path for a `COPY` CSV. Process-local monotonic counter — no `Date`/random
/// needed, and collisions across processes are avoided by the pid segment.
fn scratch_csv(tag: &str) -> std::path::PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("selene_lbug_{}_{}_{tag}_{n}.csv", std::process::id(), tag))
}

impl LadybugStore {
    /// Open (creating if absent) an on-disk LadybugDB store at `dir`, and apply the schema.
    pub async fn open(dir: &Path) -> Result<Self> {
        let dir = dir.to_path_buf();
        let db = tokio::task::spawn_blocking(move || {
            lbug::Database::new(&dir, lbug::SystemConfig::default()).map_err(lbug_err)
        })
        .await
        .map_err(lbug_err)??;
        let store = Self { db: Arc::new(db) };
        store.apply_schema().await?;
        Ok(store)
    }

    /// Run one write statement (no result rows needed).
    async fn exec(&self, sql: String) -> Result<()> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db).map_err(lbug_err)?;
            conn.query(&sql).map_err(lbug_err)?;
            Ok::<(), Error>(())
        })
        .await
        .map_err(lbug_err)?
    }

    /// Run a query expected to return a single `INT64` in row 0, column 0.
    async fn scalar_i64(&self, sql: String) -> Result<i64> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db).map_err(lbug_err)?;
            let mut result = conn.query(&sql).map_err(lbug_err)?;
            match result.next() {
                Some(row) => match row.into_iter().next() {
                    Some(lbug::Value::Int64(n)) => Ok(n),
                    other => Err(Error::Decode(format!("expected INT64 scalar, got {other:?}"))),
                },
                None => Ok(0),
            }
        })
        .await
        .map_err(lbug_err)?
    }

    /// Apply the v1 schema idempotently: one `Node` table, one `Edge` relation carrying its `kind`,
    /// plus `File` and `Meta`. `IF NOT EXISTS` so re-opening an existing store is a no-op.
    ///
    /// Node columns are declared in the exact order `insert_nodes` writes its CSV — `COPY` with
    /// `HEADER=false` maps by position.
    async fn apply_schema(&self) -> Result<()> {
        // FOUNDATION schema: scalar node fields + the four common optionals. LIST fields
        // (decorators, type_parameters) and the route fields are deferred to the full-fidelity pass.
        self.exec(
            "CREATE NODE TABLE IF NOT EXISTS Node(\
                 id STRING, name STRING, kind STRING, qualified_name STRING, \
                 file STRING, language STRING, \
                 start_line INT64, end_line INT64, start_col INT64, end_col INT64, \
                 docstring STRING, signature STRING, return_type STRING, visibility STRING, \
                 PRIMARY KEY(id));"
                .into(),
        )
        .await?;
        self.exec("CREATE REL TABLE IF NOT EXISTS Edge(FROM Node TO Node, kind STRING);".into())
            .await?;
        self.exec(
            "CREATE NODE TABLE IF NOT EXISTS File(\
                 path STRING, language STRING, hash STRING, indexed_at INT64, PRIMARY KEY(path));"
                .into(),
        )
        .await?;
        self.exec(
            "CREATE NODE TABLE IF NOT EXISTS Meta(key STRING, value STRING, PRIMARY KEY(key));"
                .into(),
        )
        .await
    }

    /// Bulk-insert `nodes` via `COPY` from a scratch CSV. Column order matches `apply_schema`.
    pub async fn insert_nodes(&self, nodes: &[Node]) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        let mut csv = String::new();
        for n in nodes {
            csv.push_str(&csv_quote(&n.id));
            csv.push(',');
            csv.push_str(&csv_quote(&n.name));
            csv.push(',');
            csv.push_str(&csv_quote(n.kind.as_str()));
            csv.push(',');
            csv.push_str(&csv_quote(&n.qualified_name));
            csv.push(',');
            csv.push_str(&csv_quote(&n.file_path));
            csv.push(',');
            csv.push_str(&csv_quote(&n.language));
            csv.push(',');
            csv.push_str(&n.start_line.to_string());
            csv.push(',');
            csv.push_str(&n.end_line.to_string());
            csv.push(',');
            csv.push_str(&n.start_column.to_string());
            csv.push(',');
            csv.push_str(&n.end_column.to_string());
            csv.push(',');
            csv.push_str(&csv_quote(n.docstring.as_deref().unwrap_or("")));
            csv.push(',');
            csv.push_str(&csv_quote(n.signature.as_deref().unwrap_or("")));
            csv.push(',');
            csv.push_str(&csv_quote(n.return_type.as_deref().unwrap_or("")));
            csv.push(',');
            csv.push_str(&csv_quote(visibility_str(n.visibility)));
            csv.push('\n');
        }
        self.copy_into("Node", csv).await
    }

    /// Bulk-insert `edges` via `COPY` into the `Edge` relation. CSV columns: FROM-id, TO-id, kind.
    /// Returns the number of edge rows written. NOTE: `COPY` requires both endpoints to already
    /// exist (referential integrity) — the caller inserts nodes first; filtering dangling edges is
    /// deferred to the full pass.
    pub async fn insert_edges(&self, edges: &[Edge]) -> Result<u64> {
        if edges.is_empty() {
            return Ok(0);
        }
        let mut csv = String::new();
        for e in edges {
            csv.push_str(&csv_quote(&e.source));
            csv.push(',');
            csv.push_str(&csv_quote(&e.target));
            csv.push(',');
            csv.push_str(&csv_quote(e.kind.as_str()));
            csv.push('\n');
        }
        let n = edges.len() as u64;
        self.copy_into("Edge", csv).await?;
        Ok(n)
    }

    /// Write `csv` to a scratch file and `COPY` it into `table`, removing the file after.
    async fn copy_into(&self, table: &'static str, csv: String) -> Result<()> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let path = scratch_csv(table);
            std::fs::write(&path, csv.as_bytes()).map_err(lbug_err)?;
            let conn = lbug::Connection::new(&db).map_err(lbug_err)?;
            let sql = format!("COPY {table} FROM '{}' (HEADER=false);", path.display());
            let res = conn.query(&sql).map_err(lbug_err);
            let _ = std::fs::remove_file(&path);
            res.map(|_| ())
        })
        .await
        .map_err(lbug_err)?
    }

    /// `(nodes, edges)` counts.
    pub async fn node_edge_count(&self) -> Result<(u64, u64)> {
        let nodes = self.scalar_i64("MATCH (n:Node) RETURN count(n);".into()).await?;
        let edges = self.scalar_i64("MATCH ()-[e:Edge]->() RETURN count(e);".into()).await?;
        Ok((nodes.max(0) as u64, edges.max(0) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selene_core::{Edge, EdgeKind, Node, NodeKind};

    fn node(id: &str, name: &str, file: &str) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: name.into(),
            qualified_name: name.into(),
            file_path: file.into(),
            language: "rust".into(),
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: Some(format!("fn {name}()")),
            visibility: None,
            is_exported: None,
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: vec![],
            type_parameters: vec![],
            return_type: None,
            route_method: None,
            route_path: None,
            framework: None,
            updated_at: 0,
        }
    }

    #[tokio::test]
    async fn bulk_copy_inserts_nodes_and_edges_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        // lbug creates the store directory itself; point it at a non-existent subpath.
        let store = LadybugStore::open(&dir.path().join("db")).await.unwrap();

        let nodes: Vec<Node> = (0..1000)
            .map(|i| node(&format!("n{i}"), &format!("fn{i}"), &format!("f{}.rs", i % 20)))
            .collect();
        store.insert_nodes(&nodes).await.unwrap();

        // A call chain n_i -> n_{i+1}.
        let edges: Vec<Edge> = (0..999)
            .map(|i| Edge {
                source: format!("n{i}"),
                target: format!("n{}", i + 1),
                kind: EdgeKind::Calls,
                metadata: None,
                line: None,
                column: None,
                provenance: None,
            })
            .collect();
        let written = store.insert_edges(&edges).await.unwrap();
        assert_eq!(written, 999);

        let (n, e) = store.node_edge_count().await.unwrap();
        assert_eq!(n, 1000, "node count");
        assert_eq!(e, 999, "edge count");
    }
}
