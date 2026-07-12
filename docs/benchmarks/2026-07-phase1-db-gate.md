# Phase 1 DB benchmark gate — PRD §5.3 results

Date: 2026-07-12 · Branch: `feat/phase1-selene-db` · Task 9c (measurement) + Task 9d
(remediation) · Baseline measured @ `b937e07`; post-fix measured @ the Task 9d commits
(`feat(db): rocksdb default backend + deferred-FTS bulk-load mode`, `perf(db): prune
frontier re-expansion in hub-rooted traversals`).

**Maintainer decision (2026-07-12): fix + recalibrate.** The gate as written failed on
3 of 4 measures (see PASS/FAIL below). Approved scope: bounded engineering on the two
fixable failures (deferred-FTS bulk mode, hub-traversal frontier pruning), adopt the
two evidence-backed platform decisions (kv-rocksdb default, deferred-FTS load pattern),
re-measure, and record recalibrated targets. All landed in Task 9d; recalibrated
targets are at the end of this document.

## Machine & toolchain

| | |
|---|---|
| Chip | Apple M1 Max (10 cores: 8 performance + 2 efficiency) |
| RAM | 32 GiB |
| OS | macOS 26.5 (build 25F71) |
| rustc | 1.97.0 (2d8144b78 2026-07-07), stable, pinned via `rust-toolchain.toml` |
| cargo profile | `release` (thin LTO, codegen-units = 1, strip) — criterion `bench` profile inherits it |
| surrealdb | 3.2.1 (embedded; engines measured: `kv-mem`, `kv-surrealkv`, `kv-rocksdb`) |
| criterion | 0.8 (async_tokio; 30 samples, 1 s warm-up, 5 s target measurement window) |

**Measurement caveats:**
- Another light agent (a read-only code reviewer) shared this machine during the
  baseline runs. Its load is small (file reads/greps), but numbers carry some noise;
  criterion's [low / median-estimate / high] spread is quoted for every query measure,
  and bulk lists all three per-repeat wall times.
- Backend coverage of the baseline needed **two separate compilations** (see
  Methodology). The kv-mem bulk load was measured in both: 181.9 s median in run 1 vs
  237.6 s in run 2 (~30% cross-run/cross-compile variance — different surrealdb feature
  unification and/or machine state). Query benches were stable across runs (< 2%
  drift). Cross-backend comparisons within the same run are the reliable ones.
- The post-fix re-measure ran bulk with **1 repeat** per backend/mode (noted per the
  remediation brief; the baseline's 3-repeat spread was tight — ≤ 2% on disk engines).

## Methodology

Harness: `crates/selene-db/benches/bulk_and_traverse.rs`, synthetic graph from
`crates/selene-db/src/bench_support.rs` — deterministic from seed `0xC0DE5EED`,
realistic shape (16-node call-chain backbones, one reserved clean 16-node corridor, a
hub with 150+ direct callers plus random extra fan-in, ~5.1 edges/node across 6 edge
kinds, 4 languages, ~1 file per 25 symbols, docstrings on ~30%).

- **bulk_load** — 100,000 nodes + 508,921 edges (608,921 rows) into a freshly-schema'd
  store via `insert_nodes` + `insert_edges`; the full load is timed per repeat,
  **3 repeats, median reported** (baseline; post-fix is 1 repeat). Disk stores get a
  fresh tempdir per repeat. Since Task 9d the harness also measures **deferred-FTS
  mode**: `bulk_load_begin` → nodes → edges → `bulk_load_finish`, with per-phase split.
- **query benches** — criterion on a 20,000-node / 102,008-edge graph loaded once per
  backend:
  - `callers_d1` / `callers_d3` — reverse `calls` traversal from the hub (2,038 direct
    callers at 20k scale), depth 1 / 3, includes neighbor-node fetch
  - `impact_d3` / `impact_d5` — impact radius from the deep-corridor tail, depth 3 / 5
  - `find_path` — shortest path across the reserved corridor (15 hops, `calls` only)
  - `search_fts` — FTS query for `user` (high-frequency vocabulary term), limit 20
- **Baseline backend coverage needed two runs** (the store's `open()` prefers SurrealKV
  whenever `kv-surrealkv` is compiled; RocksDB was only reachable when it was the sole
  disk backend):
  1. `cargo bench -p selene-db --features bench-support,kv-rocksdb` → **kv-mem + kv-surrealkv**
  2. `cargo bench -p selene-db --no-default-features --features bench-support,kv-mem,kv-rocksdb` → **kv-mem (repeat) + kv-rocksdb**

  Post-fix, `kv-rocksdb` is the default disk backend, so one default-features run
  covers kv-mem + kv-rocksdb.
- Runs were serialized: no compilation or other benchmark overlapped a measurement.

## Gate targets (PRD §5.3, as written)

| Measure | Target |
|---|---|
| Bulk load | ≥ 20,000 nodes/s |
| Deep traversal (depth 3–5) p50 | < 50 ms |
| FTS query | < 20 ms |

## Baseline results (@ b937e07, pre-remediation)

### bulk_load — 100k nodes + 508,921 edges, 3 full-load repeats, median

| Backend | Repeats (s) | Median | rows/s | nodes/s (over full load) | vs ≥ 20k nodes/s |
|---|---|---|---|---|---|
| kv-mem (run 1) | 179.6 / 181.9 / 183.9 | 181.9 s | 3,348 | 550 | **FAIL** |
| kv-mem (run 2) | 227.7 / 237.6 / 247.5 | 237.6 s | 2,562 | 421 | **FAIL** |
| kv-surrealkv | 2146.3 / 2161.8 / 2193.3 | 2161.8 s | 282 | 46 | **FAIL** |
| kv-rocksdb | 139.9 / 141.7 / 142.6 | 141.7 s | 4,296 | 706 | **FAIL** |

- Split of the load (remediation experiment, same graph, kv-mem): nodes alone 124.6 s
  (**803 nodes/s**), edges alone 51.2 s (**9,938 edges/s**) — node insertion under the
  4 FULLTEXT indexes dominates, matching the Task 9b probe (~0.8k nodes/s with FTS vs
  ~4.9k without at 20k scale).
- **kv-surrealkv is pathologically slow on this write load**: 36 min per full load,
  15.3x slower than kv-rocksdb in like-for-like disk terms, ~900 MB written per load
  dir; process sat at ~8–15% CPU (sync/IO-bound). One load tempdir was also left
  behind un-cleaned per run (leaked file handle keeping `TempDir::drop` from removing
  it — bench-only nuisance, cleaned up manually).
- **kv-rocksdb beat even the same build's kv-mem** on bulk load (141.7 s vs 237.6 s
  run-2 mem) — the in-memory engine is not write-optimized; the LSM write path is.

### Queries — criterion, 30 samples, [low / median-estimate / high]

| Bench | kv-mem (run 1) | kv-mem (run 2) | kv-surrealkv | kv-rocksdb | Target |
|---|---|---|---|---|---|
| callers_d1 | 48.4 / **48.7** / 49.0 ms | 48.6 / **49.0** / 49.3 ms | 49.2 / **49.5** / 49.9 ms | 52.5 / **52.9** / 53.3 ms | (depth-1 hub fan-in probe; informational) |
| callers_d3 | 2.00 / **2.05** / 2.14 s | 2.05 / **2.07** / 2.11 s | 2.03 / **2.03** / 2.04 s | 2.39 / **2.42** / 2.44 s | < 50 ms → **FAIL** |
| impact_d3 | 1.17 / **1.19** / 1.22 ms | 1.18 / **1.21** / 1.27 ms | 1.19 / **1.19** / 1.20 ms | 1.49 / **1.50** / 1.51 ms | < 50 ms → **PASS** |
| impact_d5 | 1.86 / **1.87** / 1.88 ms | 1.89 / **1.92** / 1.94 ms | 1.91 / **1.92** / 1.93 ms | 2.40 / **2.42** / 2.45 ms | < 50 ms → **PASS** |
| find_path (15 hops) | 3.68 / **3.72** / 3.78 ms | 3.77 / **3.81** / 3.84 ms | 3.80 / **3.93** / 4.10 ms | 4.37 / **4.43** / 4.50 ms | < 50 ms → **PASS** |
| search_fts | 25.8 / **26.0** / 26.2 ms | 25.7 / **25.9** / 26.1 ms | 42.3 / **42.7** / 43.2 ms | 51.2 / **52.1** / 53.2 ms | < 20 ms → **FAIL** |

Outliers: criterion flagged ≤ 5/30 (mild-to-severe) per bench; medians quoted.
`callers_d3` needed 63–76 s of sampling per backend (30 iterations at ~2+ s each).

Notes:
- `callers_d1` ≈ 48–53 ms is the 2,038-caller hub *including* the neighbor-node fetch
  of all callers — consistent with the Task 9b probe (46–52 ms end-to-end). On a
  realistic hub (100–150 callers) this is single-digit ms.
- `callers_d3` explodes because depth-3 reverse traversal **from the hub** expands a
  huge fan-in frontier; the deep-but-narrow corridor traversals (impact_d3/d5,
  find_path over 15 hops) are 1.2–4.4 ms. The traversal engine is fine on deep paths;
  hub-rooted deep reverse traversal is the pathological shape (frontier size, not
  depth). See the Task 9d probe below for the exact cost split.
- Disk engines pay ~1.6x (surrealkv) / ~2x (rocksdb) over mem on FTS; graph traversals
  are near-mem on both disk engines (block cache).

### PASS/FAIL summary (gate as written)

| Measure | Target | kv-mem | kv-surrealkv | kv-rocksdb |
|---|---|---|---|---|
| Bulk load | ≥ 20k nodes/s | **FAIL** (550/s run 1; 803/s nodes-only) | **FAIL** (46/s) | **FAIL** (706/s) |
| Deep traversal p50 — corridor (impact_d3/d5, find_path) | < 50 ms | **PASS** (1.2–3.8 ms) | **PASS** (1.2–3.9 ms) | **PASS** (1.5–4.4 ms) |
| Deep traversal p50 — hub-rooted (callers_d3) | < 50 ms | **FAIL** (2.05 s) | **FAIL** (2.03 s) | **FAIL** (2.42 s) |
| FTS | < 20 ms | **FAIL** (26.0 ms) | **FAIL** (42.7 ms) | **FAIL** (52.1 ms) |

## Remediation experiment — deferred FULLTEXT indexes (mandated: bulk failed)

Scratch integration test on **kv-mem**, same 100k-node / 508,921-edge graph, raw
SurrealQL via `store.db()`. Legs: (A) load with full schema; (B) load with full schema
minus the 4 FULLTEXT indexes (`REMOVE INDEX` before load); (C) blocking
`DEFINE INDEX ... FULLTEXT` ×4 after the data is loaded; (D) same with `CONCURRENTLY`,
polled to `status: "ready"` via `INFO FOR INDEX`.

| Leg | nodes | edges | index build | total |
|---|---|---|---|---|
| A. with-FTS load (baseline) | 124.57 s (**803 nodes/s**) | 51.21 s (**9,938 edges/s**) | — (inline) | **175.8 s** |
| B. no-FTS load | 21.26 s (**4,703 nodes/s**) | 53.03 s (**9,597 edges/s**) | — | 74.3 s |
| B + C. blocking post-load DEFINE ×4 | ″ | ″ | 16.01 s | **90.3 s** |
| B + D. `CONCURRENTLY` DEFINE ×4 | ″ | ″ | **7.56 s** (parallel; DEFINE returns ~0 s) | **81.9 s** |

- **`CONCURRENTLY` is supported by embedded SurrealDB 3.2.1** and builds the four
  indexes in parallel (7.6 s vs 16.0 s blocking-sequential). Progress is observable:
  `INFO FOR INDEX <name> ON TABLE node` → `{ building: { status: "indexing",
  initial: N, pending: N } }` → `{ status: "ready" }`.
- `search_fts('user')` returns identical results (20 hits) after either post-load
  build — the deferred pattern is functionally equivalent (now pinned by
  `deferred_and_inline_fts_agree` in `tests/store_test.rs`).
- Edge rate is FTS-independent (9,938 vs 9,597 edges/s, within noise) — edge tables
  carry no FULLTEXT index. Edges (5x the node count) cost ~51 s either way and become
  the next bottleneck once FTS is deferred.
- Net effect of deferral: full-load 175.8 s → **81.9 s (2.15x)**; node-only throughput
  803 → 4,703 nodes/s (5.9x). **Still ~4.3x short of the ≥ 20k nodes/s target** —
  deferral is necessary but not sufficient for the bulk gate as written.

This pattern shipped as `GraphStore::bulk_load_begin` / `bulk_load_finish` (Task 9d).

## Task 9d remediation probes (release, kv-mem, 20k-node graph)

### Hub-rooted traversal — where callers_d3's 2.05–2.26 s actually goes

Instrumented probe of `callers(hub, 3)` (2,038-caller hub; result = **12,743
entries**), pre-fix:

| Prefetch level | Frontier | Batch time | Edge entries fetched | Distinct payloads | of which re-fetches of earlier levels' payloads |
|---|---|---|---|---|---|
| 0 | 1 | 49.7 ms | 2,125 | 2,038 | 0 |
| 1 | 2,038 | 341.6 ms | 8,403 | 6,965 | 782 |
| 2 | 6,183 | 1,646.3 ms | 25,264 | 14,904 | 6,052 |

Raw-SQL cost split of the level-2 expansion (6,183-id frontier, 25,264 edges):

| Leg | Time |
|---|---|
| Graph-pointer scans only (single-pass `[<-k1, <-k2, ...]` projection) | **225 ms** |
| Scans + edge-row point-fetch (the full adjacency read) | **1,217 ms** |
| Scans + row fetch, pre-fix per-kind `LET` shape | 1,390 ms |
| `get_nodes` for the level's 14,904 distinct payloads | 367 ms |

Findings:
1. **The frontier itself was never re-expanded** — the level loops already gate on a
   fetched-set, so no node is re-enqueued or re-sent (the draft's "re-visits at each
   level" diagnosis was wrong at the node level). Now pinned by a dense fan-in
   regression test (`callers_dense_fan_in_expands_each_node_once`).
2. The measured re-work was **cross-level node-payload re-fetch** (6,834 of 17,074
   payload fetches were repeats) and **per-kind pointer re-scans** (one `LET` subquery
   per edge kind re-walked the frontier k times). Both fixed in Task 9d (walk-long
   payload cache over edges-only batch readers; single-pass pointer projection).
3. The dominant remaining cost is the **edge-row point-fetch: ~39 µs/row × 25k rows ≈
   1.0 s** on the biggest level, an engine per-record rate, not an algorithmic
   re-expansion. A depth-first-exact fetch was evaluated and rejected: demand-driven
   fetching degenerates to the same breadth-first superset when deeper adjacency is
   missing (treating unfetched nodes as leaves makes the partial replay breadth-first),
   and the true-DFS-expanded set (4,242 of 8,222 prefetched lists) is only reachable
   with serial per-node round trips.
4. **Floor analysis:** the result itself is 12,743 `(node, edge)` entries. At measured
   engine rates (~25 µs/node payload, ~39 µs/edge row) just materializing the result
   costs ~0.8–0.9 s — hub-rooted depth-3 on this fixture cannot approach 50 ms without
   changing what the query returns (caps/pagination are product-layer concerns,
   Phase 4). Hub-rooted traversal cost is **O(result size)**, ~70–110 µs per returned
   entry end-to-end.

Known future lever (deferred, out of Task 9d's bounded scope): the deterministic edge
record id already encodes `(kind, source, target, line, col)`, so adjacency could skip
the edge-row point-fetch entirely and lazily hydrate `provenance`/`metadata` for
result edges only — roughly halving hub-rooted d3 again. Recorded here, not built.

### FTS probe — where search_fts's 26 ms goes (item 4, timeboxed)

`search_fts("user", limit 20)` on the 20k corpus, kv-mem. The worst-case term matches
**1,302 of 20,000 rows** via the 4-way OR (per-index match counts: name 1,157,
qualifiedName 1,157, docstring 463, signature 750).

| Variant | Time | Contract-safe? |
|---|---|---|
| Baseline (current statement) | 26.0 ms (criterion) / 29–33 ms (probe loop) | — |
| Two-phase: `id`+`rawScore` only, then point-fetch the 20 winners | 33.7 ms | yes — but **slower** |
| Single-score expression (name only), id-only projection | 19.0 ms | **no** — drops 3 of 4 weighted scores |
| Two-index predicate (name/qualifiedName only), id-only | 15.7 ms | **no** — changes recall |

Outcome: **no change adopted; 26 ms stands as the recorded number.** The cost is
per-row BM25 scoring of ~1,300 candidates across four single-column indexes — not row
materialization (the two-phase cut made it slower). Every shape that reached < 20 ms
violates the scoring contract (weights 20/5/1/2 over all four fields, `?? 0` coalesce)
or recall; per-index candidate capping (`LIMIT` per `@@` subselect before a merge)
changes scores whenever an index has more matches than the cap, so it was not pursued
past the probe. Real-corpus terms are typically far less frequent than the synthetic
worst case.

## Post-fix results (Task 9d, 1-repeat bulk, criterion queries)

Measured on the Task 9d commits with `kv-rocksdb` as the compiled disk backend
(default features).

### bulk_load — inline vs deferred FTS, 100k nodes + 508,921 edges (1 repeat)

| Backend / mode | nodes | edges | FTS build | total | nodes/s (node phase) |
|---|---|---|---|---|---|
| kv-mem / inline | — | — | inline | 189.8 s | 527 (full-load) |
| kv-mem / deferred | 21.3 s | 52.6 s | 8.6 s | **82.5 s** | **4,695** |
| kv-rocksdb / inline | — | — | inline | 133.9 s | 747 (full-load) |
| kv-rocksdb / deferred | 17.0 s | 50.2 s | 25.8 s | **93.1 s** | **5,875** |

- The shipped `bulk_load_begin`/`finish` API reproduces the scratch experiment almost
  exactly on kv-mem (82.5 s vs leg B+D's 81.9 s; 4,695 vs 4,703 nodes/s).
- kv-rocksdb's **node phase beats kv-mem** (17.0 s vs 21.3 s — the LSM write path
  again), but its post-load `CONCURRENTLY` FTS build is ~3x slower (25.8 s vs 8.6 s),
  netting 93.1 s total: **1.44x over its inline load** (kv-mem nets 2.30x).
- Deferred mode clears the recalibrated ≥ 4,000 nodes/s node-phase bar on both
  engines, and the full 100k-node/509k-edge initial index lands at ~1.5 min on the
  default disk backend.

### Queries — criterion, 30 samples, [low / median-estimate / high]

| Bench | kv-mem baseline | kv-mem post-fix | kv-rocksdb baseline | kv-rocksdb post-fix |
|---|---|---|---|---|
| callers_d1 | 48.7 ms | 48.2 / **48.6** / 49.1 ms | 52.9 ms | 52.3 / **52.6** / 53.0 ms |
| callers_d3 | 2.05 s | 1.63 / **1.64** / 1.65 s (**1.25x**) | 2.42 s | 1.93 / **1.93** / 1.95 s (**1.25x**) |
| impact_d3 | 1.19 ms | 0.95 / **0.96** / 0.98 ms | 1.50 ms | 1.20 / **1.21** / 1.23 ms |
| impact_d5 | 1.87 ms | 1.53 / **1.55** / 1.57 ms | 2.42 ms | 1.92 / **1.94** / 1.96 ms |
| find_path | 3.72 ms | 3.71 / **3.76** / 3.81 ms | 4.43 ms | 4.22 / **4.25** / 4.28 ms |
| search_fts | 26.0 ms | 25.9 / **26.0** / 26.1 ms | 52.1 ms | 48.6 / **48.9** / 49.2 ms |

- `callers_d3`: 1.25x on both engines from the payload cache + single-pass pointer
  scans; the remainder is the O(result size) floor analyzed above (12,743 entries).
- `callers_d1` and `find_path` unchanged (their level-0/no-repeat shapes had no
  cross-level re-fetch to eliminate); `impact_d3/d5` pick up ~1.2x from the same cache.
- `search_fts` unchanged on kv-mem (no change was adopted — see the FTS probe);
  the rocksdb FTS delta (52.1 → 48.9 ms) is run-to-run/block-cache variance, not a fix.

## Decisions & recalibrated targets (2026-07-12)

### Default disk backend: `kv-rocksdb` (landed)

- Bulk load: 141.7 s vs 2,161.8 s (15.3x faster than surrealkv); indexing/re-indexing
  is the product's heaviest recurring operation, and **36 min for a 100k-node repo on
  surrealkv is unshippable** (sync/IO-bound at ~8–15% CPU, ~900 MB written per load).
- Traversals: equivalent (1.5–4.4 ms vs 1.2–3.9 ms on the corridor shapes; both orders
  of magnitude under the 50 ms bar).
- FTS reads: rocksdb is the slowest (52 vs 43 ms) — but both disk engines miss the
  20 ms target anyway, and FTS cost is dwarfed by the write-path difference.
- Cost: the C++ first build (~7.5 min clean) and a fatter binary — build-time-only.
  `kv-surrealkv` stays behind its feature flag; when explicitly compiled it keeps
  preference in `open()` (an opt-in, e.g. to open an existing SurrealKV store).

### Recalibrated targets

| Measure | Recalibrated target | Rationale |
|---|---|---|
| Bulk load | **≥ 4,000 nodes/s (node phase), deferred-FTS, on the default disk backend**; full 100k-node/509k-edge load ≤ ~2.5 min | The 20k/s figure was calibrated against raw-KV writes (the TS store's SQLite bulk path), not a document-graph engine maintaining 7+ secondary indexes + unique constraints per row. 100k nodes ≈ a very large repo; a ~1.5–2.5 min initial index is acceptable product-wise (initial index is once per repo; incremental re-index is per-file). Deferred-FTS is the shipped load pattern (`bulk_load_begin`/`finish`). |
| Deep traversal (corridor, depth 3–5, path-finding) | **< 50 ms p50** (unchanged) | Passes with 10x+ headroom on every engine (1.2–4.4 ms). |
| Deep traversal (hub-rooted) | **< 50 ms for product-realistic result sizes (≲ 500 entries); O(result size) beyond — ~70–110 µs/entry** (record actual: see post-fix table) | The synthetic 2,038-caller hub at depth 3 returns 12,743 entries; materializing that result alone costs ~0.8–0.9 s at engine point-fetch rates. This is result-size-bound, not frontier-algorithm-bound (the probe section above). Product surfaces cap/paginate explore output (Phase 4 explore budgets), so the honest per-entry rate is the durable number. |
| FTS | **< 20 ms for typical terms; ≤ ~30 ms (kv-mem) / ~55 ms (rocksdb) worst-case high-frequency term at 20k nodes** | The probe exhausted the contract-preserving shapes; 26 ms (kv-mem) stands. The worst-case term matches 6.5% of the corpus in two indexes at once — rare in real queries. Final ranking blends upstream (Phase 4), which can also cache/limit hot terms. |

### Gate verdict

Corridor traversal and path-finding **pass** as written. Bulk, hub-rooted traversal,
and FTS **fail as originally written** and are recalibrated per the table above with
the maintainer's fix + recalibrate decision (2026-07-12). The two structural fixes
(deferred-FTS bulk mode: 2.15x total-load, 5.9x node throughput; frontier pruning:
payload-once-per-walk + single-pass pointer scans) are landed and pinned by tests; the
remaining gaps are engine per-record rates and result-size floors, documented above
with the evidence.
