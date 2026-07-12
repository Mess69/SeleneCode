//! Search candidate fetch: inherent methods on [`SurrealStore`] mirroring the
//! search-candidates section of [`crate::GraphStore`] (Task 7). `impl
//! GraphStore for SurrealStore` is wired later (Task 10); until then these
//! are plain inherent `async fn`s with identical signatures. Final ranking
//! (kind/path/name-match bonuses, blending FTS with LIKE/fuzzy candidates)
//! stays out of this crate — see the trait's "search candidates are
//! unranked" contract doc.
//!
//! ## `search_fts` statement shape
//!
//! `src/schema.rs` defines one single-column `FULLTEXT` index per searchable
//! field (`name`, `qualifiedName`, `docstring`, `signature` — SurrealDB caps
//! a `FULLTEXT` index at one column, Task 1 finding), so one query ORs four
//! `@N@` match predicates, one per field, all against the same search string
//! `$q`:
//!
//! ```sql
//! SELECT <NODE_FIELDS>,
//!        (20 * (search::score(0) ?? 0)) + (5 * (search::score(1) ?? 0))
//!      + (1 * (search::score(2) ?? 0)) + (2 * (search::score(3) ?? 0)) AS rawScore
//! FROM node
//! WHERE (name @0@ $q OR qualifiedName @1@ $q OR docstring @2@ $q OR signature @3@ $q)
//!   [AND kind IN $kinds] [AND language IN $languages]
//! ORDER BY rawScore DESC LIMIT $limit START $offset
//! ```
//!
//! ### Score expression: BM25 `(0, 20, 5, 1, 2)` weight intent, additive
//!
//! CodeGraph TS calls SQLite `bm25(nodes_fts, 0, 20, 5, 1, 2)` over its FTS5
//! table's column order `(id, name, qualified_name, docstring, signature)` —
//! weight `0` (unweighted `id`), `20` (`name`), `5` (`qualified_name`), `1`
//! (`docstring`), `2` (`signature`). This port has no `id` column to weight
//! and one independent BM25 score per field (four single-column indexes, not
//! one five-column virtual table), so the TS single `bm25()` call becomes an
//! explicit weighted sum of the four `search::score(N)` calls, same weights:
//! `name=20, qualifiedName=5, docstring=1, signature=2`.
//!
//! ### `?? 0`: coalescing `NONE` before it poisons the sum
//!
//! `search::score(N)` resolves per-row via the query executor's per-match-ref
//! doc-id lookup (verified against the `surrealdb-core` 3.2.1 source,
//! `idx/planner/executor.rs::score`): a row with no value at all for field N
//! (e.g. `docstring` is `NONE` on most nodes) never gets a doc id in that
//! field's index, so `search::score(N)` returns `NONE`, not `0.0`. Multiplying
//! `NONE` into an additive sum would poison the whole `rawScore` to `NONE`
//! for every row that doesn't populate all four fields — i.e. almost every
//! row. `?? 0` (SurrealDB's null-coalescing operator, already load-bearing in
//! `src/schema.rs`'s `lineKey`/`colKey` computed fields) folds the miss to
//! `0` before multiplying, so a field with no value/no match contributes
//! nothing instead of erasing the row's score.
//!
//! ### Known deviation vs TS: no prefix-OR, single joined match string
//!
//! CodeGraph TS builds its FTS5 `MATCH` string as `"term1"* OR "term2"* OR
//! ...` — an explicit prefix-match OR-chain per term. SurrealDB's `@N@` match
//! operator has no prefix-match syntax; the brief's resolution (confirmed
//! against the `identifier` analyzer, `TOKENIZERS class, camel FILTERS
//! lowercase, ascii`) is to bind all of `terms` **joined by a single space**
//! as one match string per field — the BM25 analyzer tokenizes the joined
//! string and scores by term/document frequency across those tokens, which is
//! a materially different (and typically *more* permissive on multi-term
//! queries, less permissive on partial-word prefixes) recall shape than TS's
//! explicit per-term prefix-OR. Documented here as a **known deviation**, not
//! silently absorbed.
//!
//! ### Errors are swallowed, never propagated
//!
//! Per the trait contract ("malformed or empty `terms` yields an empty
//! result, never an error") and the TS reference behavior ("FTS errors →
//! empty", `db-graph-search.md`): empty/blank `terms` short-circuits before
//! any query runs, and both `query().await` failing outright (a parse-time
//! error — Task 1 spike finding: some SurrealQL errors surface here, not at
//! `take`) and `.take(0)` failing (a runtime error) are caught and mapped to
//! `Ok(vec![])` rather than propagated. A genuine row-decode failure *after* a
//! successful fetch (a store-internal consistency error, not a "bad search
//! input" case) still propagates via `?` — that's `Error::Decode`/`Error::Serde`
//! territory, not the FTS-swallow contract.
//!
//! ### `raw_score`: rank-only, may be near-zero or tiny
//!
//! BM25 can go negative on a tiny/skewed corpus (Task 1 finding). This port
//! does **not** `abs()` the way TS does (TS: `score = abs(bm25)`) — the
//! weighted-sum expression is already a different scale than raw BM25, and
//! `abs()` would hide a genuinely negative (poor/anti-)match behind a
//! positive number instead of letting `ORDER BY rawScore DESC` rank it
//! correctly. Callers must treat `raw_score` as rank-only, per the trait's
//! `SearchCandidate` doc, exactly as before.
//!
//! ## `search_name_like`: fetch candidates, score in Rust
//!
//! One `WHERE nameLower CONTAINS $q OR string::contains(string::lowercase(qualifiedName),
//! $q)` fetch (optionally kind-filtered), then the exact/starts/contains/
//! qualified-contains/else CASE tiers are computed in Rust per row and used to
//! sort + truncate to `limit`. Chosen over a SurrealQL `IF/ELSE` CASE
//! expression because `N` here is bounded by the `CONTAINS` prefilter (never
//! the whole table) and the plain-Rust version is easier to verify against
//! the exact TS tier values — correctness first, per the task brief.
//!
//! ## `find_by_exact_names`: one multi-statement round trip
//!
//! One `SELECT ... WHERE nameLower = $nN LIMIT $capN;` statement per (deduped)
//! name, all combined into a single query per [`CHUNK`] — the same
//! multi-statement-batch shape `src/unresolved.rs`'s keyed-statement helpers
//! use, since SurrealQL has no per-key-capped composite batch primitive. Results are deduped by node id
//! across per-name statements (two queried names can both match the same
//! node only if names collide case-insensitively, but the dedup is cheap
//! insurance either way).
//!
//! ## `all_node_names`: `SELECT VALUE`, dedup in Rust
//!
//! `SELECT DISTINCT name FROM node` is avoided — Task 5 found `SELECT
//! DISTINCT <field>` parser quirks on this SurrealDB version elsewhere in
//! this crate (`src/edges.rs`'s file-projection queries), and
//! `src/files.rs`'s `distinct_file_languages` already established the
//! `SELECT VALUE` + Rust-side `sort_unstable`+`dedup` pattern as this crate's
//! house style for "distinct" reads.

use selene_core::{Node, NodeKind};

use crate::nodes::NODE_FIELDS;
use crate::{Result, SearchCandidate, SurrealStore};

/// Names/keys processed per round trip. Mirrors `src/nodes.rs`'s `CHUNK`.
const CHUNK: usize = 500;

impl SurrealStore {
    /// Full-text candidate fetch. See the module docs for the statement
    /// shape, the weighted-sum score expression, the `?? 0` NONE-coalescing,
    /// and the documented prefix-match deviation from TS. Empty/blank `terms`
    /// and any FTS query failure both yield `Ok(vec![])`, never `Err`.
    pub async fn search_fts(
        &self,
        terms: &[String],
        kinds: &[NodeKind],
        languages: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchCandidate>> {
        let q = terms
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if q.is_empty() {
            return Ok(Vec::new());
        }

        let mut sql = format!(
            "SELECT {NODE_FIELDS}, \
             (20 * (search::score(0) ?? 0)) + (5 * (search::score(1) ?? 0)) + \
             (1 * (search::score(2) ?? 0)) + (2 * (search::score(3) ?? 0)) AS rawScore \
             FROM node WHERE (name @0@ $q OR qualifiedName @1@ $q OR docstring @2@ $q \
             OR signature @3@ $q)"
        );
        if !kinds.is_empty() {
            sql.push_str(" AND kind IN $kinds");
        }
        if !languages.is_empty() {
            sql.push_str(" AND language IN $languages");
        }
        sql.push_str(" ORDER BY rawScore DESC LIMIT $limit START $offset");

        let mut query = self.db().query(sql).bind(("q", q));
        if !kinds.is_empty() {
            query = query.bind(("kinds", kind_strings(kinds)));
        }
        if !languages.is_empty() {
            query = query.bind(("languages", languages.to_vec()));
        }
        query = query
            .bind(("limit", clamp_i64(limit)))
            .bind(("offset", clamp_i64(offset)));

        // Malformed/failing FTS queries never propagate — see the module docs.
        let mut resp = match query.await {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        let rows: std::result::Result<Vec<serde_json::Value>, surrealdb::Error> = resp.take(0);
        let rows = match rows {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        rows.into_iter().map(row_to_candidate).collect()
    }

    /// LIKE-style fallback candidate fetch (exact/prefix/contains/qualified/
    /// else tiers, computed in Rust — see the module docs). `kinds` empty
    /// means no filter; blank `q` yields `Ok(vec![])`, zero queries.
    pub async fn search_name_like(
        &self,
        q: &str,
        kinds: &[NodeKind],
        limit: usize,
    ) -> Result<Vec<SearchCandidate>> {
        let q_lower = q.trim().to_lowercase();
        if q_lower.is_empty() {
            return Ok(Vec::new());
        }

        let mut sql = format!(
            "SELECT {NODE_FIELDS} FROM node WHERE \
             (nameLower CONTAINS $q OR string::contains(string::lowercase(qualifiedName), $q))"
        );
        if !kinds.is_empty() {
            sql.push_str(" AND kind IN $kinds");
        }

        let mut query = self.db().query(sql).bind(("q", q_lower.clone()));
        if !kinds.is_empty() {
            query = query.bind(("kinds", kind_strings(kinds)));
        }
        let mut resp = query.await?;
        let rows: Vec<serde_json::Value> = resp.take(0)?;

        let mut candidates = rows
            .into_iter()
            .map(|row| {
                let node: Node = serde_json::from_value(row)?;
                let raw_score = like_tier_score(&node, &q_lower);
                Ok(SearchCandidate { node, raw_score })
            })
            .collect::<Result<Vec<_>>>()?;

        // ORDER BY score DESC, then shorter name first (TS: `length(name) ASC`).
        candidates.sort_by(|a, b| {
            b.raw_score
                .partial_cmp(&a.raw_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.node.name.len().cmp(&b.node.name.len()))
        });
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// Exact-name lookup across multiple names in one round trip, capped at
    /// `per_name_limit` results per (case-insensitive) name. See the module
    /// docs for the multi-statement batch shape.
    pub async fn find_by_exact_names(
        &self,
        names: &[String],
        per_name_limit: usize,
    ) -> Result<Vec<Node>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut dedup_names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        dedup_names.sort_unstable();
        dedup_names.dedup();

        let cap = clamp_i64(per_name_limit);
        let mut out = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        for chunk in dedup_names.chunks(CHUNK) {
            let mut sql = String::new();
            for i in 0..chunk.len() {
                sql.push_str(&format!(
                    "SELECT {NODE_FIELDS} FROM node WHERE nameLower = $n{i} LIMIT $cap{i};"
                ));
            }
            let mut query = self.db().query(sql);
            for (i, name) in chunk.iter().enumerate() {
                query = query
                    .bind((format!("n{i}"), name.clone()))
                    .bind((format!("cap{i}"), cap));
            }
            let mut resp = query.await?;
            for i in 0..chunk.len() {
                let rows: Vec<serde_json::Value> = resp.take(i)?;
                for row in rows {
                    let node: Node = serde_json::from_value(row)?;
                    if seen_ids.insert(node.id.clone()) {
                        out.push(node);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Every distinct node name — see the module docs for why `SELECT VALUE`
    /// + Rust-side dedup is used instead of `SELECT DISTINCT`.
    pub async fn all_node_names(&self) -> Result<Vec<String>> {
        let mut resp = self.db().query("SELECT VALUE name FROM node").await?;
        let mut names: Vec<String> = resp.take(0)?;
        names.sort_unstable();
        names.dedup();
        Ok(names)
    }
}

/// [`NodeKind`] wire strings for an `IN`-list bind.
fn kind_strings(kinds: &[NodeKind]) -> Vec<String> {
    kinds.iter().map(|k| k.as_str().to_string()).collect()
}

/// `usize` → `i64`, saturating at `i64::MAX` (mirrors `src/nodes.rs`'s
/// `get_nodes_by_name_prefix` limit-binding pattern).
fn clamp_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Decodes one `search_fts` row into a [`SearchCandidate`]. `Node`'s
/// `Deserialize` has no `deny_unknown_fields`, so the extra `rawScore` column
/// alongside [`NODE_FIELDS`] is simply ignored when building the `Node`; it's
/// read out separately for `raw_score`.
fn row_to_candidate(row: serde_json::Value) -> Result<SearchCandidate> {
    let raw_score = row
        .get("rawScore")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let node: Node = serde_json::from_value(row)?;
    Ok(SearchCandidate { node, raw_score })
}

/// The LIKE-fallback CASE tier for `node` against pre-lowercased `q_lower`:
/// exact `1.0` / starts-with `0.9` / name-contains `0.8` /
/// qualified-name-contains `0.7` / else `0.5` (ports CodeGraph TS's
/// `searchNodes` LIKE-branch scoring verbatim).
fn like_tier_score(node: &Node, q_lower: &str) -> f64 {
    let name_lower = node.name.to_lowercase();
    if name_lower == q_lower {
        1.0
    } else if name_lower.starts_with(q_lower) {
        0.9
    } else if name_lower.contains(q_lower) {
        0.8
    } else if node.qualified_name.to_lowercase().contains(q_lower) {
        0.7
    } else {
        0.5
    }
}
