#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Spike smoke-test — validates every SurrealQL assumption the `selene-db`
//! design rests on, against the real `surrealdb = "3.2"` crate (resolves to
//! 3.2.1) with the embedded in-memory engine (`kv-mem`).
//!
//! This file is throwaway *knowledge*, kept as a regression smoke test: if a
//! future SurrealDB bump breaks one of these primitives, a spike test goes red
//! before the production query built on top of it does. Every assertion runs
//! against an actual DB response — no `println` debugging survives here.
//!
//! ## What the brief sketched vs. what SurrealDB 3.2 actually accepts
//!
//! | # | Assumption in the brief                              | 3.2 reality (see the test that proves it)                                     |
//! |---|------------------------------------------------------|-------------------------------------------------------------------------------|
//! | 2 | `type::thing('node', 'x')` to address a record       | **Renamed to `type::record('node', 'x')`** — `type::thing` is a parse error.  |
//! | 4 | unique index over `(in, out, line, col)`             | Works; `in`/`out` usable **unquoted** as index columns. Violation surfaces at `resp.take(n)`, not at `query().await`. |
//! | 5 | `SELECT @.{1..2}(->calls->node)`                      | Recursive **idiom** form `start.{lo..hi+collect}(path)`; the `.{+collect}` separate-dot form is a parse error. |
//! | 6 | `DEFINE INDEX … FULLTEXT` on `name`/`docstring`      | Keyword is `FULLTEXT ANALYZER … BM25` (3.0 renamed the old `SEARCH ANALYZER`); a FULLTEXT index takes **exactly one column** — one index per field. Match operator is `@N@`, tied to `search::score(N)` / `search::highlight(pre, post, N)`. |
//!
//! Rust SDK note: the embedded engine is `surrealdb::engine::local::Mem`,
//! constructed with `Surreal::new::<Mem>(())` (empty tuple = no path). A
//! `db.query(sql).await` returning `Ok` does **not** mean every statement
//! succeeded — per-statement errors are unwrapped by `Response::take(idx)`.

use serde_json::Value;
use surrealdb::Surreal;
use surrealdb::engine::local::{Db, Mem};

/// A fresh embedded in-memory DB, namespace/db already selected.
async fn mem_db() -> Surreal<Db> {
    let db = Surreal::new::<Mem>(()).await.unwrap();
    db.use_ns("selene").use_db("graph").await.unwrap();
    db
}

/// Brief #1 — `use_ns("selene").use_db("graph")` selects the working scope.
///
/// The chained `.use_ns(..).use_db(..)` returns a single awaitable; awaiting it
/// resolves `Ok(())`. A `CREATE` afterwards proves the scope is actually
/// selected (without it, an unscoped write errors with "no namespace/database").
#[tokio::test(flavor = "multi_thread")]
async fn assertion_1_use_ns_db_selects_scope() {
    let db = Surreal::new::<Mem>(()).await.unwrap();
    db.use_ns("selene").use_db("graph").await.unwrap();

    // Proof the scope took: a write into it succeeds and reads back.
    let created: Option<Value> = db
        .create(("marker", "ok"))
        .content(serde_json::json!({ "v": 1 }))
        .await
        .unwrap();
    assert!(
        created.is_some(),
        "write into the selected ns/db must succeed"
    );
}

/// Brief #2 — a `node` record whose key contains `:` round-trips.
///
/// Selene node ids look like `function:<hash>`; the colon is significant. The
/// Rust `create(("node", "function:abc123"))` tuple form takes the raw string
/// as the record-id key and escapes it for us. In responses the id serializes
/// as `node:` + a **backtick-quoted** segment (`node:`function:abc123``), NOT
/// the `⟨…⟩` angle-bracket form the brief guessed — that is only the display
/// form in some CLI contexts.
///
/// To address such a record from raw SurrealQL, use `type::record('node', 'k')`
/// — the brief's `type::thing(...)` was **renamed** in 3.x and is now a parse
/// error ("Invalid function/constant path, did you maybe mean `type::record`").
#[tokio::test(flavor = "multi_thread")]
async fn assertion_2_colon_in_record_key_roundtrips() {
    let db = mem_db().await;

    let created: Option<Value> = db
        .create(("node", "function:abc123"))
        .content(serde_json::json!({ "name": "calculateTotal" }))
        .await
        .unwrap();
    let created = created.expect("create returns the new record");
    assert_eq!(created["name"], "calculateTotal");
    // The colon key is preserved verbatim, and the id serializes exactly as
    // `node:` + backtick-quoted key (the form the doc comment above claims).
    assert_eq!(
        created["id"], "node:`function:abc123`",
        "id must serialize as node:`function:abc123`"
    );

    // Read back through the typed API by the same key.
    let read: Option<Value> = db.select(("node", "function:abc123")).await.unwrap();
    assert_eq!(read.unwrap()["name"], "calculateTotal");

    // Read back through raw SurrealQL — this is where `type::record` (not the
    // old `type::thing`) is required to build the record id from strings.
    let mut resp = db
        .query("SELECT name FROM type::record('node', 'function:abc123')")
        .await
        .unwrap();
    let rows: Vec<Value> = resp.take(0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "calculateTotal");
}

/// Brief #3 — `RELATE a->calls->b` with edge fields; `->calls->node` reaches b.
///
/// `RELATE @from->calls->@to SET line = .., col = .., provenance = ..` creates a
/// row in the `calls` edge table carrying the auto `in`/`out` endpoint links
/// plus our arbitrary fields. The graph idiom `->calls->node` from `a` returns
/// the set of `node` records reached across `calls` edges.
#[tokio::test(flavor = "multi_thread")]
async fn assertion_3_relate_and_traverse() {
    let db = mem_db().await;
    let _: Option<Value> = db
        .create(("node", "a"))
        .content(serde_json::json!({"name":"a"}))
        .await
        .unwrap();
    let _: Option<Value> = db
        .create(("node", "b"))
        .content(serde_json::json!({"name":"b"}))
        .await
        .unwrap();

    let mut resp = db
        .query("RELATE node:a->calls->node:b SET line = 10, col = 5, provenance = 'tree-sitter'")
        .await
        .unwrap();
    let edges: Vec<Value> = resp.take(0).unwrap();
    assert_eq!(edges.len(), 1, "one edge created");
    let edge = &edges[0];
    // Endpoints land in the reserved `in`/`out` fields; our SET fields persist.
    assert_eq!(edge["in"], "node:a");
    assert_eq!(edge["out"], "node:b");
    assert_eq!(edge["line"], 10);
    assert_eq!(edge["col"], 5);
    assert_eq!(edge["provenance"], "tree-sitter");

    // `->calls->node` yields the reached target(s).
    let mut resp = db
        .query("SELECT ->calls->node AS targets FROM node:a")
        .await
        .unwrap();
    let rows: Vec<Value> = resp.take(0).unwrap();
    let targets = rows[0]["targets"].as_array().unwrap();
    assert_eq!(targets, &vec![Value::from("node:b")]);
}

/// Brief #4 — a UNIQUE index over `(in, out, line, col)` dedupes edges.
///
/// `DEFINE INDEX … ON TABLE calls COLUMNS in, out, line, col UNIQUE` — the
/// reserved edge fields `in`/`out` are accepted **unquoted** as index columns.
/// A second RELATE with an identical `(in,out,line,col)` tuple is rejected; a
/// RELATE differing only in `line` is accepted (distinct call sites coexist).
///
/// Deviation worth baking into production error handling: the uniqueness
/// violation does **not** fail `db.query(..).await` — that resolves `Ok`. The
/// error is per-statement and surfaces only when you unwrap it with
/// `Response::take(idx)`. Production code must inspect each statement, not just
/// the outer `Result`.
#[tokio::test(flavor = "multi_thread")]
async fn assertion_4_unique_index_dedupes_edges() {
    let db = mem_db().await;
    let _: Option<Value> = db
        .create(("node", "a"))
        .content(serde_json::json!({"name":"a"}))
        .await
        .unwrap();
    let _: Option<Value> = db
        .create(("node", "b"))
        .content(serde_json::json!({"name":"b"}))
        .await
        .unwrap();

    db.query("DEFINE INDEX calls_unique ON TABLE calls COLUMNS in, out, line, col UNIQUE")
        .await
        .unwrap();

    // First edge: accepted.
    let mut resp = db
        .query("RELATE node:a->calls->node:b SET line = 10, col = 5")
        .await
        .unwrap();
    let first: Vec<Value> = resp.take(0).unwrap();
    assert_eq!(first.len(), 1);

    // Identical (in,out,line,col): the outer future is Ok, but take(0) errors.
    let mut resp = db
        .query("RELATE node:a->calls->node:b SET line = 10, col = 5")
        .await
        .unwrap();
    let dup: Result<Vec<Value>, surrealdb::Error> = resp.take(0);
    assert!(dup.is_err(), "duplicate edge must violate the unique index");
    assert!(
        dup.unwrap_err().to_string().contains("calls_unique"),
        "error should name the violated index"
    );

    // Differ only in `line`: a distinct call site, accepted.
    let mut resp = db
        .query("RELATE node:a->calls->node:b SET line = 11, col = 5")
        .await
        .unwrap();
    let ok: Vec<Value> = resp.take(0).unwrap();
    assert_eq!(ok.len(), 1, "line-differing edge must be accepted");

    // Exactly two edges persisted (line 10 and line 11).
    let mut resp = db
        .query("SELECT count() FROM calls GROUP ALL")
        .await
        .unwrap();
    let counted: Vec<Value> = resp.take(0).unwrap();
    assert_eq!(counted[0]["count"], 2);
}

/// Brief #5 — depth-limited recursive traversal reaches c but not d.
///
/// Chain: `a -> b -> c -> d`. The brief sketched `SELECT @.{1..2}(->calls->node)`;
/// what SurrealDB 3.2 accepts is the **recursive idiom** on a starting record:
///
/// ```surql
/// RETURN node:a.{1..2+collect}(->calls->node)
/// ```
///
/// - `{lo..hi}` bounds the depth (hops); `{n}` is exactly n; `{lo..}` unbounded.
/// - `+collect` (written **inside** the braces, `{1..2+collect}`) flattens every
///   node visited across all depths into one deduped array. The brief's guess of
///   a separate `.{+collect}` step is a parse error ("Unexpected token `+`").
/// - Depth counts hops from the start: depth 1 = b, 2 = c, 3 = d. So `{1..2}`
///   collects `[b, c]` — c is reached, d is not.
///
/// The companion `{1..3+collect}` assertion proves d is excluded by the *depth
/// limit*, not because it is unreachable.
#[tokio::test(flavor = "multi_thread")]
async fn assertion_5_depth_limited_recursion() {
    let db = mem_db().await;
    for id in ["a", "b", "c", "d"] {
        let _: Option<Value> = db
            .create(("node", id))
            .content(serde_json::json!({ "name": id }))
            .await
            .unwrap();
    }
    db.query("RELATE node:a->calls->node:b SET line = 1, col = 1")
        .await
        .unwrap();
    db.query("RELATE node:b->calls->node:c SET line = 1, col = 1")
        .await
        .unwrap();
    db.query("RELATE node:c->calls->node:d SET line = 1, col = 1")
        .await
        .unwrap();

    // Deserialize as `Vec<Value>`, not `Vec<String>`: `+collect` yields
    // `RecordId`s, and `take::<Vec<String>>` rejects them ("Expected string,
    // got record"). A `RecordId` bridges to a JSON string (`"node:c"`) only
    // through `serde_json::Value`.
    let target = |id: &str| Value::from(id);

    // Depth 1..2: reaches b and c, stops before d.
    let mut resp = db
        .query("RETURN node:a.{1..2+collect}(->calls->node)")
        .await
        .unwrap();
    let reached: Vec<Value> = resp.take(0).unwrap();
    assert!(
        reached.contains(&target("node:c")),
        "depth 2 reaches c: {reached:?}"
    );
    assert!(
        !reached.contains(&target("node:d")),
        "depth 2 must NOT reach d: {reached:?}"
    );

    // Depth 1..3: now d IS reached — confirms the limit (not reachability) hid d.
    let mut resp = db
        .query("RETURN node:a.{1..3+collect}(->calls->node)")
        .await
        .unwrap();
    let deeper: Vec<Value> = resp.take(0).unwrap();
    assert!(
        deeper.contains(&target("node:d")),
        "depth 3 reaches d: {deeper:?}"
    );
}

/// Brief #6 — full-text search: analyzer + FULLTEXT index + `@@` match + ranking.
///
/// 3.x deviations from the brief's sketch:
/// - The index keyword is `FULLTEXT ANALYZER … BM25` (SurrealDB 3.0 renamed the
///   pre-3.0 `SEARCH ANALYZER … BM25` form).
/// - A FULLTEXT index covers **exactly one column**. `COLUMNS name, docstring`
///   is a parse error ("Expected one column, found 2"); define one index per
///   searchable field.
/// - The "matches" operator is `@N@` where `N` is a reference number that ties
///   the predicate to `search::score(N)` and `search::highlight(pre, post, N)`
///   in the same statement.
///
/// The analyzer uses `snowball(english)` stemming so that `invoice` matches both
/// `invoice` and `invoices`; BM25 then ranks the doc mentioning the term more
/// often (f1, ×3) above the one mentioning it once (f2). Both per-field indexes
/// are exercised: `docstring @0@` for ranking/highlighting, `name @0@` for
/// symbol-name lookup.
#[tokio::test(flavor = "multi_thread")]
async fn assertion_6_fulltext_search_ranks_hits() {
    let db = mem_db().await;

    db.query("DEFINE ANALYZER code TOKENIZERS class FILTERS lowercase, snowball(english)")
        .await
        .unwrap();

    // Negative proof of the one-column rule: the brief's original two-column
    // form is rejected. Note WHERE the error surfaces: this is a *parse* error,
    // so it fails the whole `query().await` (unlike runtime errors such as the
    // unique-index violation in assertion 4, which resolve Ok and only error at
    // `take(idx)`). Handle both surfaces so the assertion pins the message, not
    // the transport.
    let two_col_err = match db
        .query(
            "DEFINE INDEX node_bad_fts ON TABLE node COLUMNS name, docstring \
             FULLTEXT ANALYZER code BM25 HIGHLIGHTS",
        )
        .await
    {
        Err(e) => e.to_string(),
        Ok(mut resp) => {
            let taken: Result<Vec<Value>, surrealdb::Error> = resp.take(0);
            taken
                .expect_err("two-column FULLTEXT index must be rejected")
                .to_string()
        }
    };
    assert!(
        two_col_err.contains("Expected one column"),
        "two-column FULLTEXT must fail with the one-column parse error, got: {two_col_err}"
    );

    // One FULLTEXT index per field (multi-column FULLTEXT is rejected above).
    db.query("DEFINE INDEX node_name_fts ON TABLE node COLUMNS name FULLTEXT ANALYZER code BM25 HIGHLIGHTS")
        .await
        .unwrap();
    db.query("DEFINE INDEX node_doc_fts ON TABLE node COLUMNS docstring FULLTEXT ANALYZER code BM25 HIGHLIGHTS")
        .await
        .unwrap();

    let _: Option<Value> = db
        .create(("node", "f1"))
        .content(serde_json::json!({
            "name": "calculateTotal",
            "docstring": "invoice parser: parse invoice, validate invoice totals"
        }))
        .await
        .unwrap();
    let _: Option<Value> = db
        .create(("node", "f2"))
        .content(serde_json::json!({
            "name": "total",
            "docstring": "grand total helper for invoices"
        }))
        .await
        .unwrap();
    // Filler symbols that do NOT mention the query term. This matters: BM25's
    // IDF term is `log((N - n + 0.5)/(n + 0.5))`, which goes **negative** when a
    // term appears in a large fraction of a tiny corpus (e.g. 2 of 2 docs). A
    // realistic code index has many symbols and few match any one query term, so
    // scores are positive — we mirror that here. Production FTS assertions must
    // rank by score, not assume a positive floor on a toy corpus.
    for (i, doc) in [
        "renders the login form component",
        "http client retry with backoff",
        "serialize the config to yaml",
        "compute a sha256 digest",
    ]
    .iter()
    .enumerate()
    {
        let _: Option<Value> = db
            .create(("node", format!("filler{i}")))
            .content(serde_json::json!({ "name": format!("f{i}"), "docstring": doc }))
            .await
            .unwrap();
    }

    // `@0@` matches `docstring` against the analyzed query; `search::score(0)`
    // pulls the BM25 score for that same reference. ORDER BY ranks the hits.
    //
    // Note on deserialization: 3.2's `Response::take::<R>` requires
    // `R: surrealdb::SurrealValue`, NOT serde's `DeserializeOwned`. Built-ins
    // (`serde_json::Value`, `String`, `Vec<_>`) implement it, so raw
    // `Vec<Value>` reads work out of the box; a *custom* struct would need
    // `#[derive(surrealdb::SurrealValue)]`. The spike stays on `Value`.
    let mut resp = db
        .query(
            "SELECT name, search::score(0) AS score FROM node \
             WHERE docstring @0@ 'invoice' ORDER BY score DESC",
        )
        .await
        .unwrap();
    let hits: Vec<Value> = resp.take(0).unwrap();
    let score = |h: &Value| h["score"].as_f64().unwrap();

    assert_eq!(
        hits.len(),
        2,
        "both docstrings mention 'invoice'/'invoices'"
    );
    assert!(
        score(&hits[0]) > 0.0,
        "matched hits carry a positive BM25 score"
    );
    assert!(
        score(&hits[0]) >= score(&hits[1]),
        "results are ranked by score DESC: {} then {}",
        score(&hits[0]),
        score(&hits[1])
    );
    // The doc mentioning the term three times ranks first.
    assert_eq!(hits[0]["name"], "calculateTotal");
    assert_eq!(hits[1]["name"], "total");

    // `search::highlight(pre, post, N)` wraps the matched term, keyed by the
    // same reference N as the `@N@` predicate.
    let mut resp = db
        .query(
            "SELECT search::highlight('<b>', '</b>', 0) AS hl FROM node \
             WHERE docstring @0@ 'invoice'",
        )
        .await
        .unwrap();
    let highlighted: Vec<Value> = resp.take(0).unwrap();
    assert!(
        highlighted[0]["hl"]
            .as_str()
            .unwrap()
            .contains("<b>invoice"),
        "highlight wraps the matched term: {:?}",
        highlighted[0]["hl"]
    );

    // --- the `name` index (node_name_fts) — symbol-name lookup ---
    // Same `@N@` + `search::score(N)` contract, targeting the other per-field
    // index. Reference numbers are scoped per statement, so `0` is reused here.
    let mut resp = db
        .query(
            "SELECT name, search::score(0) AS score FROM node \
             WHERE name @0@ 'calculateTotal' ORDER BY score DESC",
        )
        .await
        .unwrap();
    let name_hits: Vec<Value> = resp.take(0).unwrap();
    assert_eq!(
        name_hits.len(),
        1,
        "exactly one symbol named calculateTotal"
    );
    assert_eq!(name_hits[0]["name"], "calculateTotal");
    assert!(
        score(&name_hits[0]) > 0.0,
        "name match carries a positive BM25 score"
    );

    // Tokenizer finding for the future search API: the `class` tokenizer does
    // NOT split camelCase (upper/lowercase are both the letter class), so
    // 'total' matches the symbol literally named "total" but not
    // "calculateTotal". Splitting identifiers needs the `camel` tokenizer in
    // the analyzer chain — a decision for the production FTS design.
    let mut resp = db
        .query("SELECT name FROM node WHERE name @0@ 'total'")
        .await
        .unwrap();
    let total_hits: Vec<Value> = resp.take(0).unwrap();
    assert_eq!(
        total_hits.len(),
        1,
        "class tokenizer keeps camelCase whole — only the literal 'total' matches: {total_hits:?}"
    );
    assert_eq!(total_hits[0]["name"], "total");
}
