# LadybugDB (lbug) migration — plan + spike results

**Decision (2026-07-16): migrate the write/store path from SurrealDB to LadybugDB behind
`GraphStore`, feature-gated, SurrealDB staying default until parity is proven.**

## Why (measured, this session)

SeleneCode's memory is ~3× CodeGraph on small/medium repos (1.3 GiB vs 0.45) and ~3× on VS Code
(6.28→4.58 GiB after capping RocksDB). Measurement ruled out every knob (block cache, write buffers,
allocator — already mimalloc, parallelism, the 5× eager clone): the cost is **SurrealDB's embedded
working set + our in-RAM pipeline**, not a bug. The winning competitors don't pay it — CodeGraph and
KiroGraph use **SQLite**, Graphify a **JSON file**. Nobody uses a heavy multi-model DB for a local
code graph. This is the CLAUDE.md-flagged "SurrealQL-max never costed on writes" decision, re-argued.

**LadybugDB** (`lbug` crate) is the Kuzu successor fork: embedded in-process **property graph** DB,
Rust, columnar, Cypher, MIT, actively shipped (v0.18.2, 2026-07-15; 134k downloads). It is the only
option that keeps **native graph traversal** (for `callers`/`callees`/`impact`) AND vector+FTS AND
is memory-lean. Risk: pre-1.0, young (<1yr), effectively one maintainer.

## Spike results (GO) — `$scratch/lbug-spike`, 2026-07-16

| test | result | vs SurrealDB |
|---|---|---|
| build C++ core (cmake+clang, offline) | ✅ compiles on macOS (clang 21, cmake 4.0.2) | — |
| **bulk `COPY` 50k nodes** | **128 ms** (~390k/s → VS Code 257k ≈ 0.66 s) | — |
| **bulk `COPY` 50k edges** | **39 ms** (~1.3M/s → VS Code 1.2M ≈ ~1 s) | persist **82 s** |
| per-row `CREATE` 5k nodes | 54 s — **unusable; MUST use COPY** | — |
| 5-hop `MATCH (a)-[:CALLS*1..5]->(b)` | 27 ms, native | — |
| peak RSS (50k nodes + 50k edges) | **244 MB** | ~1.3 GiB floor |
| FTS extension | ⚠️ `INSTALL FTS` needs the extension present; `CREATE_FTS_INDEX` undefined offline | built-in |

**The write path is ~80× faster and ~5× leaner. The COPY path is mandatory (per-row CREATE is dead).**

## API facts (from `LadybugDB/ladybug-rust`, v0.18)

- `Database::new(path, SystemConfig::default())` — `Database: Send + Sync`.
- `Connection<'a>` borrows `&'a Database`; is `Send + Sync`; `query(&self, &str) -> QueryResult`.
  → store design: `LadybugStore { db: Arc<Database> }`; each async method does
  `spawn_blocking(move || { let conn = Connection::new(&db)?; conn.query(...) })` (lbug is SYNC).
- Schema is typed: `CREATE NODE TABLE Node(... PRIMARY KEY(id))`, `CREATE REL TABLE Calls(FROM Node TO Node)`.
- Bulk: `COPY Node FROM 'file.csv' (HEADER=false)` — the fast path. Arrow ingestion via the `arrow` feature.
- Extensions (`fts`, `vector`, …) load from a local file: `LOAD EXTENSION '<path>/libfts.lbug_extension'`
  (env `LBUG_LOCAL_EXTENSIONS`). Offline needs them BUILT + bundled, or statically linked. **Deferrable.**

## Phases

- [x] **Phase 0 — Spike / GO** (above).
- [ ] **Phase 1 — Foundation** (this session): `lbug` optional dep + `kv-ladybug` feature; `ladybug.rs`
  with `LadybugStore` — open, schema (Node table + 12 rel tables + File + Meta + Unresolved), the
  async-over-sync `exec` helper, bulk **COPY** insert for nodes+edges (temp-CSV first, Arrow later),
  `node_edge_count`/`stats`. In-crate test that inserts + counts + measures RSS.
- [ ] **Phase 2 — CRUD + reads**: get_node(s), by-name/kind/file/qname, counts, files, meta — Cypher.
- [ ] **Phase 3 — Unresolved queue**: insert/pending/delete_resolved/mark_failed (Cypher on Unresolved).
- [ ] **Phase 4 — Traversal**: callers/callees/impact/find_path/type_hierarchy/ancestors/children as
  native variable-length `MATCH` (the paradigm win).
- [ ] **Phase 5 — Search**: `search_name_like`/`find_by_exact_names`/`all_node_names` native first;
  then FTS + vector via bundled extensions (offline packaging problem to solve here).
- [ ] **Phase 6 — Wire + measure**: select `LadybugStore` in `selene-cli` index path under the feature;
  run the graph-identity gates; head-to-head RAM+speed vs SurrealDB on all corpora. Moment of truth.

## RESULTS (2026-07-16) — honest head-to-head, and it does NOT beat SurrealDB

The backend is complete (full 62-method trait, wired into `index` via `SELENE_BACKEND=ladybug`,
graph correct) and heavily write-optimized (per-table bulk `COPY` buffering: django pipeline write
5.8s→0.35s). Measured, release, best-of-2:

| corpus | LadybugDB | SurrealDB | CodeGraph | verdict |
|---|---|---|---|---|
| codegraph-src (TS, 162f) | 3.4 s, 1.30 GiB | 1.6 s, 1.28 GiB | 2.4 s, 0.43 GiB | 2.1× SLOWER, = RAM |
| selene-crates (Rust, 344f) | 4.1 s, 1.62 GiB | 1.1 s, 1.58 GiB | 2.9 s, 1.05 GiB | 3.7× SLOWER, = RAM |
| django (Python, 931f) | 8.3 s, 1.19 GiB | 6.4 s, 1.35 GiB | 5.8 s, 0.45 GiB | 1.3× SLOWER, ~12% lighter |

**The forecast was wrong on both axes, for two measured reasons:**
1. **RAM:** the ~1.3 GiB is SeleneCode's in-RAM pipeline (extraction buffers, 976k-ref queue, eager
   index), NOT the DB — so swapping the DB barely moves it. Worse, the bulk-COPY buffer now holds the
   whole extraction in RAM too, adding to the peak. The DB was never the RAM story on small/medium.
2. **Speed:** Kuzu's `COPY` is the bulk win, but the resolve's per-batch edge writes CAN'T buffer
   (cross-batch visibility), so they pay temp-CSV `COPY` overhead (ms_persist ~3.1 s on django);
   and `all_nodes` parsing 19k JSON `data` blobs is ~2 s. SurrealDB's tuned concurrent-write path
   beats this. The spike's 128ms/50k was a 4-column table into an empty table — not this workload.

**Bottom line: the migration is functionally complete and correct but does not deliver the forecast
win.** Making it competitive needs (uncertain, substantial): Arrow in-memory ingestion (kill temp-CSV
overhead), typed columns instead of a JSON blob (kill the parse), storage-level edge dedup (parity —
edges run ~1-3% high), and streaming the pipeline (the real RAM lever, DB-independent). This is a
kept-alive experiment behind a feature flag, not a default switch.

## Open risks / decisions

1. **FTS/vector offline packaging** — must build + bundle the `.lbug_extension` libs, or statically
   link, or replace BM25 with native name indexes. Biggest unknown for `search`/`explore` parity.
2. **COPY vs Arrow ingestion** — temp-CSV works and is fast; the `arrow` feature avoids the CSV write
   and may be faster still. Start CSV, measure, upgrade if needed.
3. **Graph identity / parity gate** — the tolerance-0 gate must pass against `LadybugStore`; node/edge
   id scheme is store-agnostic (hashed ids), so this should hold, but ordering (`best_candidate` ties)
   must be reproduced.
4. **Node field fidelity** — Node has ~20 fields incl. LIST types (decorators, type_parameters). Map
   all for parity; the foundation may start with a core subset for the speed/RAM measurement.
