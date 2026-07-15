# Speed: SeleneCode (Rust) vs CodeGraph (TypeScript) — the first head-to-head

**2026-07-14. The Rust port is 7.7–10.7× SLOWER than the TypeScript build it replaces, producing
the same graph. The gap widens with repo size.**

This is the benchmark that had never been run. Every perf number previously on record
(`6×`, `2.5×`) was **Rust against its own earlier self** — we fixed two bugs of our own and called
it a win. Nothing had ever been compared to TS.

---

## Result

| corpus | files | **selene (Rust)** | **codegraph (TS)** | gap | nodes S / TS | edges S / TS |
|---|---:|---:|---:|---|---|---|
| `codegraph/src` (TS) | 162 | 18.4 s | **2.4 s** | **TS 7.7×** | 3 803 / 3 803 | 14 081 / 14 078 |
| `SeleneCode/crates` (Rust) | 328 | 22.8 s | **2.8 s** | **TS 8.2×** | 5 086 / 5 090 | 17 192 / 17 255 |
| `django/django` (Python) | 931 | 61.1 s | **5.7 s** | **TS 10.7×** | 19 061 / 19 063 | 46 942 / 46 488 |

**The graphs are equivalent** — node counts match to within 4, edges to within ~1%. So "faster"
does **not** mean "does less": TS does the same work, in a tenth of the time. That check is the
whole point of the benchmark and it is the one this project's own history says never to skip.

**The gap grows with size** (7.7× → 8.2× → 10.7×). We do not merely start behind; we **scale
worse**.

### Method (so it can be re-run and disputed)

- **Source-only corpora**, copied out to a scratch dir: no `node_modules/`, `target/`, `dist/`, and
  the vendored `.wasm` grammars deleted — otherwise one tool indexes generated code the other skips
  and the winner is an artifact of the corpus.
- **Cold** every time (`.selene/` and `.codegraph/` deleted before each run), **sequential** (never
  in parallel — they would fight for cores and the winner would be whoever started first), same
  machine.
- selene: `--release` (`lto = "thin"`, `codegen-units = 1`). Not a debug build.
- codegraph: `dist/` build, `codegraph init` (its `index` subcommand requires a prior `init` and
  **exits 0 when it refuses to run** — a `-q` run "finished" in 0.09 s and had indexed nothing. A
  0.09 s win is not a win; the runner now asserts the index directory exists before reporting a
  time).

---

## Where the time goes — it is not the Rust, it is the WRITES

`RUST_LOG=info selene index django` (61 s total):

```
extract + node persist   ≈ 18 s
resolve TOTAL              43.3 s
    fetch                   1.3 s
    ladder                  8.4 s   <- the actual resolution logic:  14%
    persist                29.5 s   <- the writes:  68% of resolve, ~48% of the WHOLE run
    synthesis               2.8 s
```

And in SurrealDB's own RocksDB startup log:

```
Sync mode: every transaction commit
inline-blocking summary at shutdown:  granted = 2,464,643
```

**RocksDB is fsync-ing on every transaction commit**, and the run took 2.46 M inline-blocking
grants. CodeGraph writes to **`node-sqlite` in WAL mode** — which batches and does not fsync per
transaction. Its backend is reported in its own `status --json`: `"backend": "node-sqlite"`,
`"journalMode": "wal"`.

**The resolution ladder — the part everyone assumed was expensive — is 14% of the run.** Rewriting
it, parallelizing it, optimizing it: all of that competes for a sixth of the budget. The database
owns the other five sixths.

---

## What this contradicts

The PRD's central DB decision (`CLAUDE.md`, PRD §5.2/§5.4, 2026-07-12): **"SurrealQL-max"** — push
traversal into SurrealQL, make SurrealDB embedded the **sole** backend, and **drop** the permissive
fallback (IndraDB/redb + Tantivy).

That decision was taken on *query-side* reasoning (recursive traversal, shortest-path in the
engine). It was **never costed on the write side**, and the write side is where this product spends
its time. The Phase-1 DB gate that looks like it settled this compared **SurrealKV against RocksDB**
— *two backends of ours*. It never compared SurrealDB against anything else, and never against
SQLite.

**This does not automatically mean "rip out SurrealDB".** It means the decision is now standing on a
measurement that contradicts it, and it must be re-argued rather than inherited.

---

## Next experiments — ⚠ SUPERSEDED, all three were run the same day. See the follow-up below.

This section originally proposed (1) disabling fsync-per-commit, (2) "collapsing the per-key DELETE
storm" into one query per chunk, and (3) reopening the backend question. **They were measured, and
two of the three were wrong** — (2) in particular is a **64× regression**. The section is kept only
so the reasoning is visible; **do not act on it. Read the follow-up.**

⛔ **Do NOT parallelize the resolve ladder.** It is 8.4 s of a 61 s run. Perfect parallelism buys
almost nothing. (This was written into a commit message once *before* being measured, and it was
wrong then too.)

---

## The honest summary

We assumed the port would be faster because it drops the WASM layer (worker pool, parser resets, OOM
retries) and links tree-sitter natively. That argument is probably *true about extraction* — and
irrelevant, because extraction is not the bottleneck. We replaced a fast embedded SQLite with a
general-purpose multi-model database and paid for it on every write, and nobody noticed for four
phases **because nobody had ever run the two side by side.**

---

# Follow-up (same day): where the writes actually go — and the "obvious" fix is a 64× regression

Three hypotheses, measured in order. **Two died, and the third contradicts the advice this repo was
carrying.**

## 1. "RocksDB fsyncs on every commit" — TRUE, and it costs ~12%. Noise.

| sync mode | django-scale bulk insert (19 k nodes + 47 k edges) |
|---|---|
| what the SDK gives us today (`every`) | 1.45 s |
| `datastore_sync=1s` (interval, SQLite-WAL-like) | 1.32 s |
| `datastore_sync=never` (OS-managed) | **1.28 s** |

**And the knob is unreachable anyway.** The `surrealdb` SDK builds the datastore with

```rust
Datastore::builder()
    .with_query_timeout(..).with_transaction_timeout(..).with_auth(..)
    .build_with_path(endpoint)          // <- .with_config(..) is NEVER called
```

(`engine/local/native.rs:131`) and `Builder::new()` starts from `ConfigMap::empty()`. So **every
`SURREAL_*` environment variable is inert in-process** — `SURREAL_DATASTORE_SYNC=never` provably
changed nothing (the log still printed `Sync mode: every transaction commit`). Those variables only
work through the `surreal` *server* binary, which builds its own `ConfigMap::from_env()`. Reaching
the knob would mean abandoning the SDK client for `surrealdb-core`'s `Datastore` directly. **It
would have bought 12%. We nearly spent a week on it.**

## 2. "SurrealDB/RocksDB is slow at writing" — FALSE, by a factor of 20.

It inserts django-scale data in **1.4 s**. Our persist takes **29.5 s**. *The database is 20×
faster than what we ask of it.* The engine is not the problem; the query shape is.

## 3. The query shape — and the advice that was wrong

Production emits one statement per resolved reference (52 358 on django), concatenated `CHUNK` at a
time into a single round trip. The round trips were already batched; the **statements** are not.

`RESUME.md` §3 carried this, unmeasured, as "the remaining lever":

> *"`run_keyed_statements` emits one DELETE query per key (22 462). **One query per chunk would
> collapse them.**"*

Measured, 4 000 keys, real schema, real composite index, projected to django's 52 358:

| shape | measured | projected at 52 358 refs |
|---|---|---|
| **A** — production: one indexed `DELETE ... WHERE f=.. AND n=.. AND k=..` per key | 0.94 s | **12.3 s** |
| **B** — the recommended batch: `WHERE [f,n,k] IN [...]`, one per chunk | 59.80 s | **782.8 s** |
| **C** — the 3-tuple **as the record id**: `DELETE unresolved_ref:['f','n','k'], ...` | **0.27 s** | **3.5 s** |

**B — the fix the handoff recommended — is a 64× REGRESSION.** An expression over an array cannot
use the composite index `(fromNodeId, referenceName, referenceKind)`, so every chunk degrades to a
full table scan. It is not slower; it is *quadratic*. A first cut of the probe ran B at full django
scale and was still going after **30 minutes**. Someone would have shipped this in good faith.

**C is the answer, and it is safe.** Making the record id the exact 3-tuple *array* is **not a
hash**: it is the tuple itself, so incident #760's collision risk (a *concatenated* 2-field key
matching the wrong row) does not apply. Deletes become primary-key lookups and batch trivially.
Both `delete_resolved` and `mark_failed` go through `run_keyed_statements`, so both benefit.

### What C actually buys, honestly

Persist ≈ 29.5 s ≈ 1 s (edge INSERTs) + ~12 s (`delete_resolved`) + ~16 s (`mark_failed`, same
shape). C takes the keyed writes from ~28 s to ~7 s ⇒ **persist ~29.5 s → ~8 s, django total ~61 s →
~40 s.**

**That is still ~7× slower than the TS build (5.7 s). C is necessary and not sufficient.** The next
targets, in size order, are extraction (~18 s of the run — and the probe shows inserting the 19 k
nodes it produces costs only 0.4 s, so the time is in the *parse*, which is exactly where the native
tree-sitter port was supposed to WIN) and the ladder (8.4 s). Neither has been investigated.

## The meta-lesson, again

The repo was carrying a confident, plausible, **unmeasured** optimisation that would have made things
**64× worse**, filed under "optional". Today the probe lied twice more before it told the truth: it
first clocked 66 000 rows in 0.16 s (412 k rows/s, against a Phase-1 benchmark of 706 nodes/s)
because `Datastore::execute` reports per-statement errors *inside* the responses and the INSERTs were
failing on a missing namespace; and its first design printed nothing until the end, so 30 minutes of
running produced zero numbers. **Assert the write landed. Print as you go.**

---

# Follow-up 2: the parse is 270 ms. The COMMIT is 19 seconds.

`selene-extract` had **no `tracing` dependency at all** — the crate was structurally incapable of
saying where its time went, which is precisely why nobody had ever looked at the 18 s. Added it and
instrumented `index_all` / `run_pipeline`. django, 931 files:

```
index/1  scan                                    174 ms
index/2  bulk_load_begin (schema + drop FTS)      17 ms
index/3a read 98 ms | PARSE 270 ms | COMMIT 19 018 ms
index/4  bulk_load_finish (rebuild + poll FTS)  3 544 ms
resolve   fetch 1 288 | ladder 8 991 | persist 30 177 ms
```

## The native tree-sitter parse takes 0.27 s. It was never the problem.

931 Python files, parsed, in **270 milliseconds**. The port's headline argument — that dropping the
WASM layer and linking the grammars natively would be fast — is **true, and it is worth 0.4% of the
run**. Every previous guess about where the time went (including this document's own, one section
ago: *"the time is in the parse, which is exactly where the native tree-sitter port was supposed to
WIN"*) was wrong.

## The real shape of a 61-second run

| | | |
|---|---:|---|
| **DB writes** (commit 19.0 s + persist 30.2 s) | **49.2 s** | **81%** |
| resolve ladder (the actual work) | 9.0 s | 15% |
| FTS rebuild + poll | 3.5 s | 6% |
| **parse + scan** (the presumed culprit) | **0.44 s** | **0.7%** |

## Why the commit costs 19 s to write what the DB ingests in 0.4 s

`run_pipeline` parses a batch in parallel (rayon) and then commits it **strictly sequentially, one
file at a time**, and per file it does:

```rust
self.store.get_file(&input.rel).await          // round trip: does this file exist? same hash?
self.commit(input, extraction, hash).await     // round trip(s): replace_file_extraction (delete+insert)
```

931 files × (a lookup + a delete-then-insert) ≈ **20 ms per file**, awaited one after another.
`probe_sync_mode` shows the same database ingests those 19 061 nodes in **0.4 s** when handed them
in bulk. **We are 47× slower than the store we chose, because we talk to it one file at a time.**

And on a **fresh** index — which `index_all` always is, it runs inside `bulk_load_begin/finish` —
`get_file` returns `None` every single time and `replace_file_extraction` has nothing to replace.
**931 of those round trips are pure waste by construction.**

The sequential order is a real contract (#1015: commit order *is* the determinism contract), but
**ordering is not the same as one-round-trip-per-file**: a batch can be accumulated in scan order
and written in one call, preserving the order exactly.

## Revised, honest projection

| fix | django |
|---|---|
| today | 61 s |
| + batched commit (19.0 s → ~1 s) | ~43 s |
| + keyed-write variant C (persist 30.2 s → ~8 s) | **~21 s** |
| CodeGraph TS | **5.7 s** |

Both write fixes together take us from **10.7× to ~3.7×** behind. The remainder is then the ladder
(9.0 s — single-core, and the one place where "parallelize it" is *finally* the right instinct rather
than the wrong one) and the FTS rebuild (3.5 s).

**Nothing here argues for replacing SurrealDB.** The engine ingests our whole django graph in under
1.5 s. Every second we lose, we lose in how we talk to it.

---

# Follow-up 3: the optimisation pass. 1.9× faster, and one silent data-loss bug found.

| corpus | files | before | **after** | codegraph TS | gap |
|---|---:|---:|---:|---:|---|
| codegraph/src | 162 | 18.4 s | **10.1 s** | 2.4 s | TS 4.2× |
| SeleneCode/crates | 328 | 22.8 s | **12.3 s** | 2.8 s | TS 4.4× |
| django | 931 | 61.1 s | **31.5 s** | 5.7 s | TS 5.5× |

The gap closed from **7.7–10.7× to 4.2–5.5×**. What actually moved, in order of size:

### 1. The commit talked to the store one file at a time (19.0 s → 9.3 s)

`get_file()` + `replace_file_extraction()` per file, awaited serially — ~5 round trips × 931 files.
On a fresh index (which `index_all` always is) `get_file` returns `None` every time and there is
nothing to replace: **most of it was waste by construction.** Now: one `all_files()`, and new files
are accumulated in scan order and written in four calls. Already-indexed files keep the per-file
REPLACE protocol, which is correctness (#1015), not overhead.

### 2. The resolve loop DRAINED a queue to advance — and dropped references doing it (24 s → ~6 s)

It ran at `offset 0` forever and relied on `delete_resolved` + `mark_failed` to remove each batch's
rows so the next fetch returned the next ones. **The key it deleted by —
`(fromNodeId, referenceName, referenceKind)` — is not unique.** django's `_check_token` raises
`RejectRequest` on five different lines: five rows, one key. The loop resolved the first, deleted by
key, and **took the other four with it**. Edge identity includes the line, so those were four real
edges that never got made. **The graph shipped without them.** This is incident #760's exact
species, and it was still live.

The loop now walks the queue (`START offset`), mutates nothing during the pass, and rewrites the
queue once at the end: *drop every pending row, re-insert the failed ones as failed.* Two statements
instead of 52 354 keyed writes — and the bug is structurally impossible, because nothing is deleted
by a non-unique key any more.

⚠ **The edges still go in per batch, and that is a DEPENDENCY.** Deferring every insert to the end
ran 3 s faster and produced **46 937 edges instead of 46 946** — the ladder reads the graph earlier
batches wrote. Hoisting the writes out of the loop silently changes the answer.

### 3. `START offset` is a skip-scan (2.2 s → 0.36 s)

The offset walk I had just introduced paged with `LIMIT n START offset`, which walks the first
`offset` rows to discard them — O(n²/batch), and it would have gone quadratic on VS Code. Nothing
mutates the queue during the pass, so there is nothing to page around: fetch once, iterate in memory.

### 4. Three indexes were being maintained for no reader (36.2 s → 33.5 s)

`unresolved_key` — the 3-field composite — was added two days earlier and took persist 42.8 s →
11.0 s. It was a real fix *for the keyed writes*. Once the keyed writes were gone, **nothing queried
it**, and it was still being maintained on 52 358 inserts and 52 358 deletes. Same for
`unresolved_ref_name` and `unresolved_file_path`, whose only queries have zero callers.

> An index is a cost paid on every write to serve a read. When the read goes away, nobody goes back
> and removes the index.

### Measured and REJECTED

- **`CHUNK` tuning** (100/250/500/1000): no effect — bulk 9.1–9.2 s, persist 10.9–12.3 s, all noise.
  SurrealDB's own benchmark shows batch-100 at ~2× the row throughput of batch-1000, but that
  measures **network** round trips; we are in-process.
- **`SURREAL_DATASTORE_SYNC`**: inert. The SDK never calls `Builder::with_config()`, so every
  `SURREAL_*` variable is dead in embedded mode. Worth 12% anyway.

## Where the remaining 31.5 s goes, and what is left

```
bulk write   ~8.1 s   (nodes 3.6 · edges 1.8 · unresolved 2.5)
ladder        8.6 s   ← SINGLE CORE
persist      ~8.4 s   (insert_edges 2.4 · queue rewrite ~6)
FTS rebuild   3.2 s
synthesis     2.8 s
scan + parse  0.4 s   ← the thing everyone suspected
```

Remaining levers, largest first — **none of them is the database engine**:

1. **Don't round-trip the queue through the disk at all** (~−7 s). We write 52 358 unresolved refs,
   read them back, and delete them — a hand-off buffer between two phases of the same process.
   ⚠ **Blocked on a decision**: the resolution order is `(fromNodeId, referenceName, referenceKind,
   id)` and `id` is a **SurrealDB-generated record id**. Resolution results depend on it (batch N
   sees batch N-1's edges). Handing refs over in memory means ordering them without that id, which
   **changes the graph**. It would also make determinism independent of the engine's id generation,
   which is arguably a latent bug — but it is a deliberate change, not a free win.
2. ~~**Defer the node indexes during bulk load, like FTS already is**~~ — **TRIED, MEASURED, A WASH.
   Reverted.** Dropping the nine ordinary `node` indexes for the load and rebuilding them in
   `bulk_load_finish` did exactly what it was supposed to on the insert side — `insert_nodes`
   **3 634 ms → 2 254 ms** — and then handed the saving straight back at the rebuild:
   `bulk_load_finish` **3 241 ms → 5 066 ms**. Net **31.5 s → 32.5 s**. SurrealDB's bulk
   `DEFINE INDEX` costs *more* than the 19 061 incremental maintenances it avoids, which is the
   opposite of the usual bulk-load intuition and is why it had to be measured rather than assumed.
   The graph was byte-identical throughout, so this is a clean negative result: **the 3.6 s in
   `insert_nodes` is not reachable by moving *when* the indexes are built.**
3. **Parallelize the ladder** (8.6 s, one core). ⚠ Batch N depends on batch N-1's edges, so this
   cannot be parallelized *across* batches without changing the answer.

**Honest ceiling:** 1 + 2 lands around ~22 s; adding 3 lands around ~16 s. **That is still ~3× TS.**
Our node insert alone (19 061 rows, 3.6 s) is most of TS's entire 5.7 s run. Beating SQLite here is
not obviously reachable by tuning — it needs the write volume itself to come down.

---

# Follow-up 4: 10.7× → 2.0–2.9×. The database was never the problem.

| corpus | files | this morning | **now** | codegraph TS | gap |
|---|---:|---:|---:|---:|---|
| codegraph/src | 162 | 18.4 s | **6.2 s** | 2.4 s | **TS 2.6×** |
| SeleneCode/crates | 328 | 22.8 s | **5.5 s** | 2.8 s | **TS 2.0×** |
| django | 931 | 61.1 s | **16.4 s** | 5.7 s | **TS 2.9×** |

Deterministic across runs. Coverage unchanged (and 4 references RECOVERED that the old code silently
dropped). Every gate green. `explore` still answers the milestone question 3/3.

## The two findings that did the work, and they are the same finding

**1. The "ladder" was not CPU-bound. It was 32,524 blocking point lookups.**

The module docs said *"resolution is CPU-bound over a warm cache, not I/O-bound"*. Counted:

```
blocking store reads   32,524      time blocked  4,810 ms  = 69% of the ladder
get_node (POINT LOOKUP BY PRIMARY KEY)   14,674 calls
get_nodes_by_name                        12,279 calls
```

We were asking a database, one row at a time, 32,524 times, to reconstruct a table of 19,061 rows
that fits in ~8 MB of RAM. The LRU could not help and its size was not the fix: raising it from 5,000
to 200,000 removed **8%** of the reads. They are **cold misses** — 12,279 distinct names, each
fetched once. *A lazy cache pays one round trip per distinct key, forever.*

One scan (127 ms) replaced all of them. Ladder: **6,839 ms → 1,889 ms**.

**2. The reference queue was a hand-off buffer round-tripped through the disk.**

`index_all` wrote 52,358 unresolved references to the store (2.4 s) so the resolver could read them
back (0.3 s) and delete them (~3.5 s) — between two phases of the **same process**. They are passed
in memory now. Persist: **7.3 s → 3.4 s**.

It also removed a **determinism bug**: the store ordered by `(fromNodeId, referenceName,
referenceKind, id)` and that `id` is a **SurrealDB-generated record id**. The graph we shipped
depended on the engine's id generation.

## The lesson, stated once

**The resolution phase is not a graph problem. It is a symbol-table lookup** — *"which nodes are
named `foo`?"* — and the right place for a dictionary you hit 32,524 times is **RAM**, not a
database, whatever kind of database it is.

The graph engine is for **storing** the result and **traversing** it (`explore`, callers, impact).
It was never the right tool for the **build** side. Every second we lost, we lost by treating an
in-process computation as a database workload.

**Nothing here argued for replacing SurrealDB.** It ingests the whole django graph in under 1.5 s.
Its own benchmark has it at 300 k CRUD ops/s — 3.5× Redis. It was never slow. We were loud.

## Also measured, also rejected (so nobody re-runs them)

| idea | verdict |
|---|---|
| `SURREAL_DATASTORE_SYNC` / fsync | **inert** — the SDK never calls `Builder::with_config()`, so every `SURREAL_*` var is dead in embedded mode. Worth 12% anyway. |
| `WHERE [a,b,c] IN [...]` batch delete (**recommended by our own docs**) | **64× REGRESSION** — an array expression cannot use the composite index |
| `CHUNK` tuning (100/250/500/1000) | **no effect** — SurrealDB's batch-size numbers measure *network* round trips; we are in-process |
| deferring the node indexes for the bulk load | **a wash** — the bulk `DEFINE INDEX` costs more than the 19,061 incremental maintenances it avoids |
| `lto = true` (fat) | **no gain**, >10 min build |
| tokio 10 MiB stack, explicit multi_thread | **no gain** |
| ⛔ `panic = 'abort'` (SurrealDB recommends it) | **NEVER** — `selene-resolve` wraps the framework detectors and synthesizers in `catch_unwind`; a panicking resolver is a *collected error*, not a dead index |
| SurrealDB `allocator` feature (mimalloc) | ✅ **31.5 s → 28.9 s** — not on by default, and a `default-features = false` dep never gets it |

## What is left on django's 16.4 s

```
bulk write   ~5.5 s   (nodes 3.4 · edges 1.6)
persist       3.4 s   (insert_edges 2.4 · queue rewrite ~1)
FTS rebuild   3.2 s
synthesis     2.8 s
ladder        1.9 s
parse+scan    0.3 s
```

No single item dominates any more. Beating 5.7 s needs the **write volume** to come down (the node
table is SCHEMAFULL with ~25 fields and 9 indexes) or the FTS/synthesis passes to be reconsidered —
not more tuning.

---

# Follow-up 5: concurrency. 16.4s → 10.9s, and now within 1.4–1.9× of TS.

| corpus | files | this morning | **now** | codegraph TS | gap |
|---|---:|---:|---:|---:|---|
| codegraph/src | 162 | 18.4 s | **3.6 s** | 2.4 s | **TS 1.5×** |
| SeleneCode/crates | 328 | 22.8 s | **4.0 s** | 2.8 s | **TS 1.4×** |
| django | 931 | 61.1 s | **10.9 s** | 5.7 s | **TS 1.9×** |

The whole day's factor: **5.1–5.7× faster than where it started.** Deterministic, graph
byte-identical, every gate green, `explore` still 3/3.

## The store was talked to one query at a time

SurrealDB's own benchmark reaches 300k ops/s **with 128 clients issuing 48 concurrent queries each**.
We awaited one query at a time: 39 sequential round trips to insert django's 19,061 nodes, 94 for its
edges, and a 3.2s FULLTEXT poll standing in front of a resolve that reads none of it. The engine was
built for concurrency; the caller supplied none.

- **`insert_nodes` concurrent** (buffer_unordered, cap 16): **3,416 ms → 793 ms**. Ids are
  content-hashed and unique — no two chunks touch the same record.
- **`insert_edges` concurrent** (bounded try_join_all): **1,479 ms → 555 ms**. The within-call dedup
  made chunk identities disjoint; the count is a sum.
- **FULLTEXT build `tokio::join!`ed with the resolve**: the `DEFINE INDEX … CONCURRENTLY` poll was
  3.2s of pure waiting. `search_fts` has one consumer — `explore` — so the resolve does not need it.
  It now builds while the resolve runs.

Concurrency cap is 16, not unbounded: SurrealDB's RocksDB layer sizes its inline-blocking permit pool
from the tokio worker count, so past that the futures just queue on a semaphore inside the engine.

**Proven, not asserted:** graph diffed edge-by-edge vs the sequential build, parity gate at tolerance
0 (6/6), dispatch gate asserting route→handler flows survive the FTS build running *while the
framework pass inserts route nodes* (5/5). A concurrent write that invented or dropped an edge fails
all three. None does.

## The full arc, one line each

```
morning (sequential, queue-through-disk, 32k blocking lookups)   django 61.1s   TS 10.7x
+ batched commit (931 round trips -> 4)                                  51.2s
+ keyed-write bug fixed (was DROPPING references) + queue rewrite        40.0s
+ fetch once (START offset was O(n^2))                                   36.2s
+ dead indexes removed                                                   33.5s
+ SurrealDB `allocator` feature (mimalloc)                               28.9s
+ eager node index (32,524 blocking lookups -> 48)                       23.7s
+ reference queue kept in memory (no disk round-trip)                    16.4s
+ concurrent writes + FTS overlapped with resolve                        10.9s   TS 1.9x
```

## What beating TS outright would take

django's 10.9s is now: bulk write ~4.5s (nodes 0.8 · edges 0.6 · the rest is serialization + files),
synthesis 2.8s, persist ~2s, ladder 1.9s. No single item dominates, and the FTS is now free
(overlapped). To go *under* 5.7s the write VOLUME has to drop — the `node` table is SCHEMAFULL with
~25 fields and 9 indexes — or synthesis (2.8s, single-threaded) has to be revisited. That is a
data-model change, not tuning, and it is the only lever left that is worth more than a fraction of a
second.

---

# Follow-up 6 (2026-07-15): SeleneCode now BEATS CodeGraph on 2 of 3 corpora

| corpus | files | session start | **now** | codegraph TS | verdict |
|---|---:|---:|---:|---:|---|
| codegraph/src (TS) | 162 | 3272 ms | **1645 ms** | 2370 ms | **0.69× — SELENE 1.4× FASTER** |
| SeleneCode/crates (Rust) | 344 | 4109 ms | **1085 ms** | 2931 ms | **0.37× — SELENE 2.7× FASTER** |
| django (Python) | 931 | 10010 ms | **6374 ms** | 5788 ms | 1.10× — FTS-floored |

Best of 5, source-only, cold, sequential, same machine (`scripts/bench-vs-codegraph.sh`).
Every change graph-byte-identical and deterministic; parity gate tolerance-0 7/7, dispatch 6/6.

## What moved, largest first (all in the resolve/index path, none the DB engine)

1. **Parallelized the ladder (2087 → 656 ms on django).** Within a batch every reference resolves
   independently — the edges are created only after the whole batch resolves — so `resolve_one`
   became a pure `&self` `classify(ref) → (hit, Defer)` (deferrals returned, not pushed) and
   `resolve_all` runs it under rayon `par_iter().collect()`, which preserves reference order so the
   result is identical.
2. **Spawned the FTS build instead of `tokio::join!` (hid it behind resolve on small repos).** The
   ladder runs in `block_in_place`; on a join'd task that stalled the FTS branch, so the FTS leaked
   past resolve. A spawned task runs on another worker and overlaps the ladder's blocking sections.
   This is what took selene-crates 2158 → 1085 ms and codegraph/src 2236 → 1645 ms.
3. **Persist 3957 → 1502 ms.** (a) Stopped writing ~24 k `status = failed` rows nothing reads
   (`retryable_failed`/`unresolved_by_files` have zero callers; sync reads `pending`). (b) A batch's
   edge chunks insert concurrently (`try_join_all`) — disjoint slices, awaited before the next batch.
4. **Parallelized the 4 synthesis passes (1814 → 742 ms on django)** — read-only correlations, ctx
   is `Send + Sync`, `join_all` preserves the merge order.
5. **Skip the second `StoreContext` build when the framework pass added no nodes** (~250–470 ms) —
   most repos, and even Django (its ORM adds edges via synthesis, not nodes).
6. **Dropped `HIGHLIGHTS` from the FTS indexes** (~200 ms) — nothing calls `search::highlight`; BM25
   scoring does not need it.

## Where django's remaining 6.4 s goes — and why it is the DB, honestly

```
index (scan+parse+bulk write)  ~1.9 s
resolve TOTAL                   ~3.3 s   ← overlapped with ↓
FTS build (bulk_load_finish)    ~4.5 s   ← the floor: max(resolve, fts) = fts
```

The FTS build of 4 FULLTEXT indexes over django's 19 061 nodes takes ~4.5 s **concurrently**
(measured: a blocking serial build is ~8.8 s, so CONCURRENTLY is already the fast path). It runs
fully overlapped with the resolve, so django's phase cost is `max(resolve 3.3, fts 4.5) = 4.5 s` —
the FTS is the critical path, and it is SurrealDB's FULLTEXT build speed against SQLite's FTS5. This
is exactly the DB-choice cost this document predicted in follow-up 5 ("to go under, the write volume
or the FTS has to change — a data-model decision, not tuning"). On the TS and Rust corpora the FTS is
small enough to hide entirely behind the resolve, which is why they are now decisively faster.

**Verdict: SeleneCode beats CodeGraph outright on the TS and Rust corpora (1.4× and 2.7×) and is
within ~10 % on the large Python one, where SurrealDB's FTS build is the last floor.** The whole
session moved django 10.0 s → 6.4 s and both smaller repos from ~1.3× *slower* to 0.37–0.69× (faster).

⚠️ **This verdict is scoped to ≤1k-file corpora. It does NOT hold at VS Code scale — see follow-up 7.**

# Follow-up 7 (2026-07-15): VS Code (257k nodes) — the honest large-repo head-to-head

The first real *large* repo, and it inverts the story. All four numbers below are apples-to-apples:
the SAME `/usr/bin/time -l` instrument, same machine, same VS Code `src/` tree, cold, one process.

| metric | SeleneCode (Rust) | CodeGraph (TS) | verdict |
|---|---:|---:|---|
| **wall** | 401 s | 202 s | **2.0× SLOWER** |
| **peak RSS** | 6.28 GiB (6.74 GB) | 1.44 GiB (1.54 GB) | **4.4× MORE memory** |
| **disk (index)** | 524 M (RocksDB) | 960 M (SQLite) | **1.8× LESS — Selene wins** |
| **edges** | 1 203 398 | 1 201 805 | **MATCH (+0.13 %) — graph is correct** |

The graph is right (edge count matches within a rounding error, the parity gate is tolerance-0), and
the store is smaller on disk. **But at scale SeleneCode is 2× slower and needs 4.4× the RAM.** That is
the true picture; the small/medium wins in follow-up 6 do not generalize.

## Why the small/medium wins invert at scale — two distinct causes

1. **The concurrent-write win becomes a correctness *hazard*, not a speedup.** Edges are a SurrealDB
   `RELATION` whose endpoints are shared graph state — a popular callee is one endpoint on thousands
   of edges. Concurrent `INSERT RELATION` writes therefore collide on that endpoint under RocksDB's
   optimistic concurrency. On django (46 k edges) collisions are rare and concurrency is a real win;
   at VS Code's ~1.2 M edges they are *constant* — the concurrent resolve aborts with `Resource busy`
   or live-locks on retries. (The earlier "39 s VS Code" number was this bug: a FAILED partial index
   with only 284 k of 1.2 M edges.) The fix is a **scale gate** (`serialize_writes`, keyed on
   >100 k nodes): concurrent below, sequential above. Sequential is correct at any size, so VS Code
   completes — at 401 s. The concurrent path is not *available* at this scale, so there is no faster
   correct path to fall back to. This is the bulk of the 2× gap.

2. **SeleneCode holds the whole resolve in RAM; CodeGraph streams through SQLite.** The 6.28 GiB peak
   is the in-memory reference queue (976 k unresolved refs) plus the eager `StoreContext` index
   (257 k nodes) held live for the ladder. CodeGraph resolves against SQLite on disk and never holds
   the full set resident, so it peaks at 1.44 GiB. This is a deliberate speed-for-memory trade that
   *pays off* on small/medium repos and *overshoots* here — 6.7 GB is a real ceiling risk on a 16 GB
   laptop indexing a monorepo.

## The honest whole-picture verdict, all corpora

| corpus | size | speed vs CodeGraph | memory | correct |
|---|---|---|---|---|
| codegraph/src (TS) | 162 files | **1.4× faster** | — | ✓ |
| SeleneCode/crates (Rust) | 344 files | **2.7× faster** | — | ✓ |
| django (Python) | 931 files | ~tied (1.10×) | — | ✓ |
| **VS Code (TS)** | **257 k nodes** | **2.0× SLOWER** | **4.4× more** | **✓ (edges match)** |

**SeleneCode wins on small/medium repos and loses on huge ones.** The crossover is the write model:
below the gate, concurrent writes + in-RAM resolve make it fast; above it, RELATION-endpoint
contention forces serialization and the in-RAM resolve becomes a memory liability. Closing the
large-repo gap is a **data-model** question (how edges are stored so endpoint writes don't contend,
and whether the resolve can stream instead of holding 976 k refs), not a tuning question — exactly
the write-side DB cost `CLAUDE.md` flags as unre-argued. Nothing here is fixed by more concurrency.
