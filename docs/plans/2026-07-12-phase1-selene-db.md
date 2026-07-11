# Phase 1 — `selene-db`: GraphStore + SurrealDB embedded — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the `selene-db` crate — a `GraphStore` trait and its sole SurrealDB-embedded
implementation covering every DB operation the CodeGraph pipeline performs, with the
PRD §5.3 benchmark gate.

**Architecture:** SurrealDB 3.2 embedded (`engine::local`), SurrealQL-max (locked
decision): traversal and FTS live in SurrealQL; ranked-search *scoring* stays out of this
crate (it is product logic, ported later into `selene-graph`/`selene-context`). One edge
table per `EdgeKind` (12 tables) so multi-hop traversal is table selection, not row
filtering. Node record key = the CodeGraph node id string.

**Tech Stack:** surrealdb 3.2 (`kv-mem` for tests, `kv-surrealkv` + `kv-rocksdb` behind
features), tokio, serde, thiserror, criterion (bench), insta + tempfile (tests).

**Reference:** `docs/reference/from-codegraph/maps/db-graph-search.md` (the contract:
operations, semantics, constants). PRD §4/§5. Ecosystem notes
`docs/reference/rust-ecosystem-2026-07.md` §1.

## Global Constraints

- Node id: `"<kind>:" + hex(sha256("{filePath}:{kind}:{name}:{line}"))[..32]` (already the
  `Node.id` produced upstream; this crate treats it as opaque but tests use real ids).
- Edge identity/dedup: **storage-level uniqueness** on `(source, target, kind, line ?? -1, col ?? -1)`.
- Callers/callees follow edge kinds `calls, references, imports, instantiates` only.
- Impact radius: container kinds `{class,interface,struct,trait,protocol,module,enum}`
  descend `contains` children **at the same depth**; incoming edges of every kind except
  `contains`; visited-before-depth-check.
- Traversals never error on unknown ids — empty subgraph / `None`.
- `files.path` is the primary key; timestamps are unix millis (i64).
- Wire enums: reuse `selene_core::{NodeKind, EdgeKind, Provenance}` `as_str()` values in
  table names/fields — never restring.
- No `unwrap`/`expect` outside `#[cfg(test)]`.
- Tests run on `kv-mem` (real SurrealDB, no mocking — TS suite used real SQLite).
- All async; tokio multi-thread runtime in tests (`#[tokio::test(flavor = "multi_thread")]`).

## File structure (all under `crates/selene-db/`)

```
src/lib.rs        crate docs + re-exports (GraphStore, SurrealStore, types)
src/store.rs      GraphStore trait + shared parameter/result types (SearchCandidate,
                  Subgraph, TraversalOptions, FileRecord, UnresolvedRef, GraphStats…)
src/surreal.rs    SurrealStore: open/init/in_memory, schema apply, tx helpers
src/schema.rs     SurrealQL schema DDL (tables, indexes, analyzer, fulltext) + versioning
src/nodes.rs      node CRUD impls
src/edges.rs      edge CRUD + adjacency + file-projection impls
src/files.rs      file records + replace_file_extraction protocol
src/unresolved.rs unresolved-refs store
src/search.rs     FTS + candidate fetches (match/like/exact/prefix)
src/traverse.rs   SurrealQL traversals (callers/callees/impact/path/hierarchy/bfs)
tests/store_test.rs        integration: CRUD, files, unresolved, metadata
tests/traversal_test.rs    ports of graph.test.ts contract block (#536/#774/#1086-#1090)
tests/search_test.rs       FTS/candidate-fetch contracts
benches/bulk_and_traverse.rs  criterion: bulk load, deep traversal, FTS (gate §5.3)
```

Types that later phases share (e.g. `FileRecord`, `UnresolvedRef`, `GraphStats`,
`Subgraph`) live in `selene-core` if and when a second crate needs them; start here.

---

### Task 1: Spike smoke-test — validate SurrealQL assumptions on 3.2 embedded

**Files:** Modify: `crates/selene-db/Cargo.toml` (deps: surrealdb workspace + `features =
["kv-mem"]`, tokio, serde, serde_json, thiserror, selene-core; dev-deps: tempfile).
Create: `tests/spike_surreal.rs`.

**Interfaces:** none (throwaway knowledge, kept as smoke test).

- [ ] Write `tests/spike_surreal.rs` asserting, against `Surreal::new::<Mem>(())`:
  1. `use_ns("selene").use_db("graph")` works.
  2. CREATE a `node` record with an arbitrary string key containing `:` (e.g.
     `node:⟨function:abc123⟩` via `create(("node", "function:abc123"))`) and read it back.
  3. `RELATE` two nodes through a `calls` table with `line`/`col`/`provenance` fields;
     verify `->calls->node` returns the target.
  4. `DEFINE INDEX` unique on `(in, out, line, col)` over `calls`; verify duplicate RELATE
     errors (or is ignorable) and line-differing RELATE succeeds.
  5. Recursive traversal: chain a→b→c→d, query depth-limited
     `SELECT @.{1..2}(->calls->node) …` shape; assert it reaches c but not d (exact syntax
     per https://surrealdb.com/docs/surrealql — adjust to what 3.2 accepts; THIS is the
     knowledge the spike captures).
  6. `DEFINE ANALYZER` + `DEFINE INDEX … FULLTEXT` on `name`/`docstring`; `@@` match +
     `search::score()` returns ranked hits.
- [ ] Run `cargo test -p selene-db --test spike_surreal` until green; document every
  syntax deviation discovered as comments in the test.
- [ ] Commit: `feat(db): spike — SurrealDB 3.2 embedded semantics smoke test`

### Task 2: `GraphStore` trait + result types

**Files:** Create: `src/store.rs`; Modify: `src/lib.rs`.

**Interfaces (produces — later tasks implement; signatures are the contract):**

```rust
pub struct FileRecord { pub path: String, pub content_hash: String, pub language: String,
    pub size: u64, pub modified_at: i64, pub indexed_at: i64, pub node_count: u32,
    pub errors: Vec<serde_json::Value> }
pub struct UnresolvedRef { pub from_node_id: String, pub reference_name: String,
    pub reference_kind: String, pub line: Option<u32>, pub column: Option<u32>,
    pub candidates: Vec<serde_json::Value>, pub file_path: String, pub language: String,
    pub status: RefStatus, pub name_tail: String }
pub enum RefStatus { Pending, Failed }
pub struct GraphStats { pub nodes: u64, pub edges: u64, pub files: u64,
    pub nodes_by_kind: BTreeMap<String, u64>, pub edges_by_kind: BTreeMap<String, u64>,
    pub languages: BTreeMap<String, u64> }
pub struct Subgraph { pub nodes: indexmap::IndexMap<String, Node>, pub edges: Vec<Edge>,
    pub roots: Vec<String> }
pub struct NeighborEntry { pub node: Node, pub edge: Edge }
pub trait GraphStore: Send + Sync {
    // nodes
    async fn insert_nodes(&self, nodes: &[Node]) -> Result<()>;
    async fn get_node(&self, id: &str) -> Result<Option<Node>>;
    async fn get_nodes(&self, ids: &[String]) -> Result<HashMap<String, Node>>;
    async fn get_nodes_by_file(&self, path: &str) -> Result<Vec<Node>>;
    async fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>>;
    async fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>>;
    async fn get_nodes_by_name_ci(&self, lower: &str) -> Result<Vec<Node>>;
    async fn get_nodes_by_name_prefix(&self, prefix: &str, limit: usize) -> Result<Vec<Node>>;
    async fn get_nodes_by_qualified_name(&self, qn: &str) -> Result<Vec<Node>>;
    async fn count_nodes_matching_name_in_files(&self, name: &str) -> Result<u64>;
    // edges
    async fn insert_edges(&self, edges: &[Edge]) -> Result<u64>; // validates endpoints, dedups, returns inserted
    async fn outgoing(&self, id: &str, kinds: &[EdgeKind], provenance: Option<Provenance>) -> Result<Vec<NeighborEntry>>;
    async fn incoming(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>>;
    async fn outgoing_batch(&self, ids: &[String], kinds: &[EdgeKind]) -> Result<HashMap<String, Vec<NeighborEntry>>>;
    async fn incoming_batch(&self, ids: &[String], kinds: &[EdgeKind]) -> Result<HashMap<String, Vec<NeighborEntry>>>;
    async fn edges_between(&self, ids: &[String], kinds: &[EdgeKind]) -> Result<Vec<Edge>>;
    async fn cross_file_incoming_with_target(&self, path: &str) -> Result<Vec<(Edge, String, NodeKind)>>;
    async fn dependent_file_paths(&self, path: &str) -> Result<Vec<String>>;
    async fn dependency_file_paths(&self, path: &str) -> Result<Vec<String>>;
    // files
    async fn upsert_file(&self, f: &FileRecord) -> Result<()>;
    async fn get_file(&self, path: &str) -> Result<Option<FileRecord>>;
    async fn all_files(&self) -> Result<Vec<FileRecord>>;
    async fn delete_file(&self, path: &str) -> Result<()>; // cascades nodes+edges+unresolved
    async fn last_indexed_at(&self) -> Result<Option<i64>>;
    async fn distinct_file_languages(&self) -> Result<BTreeSet<String>>;
    // unresolved refs
    async fn insert_unresolved(&self, refs: &[UnresolvedRef]) -> Result<()>;
    async fn unresolved_pending_count(&self) -> Result<u64>;
    async fn unresolved_pending_batch(&self, offset: usize, limit: usize) -> Result<Vec<UnresolvedRef>>;
    async fn unresolved_by_files(&self, paths: &[String]) -> Result<Vec<UnresolvedRef>>;
    async fn delete_resolved(&self, keys: &[(String, String)]) -> Result<()>; // (from_node_id, reference_name)
    async fn mark_failed(&self, keys: &[(String, String)]) -> Result<()>;
    async fn retryable_failed(&self, names: &[String], per_name_ceiling: usize) -> Result<Vec<UnresolvedRef>>;
    async fn clear_unresolved(&self) -> Result<()>;
    // metadata + stats
    async fn get_meta(&self, key: &str) -> Result<Option<String>>;
    async fn set_meta(&self, key: &str, value: &str) -> Result<()>;
    async fn stats(&self) -> Result<GraphStats>;
    async fn node_edge_count(&self) -> Result<(u64, u64)>;
    async fn clear(&self) -> Result<()>;
    // search candidates (scoring lives upstream)
    async fn search_fts(&self, terms: &[String], kinds: &[NodeKind], languages: &[String], limit: usize, offset: usize) -> Result<Vec<SearchCandidate>>;
    async fn search_name_like(&self, q: &str, kinds: &[NodeKind], limit: usize) -> Result<Vec<SearchCandidate>>;
    async fn find_by_exact_names(&self, names: &[String], per_name_limit: usize) -> Result<Vec<Node>>;
    async fn all_node_names(&self) -> Result<Vec<String>>; // fuzzy pipeline input
    // traversal (SurrealQL-max: implemented in-DB; semantics per map)
    async fn callers(&self, id: &str, max_depth: u32) -> Result<Vec<NeighborEntry>>;
    async fn callees(&self, id: &str, max_depth: u32) -> Result<Vec<NeighborEntry>>;
    async fn impact_radius(&self, id: &str, max_depth: u32) -> Result<Subgraph>;
    async fn find_path(&self, from: &str, to: &str, kinds: &[EdgeKind]) -> Result<Option<Vec<(Node, Option<Edge>)>>>;
    async fn type_hierarchy(&self, id: &str) -> Result<Subgraph>;
    async fn traverse(&self, start: &str, opts: &TraversalOptions) -> Result<Subgraph>;
    async fn ancestors(&self, id: &str) -> Result<Vec<Node>>;
    async fn children(&self, id: &str) -> Result<Vec<Node>>;
}
pub struct SearchCandidate { pub node: Node, pub raw_score: f64 }
pub struct TraversalOptions { pub max_depth: Option<u32>, pub edge_kinds: Vec<EdgeKind>,
    pub node_kinds: Vec<NodeKind>, pub direction: Direction, pub limit: usize,
    pub include_start: bool }
pub enum Direction { Outgoing, Incoming, Both }
```

- [ ] Write the trait + types with full doc comments; add `indexmap` to workspace deps.
- [ ] `cargo clippy -p selene-db --all-targets` green. Commit:
  `feat(db): GraphStore trait — the full store contract`

### Task 3: `SurrealStore` open/init + schema DDL

**Files:** Create: `src/surreal.rs`, `src/schema.rs`; test in `tests/store_test.rs`.

Key content: `SurrealStore::in_memory()`, `::open(dir: &Path)` (surrealkv or rocksdb by
feature; `.selene/graph.db` naming decided here, constant `DATABASE_DIRNAME`), ns/db
`selene`/`graph`, `apply_schema()` idempotent (DEFINE … IF NOT EXISTS): `node` table
SCHEMAFULL (all Node fields, camelCase field names matching serde), 12 edge tables (one
per `EdgeKind::as_str()`, `TYPE RELATION IN node OUT node` + `line`/`col`/`provenance`/
`metadata` fields + unique index on `(in, out, line, col)` — NULL line/col folded to -1
stored value), `file`, `unresolved_ref`, `meta` tables + all secondary indexes from the
map (§Wire/contract: kind, name, file_path, language, lower-name…), FTS analyzer +
FULLTEXT index (Task 7 refines), `schema_version` in `meta` (integer 1).

- [ ] TDD: test open-in-memory → apply_schema twice (idempotent) → schema_version == 1.
- [ ] Implement; commit `feat(db): SurrealStore open/init + schema v1`.

### Task 4: Node operations

**Files:** Create: `src/nodes.rs`; tests in `tests/store_test.rs`.

Semantics: `insert_nodes` chunks (CHUNK = 500, constant with map provenance), upsert
semantics (`INSERT … ON DUPLICATE KEY UPDATE` or UPSERT per spike knowledge) — replacing
a node must keep FTS index consistent (native index ⇒ automatic). Prefix query uses
proper string successor, not `\u{FFFF}`. Round-trip every optional field (test with a
maximal Node: decorators, type_parameters, return_type, visibility…).

- [ ] TDD each method group; commit `feat(db): node CRUD + lookups`.

### Task 5: Edge operations + file projections

**Files:** Create: `src/edges.rs`; tests: port the #1034 identity block from
`db-perf.test.ts` (byte-identical collapse; metadata-differs still dedups; line/col-differs
kept; NULL folding; dedup across calls) + endpoint validation (edges referencing missing
nodes are skipped, count reflects it).

`dependent_file_paths`/`dependency_file_paths` port the SQL join semantics (kind !=
'contains', cross-file only) as one SurrealQL query over edge tables.

- [ ] TDD; commit `feat(db): edge store — identity dedup, adjacency, file projections`.

### Task 6: File records + `replace_file_extraction` protocol

**Files:** Create: `src/files.rs`; tests in `tests/store_test.rs`.

Port `storeExtractionResult` DB-side protocol as
`replace_file_extraction(path, nodes, edges, unresolved, file_record)`:
(1) caller pre-checks content_hash; (2) snapshot cross-file incoming edges w/ target
`(name, kind)`; (3) delete file (cascade); (4) insert nodes; (5) insert edges with both
endpoints present; (6) re-attach snapshotted incoming edges by `(kind, name)` match to
new ids; unmatched → resurrect as unresolved from `metadata.refName/refKind` when
stamped, else drop; (7) insert unresolved; (8) upsert file row **last**.
Test: re-index a file whose nodes shifted lines; incoming cross-file edge re-attaches to
the new node id; a removed target resurrects an unresolved ref (stamped metadata).

- [ ] TDD; commit `feat(db): file records + single-file re-index protocol`.

### Task 7: Unresolved refs + metadata + stats + FTS candidates

**Files:** Create: `src/unresolved.rs`, `src/search.rs`; tests: `tests/search_test.rs`.

FTS: analyzer `identifier` (tokenizers: blank + camel/class per spike findings; filters:
lowercase, ascii) over `name`, `qualified_name`, `docstring`, `signature` with BM25 —
field weights mirror BM25 `(0, 20, 5, 1, 2)` intent: name ≫ qualified_name > signature >
docstring; id column unweighted (record key). `search_fts` takes pre-sanitized terms
(sanitization/`::`-split is upstream product logic), ORs prefix matches, returns
`abs(score)`-like positive raw_score; malformed input → empty result, never error.
`search_name_like` ports the LIKE fallback CASE scoring (exact 1.0 / starts 0.9 /
contains 0.8 / qualified 0.7 / else 0.5, secondary order by name length).

- [ ] TDD; commit `feat(db): unresolved refs, metadata, stats, FTS candidate fetch`.

### Task 8: Traversals in SurrealQL

**Files:** Create: `src/traverse.rs`; tests: `tests/traversal_test.rs` — **port the
32-test contract block of `graph.test.ts`** (fixtures: small hand-built graphs; assert
the regression semantics #536, #774, #1086, #1087, #1088, #1089, #1090 listed in the map).

Implementation notes (from spike): callers/callees = recursive multi-edge-table
traversal with depth `{1..n}` collecting `(node, edge)` pairs, dedup at depth boundary;
impact = per-depth frontier expansion (one SurrealQL query per level batching the whole
frontier — contains-children join at same depth, then non-contains incoming) driven by a
Rust loop with visited/enqueued sets (exact TS semantics, adjacency fetched in-DB, cap
enforced per-add); find_path = `{..+shortest=…}` when kinds allow single-table, else
per-level BFS batch queries; traverse(BFS/DFS) = frontier loop honoring
edge-order contains<calls<other, parallel-edge preservation, limit-per-add.

- [ ] TDD test-by-test; commit
  `feat(db): SurrealQL traversals — callers/callees/impact/path/hierarchy/bfs`.

### Task 9: Benchmark gate (PRD §5.3)

**Files:** Create: `benches/bulk_and_traverse.rs`, `src/bench_support.rs` (feature-gated
synthetic graph generator: deterministic seed, 100k nodes, ~500k edges, call-chain depth
≥ 12, fan-out mix), `docs/benchmarks/2026-07-phase1-db-gate.md` (results).
Modify: `crates/selene-db/Cargo.toml` (criterion dev-dep, `[[bench]]`).

Measures (kv-mem, kv-surrealkv, kv-rocksdb): bulk load (batched) nodes+edges/s; callers
depth 1/3; impact depth 3/5; find_path across ≥ 10 hops; FTS query. Compare against
targets: bulk ≥ 20k nodes/s (TS indexing ballpark from PRD), deep traversal p50 < 50 ms
on 100k-node graph, FTS < 20 ms. Record numbers + backend decision (default feature) in
the results doc. **If a measure fails the target, STOP and surface to the maintainer —
this is the §5.3 gate.**

- [ ] Implement, run, record; commit `perf(db): §5.3 benchmark gate — results + backend default`.

### Task 10: Facade polish

- [ ] `src/lib.rs` re-exports + crate docs (role, PRD §5, decision note), README section
  update if drifted; `cargo doc -p selene-db` builds warning-free.
- [ ] Full workspace green: `cargo fmt --check && cargo clippy --all-targets && cargo test`.
- [ ] Commit `feat(db): selene-db public facade`.

## Self-review checklist (after Task 10)

- Every QueryBuilder operation the pipeline uses (map §Public interface) has a trait
  method or an explicit deferral note (segment-vocab + dominant/route aggregates →
  Phase 4 when their consumers arrive; WAL valve → dropped, SurrealDB manages its LSM).
- Traversal contract tests match TS assertions one-for-one.
- No constant drifted (chunk 500, callers whitelist, impact semantics, depth defaults 1/3).
