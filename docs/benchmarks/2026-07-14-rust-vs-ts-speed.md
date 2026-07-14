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

## Next experiments, cheapest first — do these before touching architecture

1. **Turn off fsync-per-commit for the bulk load.** `Sync mode: every transaction commit` is the
   single loudest line in the log. If SurrealDB exposes it (env/config), a bulk-import path that
   relaxes durability *during indexing only* is the obvious first probe. Indexing is
   reconstructible from source — a crash mid-index costs a re-index, not data.
2. **Collapse the per-key DELETE storm.** `run_keyed_statements` emits **one DELETE query per key**
   (22 462 on SeleneCode). One query per chunk would collapse them.
   ⚠ The key must stay the **exact 3-field tuple** `(fromNodeId, referenceName, referenceKind)`. A
   concatenated/hashed key can **collide**, and this project has **already lost data** to a
   keyed-delete that matched the wrong row (incident #760, a 2-field key). Do not take that
   shortcut.
3. **Only then** re-open the backend question — with a spike that costs the WRITE path, which the
   original decision never did.

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
