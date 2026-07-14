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
