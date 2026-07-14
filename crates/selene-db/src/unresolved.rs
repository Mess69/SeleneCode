//! Unresolved-reference CRUD: inherent methods on [`SurrealStore`] carrying
//! the unresolved-references section of [`crate::GraphStore`] (Task 7); the
//! trait impl in `src/store_impl.rs` delegates here (Task 10).
//!
//! ## Record identity
//!
//! `unresolved_ref` rows carry no natural single-field key: the trait's
//! [`crate::UnresolvedKey`] — `(from_node_id, reference_name, reference_kind)`,
//! used by [`SurrealStore::delete_resolved`]/[`SurrealStore::mark_failed`] — is
//! **not** declared UNIQUE, and must not be: the same triple recurs across call
//! sites on different lines, and resolving the name resolves all of them. Rows
//! therefore use SurrealDB's auto-generated record id (`CREATE unresolved_ref
//! CONTENT ..` with no explicit key); callers never see or need it, matching
//! [`crate::UnresolvedRef`] itself carrying no `id` field.
//!
//! **`reference_kind` is load-bearing in that key.** Extraction emits two rows
//! for one `(from_node_id, reference_name)` pair when a name is both called and
//! passed as a value in the same body (`register(handler); handler();` → a
//! `calls` row and a `function_ref` row, `selene-extract/src/fnref.rs`). Keyed
//! on the pair alone, resolving either row deleted BOTH — silent data loss that
//! nothing detected, because the pending count still reached 0 and the orphan
//! sweep was satisfied. `retryable_failed` deduped by the same pair, so the
//! second kind was unreachable through every door: 2 rows in, 1 row out.
//!
//! ## `insert_unresolved`: promoted from `src/files.rs` (Task 6)
//!
//! `src/files.rs`'s `replace_file_extraction` needed an unresolved-ref writer
//! before this task existed, so it grew a private `insert_unresolved_rows`
//! helper. That helper (and the `unresolved_content`/chunking it used) now
//! live here as the public [`SurrealStore::insert_unresolved`] the trait
//! requires; `src/files.rs` calls this method instead of duplicating it.
//! [`name_tail`] is promoted alongside it for the same reason:
//! `replace_file_extraction`'s resurrection path needs it to populate a
//! resurrected [`crate::UnresolvedRef`]'s `name_tail`.
//!
//! ## `retryable_failed`: reference_name OR name_tail, capped per name
//!
//! CodeGraph TS's `getRetryableFailedReferences` matches only `name_tail` and
//! *skips a whole name's group* once its failed-row count exceeds
//! `perNameCeiling` (rationale: past that population the name is
//! external/builtin noise like `get`/`map`, not a real retry candidate). This
//! port widens the match to `reference_name OR name_tail` (a bare, unqualified
//! failed reference's `reference_name` already equals its `name_tail`, but a
//! qualified one like `"Helper.M"` has `name_tail == "M"` while its
//! `reference_name` stays `"Helper.M"` — matching only `name_tail` would miss
//! it when the caller passes the qualified form) and **caps** rather than
//! *skips* each name's contribution at `per_name_ceiling` (`LIMIT` per
//! per-name statement, not a pre-count-and-exclude pass) — a deliberate,
//! documented deviation from strict TS parity per this task's brief. A row
//! that matches more than one queried name (e.g. `names = ["M", "Helper.M"]`
//! against a row with `reference_name = "Helper.M"`, `name_tail = "M"`) is
//! deduped by the full [`crate::UnresolvedKey`] across the per-name buckets —
//! **kind included**, so a name's `calls` and `function_ref` rows are both
//! returned rather than collapsed into one.
//!
//! ## Chunking
//!
//! Every batch operation chunks at [`CHUNK`] — mirrors `src/nodes.rs`.

use std::collections::HashSet;

use crate::util::{CHUNK, clamp_i64};
use crate::{RefStatus, Result, SurrealStore, UnresolvedRef};

/// Column list for every `unresolved_ref` read, in [`UnresolvedRef`]'s field
/// order. Unlike `node`/`file`, `UnresolvedRef` has no `id` field at all (see
/// the module docs' "Record identity" section), so no `record::id` bridge is
/// needed here.
const UNRESOLVED_FIELDS: &str = "fromNodeId, referenceName, referenceKind, line, column, \
     candidates, filePath, language, status, nameTail";

/// The `name_tail` of a (possibly qualified) reference name: the last
/// `.`-separated segment (`"Helper.M"` → `"M"`, `"bare"` → `"bare"`).
///
/// `pub(crate)`: promoted from a private `src/files.rs` helper (Task 6) so
/// `replace_file_extraction`'s resurrection path (`src/files.rs`) and this
/// module share one implementation instead of two.
pub(crate) fn name_tail(reference_name: &str) -> String {
    reference_name
        .rsplit('.')
        .next()
        .unwrap_or(reference_name)
        .to_string()
}

/// Serializes an [`UnresolvedRef`] for storage, dropping any `null`-valued
/// key. `line`/`column` are `Option<u32>` **without**
/// `skip_serializing_if`, so a `None` serializes to JSON `null`; the
/// SCHEMAFULL `option<int>` columns accept only absent/`NONE`, never `NULL`
/// (the same fold `src/nodes.rs`'s `node_content`/`src/edges.rs`'s
/// `edge_content` apply).
fn unresolved_content(r: &UnresolvedRef) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(r)?;
    if let serde_json::Value::Object(map) = &mut value {
        map.retain(|_, v| !v.is_null());
    }
    Ok(value)
}

/// Decodes one [`UNRESOLVED_FIELDS`] row into an [`UnresolvedRef`].
fn row_to_unresolved(row: serde_json::Value) -> Result<UnresolvedRef> {
    serde_json::from_value(row).map_err(crate::Error::from)
}

impl SurrealStore {
    /// Insert `refs` (typically as [`crate::RefStatus::Pending`]), chunked at
    /// `CHUNK`. Promoted from `src/files.rs`'s private
    /// `insert_unresolved_rows` (Task 6) — see the module docs.
    pub async fn insert_unresolved(&self, refs: &[UnresolvedRef]) -> Result<()> {
        for chunk in refs.chunks(CHUNK) {
            let rows = chunk
                .iter()
                .map(unresolved_content)
                .collect::<Result<Vec<_>>>()?;
            // **`INSERT INTO t $rows`, not `FOR $r IN $rows { CREATE ... }`.** The FOR form runs
            // one CREATE per row inside the engine; on django's 52 358 refs it measured 3.9 s. A
            // single INSERT hands the engine the whole chunk. Same rows, same order.
            self.db()
                .query("INSERT INTO unresolved_ref $rows;")
                .bind(("rows", rows))
                .await?
                .check()?;
        }
        Ok(())
    }

    /// Count of refs currently [`crate::RefStatus::Pending`]. Failed rows are
    /// excluded — they are retry candidates ([`Self::retryable_failed`]), not
    /// outstanding work.
    pub async fn unresolved_pending_count(&self) -> Result<u64> {
        let mut resp = self
            .db()
            .query("SELECT count() FROM unresolved_ref WHERE status = $status GROUP ALL")
            .bind(("status", RefStatus::Pending.as_str()))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        Ok(rows.first().and_then(|r| r["count"].as_u64()).unwrap_or(0))
    }

    /// A page of [`crate::RefStatus::Pending`] refs, ordered deterministically by
    /// `(fromNodeId, referenceName, referenceKind)` — the documented order
    /// key stable batching depends on — with the record id as the final
    /// tiebreak, so rows tied on the whole key still page stably (no row
    /// lost or duplicated across pages).
    pub async fn unresolved_pending_batch(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<UnresolvedRef>> {
        let sql = format!(
            "SELECT {UNRESOLVED_FIELDS}, id FROM unresolved_ref WHERE status = $status \
             ORDER BY fromNodeId, referenceName, referenceKind, id \
             LIMIT $limit START $offset"
        );
        let mut resp = self
            .db()
            .query(sql)
            .bind(("status", RefStatus::Pending.as_str()))
            .bind(("limit", clamp_i64(limit)))
            .bind(("offset", clamp_i64(offset)))
            .await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;
        rows.into_iter().map(row_to_unresolved).collect()
    }

    /// Every [`crate::RefStatus::Pending`] ref whose `file_path` is in `paths`,
    /// chunked at `CHUNK` per IN-list round trip.
    pub async fn unresolved_by_files(&self, paths: &[String]) -> Result<Vec<UnresolvedRef>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for chunk in paths.chunks(CHUNK) {
            let sql = format!(
                "SELECT {UNRESOLVED_FIELDS} FROM unresolved_ref \
                 WHERE status = $status AND filePath IN $paths"
            );
            let mut resp = self
                .db()
                .query(sql)
                .bind(("status", RefStatus::Pending.as_str()))
                .bind(("paths", chunk.to_vec()))
                .await?;
            let rows: Vec<serde_json::Value> = resp.take(0)?;
            for row in rows {
                out.push(row_to_unresolved(row)?);
            }
        }
        Ok(out)
    }

    /// Delete refs matching [`crate::UnresolvedKey`] keys, chunked at `CHUNK`.
    /// One `DELETE ... WHERE fromNodeId = $fromN AND referenceName = $nameN AND
    /// referenceKind = $kindN` statement per key, combined into a single
    /// multi-statement query per chunk (SurrealQL has no composite-tuple
    /// `IN` list).
    ///
    /// The `referenceKind` predicate is what keeps a resolved `calls` row from
    /// deleting its `function_ref` twin — see [`crate::UnresolvedKey`].
    /// **Rewrite the whole pending queue in two statements**, replacing 52 358 keyed writes.
    ///
    /// After a resolve pass, every row that was `Pending` has been decided: it either produced an
    /// edge (and must go) or it did not (and must be marked failed, so the next pass does not
    /// retry it). The old shape asked the database to reach that state one key at a time —
    /// `delete_resolved` (28 176 keys, **17.3 s**) plus `mark_failed` (24 178 keys, **6.7 s**) —
    /// because the batch loop *drained* the queue to advance, so each batch had to mutate it.
    ///
    /// Once the loop pages by `START offset` instead of draining, the queue only has to reach its
    /// final state ONCE, and its final state is expressible directly: **drop every pending row,
    /// re-insert the failed ones as failed.** Two statements.
    ///
    /// ⚠ **This is only equivalent if every pending row was processed in this pass** — which is
    /// what the offset walk guarantees, and why the two must land together. Rows already marked
    /// `failed` by an earlier pass are not `Pending`, so the DELETE does not touch them.
    ///
    /// ⚠ **#760 lives here.** `failed` must carry the rows as STORED. A resolver that mutated
    /// `reference_name` used to make the keyed delete match nothing and the drain loop spin
    /// forever (5M edges / 1.4 GB in the TS build). It can no longer hang — the DELETE is
    /// unconditional — but a mutated row would be re-inserted under the wrong key, so the
    /// invariant is unchanged: **pass the stored rows.**
    pub async fn replace_pending_with_failed(&self, failed: &[UnresolvedRef]) -> Result<()> {
        self.db()
            .query("DELETE unresolved_ref WHERE status = $status;")
            .bind(("status", RefStatus::Pending.as_str()))
            .await?
            .check()?;

        let marked: Vec<UnresolvedRef> = failed
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.status = RefStatus::Failed;
                r
            })
            .collect();
        self.insert_unresolved(&marked).await
    }

    pub async fn delete_resolved(&self, keys: &[crate::UnresolvedKey]) -> Result<()> {
        self.run_keyed_statements(
            keys,
            "DELETE unresolved_ref WHERE fromNodeId = $from{i} AND referenceName = $name{i} \
             AND referenceKind = $kind{i};",
            &[],
        )
        .await
    }

    /// Flip refs matching [`crate::UnresolvedKey`] keys to
    /// [`crate::RefStatus::Failed`] (kept for [`Self::retryable_failed`] rather than
    /// deleted). Same chunked multi-statement shape as [`Self::delete_resolved`],
    /// and kind-scoped for the same reason.
    pub async fn mark_failed(&self, keys: &[crate::UnresolvedKey]) -> Result<()> {
        self.run_keyed_statements(
            keys,
            "UPDATE unresolved_ref SET status = $status \
             WHERE fromNodeId = $from{i} AND referenceName = $name{i} \
             AND referenceKind = $kind{i};",
            &[("status", RefStatus::Failed.as_str())],
        )
        .await
    }

    /// [`crate::RefStatus::Failed`] refs whose `reference_name` OR `name_tail`
    /// matches any of `names`, capped at `per_name_ceiling` entries per
    /// queried name — see the module docs for the TS-parity deviation this
    /// implements (widened match field, cap instead of skip).
    pub async fn retryable_failed(
        &self,
        names: &[String],
        per_name_ceiling: usize,
    ) -> Result<Vec<UnresolvedRef>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut dedup_names: Vec<String> = names.to_vec();
        dedup_names.sort_unstable();
        dedup_names.dedup();

        let cap = clamp_i64(per_name_ceiling);
        let mut out = Vec::new();
        let mut seen: HashSet<crate::UnresolvedKey> = HashSet::new();
        for chunk in dedup_names.chunks(CHUNK) {
            let mut sql = String::new();
            for i in 0..chunk.len() {
                sql.push_str(&format!(
                    "SELECT {UNRESOLVED_FIELDS} FROM unresolved_ref \
                     WHERE status = $status AND (referenceName = $n{i} OR nameTail = $n{i}) \
                     LIMIT $cap{i};"
                ));
            }
            let mut query = self
                .db()
                .query(sql)
                .bind(("status", RefStatus::Failed.as_str()));
            for (i, name) in chunk.iter().enumerate() {
                query = query
                    .bind((format!("n{i}"), name.clone()))
                    .bind((format!("cap{i}"), cap));
            }
            let mut resp = query.await?;
            for i in 0..chunk.len() {
                let rows: Vec<serde_json::Value> = resp.take(i)?;
                for row in rows {
                    let r = row_to_unresolved(row)?;
                    let key = (
                        r.from_node_id.clone(),
                        r.reference_name.clone(),
                        r.reference_kind.clone(),
                    );
                    if seen.insert(key) {
                        out.push(r);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Delete every unresolved ref (full re-index discard).
    pub async fn clear_unresolved(&self) -> Result<()> {
        self.db().query("DELETE unresolved_ref").await?.check()?;
        Ok(())
    }

    /// Shared tail of [`Self::delete_resolved`]/[`Self::mark_failed`]: run one
    /// `template` statement per [`crate::UnresolvedKey`], `{i}` substituted with
    /// the key's index, all statements in one chunk combined into a single round
    /// trip. `extra_binds` are bound once per chunk query (e.g. `mark_failed`'s
    /// `$status`), on top of the per-key `$from{i}`/`$name{i}`/`$kind{i}` binds.
    /// Empty `keys` is a no-op (zero queries).
    async fn run_keyed_statements(
        &self,
        keys: &[crate::UnresolvedKey],
        template: &str,
        extra_binds: &[(&'static str, &'static str)],
    ) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        for chunk in keys.chunks(CHUNK) {
            let mut sql = String::new();
            for i in 0..chunk.len() {
                sql.push_str(&template.replace("{i}", &i.to_string()));
            }
            let mut query = self.db().query(sql);
            for (key, value) in extra_binds {
                query = query.bind((*key, *value));
            }
            for (i, (from, name, kind)) in chunk.iter().enumerate() {
                query = query
                    .bind((format!("from{i}"), from.clone()))
                    .bind((format!("name{i}"), name.clone()))
                    .bind((format!("kind{i}"), kind.clone()));
            }
            query.await?.check()?;
        }
        Ok(())
    }
}
