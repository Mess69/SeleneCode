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
