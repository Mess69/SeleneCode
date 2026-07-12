//! File records + the single-file re-index protocol: inherent methods on
//! [`SurrealStore`] mirroring the file section of [`crate::GraphStore`]
//! (Task 6). `impl GraphStore for SurrealStore` is wired later (Task 10); until
//! then these are plain inherent `async fn`s. [`SurrealStore::replace_file_extraction`]
//! is *not* on the trait at all yet (see [`ReplaceStats`] — Task 10 may lift it).
//!
//! ## `file` record id ↔ `FileRecord.path`
//!
//! Unlike `node` (whose `id` is only the record key, never a stored field), the
//! `file` table stores `path` **both** as the record key *and* as an ordinary
//! `path` field (`src/schema.rs`). So the id bridge that `src/nodes.rs` needs
//! (`record::id(id) AS id`) is unnecessary here — every read projects the
//! `path` field directly, and every write addresses the record by
//! `type::record('file', $path)` while `CONTENT` carries the full `FileRecord`
//! (`path` included). `upsert_file` uses `UPSERT ... CONTENT` for the TS store's
//! INSERT-OR-REPLACE semantics, exactly like `insert_nodes`.
//!
//! ## `delete_file` cascade relies on SurrealDB relation semantics
//!
//! The 12 edge tables are `TYPE RELATION IN node OUT node ENFORCED`
//! (`src/schema.rs`). **Probed against the embedded 3.2 engine for this task**
//! (a throwaway exploration probe, not kept): deleting a `node` record —
//! including a `DELETE node WHERE filePath = $path` multi-record delete —
//! automatically
//! removes every RELATE edge whose reserved `in`/`out` link points at a deleted
//! node, across *all* edge tables, while nodes in other files (and their edges)
//! are untouched. So `delete_file` does **not** enumerate the 12 edge tables:
//! deleting the file's nodes cascades their edges for free. Only `unresolved_ref`
//! (a plain, non-relation table) needs an explicit delete — by its denormalized
//! `filePath` *and* by `fromNodeId ∈ {deleted node ids}` (belt-and-suspenders
//! for the TS FK-on-`from_node_id` cascade, in case a ref's `filePath` ever
//! diverges from its source node's file).
//!
//! ## `replace_file_extraction` — the re-index write protocol (no rollback)
//!
//! A port of `storeExtractionResult` steps 2–8 (step 1, the content-hash skip,
//! is the caller's job). SurrealDB embedded gives per-query atomicity only —
//! there is no multi-statement transaction spanning the whole protocol, exactly
//! as the TS/SQLite original had none. The crash-safety mechanism is the **step
//! order**: the `file` row is upserted **last**. A crash between any earlier
//! step and the final `upsert_file` leaves no `file` row, so the next indexing
//! run sees the file as un-indexed and re-runs the whole protocol — a partial
//! re-index never masquerades as complete. This ordering is load-bearing; do
//! not move `upsert_file` earlier.
//!
//! ## Unresolved-ref helpers live in `src/unresolved.rs` (Task 7)
//!
//! This module used to carry private `unresolved_content`/`name_tail`/
//! `insert_unresolved_rows` helpers (Task 6, written before the
//! unresolved-ref CRUD section of `GraphStore` had a home). They are now the
//! public [`SurrealStore::insert_unresolved`] and `pub(crate)`
//! `crate::unresolved::name_tail` in `src/unresolved.rs`;
//! [`SurrealStore::build_resurrected`] and [`SurrealStore::replace_file_extraction`]
//! below call those instead of duplicating them.

use std::collections::{BTreeSet, HashMap};

use selene_core::{Edge, Node, NodeKind};
use surrealdb::types::RecordId;

use crate::unresolved::name_tail;
use crate::{FileRecord, RefStatus, ReplaceStats, Result, SurrealStore, UnresolvedRef};

/// Column list for every `file` read, in `FileRecord`'s field order. `path` is
/// a stored field (also the record key — see the module docs), so no
/// `record::id` bridge is needed.
const FILE_FIELDS: &str =
    "path, contentHash, language, size, modifiedAt, indexedAt, nodeCount, errors";

/// Serializes a [`FileRecord`] into its stored content object. `FileRecord` has
/// no optional fields, so — unlike `node_content`/`edge_content` — nothing is
/// stripped: the full camelCase object (including `path`, which is both key and
/// field) is written verbatim.
fn file_content(f: &FileRecord) -> Result<serde_json::Value> {
    serde_json::to_value(f).map_err(crate::Error::from)
}

/// The stamped `(refName, refKind)` an edge carries for resurrection, or `None`
/// if either is absent/non-string. Synthesized/heuristic edges stamp these in
/// `metadata` so an unmatched cross-file edge can be resurrected as an
/// unresolved reference rather than lost (`#899`).
fn ref_stamp(edge: &Edge) -> Option<(String, String)> {
    let obj = edge.metadata.as_ref()?.as_object()?;
    let ref_name = obj.get("refName")?.as_str()?.to_string();
    let ref_kind = obj.get("refKind")?.as_str()?.to_string();
    Some((ref_name, ref_kind))
}

impl SurrealStore {
    /// Insert or replace the file record for `f.path` (INSERT-OR-REPLACE via
    /// `UPSERT ... CONTENT`; see the module docs).
    pub async fn upsert_file(&self, f: &FileRecord) -> Result<()> {
        self.db()
            .query("UPSERT type::record('file', $path) CONTENT $content")
            .bind(("path", f.path.clone()))
            .bind(("content", file_content(f)?))
            .await?
            .check()?;
        Ok(())
    }

    /// Look up a file record by path. `None` if not tracked — not an error.
    pub async fn get_file(&self, path: &str) -> Result<Option<FileRecord>> {
        let sql = format!("SELECT {FILE_FIELDS} FROM $rid");
        let rid = RecordId::new("file", path);
        let mut resp = self.db().query(sql).bind(("rid", rid)).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter()
            .next()
            .map(|row| serde_json::from_value(row).map_err(crate::Error::from))
            .transpose()
    }

    /// Every tracked file record.
    pub async fn all_files(&self) -> Result<Vec<FileRecord>> {
        let sql = format!("SELECT {FILE_FIELDS} FROM file");
        let mut resp = self.db().query(sql).await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter()
            .map(|row| serde_json::from_value(row).map_err(crate::Error::from))
            .collect()
    }

    /// Delete the file record for `path` and cascade: every node attributed to
    /// this file (which auto-removes every edge touching one of those nodes —
    /// see the module docs' cascade note), and every unresolved reference
    /// sourced from one of those nodes or denormalized to this file. Safe when
    /// the file/nodes do not exist (a no-op, never an error).
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        // The file's node ids, captured before deletion so the unresolved-ref
        // cleanup can match `fromNodeId` even where its denormalized `filePath`
        // diverged from the source node's file.
        let node_ids = self.node_ids_for_file(path).await?;

        // One round trip: drop the file's unresolved refs, then its nodes
        // (edges cascade), then the file row. `.check()` validates every
        // statement. DELETE over an empty set is a no-op.
        self.db()
            .query(
                "DELETE unresolved_ref WHERE filePath = $path OR fromNodeId IN $ids;\
                 DELETE node WHERE filePath = $path;\
                 DELETE file WHERE path = $path;",
            )
            .bind(("path", path.to_string()))
            .bind(("ids", node_ids))
            .await?
            .check()?;
        Ok(())
    }

    /// The most recent `indexedAt` across all tracked files, or `None` if no
    /// file has been indexed yet. Folded in Rust over the raw column (avoids a
    /// `GROUP ALL` aggregate that returns an ambiguous shape on an empty table).
    pub async fn last_indexed_at(&self) -> Result<Option<i64>> {
        let mut resp = self.db().query("SELECT VALUE indexedAt FROM file").await?;
        let values: Vec<i64> = resp.take(0)?;
        Ok(values.into_iter().max())
    }

    /// The distinct set of `language` values across all tracked files.
    pub async fn distinct_file_languages(&self) -> Result<BTreeSet<String>> {
        let mut resp = self.db().query("SELECT VALUE language FROM file").await?;
        let values: Vec<String> = resp.take(0)?;
        Ok(values.into_iter().collect())
    }

    /// Replace the entire extraction for `path` in one protocol pass, preserving
    /// cross-file incoming edges across the node-id churn a re-extraction
    /// causes (any line shift changes a node id). A port of CodeGraph's
    /// `storeExtractionResult` steps 2–8; see the module docs for the ordering /
    /// crash-safety contract and [`ReplaceStats`] for the returned counts.
    ///
    /// `nodes`/`edges`/`unresolved` are the freshly extracted rows for this
    /// file; `file_record` is written **last**. Nodes are pre-validated by the
    /// caller (the extractor); this method additionally skips any node missing a
    /// required field (`id`/`name`/`file_path`) silently, mirroring TS.
    pub async fn replace_file_extraction(
        &self,
        path: &str,
        nodes: &[Node],
        edges: &[Edge],
        unresolved: &[UnresolvedRef],
        file_record: &FileRecord,
    ) -> Result<ReplaceStats> {
        // (a) Snapshot cross-file incoming edges with their target (name, kind)
        // BEFORE deleting — deleting the file's nodes cascades these edges away.
        let snapshot = self.cross_file_incoming_with_target(path).await?;

        // (b) Delete the old extraction (nodes + cascaded edges + unresolved).
        self.delete_file(path).await?;

        // (c) Insert the re-extracted nodes (required-field validation: skip
        // any node with an empty id/name/filePath silently).
        let valid_nodes: Vec<Node> = nodes
            .iter()
            .filter(|n| !n.id.is_empty() && !n.name.is_empty() && !n.file_path.is_empty())
            .cloned()
            .collect();
        self.insert_nodes(&valid_nodes).await?;
        let nodes_inserted = valid_nodes.len() as u64;

        // (d) Insert the re-extracted edges (endpoint validation + dedup live in
        // insert_edges).
        let edges_inserted = self.insert_edges(edges).await?;

        // (e) Re-attach / resurrect / drop each snapshotted incoming edge.
        // Match against the just-inserted nodes by (name, kind); on ambiguity
        // pick the first by (start_line, id) — deterministic.
        let mut by_name_kind: HashMap<(&str, NodeKind), &Node> = HashMap::new();
        for n in &valid_nodes {
            by_name_kind
                .entry((n.name.as_str(), n.kind))
                .and_modify(|winner| {
                    if (n.start_line, n.id.as_str()) < (winner.start_line, winner.id.as_str()) {
                        *winner = n;
                    }
                })
                .or_insert(n);
        }

        let mut reattach_edges: Vec<Edge> = Vec::new();
        // (edge, refName, refKind) awaiting the source node's file/language.
        let mut resurrect_pending: Vec<(Edge, String, String)> = Vec::new();
        let mut incoming_dropped: u64 = 0;

        for (edge, target_name, target_kind) in &snapshot {
            if let Some(target) = by_name_kind.get(&(target_name.as_str(), *target_kind)) {
                reattach_edges.push(Edge {
                    target: target.id.clone(),
                    ..edge.clone()
                });
            } else if let Some((ref_name, ref_kind)) = ref_stamp(edge) {
                resurrect_pending.push((edge.clone(), ref_name, ref_kind));
            } else {
                incoming_dropped += 1;
            }
        }

        let incoming_reattached = self.insert_edges(&reattach_edges).await?;

        // Build resurrected unresolved refs, sourcing file/language from the
        // (surviving, cross-file) source node.
        let resurrected = self.build_resurrected(&resurrect_pending).await?;
        let incoming_resurrected = resurrected.len() as u64;
        self.insert_unresolved(&resurrected).await?;

        // (f) Insert the caller-supplied unresolved refs.
        self.insert_unresolved(unresolved).await?;

        // (g) Write the file row LAST (crash-safety — see the module docs).
        self.upsert_file(file_record).await?;

        Ok(ReplaceStats {
            nodes_inserted,
            edges_inserted,
            incoming_reattached,
            incoming_resurrected,
            incoming_dropped,
        })
    }

    /// The raw node ids (`Node.id` strings) attributed to `path`.
    async fn node_ids_for_file(&self, path: &str) -> Result<Vec<String>> {
        let mut resp = self
            .db()
            .query("SELECT VALUE record::id(id) FROM node WHERE filePath = $path")
            .bind(("path", path.to_string()))
            .await?;
        let ids: Vec<String> = resp.take(0)?;
        Ok(ids)
    }

    /// Turn each `(edge, refName, refKind)` into a pending [`UnresolvedRef`],
    /// denormalizing `file_path`/`language` from the edge's (surviving,
    /// cross-file) source node. A source that cannot be found falls back to the
    /// TS schema defaults (`file_path = ""`, `language = "unknown"`).
    async fn build_resurrected(
        &self,
        pending: &[(Edge, String, String)],
    ) -> Result<Vec<UnresolvedRef>> {
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        let mut source_ids: Vec<String> =
            pending.iter().map(|(e, _, _)| e.source.clone()).collect();
        source_ids.sort_unstable();
        source_ids.dedup();
        let sources = self.get_nodes(&source_ids).await?;

        Ok(pending
            .iter()
            .map(|(edge, ref_name, ref_kind)| {
                let (file_path, language) = sources
                    .get(&edge.source)
                    .map(|n| (n.file_path.clone(), n.language.clone()))
                    .unwrap_or_else(|| (String::new(), "unknown".to_string()));
                UnresolvedRef {
                    from_node_id: edge.source.clone(),
                    reference_name: ref_name.clone(),
                    reference_kind: ref_kind.clone(),
                    line: edge.line,
                    column: edge.column,
                    candidates: Vec::new(),
                    file_path,
                    language,
                    status: RefStatus::Pending,
                    name_tail: name_tail(ref_name),
                }
            })
            .collect())
    }
}
