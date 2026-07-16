//! `LadybugStore` — the LadybugDB (`lbug`) [`crate::GraphStore`] backend, the migration target off
//! SurrealDB. Off by default behind `kv-ladybug` (its C++ core is a cmake build).
//!
//! # Storage model — indexed columns + a JSON blob
//!
//! Every node/edge/file/unresolved row stores the query-relevant fields as typed, indexed columns
//! **plus** a `data STRING` holding the full `serde_json` of the struct. Reads are therefore a
//! `MATCH … RETURN x.data` followed by one `serde_json::from_str` — perfect round-trip fidelity for
//! all ~24 Node fields (incl. the `Vec<String>` list fields and route fields) with no Kuzu
//! LIST-column `COPY` gymnastics. The indexed columns exist only for `WHERE`/traversal.
//!
//! # Writes go through `COPY`, never `CREATE`
//!
//! Measured: 5k per-row `CREATE` = 54 s; 50k via bulk `COPY FROM` a CSV = 128 ms. Every bulk write
//! batches through `COPY`.
//!
//! # Async over a sync engine
//!
//! `lbug` is synchronous; `Connection<'a>` borrows the `Database` but is `Send + Sync`. We hold an
//! `Arc<Database>` and run each call in `spawn_blocking` with a short-lived `Connection`.

use crate::store::{
    Direction, GraphStats, GraphStore, NeighborEntry, ReplaceStats, SearchCandidate, Subgraph,
    TraversalOptions,
};
use crate::{Error, Result};
use indexmap::IndexMap;
use selene_core::{
    Edge, EdgeKind, FileRecord, Node, NodeKind, Provenance, RefStatus, UnresolvedRef,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

type UnresolvedKey = (String, String, String);

/// Nodes + edges + file records accumulated during bulk-load mode, flushed as one `COPY` each at
/// the extraction→resolve boundary. Kuzu's `COPY` is the fast path only into a fresh table; the
/// per-batch alternative is slow — and `upsert_files` in particular was 931 sequential `MERGE`s.
/// Buffering the whole extraction into one `COPY` per table restores the fast path.
type BulkBuf = (Vec<Node>, Vec<Edge>, Vec<FileRecord>);

/// The LadybugDB-backed graph store.
#[derive(Clone)]
pub struct LadybugStore {
    db: Arc<lbug::Database>,
    /// `Some` between `bulk_load_begin` and `bulk_load_finish` — writes accumulate here instead of
    /// issuing a `COPY` per call. `None` = direct-write mode (single-file sync, and the resolve
    /// phase where per-batch edge visibility is required).
    bulk: Arc<std::sync::Mutex<Option<BulkBuf>>>,
}

fn lbug_err(e: impl std::fmt::Display) -> Error {
    Error::Ladybug(e.to_string())
}

/// A safe single-quoted Cypher string literal (escape `\` and `'`). Names/paths can carry quotes.
fn lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// RFC4180 CSV field (wrap in `"`, double embedded `"`).
fn csv_q(s: &str) -> String {
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

fn scratch_csv(tag: &str) -> std::path::PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("selene_lbug_{}_{tag}_{n}.csv", std::process::id()))
}

fn as_string(v: &lbug::Value) -> String {
    match v {
        lbug::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn as_i64(v: &lbug::Value) -> i64 {
    match v {
        lbug::Value::Int64(n) => *n,
        lbug::Value::Int32(n) => *n as i64,
        lbug::Value::UInt64(n) => *n as i64,
        _ => 0,
    }
}

/// Parse a `data` JSON blob column into `T`.
fn from_data<T: serde::de::DeserializeOwned>(v: &lbug::Value) -> Result<T> {
    serde_json::from_str(&as_string(v)).map_err(Error::from)
}

/// Extract a `LIST` column (e.g. a Cypher list comprehension `[x IN nodes(p) | x.data]`) as strings.
fn as_string_list(v: &lbug::Value) -> Vec<String> {
    match v {
        lbug::Value::List(_, items) | lbug::Value::Array(_, items) => {
            items.iter().map(as_string).collect()
        }
        _ => Vec::new(),
    }
}

impl LadybugStore {
    /// Open (creating if absent) an on-disk LadybugDB store at `dir` (which must not pre-exist as a
    /// plain directory — lbug creates its own store dir), and apply the schema.
    pub async fn open(dir: &Path) -> Result<Self> {
        let dir = dir.to_path_buf();
        let db = tokio::task::spawn_blocking(move || {
            // Two knobs matter for an indexing workload:
            //  - buffer_pool_size: default auto-detects a large fraction of system RAM (like
            //    SurrealDB's RocksDB block cache). A code graph is a few hundred MB, so cap it.
            //  - auto_checkpoint(false): Kuzu checkpoints (WAL -> main store, fsync-heavy) whenever
            //    the WAL crosses a threshold; during a many-COPY bulk load that fsync is the
            //    per-COPY overhead. Disable it and CHECKPOINT once in `bulk_load_finish`.
            let cfg = lbug::SystemConfig::default()
                .buffer_pool_size(512 * 1024 * 1024)
                .auto_checkpoint(false);
            lbug::Database::new(&dir, cfg).map_err(lbug_err)
        })
        .await
        .map_err(lbug_err)??;
        let store = Self { db: Arc::new(db), bulk: Arc::new(std::sync::Mutex::new(None)) };
        store.apply_schema().await?;
        Ok(store)
    }

    // -- primitives ---------------------------------------------------------

    /// Run a write statement; discard rows.
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

    /// Flush the bulk-load write buffer as one node `COPY` + one edge `COPY`, and leave bulk mode.
    ///
    /// Call this at the extraction→resolve boundary (the indexer does). It must NOT be tied to reads
    /// generically: the pipeline issues `get_meta`/`get_file` reads *during* extraction, and draining
    /// the buffer on those would exit bulk mode before extraction writes anything — defeating the
    /// batching (measured: ms_bulk stayed 7.3 s). Idempotent: a no-op once drained (`None`).
    pub async fn flush_bulk(&self) -> Result<()> {
        let taken = self.bulk.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some((nodes, edges, files)) = taken {
            self.insert_files_impl(&files).await?; // nodes/edges are independent of File
            self.insert_nodes_impl(&nodes).await?;
            self.insert_edges_impl(&edges).await?; // after nodes (endpoint referential integrity)
        }
        Ok(())
    }

    /// Bulk-insert fresh file records via `COPY` (the `Some` bulk path; the direct path MERGEs).
    async fn insert_files_impl(&self, files: &[FileRecord]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let mut csv = String::new();
        for f in files {
            let data = serde_json::to_string(f)?;
            csv.push_str(&format!(
                "{},{},{},{}\n",
                csv_q(&f.path),
                csv_q(&f.language),
                f.indexed_at,
                csv_q(&data),
            ));
        }
        self.copy_into("File", csv).await
    }

    /// Run a read query and map each result row through `f`.
    async fn rows<T, F>(&self, sql: String, f: F) -> Result<Vec<T>>
    where
        T: Send + 'static,
        F: Fn(Vec<lbug::Value>) -> Result<T> + Send + 'static,
    {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db).map_err(lbug_err)?;
            let result = conn.query(&sql).map_err(lbug_err)?;
            let mut out = Vec::new();
            for row in result {
                out.push(f(row)?);
            }
            Ok(out)
        })
        .await
        .map_err(lbug_err)?
    }

    /// Single `INT64` scalar (row 0, col 0), or 0 if no rows.
    async fn scalar_i64(&self, sql: String) -> Result<i64> {
        let rows = self.rows(sql, |r| Ok(r.first().map(as_i64).unwrap_or(0))).await?;
        Ok(rows.into_iter().next().unwrap_or(0))
    }

    /// Write `csv` to a scratch file and `COPY` it into `table`.
    async fn copy_into(&self, table: &'static str, csv: String) -> Result<()> {
        if csv.is_empty() {
            return Ok(());
        }
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let path = scratch_csv(table);
            std::fs::write(&path, csv.as_bytes()).map_err(lbug_err)?;
            let conn = lbug::Connection::new(&db).map_err(lbug_err)?;
            let sql = format!("COPY {table} FROM '{}' (HEADER=false);", path.display());
            let res = conn.query(&sql).map(|_| ()).map_err(lbug_err);
            let _ = std::fs::remove_file(&path);
            res
        })
        .await
        .map_err(lbug_err)?
    }

    // -- schema -------------------------------------------------------------

    async fn apply_schema(&self) -> Result<()> {
        // Node: indexed query columns (positions must match `insert_nodes`' CSV) + full JSON blob.
        self.exec(
            "CREATE NODE TABLE IF NOT EXISTS Node(\
                 id STRING, name STRING, name_lower STRING, kind STRING, \
                 qualified_name STRING, file STRING, language STRING, \
                 start_line INT64, route_method STRING, route_path STRING, framework STRING, \
                 data STRING, PRIMARY KEY(id));"
                .into(),
        )
        .await?;
        // Edge: FROM/TO ids + kind + line/col for identity + provenance + JSON blob.
        self.exec(
            "CREATE REL TABLE IF NOT EXISTS Edge(\
                 FROM Node TO Node, kind STRING, line INT64, col INT64, \
                 provenance STRING, data STRING);"
                .into(),
        )
        .await?;
        self.exec(
            "CREATE NODE TABLE IF NOT EXISTS File(\
                 path STRING, language STRING, indexed_at INT64, data STRING, PRIMARY KEY(path));"
                .into(),
        )
        .await?;
        // Unresolved: a synthetic PK (from|name|kind|seq) + query columns + blob.
        self.exec(
            "CREATE NODE TABLE IF NOT EXISTS Unresolved(\
                 uid STRING, from_node_id STRING, reference_name STRING, reference_kind STRING, \
                 name_tail STRING, file STRING, status STRING, data STRING, PRIMARY KEY(uid));"
                .into(),
        )
        .await?;
        self.exec(
            "CREATE NODE TABLE IF NOT EXISTS Meta(key STRING, value STRING, PRIMARY KEY(key));"
                .into(),
        )
        .await
    }

    // -- node CSV row -------------------------------------------------------

    fn node_csv(n: &Node) -> Result<String> {
        let data = serde_json::to_string(n)?;
        Ok(format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_q(&n.id),
            csv_q(&n.name),
            csv_q(&n.name.to_lowercase()),
            csv_q(n.kind.as_str()),
            csv_q(&n.qualified_name),
            csv_q(&n.file_path),
            csv_q(&n.language),
            n.start_line,
            csv_q(n.route_method.as_deref().unwrap_or("")),
            csv_q(n.route_path.as_deref().unwrap_or("")),
            csv_q(n.framework.as_deref().unwrap_or("")),
            csv_q(&data),
        ))
    }

    async fn insert_nodes_impl(&self, nodes: &[Node]) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        let mut csv = String::new();
        for n in nodes {
            csv.push_str(&Self::node_csv(n)?);
        }
        self.copy_into("Node", csv).await
    }

    /// Insert edges via COPY. Endpoints must already exist (referential integrity); dangling edges
    /// are filtered by pre-checking existing node ids. Returns rows written.
    async fn insert_edges_impl(&self, edges: &[Edge]) -> Result<u64> {
        if edges.is_empty() {
            return Ok(0);
        }
        let mut csv = String::new();
        let mut n = 0u64;
        for e in edges {
            let data = serde_json::to_string(e)?;
            let prov = match e.provenance {
                Some(Provenance::TreeSitter) => "tree_sitter",
                Some(Provenance::Scip) => "scip",
                Some(Provenance::Heuristic) => "heuristic",
                None => "",
            };
            csv.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                csv_q(&e.source),
                csv_q(&e.target),
                csv_q(e.kind.as_str()),
                e.line.map(|l| l as i64).unwrap_or(-1),
                e.column.map(|c| c as i64).unwrap_or(-1),
                csv_q(prov),
                csv_q(&data),
            ));
            n += 1;
        }
        self.copy_into("Edge", csv).await?;
        Ok(n)
    }

    fn node_kinds_filter(col: &str, kinds: &[NodeKind]) -> String {
        if kinds.is_empty() {
            return String::new();
        }
        let list = kinds.iter().map(|k| lit(k.as_str())).collect::<Vec<_>>().join(",");
        format!(" AND {col} IN [{list}]")
    }

    fn edge_kinds_filter(col: &str, kinds: &[EdgeKind]) -> String {
        if kinds.is_empty() {
            return String::new();
        }
        let list = kinds.iter().map(|k| lit(k.as_str())).collect::<Vec<_>>().join(",");
        format!(" AND {col} IN [{list}]")
    }
}

/// The `data`-column projection: `RETURN x.data`.
const NODE_RET: &str = "RETURN n.data";

// The trait declares every method in desugared `-> impl Future + Send` form (its docs explain why:
// a plain `async fn` in a public trait can't prove Send across the boundary). Matching that here
// trips clippy's `manual_async_fn`; the desugared form is intentional.
#[allow(clippy::manual_async_fn)]
impl GraphStore for LadybugStore {
    // ---- nodes: writes ----
    fn insert_nodes(&self, nodes: &[Node]) -> impl Future<Output = Result<()>> + Send {
        let nodes = nodes.to_vec();
        async move {
            {
                let mut g = self.bulk.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(b) = g.as_mut() {
                    b.0.extend(nodes);
                    return Ok(());
                }
            }
            self.insert_nodes_impl(&nodes).await
        }
    }

    // ---- nodes: reads ----
    fn get_node(&self, id: &str) -> impl Future<Output = Result<Option<Node>>> + Send {
        let sql = format!("MATCH (n:Node) WHERE n.id = {} {NODE_RET};", lit(id));
        async move {
            let mut v = self.rows(sql, |r| from_data::<Node>(&r[0])).await?;
            Ok(v.pop())
        }
    }

    fn get_nodes(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Result<HashMap<String, Node>>> + Send {
        let list = ids.iter().map(|i| lit(i)).collect::<Vec<_>>().join(",");
        async move {
            if list.is_empty() {
                return Ok(HashMap::new());
            }
            let sql = format!("MATCH (n:Node) WHERE n.id IN [{list}] {NODE_RET};");
            let nodes = self.rows(sql, |r| from_data::<Node>(&r[0])).await?;
            Ok(nodes.into_iter().map(|n| (n.id.clone(), n)).collect())
        }
    }

    fn all_nodes(&self) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!("MATCH (n:Node) {NODE_RET};");
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn get_nodes_by_file(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!("MATCH (n:Node) WHERE n.file = {} {NODE_RET};", lit(path));
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn get_nodes_by_kind(
        &self,
        kind: NodeKind,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!("MATCH (n:Node) WHERE n.kind = {} {NODE_RET};", lit(kind.as_str()));
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn get_nodes_by_name(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!("MATCH (n:Node) WHERE n.name = {} {NODE_RET};", lit(name));
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn get_nodes_by_name_ci(
        &self,
        lower: &str,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!("MATCH (n:Node) WHERE n.name_lower = {} {NODE_RET};", lit(lower));
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn get_nodes_by_name_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!(
            "MATCH (n:Node) WHERE starts_with(n.name, {}) {NODE_RET} LIMIT {limit};",
            lit(prefix)
        );
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn get_nodes_by_qualified_name(
        &self,
        qn: &str,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!("MATCH (n:Node) WHERE n.qualified_name = {} {NODE_RET};", lit(qn));
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn count_nodes_matching_name_in_files(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<u64>> + Send {
        let sql = format!(
            "MATCH (n:Node) WHERE n.name = {} RETURN count(DISTINCT n.file);",
            lit(name)
        );
        async move { Ok(self.scalar_i64(sql).await?.max(0) as u64) }
    }

    fn count_nodes_named(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<u64>> + Send {
        let sql = format!("MATCH (n:Node) WHERE n.name = {} RETURN count(n);", lit(name));
        async move { Ok(self.scalar_i64(sql).await?.max(0) as u64) }
    }

    fn nodes_by_kind_page(
        &self,
        kind: NodeKind,
        after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let after_clause = after.map(|a| format!(" AND n.id > {}", lit(a))).unwrap_or_default();
        let sql = format!(
            "MATCH (n:Node) WHERE n.kind = {}{after_clause} {NODE_RET} ORDER BY n.id LIMIT {limit};",
            lit(kind.as_str())
        );
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn find_route(
        &self,
        framework: Option<&str>,
        method: Option<&str>,
        path: &str,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let mut cond = format!("n.route_path = {}", lit(path));
        if let Some(fw) = framework {
            cond.push_str(&format!(" AND n.framework = {}", lit(fw)));
        }
        if let Some(m) = method {
            cond.push_str(&format!(" AND n.route_method = {}", lit(m)));
        }
        let sql = format!(
            "MATCH (n:Node) WHERE n.kind = 'route' AND {cond} {NODE_RET} \
             ORDER BY n.file, n.start_line, n.name;"
        );
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    // ---- edges ----
    fn insert_edges(&self, edges: &[Edge]) -> impl Future<Output = Result<u64>> + Send {
        let edges = edges.to_vec();
        async move {
            {
                let mut g = self.bulk.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(b) = g.as_mut() {
                    let n = edges.len() as u64;
                    b.1.extend(edges);
                    return Ok(n);
                }
            }
            self.insert_edges_impl(&edges).await
        }
    }

    fn outgoing(
        &self,
        id: &str,
        kinds: &[EdgeKind],
        provenance: Option<Provenance>,
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send {
        let kf = Self::edge_kinds_filter("e.kind", kinds);
        let pf = provenance
            .map(|p| {
                let s = match p {
                    Provenance::TreeSitter => "tree_sitter",
                    Provenance::Scip => "scip",
                    Provenance::Heuristic => "heuristic",
                };
                format!(" AND e.provenance = {}", lit(s))
            })
            .unwrap_or_default();
        let sql = format!(
            "MATCH (a:Node)-[e:Edge]->(n:Node) WHERE a.id = {}{kf}{pf} RETURN e.data, n.data;",
            lit(id)
        );
        async move {
            self.rows(sql, |r| {
                Ok(NeighborEntry { edge: from_data(&r[0])?, node: from_data(&r[1])? })
            })
            .await
        }
    }

    fn incoming(
        &self,
        id: &str,
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send {
        let kf = Self::edge_kinds_filter("e.kind", kinds);
        let sql = format!(
            "MATCH (n:Node)-[e:Edge]->(a:Node) WHERE a.id = {}{kf} RETURN e.data, n.data;",
            lit(id)
        );
        async move {
            self.rows(sql, |r| {
                Ok(NeighborEntry { edge: from_data(&r[0])?, node: from_data(&r[1])? })
            })
            .await
        }
    }

    fn outgoing_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<HashMap<String, Vec<NeighborEntry>>>> + Send {
        let list = ids.iter().map(|i| lit(i)).collect::<Vec<_>>().join(",");
        let kf = Self::edge_kinds_filter("e.kind", kinds);
        async move {
            if list.is_empty() {
                return Ok(HashMap::new());
            }
            let sql = format!(
                "MATCH (a:Node)-[e:Edge]->(n:Node) WHERE a.id IN [{list}]{kf} \
                 RETURN a.id, e.data, n.data;"
            );
            let rows = self
                .rows(sql, |r| {
                    Ok((as_string(&r[0]), NeighborEntry { edge: from_data(&r[1])?, node: from_data(&r[2])? }))
                })
                .await?;
            let mut map: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
            for (k, ne) in rows {
                map.entry(k).or_default().push(ne);
            }
            Ok(map)
        }
    }

    fn incoming_batch(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<HashMap<String, Vec<NeighborEntry>>>> + Send {
        let list = ids.iter().map(|i| lit(i)).collect::<Vec<_>>().join(",");
        let kf = Self::edge_kinds_filter("e.kind", kinds);
        async move {
            if list.is_empty() {
                return Ok(HashMap::new());
            }
            let sql = format!(
                "MATCH (n:Node)-[e:Edge]->(a:Node) WHERE a.id IN [{list}]{kf} \
                 RETURN a.id, e.data, n.data;"
            );
            let rows = self
                .rows(sql, |r| {
                    Ok((as_string(&r[0]), NeighborEntry { edge: from_data(&r[1])?, node: from_data(&r[2])? }))
                })
                .await?;
            let mut map: HashMap<String, Vec<NeighborEntry>> = HashMap::new();
            for (k, ne) in rows {
                map.entry(k).or_default().push(ne);
            }
            Ok(map)
        }
    }

    fn edges_between(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<Vec<Edge>>> + Send {
        let list = ids.iter().map(|i| lit(i)).collect::<Vec<_>>().join(",");
        let kf = Self::edge_kinds_filter("e.kind", kinds);
        async move {
            if list.is_empty() {
                return Ok(Vec::new());
            }
            let sql = format!(
                "MATCH (a:Node)-[e:Edge]->(b:Node) WHERE a.id IN [{list}] AND b.id IN [{list}]{kf} \
                 RETURN e.data;"
            );
            self.rows(sql, |r| from_data::<Edge>(&r[0])).await
        }
    }

    fn cross_file_incoming_with_target(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Vec<(Edge, String, NodeKind)>>> + Send {
        let sql = format!(
            "MATCH (s:Node)-[e:Edge]->(t:Node) \
             WHERE t.file = {p} AND s.file <> {p} AND e.kind <> 'contains' \
             RETURN e.data, t.name, t.kind;",
            p = lit(path)
        );
        async move {
            self.rows(sql, |r| {
                let edge: Edge = from_data(&r[0])?;
                let name = as_string(&r[1]);
                let kind = NodeKind::from_str(&as_string(&r[2])).map_err(Error::from)?;
                Ok((edge, name, kind))
            })
            .await
        }
    }

    fn dependent_file_paths(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Vec<String>>> + Send {
        let sql = format!(
            "MATCH (s:Node)-[e:Edge]->(t:Node) \
             WHERE t.file = {p} AND s.file <> {p} AND e.kind <> 'contains' \
             RETURN DISTINCT s.file;",
            p = lit(path)
        );
        async move { self.rows(sql, |r| Ok(as_string(&r[0]))).await }
    }

    fn dependency_file_paths(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Vec<String>>> + Send {
        let sql = format!(
            "MATCH (s:Node)-[e:Edge]->(t:Node) \
             WHERE s.file = {p} AND t.file <> {p} AND e.kind <> 'contains' \
             RETURN DISTINCT t.file;",
            p = lit(path)
        );
        async move { self.rows(sql, |r| Ok(as_string(&r[0]))).await }
    }

    // ---- files ----
    fn upsert_file(&self, f: &FileRecord) -> impl Future<Output = Result<()>> + Send {
        let f = f.clone();
        async move { self.upsert_files(std::slice::from_ref(&f)).await }
    }

    fn upsert_files(
        &self,
        files: &[FileRecord],
    ) -> impl Future<Output = Result<()>> + Send {
        let files = files.to_vec();
        async move {
            {
                let mut g = self.bulk.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(b) = g.as_mut() {
                    b.2.extend(files);
                    return Ok(());
                }
            }
            // Direct path (incremental sync): MERGE = upsert on the PK, one per file.
            for f in &files {
                let data = serde_json::to_string(f)?;
                let sql = format!(
                    "MERGE (x:File {{path: {}}}) SET x.language = {}, x.indexed_at = {}, x.data = {};",
                    lit(&f.path),
                    lit(&f.language),
                    f.indexed_at,
                    lit(&data),
                );
                self.exec(sql).await?;
            }
            Ok(())
        }
    }

    fn get_file(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Option<FileRecord>>> + Send {
        let sql = format!("MATCH (x:File) WHERE x.path = {} RETURN x.data;", lit(path));
        async move {
            let mut v = self.rows(sql, |r| from_data::<FileRecord>(&r[0])).await?;
            Ok(v.pop())
        }
    }

    fn all_files(&self) -> impl Future<Output = Result<Vec<FileRecord>>> + Send {
        let sql = "MATCH (x:File) RETURN x.data;".to_string();
        async move { self.rows(sql, |r| from_data::<FileRecord>(&r[0])).await }
    }

    fn delete_file(&self, path: &str) -> impl Future<Output = Result<()>> + Send {
        let p = lit(path);
        async move {
            // Delete nodes in the file (DETACH removes their edges), the file row, and unresolved.
            self.exec(format!("MATCH (n:Node) WHERE n.file = {p} DETACH DELETE n;")).await?;
            self.exec(format!("MATCH (u:Unresolved) WHERE u.file = {p} DELETE u;")).await?;
            self.exec(format!("MATCH (x:File) WHERE x.path = {p} DELETE x;")).await
        }
    }

    fn last_indexed_at(&self) -> impl Future<Output = Result<Option<i64>>> + Send {
        let sql = "MATCH (x:File) RETURN max(x.indexed_at);".to_string();
        async move {
            let rows = self.rows(sql, |r| Ok(r.first().map(as_i64))).await?;
            Ok(rows.into_iter().flatten().next().filter(|&v| v != 0))
        }
    }

    fn distinct_file_languages(
        &self,
    ) -> impl Future<Output = Result<BTreeSet<String>>> + Send {
        let sql = "MATCH (x:File) RETURN DISTINCT x.language;".to_string();
        async move {
            let v = self.rows(sql, |r| Ok(as_string(&r[0]))).await?;
            Ok(v.into_iter().filter(|s| !s.is_empty()).collect())
        }
    }

    fn replace_file_extraction(
        &self,
        path: &str,
        nodes: &[Node],
        edges: &[Edge],
        unresolved: &[UnresolvedRef],
        file_record: &FileRecord,
    ) -> impl Future<Output = Result<ReplaceStats>> + Send {
        let (path, nodes, edges, unresolved, file_record) = (
            path.to_string(),
            nodes.to_vec(),
            edges.to_vec(),
            unresolved.to_vec(),
            file_record.clone(),
        );
        async move {
            // Simplified protocol (no cross-file incoming snapshot yet — parity TODO): delete old,
            // insert fresh, upsert file last.
            self.delete_file(&path).await?;
            self.insert_nodes_impl(&nodes).await?;
            let edges_inserted = self.insert_edges_impl(&edges).await?;
            self.insert_unresolved(&unresolved).await?;
            self.upsert_files(std::slice::from_ref(&file_record)).await?;
            Ok(ReplaceStats {
                nodes_inserted: nodes.len() as u64,
                edges_inserted,
                incoming_reattached: 0,
                incoming_resurrected: 0,
                incoming_dropped: 0,
            })
        }
    }

    // ---- unresolved ----
    fn insert_unresolved(
        &self,
        refs: &[UnresolvedRef],
    ) -> impl Future<Output = Result<()>> + Send {
        let refs = refs.to_vec();
        async move {
            if refs.is_empty() {
                return Ok(());
            }
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let mut csv = String::new();
            for r in &refs {
                let uid = format!(
                    "{}|{}|{}|{}",
                    r.from_node_id,
                    r.reference_name,
                    r.reference_kind,
                    SEQ.fetch_add(1, Ordering::Relaxed)
                );
                let data = serde_json::to_string(r)?;
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{}\n",
                    csv_q(&uid),
                    csv_q(&r.from_node_id),
                    csv_q(&r.reference_name),
                    csv_q(&r.reference_kind),
                    csv_q(&r.name_tail),
                    csv_q(&r.file_path),
                    csv_q(r.status.as_str()),
                    csv_q(&data),
                ));
            }
            self.copy_into("Unresolved", csv).await
        }
    }

    fn unresolved_pending_count(&self) -> impl Future<Output = Result<u64>> + Send {
        let sql = "MATCH (u:Unresolved) WHERE u.status = 'pending' RETURN count(u);".to_string();
        async move { Ok(self.scalar_i64(sql).await?.max(0) as u64) }
    }

    fn unresolved_pending_batch(
        &self,
        offset: usize,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<UnresolvedRef>>> + Send {
        let sql = format!(
            "MATCH (u:Unresolved) WHERE u.status = 'pending' RETURN u.data \
             ORDER BY u.uid SKIP {offset} LIMIT {limit};"
        );
        async move { self.rows(sql, |r| from_data::<UnresolvedRef>(&r[0])).await }
    }

    fn unresolved_by_files(
        &self,
        paths: &[String],
    ) -> impl Future<Output = Result<Vec<UnresolvedRef>>> + Send {
        let list = paths.iter().map(|p| lit(p)).collect::<Vec<_>>().join(",");
        async move {
            if list.is_empty() {
                return Ok(Vec::new());
            }
            let sql = format!(
                "MATCH (u:Unresolved) WHERE u.status = 'pending' AND u.file IN [{list}] RETURN u.data;"
            );
            self.rows(sql, |r| from_data::<UnresolvedRef>(&r[0])).await
        }
    }

    fn delete_resolved(
        &self,
        keys: &[UnresolvedKey],
    ) -> impl Future<Output = Result<()>> + Send {
        let keys = keys.to_vec();
        async move {
            for (from, name, kind) in &keys {
                let sql = format!(
                    "MATCH (u:Unresolved) WHERE u.from_node_id = {} AND u.reference_name = {} \
                     AND u.reference_kind = {} DELETE u;",
                    lit(from),
                    lit(name),
                    lit(kind)
                );
                self.exec(sql).await?;
            }
            Ok(())
        }
    }

    fn replace_pending_with_failed(
        &self,
        failed: &[UnresolvedRef],
    ) -> impl Future<Output = Result<()>> + Send {
        let failed = failed.to_vec();
        async move {
            self.exec("MATCH (u:Unresolved) WHERE u.status = 'pending' DELETE u;".into()).await?;
            let mut failed = failed;
            for r in &mut failed {
                r.status = RefStatus::Failed;
            }
            self.insert_unresolved(&failed).await
        }
    }

    fn mark_failed(
        &self,
        keys: &[UnresolvedKey],
    ) -> impl Future<Output = Result<()>> + Send {
        let keys = keys.to_vec();
        async move {
            for (from, name, kind) in &keys {
                let sql = format!(
                    "MATCH (u:Unresolved) WHERE u.from_node_id = {} AND u.reference_name = {} \
                     AND u.reference_kind = {} SET u.status = 'failed';",
                    lit(from),
                    lit(name),
                    lit(kind)
                );
                self.exec(sql).await?;
            }
            Ok(())
        }
    }

    fn retryable_failed(
        &self,
        names: &[String],
        per_name_ceiling: usize,
    ) -> impl Future<Output = Result<Vec<UnresolvedRef>>> + Send {
        let list = names.iter().map(|n| lit(n)).collect::<Vec<_>>().join(",");
        async move {
            if list.is_empty() {
                return Ok(Vec::new());
            }
            // Per-name ceiling enforced client-side (Cypher lacks a simple per-group cap here).
            let sql = format!(
                "MATCH (u:Unresolved) WHERE u.status = 'failed' AND u.reference_name IN [{list}] \
                 RETURN u.data ORDER BY u.uid;"
            );
            let all = self.rows(sql, |r| from_data::<UnresolvedRef>(&r[0])).await?;
            let mut per: HashMap<String, usize> = HashMap::new();
            let mut out = Vec::new();
            for r in all {
                let c = per.entry(r.reference_name.clone()).or_default();
                if *c < per_name_ceiling {
                    *c += 1;
                    out.push(r);
                }
            }
            Ok(out)
        }
    }

    fn clear_unresolved(&self) -> impl Future<Output = Result<()>> + Send {
        async move { self.exec("MATCH (u:Unresolved) DELETE u;".into()).await }
    }

    // ---- meta + stats ----
    fn get_meta(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<String>>> + Send {
        let sql = format!("MATCH (m:Meta) WHERE m.key = {} RETURN m.value;", lit(key));
        async move {
            let mut v = self.rows(sql, |r| Ok(as_string(&r[0]))).await?;
            Ok(v.pop())
        }
    }

    fn set_meta(
        &self,
        key: &str,
        value: &str,
    ) -> impl Future<Output = Result<()>> + Send {
        let sql = format!(
            "MERGE (m:Meta {{key: {}}}) SET m.value = {};",
            lit(key),
            lit(value)
        );
        async move { self.exec(sql).await }
    }

    fn stats(&self) -> impl Future<Output = Result<GraphStats>> + Send {
        async move {
            let (nodes, edges) = self.node_edge_count().await?;
            let files = self.scalar_i64("MATCH (x:File) RETURN count(x);".into()).await?.max(0) as u64;
            let nbk = self
                .rows("MATCH (n:Node) RETURN n.kind, count(n);".into(), |r| {
                    Ok((as_string(&r[0]), as_i64(&r[1]).max(0) as u64))
                })
                .await?;
            let ebk = self
                .rows("MATCH ()-[e:Edge]->() RETURN e.kind, count(e);".into(), |r| {
                    Ok((as_string(&r[0]), as_i64(&r[1]).max(0) as u64))
                })
                .await?;
            let langs = self
                .rows("MATCH (x:File) RETURN x.language, count(x);".into(), |r| {
                    Ok((as_string(&r[0]), as_i64(&r[1]).max(0) as u64))
                })
                .await?;
            Ok(GraphStats {
                nodes,
                edges,
                files,
                nodes_by_kind: nbk.into_iter().collect::<BTreeMap<_, _>>(),
                edges_by_kind: ebk.into_iter().collect::<BTreeMap<_, _>>(),
                languages: langs.into_iter().filter(|(l, _)| !l.is_empty()).collect(),
            })
        }
    }

    fn node_edge_count(&self) -> impl Future<Output = Result<(u64, u64)>> + Send {
        async move {
            let n = self.scalar_i64("MATCH (n:Node) RETURN count(n);".into()).await?;
            let e = self.scalar_i64("MATCH ()-[e:Edge]->() RETURN count(e);".into()).await?;
            Ok((n.max(0) as u64, e.max(0) as u64))
        }
    }

    fn dominant_file(
        &self,
    ) -> impl Future<Output = Result<Option<(String, u64, u64)>>> + Send {
        let sql = "MATCH (s:Node)-[e:Edge]->() RETURN s.file, count(e) AS c \
                   ORDER BY c DESC LIMIT 2;"
            .to_string();
        async move {
            let rows = self
                .rows(sql, |r| Ok((as_string(&r[0]), as_i64(&r[1]).max(0) as u64)))
                .await?;
            Ok(match rows.as_slice() {
                [] => None,
                [(f, c)] => Some((f.clone(), *c, 0)),
                [(f, c), (_, c2), ..] => Some((f.clone(), *c, *c2)),
            })
        }
    }

    fn clear(&self) -> impl Future<Output = Result<()>> + Send {
        async move {
            self.exec("MATCH (n:Node) DETACH DELETE n;".into()).await?;
            self.exec("MATCH (x:File) DELETE x;".into()).await?;
            self.exec("MATCH (u:Unresolved) DELETE u;".into()).await
        }
    }

    // ---- bulk load ----
    fn bulk_load_begin(&self) -> impl Future<Output = Result<()>> + Send {
        // Enter buffering mode: writes accumulate in memory instead of a COPY per call.
        async move {
            *self.bulk.lock().unwrap_or_else(|p| p.into_inner()) =
                Some((Vec::new(), Vec::new(), Vec::new()));
            Ok(())
        }
    }
    fn bulk_load_finish(&self) -> impl Future<Output = Result<()>> + Send {
        // Flush the buffered extraction as ONE node COPY + ONE edge COPY (Kuzu's fast fresh-table
        // path), then fold the WAL into the main store. Best-effort checkpoint — a failure is not
        // data loss (the WAL replays on next open), so it must not fail the index.
        async move {
            self.flush_bulk().await?; // idempotent; usually already drained by the first read
            let _ = self.exec("CHECKPOINT;".into()).await;
            Ok(())
        }
    }

    // ---- search (FTS/vector deferred; name-LIKE native) ----
    fn search_fts(
        &self,
        _terms: &[String],
        _kinds: &[NodeKind],
        _languages: &[String],
        _limit: usize,
        _offset: usize,
    ) -> impl Future<Output = Result<Vec<SearchCandidate>>> + Send {
        async move { Ok(Vec::new()) }
    }

    fn search_name_like(
        &self,
        q: &str,
        kinds: &[NodeKind],
        limit: usize,
    ) -> impl Future<Output = Result<Vec<SearchCandidate>>> + Send {
        let kf = Self::node_kinds_filter("n.kind", kinds);
        let qs = q.to_string();
        let ql = q.to_lowercase();
        let sql = format!(
            "MATCH (n:Node) WHERE (n.name = {exact} OR starts_with(n.name_lower, {pfx}) \
             OR contains(n.name_lower, {pfx})){kf} \
             RETURN n.data, n.name, n.name_lower LIMIT {limit};",
            exact = lit(q),
            pfx = lit(&ql),
        );
        async move {
            self.rows(sql, move |r| {
                let node: Node = from_data(&r[0])?;
                let name = as_string(&r[1]);
                let name_lower = as_string(&r[2]);
                // exact=3, prefix=2, contains=1 (mirrors the LIKE tiers).
                let raw = if name == qs {
                    3.0
                } else if name_lower.starts_with(&ql) {
                    2.0
                } else {
                    1.0
                };
                Ok(SearchCandidate { node, raw_score: raw })
            })
            .await
        }
    }

    fn find_by_exact_names(
        &self,
        names: &[String],
        per_name_limit: usize,
    ) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let list = names.iter().map(|n| lit(n)).collect::<Vec<_>>().join(",");
        async move {
            if list.is_empty() {
                return Ok(Vec::new());
            }
            let sql = format!("MATCH (n:Node) WHERE n.name IN [{list}] {NODE_RET};");
            let all = self.rows(sql, |r| from_data::<Node>(&r[0])).await?;
            let mut per: HashMap<String, usize> = HashMap::new();
            let mut out = Vec::new();
            for n in all {
                let c = per.entry(n.name.clone()).or_default();
                if *c < per_name_limit {
                    *c += 1;
                    out.push(n);
                }
            }
            Ok(out)
        }
    }

    fn all_node_names(&self) -> impl Future<Output = Result<Vec<String>>> + Send {
        let sql = "MATCH (n:Node) RETURN DISTINCT n.name;".to_string();
        async move { self.rows(sql, |r| Ok(as_string(&r[0]))).await }
    }

    // ---- traversal (native variable-length MATCH) ----
    fn callers(
        &self,
        id: &str,
        max_depth: u32,
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send {
        // Whitelist: calls/references/imports/instantiates, incoming direction.
        let sql = format!(
            "MATCH (n:Node)-[e:Edge*1..{d}]->(a:Node) \
             WHERE a.id = {id} AND ALL(r IN rels(e) WHERE r.kind IN ['calls','references','imports','instantiates']) \
             RETURN DISTINCT list_element(rels(e), size(rels(e))).data, n.data;",
            d = max_depth.max(1),
            id = lit(id)
        );
        async move {
            self.rows(sql, |r| {
                Ok(NeighborEntry { edge: from_data(&r[0])?, node: from_data(&r[1])? })
            })
            .await
        }
    }

    fn callees(
        &self,
        id: &str,
        max_depth: u32,
    ) -> impl Future<Output = Result<Vec<NeighborEntry>>> + Send {
        let sql = format!(
            "MATCH (a:Node)-[e:Edge*1..{d}]->(n:Node) \
             WHERE a.id = {id} AND ALL(r IN rels(e) WHERE r.kind IN ['calls','references','imports','instantiates']) \
             RETURN DISTINCT list_element(rels(e), size(rels(e))).data, n.data;",
            d = max_depth.max(1),
            id = lit(id)
        );
        async move {
            self.rows(sql, |r| {
                Ok(NeighborEntry { edge: from_data(&r[0])?, node: from_data(&r[1])? })
            })
            .await
        }
    }

    fn impact_radius(
        &self,
        id: &str,
        max_depth: u32,
    ) -> impl Future<Output = Result<Subgraph>> + Send {
        let id = id.to_string();
        async move {
            let entries = self.callers(&id, max_depth).await?;
            let mut nodes = IndexMap::new();
            let mut edges = Vec::new();
            for ne in entries {
                nodes.insert(ne.node.id.clone(), ne.node);
                edges.push(ne.edge);
            }
            Ok(Subgraph { nodes, edges, roots: vec![id] })
        }
    }

    fn find_path(
        &self,
        from: &str,
        to: &str,
        kinds: &[EdgeKind],
    ) -> impl Future<Output = Result<Option<Vec<(Node, Option<Edge>)>>>> + Send {
        let all = if kinds.is_empty() {
            String::new()
        } else {
            let list = kinds.iter().map(|k| lit(k.as_str())).collect::<Vec<_>>().join(",");
            format!(" AND ALL(r IN rels(p) WHERE r.kind IN [{list}])")
        };
        // Bounded to avoid all-paths explosion; takes the first path (parity TODO: prove it is the
        // shortest — Kuzu's `SHORTEST` keyword). Reconstruct the step list from the `data` blobs of
        // the path's nodes and rels via list comprehension.
        let sql = format!(
            "MATCH p = (a:Node)-[e:Edge*1..15]->(b:Node) WHERE a.id = {} AND b.id = {}{all} \
             RETURN list_transform(nodes(p), x -> x.data), list_transform(rels(p), r -> r.data) LIMIT 1;",
            lit(from),
            lit(to)
        );
        async move {
            let mut rows = self
                .rows(sql, |r| Ok((as_string_list(&r[0]), as_string_list(&r[1]))))
                .await?;
            let Some((nds, eds)) = rows.pop() else { return Ok(None) };
            let nodes = nds
                .iter()
                .map(|s| serde_json::from_str::<Node>(s).map_err(Error::from))
                .collect::<Result<Vec<_>>>()?;
            let edges = eds
                .iter()
                .map(|s| serde_json::from_str::<Edge>(s).map_err(Error::from))
                .collect::<Result<Vec<_>>>()?;
            // First node has no inbound edge; node[i] is reached by edge[i-1].
            let path = nodes
                .into_iter()
                .enumerate()
                .map(|(i, n)| (n, i.checked_sub(1).and_then(|j| edges.get(j).cloned())))
                .collect();
            Ok(Some(path))
        }
    }

    fn type_hierarchy(&self, id: &str) -> impl Future<Output = Result<Subgraph>> + Send {
        let id = id.to_string();
        async move {
            let mut nodes = IndexMap::new();
            let mut edges = Vec::new();
            // Ancestors: outgoing extends/implements. Descendants: incoming.
            let up = format!(
                "MATCH (a:Node)-[e:Edge*1..64]->(n:Node) WHERE a.id = {id} \
                 AND ALL(r IN rels(e) WHERE r.kind IN ['extends','implements']) RETURN DISTINCT list_element(rels(e), size(rels(e))).data, n.data;",
                id = lit(&id)
            );
            let down = format!(
                "MATCH (n:Node)-[e:Edge*1..64]->(a:Node) WHERE a.id = {id} \
                 AND ALL(r IN rels(e) WHERE r.kind IN ['extends','implements']) RETURN DISTINCT list_element(rels(e), size(rels(e))).data, n.data;",
                id = lit(&id)
            );
            for sql in [up, down] {
                let rows = self
                    .rows(sql, |r| Ok((from_data::<Edge>(&r[0])?, from_data::<Node>(&r[1])?)))
                    .await?;
                for (edge, node) in rows {
                    nodes.insert(node.id.clone(), node);
                    edges.push(edge);
                }
            }
            Ok(Subgraph { nodes, edges, roots: vec![id] })
        }
    }

    fn traverse(
        &self,
        start: &str,
        opts: &TraversalOptions,
    ) -> impl Future<Output = Result<Subgraph>> + Send {
        let start = start.to_string();
        let depth = opts.max_depth.unwrap_or(64).max(1);
        let dir = opts.direction;
        let ekf = if opts.edge_kinds.is_empty() {
            String::new()
        } else {
            let list = opts.edge_kinds.iter().map(|k| lit(k.as_str())).collect::<Vec<_>>().join(",");
            format!(" AND ALL(r IN rels(e) WHERE r.kind IN [{list}])")
        };
        let limit = opts.limit;
        let include_start = opts.include_start;
        async move {
            let pat = match dir {
                Direction::Outgoing => format!("(a:Node)-[e:Edge*1..{depth}]->(n:Node)"),
                Direction::Incoming => format!("(n:Node)-[e:Edge*1..{depth}]->(a:Node)"),
                Direction::Both => format!("(a:Node)-[e:Edge*1..{depth}]-(n:Node)"),
            };
            let sql = format!(
                "MATCH {pat} WHERE a.id = {}{ekf} RETURN DISTINCT list_element(rels(e), size(rels(e))).data, n.data LIMIT {limit};",
                lit(&start)
            );
            let rows = self
                .rows(sql, |r| Ok((from_data::<Edge>(&r[0])?, from_data::<Node>(&r[1])?)))
                .await?;
            let mut nodes = IndexMap::new();
            let mut edges = Vec::new();
            if include_start && let Some(s) = self.get_node(&start).await? {
                nodes.insert(s.id.clone(), s);
            }
            for (edge, node) in rows {
                nodes.insert(node.id.clone(), node);
                edges.push(edge);
            }
            Ok(Subgraph { nodes, edges, roots: vec![start] })
        }
    }

    fn ancestors(&self, id: &str) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!(
            "MATCH (c:Node)-[e:Edge*1..64]->(a:Node) WHERE a.id = {} \
             AND ALL(r IN rels(e) WHERE r.kind = 'contains') RETURN c.data;",
            lit(id)
        );
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }

    fn children(&self, id: &str) -> impl Future<Output = Result<Vec<Node>>> + Send {
        let sql = format!(
            "MATCH (a:Node)-[e:Edge]->(c:Node) WHERE a.id = {} AND e.kind = 'contains' RETURN c.data;",
            lit(id)
        );
        async move { self.rows(sql, |r| from_data::<Node>(&r[0])).await }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selene_core::{Edge, EdgeKind, Node, NodeKind, Visibility};

    fn node(id: &str, name: &str, file: &str) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: name.into(),
            qualified_name: format!("mod::{name}"),
            file_path: file.into(),
            language: "rust".into(),
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            docstring: Some("doc, with comma".into()),
            signature: Some(format!("fn {name}(x: T) -> U")),
            visibility: Some(Visibility::Public),
            is_exported: Some(true),
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: vec!["#[test]".into(), "quote'd".into()],
            type_parameters: vec!["T".into()],
            return_type: Some("U".into()),
            route_method: None,
            route_path: None,
            framework: None,
            updated_at: 0,
        }
    }

    #[tokio::test]
    async fn round_trips_nodes_edges_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let store = LadybugStore::open(&dir.path().join("db")).await.unwrap();

        let nodes: Vec<Node> = (0..500)
            .map(|i| node(&format!("n{i}"), &format!("fn{i}"), &format!("f{}.rs", i % 10)))
            .collect();
        store.insert_nodes(&nodes).await.unwrap();

        let edges: Vec<Edge> = (0..499)
            .map(|i| Edge {
                source: format!("n{i}"),
                target: format!("n{}", i + 1),
                kind: EdgeKind::Calls,
                metadata: None,
                line: Some(i as u32),
                column: None,
                provenance: Some(Provenance::TreeSitter),
            })
            .collect();
        assert_eq!(store.insert_edges(&edges).await.unwrap(), 499);

        // Counts.
        assert_eq!(store.node_edge_count().await.unwrap(), (500, 499));

        // Full-fidelity round trip: every field survives the JSON blob.
        let got = store.get_node("n0").await.unwrap().unwrap();
        assert_eq!(got, nodes[0]);

        // Reads.
        assert_eq!(store.all_nodes().await.unwrap().len(), 500);
        assert_eq!(store.get_nodes_by_name("fn5").await.unwrap().len(), 1);
        assert_eq!(store.get_nodes_by_file("f0.rs").await.unwrap().len(), 50);
        assert_eq!(store.get_nodes_by_qualified_name("mod::fn7").await.unwrap().len(), 1);

        // Adjacency.
        let out = store.outgoing("n0", &[], None).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].node.id, "n1");
        let inc = store.incoming("n1", &[]).await.unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].node.id, "n0");

        // Traversal (callees over the call chain, depth 3).
        let callees = store.callees("n0", 3).await.unwrap();
        assert_eq!(callees.len(), 3, "n1,n2,n3 reachable within 3 hops");

        // find_path reconstructs the chain n0->n1->n2->n3.
        let path = store.find_path("n0", "n3", &[]).await.unwrap().unwrap();
        assert_eq!(path.len(), 4);
        assert_eq!(path[0].0.id, "n0");
        assert!(path[0].1.is_none(), "first step has no inbound edge");
        assert_eq!(path[3].0.id, "n3");
        assert!(path[3].1.is_some());
    }
}
