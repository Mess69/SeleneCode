//! SurrealQL schema (v1) for the SeleneCode graph store, and the DDL builder
//! [`all_ddl`] that [`crate::SurrealStore::apply_schema`] applies.
//!
//! Every statement is `DEFINE ... IF NOT EXISTS`, so applying the whole schema
//! is idempotent — re-running it on an already-initialized database is a no-op
//! (and never bumps or clobbers a version a future migration set). The blocks
//! are emitted in dependency order: the FTS **analyzer** before the FULLTEXT
//! indexes that reference it, the **node** table before the ENFORCED edge
//! relations that point at it.
//!
//! ## Field-shape decisions (each probed against embedded SurrealDB 3.2.1 in
//! the Task 3 spike; every load-bearing finding is inlined below)
//!
//! - **`node` / `file` / `unresolved_ref` are `SCHEMAFULL`**; field names are
//!   the exact camelCase serde output of `selene_core::Node`, `FileRecord`, and
//!   `UnresolvedRef`. Optional Rust fields become `option<...>`; the two
//!   `Vec<String>` fields become `array<string> DEFAULT []` (absent-when-empty
//!   serde output folds to `[]`). The record **id** is the primary key and is
//!   *not* redeclared as a field (SurrealDB reserves `id`); `file`/`unresolved`
//!   keep their key *also* as a stored field per their serde shape.
//! - **`nameLower`** is a *stored, computed* field (`VALUE string::lowercase(name)`)
//!   backing the case-insensitive name index. A computed **index expression**
//!   (`FIELDS string::lowercase(name)`) was rejected: it throws on any `NONE`
//!   `name` at index-build time; a stored field over the always-present,
//!   required `name` never sees `NONE`.
//! - **12 edge tables**, one per `EdgeKind::as_str()`, each
//!   `TYPE RELATION IN node OUT node ENFORCED SCHEMAFULL`. `ENFORCED` rejects a
//!   `RELATE` to a non-existent endpoint (referential integrity); the store's
//!   edge-insert path (Task 5) must therefore pre-filter to existing endpoints,
//!   which the `insert_edges` contract already requires ("silently skip an edge
//!   with an unknown endpoint"). The edge's source/target/kind live in the
//!   reserved `in`/`out` fields and the table name, so they are not stored
//!   fields.
//! - **Edge unique index folds a missing position to `-1`.** SurrealDB treats
//!   `NONE` as *distinct* in a unique index, so a raw `(in, out, line, col)`
//!   index would let two positionless edges between the same endpoints coexist.
//!   Instead each edge table carries computed `lineKey`/`colKey`
//!   (`VALUE line ?? -1` / `VALUE col ?? -1`) and the unique index is on
//!   `(in, out, lineKey, colKey)` — folding matches the storage-identity rule
//!   `(source, target, kind, line ?? -1, col ?? -1)`.
//! - **`metadata` (edges) / `errors` (file) / `candidates` (unresolved)** hold
//!   arbitrary JSON, so they are `FLEXIBLE` (`SCHEMAFULL` otherwise rejects
//!   undeclared nested keys). Clause order is load-bearing:
//!   `TYPE <t> FLEXIBLE DEFAULT <v>` (SurrealDB requires `FLEXIBLE` *after*
//!   `TYPE`).
//! - **FTS analyzer `identifier`**: `TOKENIZERS class,camel FILTERS lowercase,
//!   ascii`. `class` alone keeps `calculateTotal` whole; adding `camel` splits
//!   camelCase humps so `total` matches `calculateTotal`. Four single-column
//!   `FULLTEXT` indexes (SurrealDB caps a FULLTEXT index at one column):
//!   `name`, `qualifiedName`, `docstring`, `signature`.
//!
//! Edge field naming note for Task 5: the edge tables store the position column
//! as **`col`** (matching the spike + this schema), whereas `selene_core::Edge`
//! serializes its column as `column`. The store's edge writer maps
//! `Edge.column` → the `col` field. (`unresolved_ref`, by contrast, stores its
//! position as `column`, matching `UnresolvedRef`'s serde output.)

use selene_core::EdgeKind;

/// Schema version seeded into `meta:schema_version` on first apply. Bump only
/// alongside a migration path (there is none yet — v1 is the initial schema).
pub const SCHEMA_VERSION: u32 = 1;

/// FTS analyzer shared by all four FULLTEXT indexes. Defined first so the index
/// definitions that name it resolve.
const ANALYZER_DDL: &str = "\
DEFINE ANALYZER IF NOT EXISTS identifier TOKENIZERS class, camel FILTERS lowercase, ascii;
";

/// `node` table: fields mirror `selene_core::Node`'s camelCase serde output.
const NODE_DDL: &str = "\
DEFINE TABLE IF NOT EXISTS node SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS kind ON node TYPE string;
DEFINE FIELD IF NOT EXISTS name ON node TYPE string;
DEFINE FIELD IF NOT EXISTS nameLower ON node TYPE string VALUE string::lowercase(name);
DEFINE FIELD IF NOT EXISTS qualifiedName ON node TYPE string;
DEFINE FIELD IF NOT EXISTS filePath ON node TYPE string;
DEFINE FIELD IF NOT EXISTS language ON node TYPE string;
DEFINE FIELD IF NOT EXISTS startLine ON node TYPE int;
DEFINE FIELD IF NOT EXISTS endLine ON node TYPE int;
DEFINE FIELD IF NOT EXISTS startColumn ON node TYPE int;
DEFINE FIELD IF NOT EXISTS endColumn ON node TYPE int;
DEFINE FIELD IF NOT EXISTS docstring ON node TYPE option<string>;
DEFINE FIELD IF NOT EXISTS signature ON node TYPE option<string>;
DEFINE FIELD IF NOT EXISTS visibility ON node TYPE option<string>;
DEFINE FIELD IF NOT EXISTS isExported ON node TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS isAsync ON node TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS isStatic ON node TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS isAbstract ON node TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS decorators ON node TYPE array<string> DEFAULT [];
DEFINE FIELD IF NOT EXISTS typeParameters ON node TYPE array<string> DEFAULT [];
DEFINE FIELD IF NOT EXISTS returnType ON node TYPE option<string>;
DEFINE FIELD IF NOT EXISTS routeMethod ON node TYPE option<string>;
DEFINE FIELD IF NOT EXISTS routePath ON node TYPE option<string>;
DEFINE FIELD IF NOT EXISTS framework ON node TYPE option<string>;
DEFINE FIELD IF NOT EXISTS updatedAt ON node TYPE int;
DEFINE INDEX IF NOT EXISTS node_kind ON node FIELDS kind;
DEFINE INDEX IF NOT EXISTS node_name ON node FIELDS name;
DEFINE INDEX IF NOT EXISTS node_name_lower ON node FIELDS nameLower;
DEFINE INDEX IF NOT EXISTS node_file_path ON node FIELDS filePath;
DEFINE INDEX IF NOT EXISTS node_language ON node FIELDS language;
DEFINE INDEX IF NOT EXISTS node_qualified_name ON node FIELDS qualifiedName;
DEFINE INDEX IF NOT EXISTS node_file_line ON node FIELDS filePath, startLine;
DEFINE INDEX IF NOT EXISTS node_route ON node FIELDS kind, routeMethod, routePath;
DEFINE INDEX IF NOT EXISTS node_framework ON node FIELDS framework;
";

/// The four FULLTEXT `(index name, node field)` pairs, in schema order. The
/// single source shared by [`all_ddl`] (inline definition), the bulk-load
/// mode's drop/rebuild DDL ([`remove_fts_index_ddl`]/[`fts_index_ddl`]), and
/// `SurrealStore::bulk_load_finish`'s readiness poll — so the deferred rebuild
/// can never drift from what `apply_schema` would have defined inline.
pub(crate) const FTS_INDEXES: [(&str, &str); 4] = [
    ("node_name_fts", "name"),
    ("node_qualified_name_fts", "qualifiedName"),
    ("node_docstring_fts", "docstring"),
    ("node_signature_fts", "signature"),
];

/// `DEFINE INDEX IF NOT EXISTS ... FULLTEXT` for the four [`FTS_INDEXES`].
/// With `concurrently`, each statement gets a trailing `CONCURRENTLY`:
/// verified on embedded SurrealDB 3.2.1 — the DEFINE returns immediately, the
/// four builds run in parallel (7.6 s vs 16.0 s blocking-sequential on a
/// 100k-node corpus), and progress is observable via `INFO FOR INDEX`
/// (`building.status`: `cleaning`/`indexing` → `ready`). Both variants are
/// idempotent (`IF NOT EXISTS` no-ops on an existing index, `CONCURRENTLY`
/// included).
pub(crate) fn fts_index_ddl(concurrently: bool) -> String {
    let suffix = if concurrently { " CONCURRENTLY" } else { "" };
    FTS_INDEXES
        .iter()
        .map(|(name, field)| {
            format!(
                "DEFINE INDEX IF NOT EXISTS {name} ON node FIELDS {field} \
                 FULLTEXT ANALYZER identifier BM25 HIGHLIGHTS{suffix};\n"
            )
        })
        .collect()
}

/// `REMOVE INDEX IF EXISTS ... ON TABLE node` for the four [`FTS_INDEXES`] —
/// the bulk-load mode's enter step. Idempotent: `IF EXISTS` no-ops on an
/// already-removed index (verified on embedded 3.2.1).
pub(crate) fn remove_fts_index_ddl() -> String {
    FTS_INDEXES
        .iter()
        .map(|(name, _)| format!("REMOVE INDEX IF EXISTS {name} ON TABLE node;\n"))
        .collect()
}

/// `file` table: fields mirror `selene_db::FileRecord`'s camelCase serde output.
const FILE_DDL: &str = "\
DEFINE TABLE IF NOT EXISTS file SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS path ON file TYPE string;
DEFINE FIELD IF NOT EXISTS contentHash ON file TYPE string;
DEFINE FIELD IF NOT EXISTS language ON file TYPE string;
DEFINE FIELD IF NOT EXISTS size ON file TYPE int;
DEFINE FIELD IF NOT EXISTS modifiedAt ON file TYPE int;
DEFINE FIELD IF NOT EXISTS indexedAt ON file TYPE int;
DEFINE FIELD IF NOT EXISTS nodeCount ON file TYPE int DEFAULT 0;
DEFINE FIELD IF NOT EXISTS errors ON file TYPE array<object> FLEXIBLE DEFAULT [];
DEFINE INDEX IF NOT EXISTS file_language ON file FIELDS language;
";

/// `unresolved_ref` table: fields mirror `selene_db::UnresolvedRef`'s serde
/// output. Note `column` (not `col`) here — matches `UnresolvedRef`.
const UNRESOLVED_DDL: &str = "\
DEFINE TABLE IF NOT EXISTS unresolved_ref SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS fromNodeId ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS referenceName ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS referenceKind ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS line ON unresolved_ref TYPE option<int>;
DEFINE FIELD IF NOT EXISTS column ON unresolved_ref TYPE option<int>;
DEFINE FIELD IF NOT EXISTS candidates ON unresolved_ref TYPE array<object> FLEXIBLE DEFAULT [];
DEFINE FIELD IF NOT EXISTS filePath ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS language ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS status ON unresolved_ref TYPE string;
DEFINE FIELD IF NOT EXISTS nameTail ON unresolved_ref TYPE string;
DEFINE INDEX IF NOT EXISTS unresolved_from_node ON unresolved_ref FIELDS fromNodeId;
DEFINE INDEX IF NOT EXISTS unresolved_status ON unresolved_ref FIELDS status;
-- ⚠ THREE INDEXES WERE REMOVED HERE (2026-07-14), AND ONE OF THEM WAS THIS FILE'S PROUDEST LINE.
--
-- `unresolved_key` — the composite (fromNodeId, referenceName, referenceKind), the #760 key — was
-- added in `899aea6` and it was a real fix: `delete_resolved`/`mark_failed` filtered on exactly
-- that tuple with no index over it, and persist measured **82% of a whole index run** (42.8 s of
-- 52 s on codegraph). Adding it took persist 42.8 s -> 11.0 s.
--
-- Then the resolve loop stopped doing keyed writes at all. It used to run at offset 0 and DRAIN
-- this table to advance, so every batch had to delete and mark by key. It now walks the queue and
-- rewrites it once (`replace_pending_with_failed`), which filters on `status` alone. **Nothing in
-- the product queries the composite any more** — so it was being maintained on all 52 358 inserts
-- and all 52 358 deletes, for no reader. Same for `unresolved_ref_name` and `unresolved_file_path`:
-- the only queries that used them (`unresolved_by_files`, `retryable_failed`) have ZERO callers.
--
-- Removing all three, measured on django: insert_unresolved 3.9 s -> 2.5 s, persist 10.9 s -> 8.4 s,
-- total 36.2 s -> 33.5 s. Graph byte-identical.
--
-- The lesson is not that indexes are bad. It is that **an index is a cost paid on every write to serve
-- a read** — and when the read goes away, nobody goes back to remove the index. `899aea6` was right
-- when it was written and wrong two days later, and the only reason we noticed is that the writes
-- were being measured. `delete_resolved`/`mark_failed` still exist on the trait and are still
-- tested; without the composite they are slower, and that is correct — nothing in the hot path
-- calls them.
";

/// `meta` table: opaque `key -> value` store (record id = key). Backs
/// `get_meta`/`set_meta` and the seeded `meta:schema_version`.
const META_DDL: &str = "\
DEFINE TABLE IF NOT EXISTS meta SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS value ON meta TYPE string;
";

/// DDL for one edge-kind relation table (`kind` = an `EdgeKind::as_str()`).
fn edge_ddl(kind: &str) -> String {
    format!(
        "\
DEFINE TABLE IF NOT EXISTS {kind} TYPE RELATION IN node OUT node ENFORCED SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS line ON {kind} TYPE option<int>;
DEFINE FIELD IF NOT EXISTS col ON {kind} TYPE option<int>;
DEFINE FIELD IF NOT EXISTS provenance ON {kind} TYPE option<string>;
DEFINE FIELD IF NOT EXISTS metadata ON {kind} TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS lineKey ON {kind} TYPE int VALUE line ?? -1;
DEFINE FIELD IF NOT EXISTS colKey ON {kind} TYPE int VALUE col ?? -1;
DEFINE INDEX IF NOT EXISTS {kind}_unique ON {kind} FIELDS in, out, lineKey, colKey UNIQUE;
"
    )
}

/// The complete v1 schema as one multi-statement SurrealQL program, in
/// dependency order (analyzer → node (+ inline FTS indexes) → 12 edge tables
/// → file → unresolved → meta). Applied as a single `query(...)` whose
/// statements are all validated via `Response::check()`.
pub(crate) fn all_ddl() -> String {
    let mut ddl = String::with_capacity(8 * 1024);
    ddl.push_str(ANALYZER_DDL);
    ddl.push_str(NODE_DDL);
    ddl.push_str(&fts_index_ddl(false));
    for kind in EdgeKind::ALL {
        ddl.push_str(&edge_ddl(kind.as_str()));
    }
    ddl.push_str(FILE_DDL);
    ddl.push_str(UNRESOLVED_DDL);
    ddl.push_str(META_DDL);
    ddl
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ddl_defines_one_relation_table_per_edge_kind() {
        let ddl = all_ddl();
        for kind in EdgeKind::ALL {
            let table = format!("DEFINE TABLE IF NOT EXISTS {} TYPE RELATION", kind.as_str());
            assert!(
                ddl.contains(&table),
                "missing edge table DDL for {}",
                kind.as_str()
            );
            let unique = format!("DEFINE INDEX IF NOT EXISTS {}_unique", kind.as_str());
            assert!(
                ddl.contains(&unique),
                "missing unique index for {}",
                kind.as_str()
            );
        }
        // Every DEFINE is guarded by IF NOT EXISTS (idempotence contract).
        for line in ddl.lines().filter(|l| l.trim_start().starts_with("DEFINE")) {
            assert!(line.contains("IF NOT EXISTS"), "non-idempotent DDL: {line}");
        }
    }

    /// The bulk-load drop/rebuild DDL and the inline schema must agree on the
    /// same four FULLTEXT indexes — `bulk_load_finish` may never rebuild a
    /// different index set than `apply_schema` defines.
    #[test]
    fn fts_ddl_variants_stay_in_sync() {
        let ddl = all_ddl();
        let inline = fts_index_ddl(false);
        let concurrent = fts_index_ddl(true);
        let removes = remove_fts_index_ddl();

        for (name, field) in FTS_INDEXES {
            let define = format!(
                "DEFINE INDEX IF NOT EXISTS {name} ON node FIELDS {field} \
                 FULLTEXT ANALYZER identifier BM25 HIGHLIGHTS"
            );
            assert!(ddl.contains(&define), "all_ddl missing inline FTS: {name}");
            assert!(inline.contains(&format!("{define};")), "inline: {name}");
            assert!(
                concurrent.contains(&format!("{define} CONCURRENTLY;")),
                "concurrent variant must end with CONCURRENTLY: {name}"
            );
            assert!(
                removes.contains(&format!("REMOVE INDEX IF EXISTS {name} ON TABLE node;")),
                "remove DDL missing: {name}"
            );
        }
        // The inline schema builds FTS blocking (no CONCURRENTLY): apply_schema
        // must stay synchronous — search-ready the moment it returns.
        assert!(!ddl.contains("CONCURRENTLY"), "all_ddl must stay blocking");
    }
}
