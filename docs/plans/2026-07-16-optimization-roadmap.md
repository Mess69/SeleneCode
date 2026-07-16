# SeleneCode — the optimization roadmap to the final, right-stacked version

**Date: 2026-07-16. Status: the bottleneck is finally MEASURED, not guessed.**

This doc is the durable output of a long measurement session. It records (1) what was measured, (2)
the single finding that matters, and (3) exactly what remains to reach the final optimized version on
the right stack. Read it before touching performance again — it exists so nobody re-runs a dead
approach (six were tried and measured this session, all wrong until the last).

---

## TL;DR

- **The bottleneck is the NAME MATCHER.** On VS Code the resolve ladder is 189 s (48 % of a 400 s
  index); profiling `classify` step-by-step shows **name matching = ~98 % of that ladder**. Frameworks
  loop: 47 ms. Not the DB, not cloning, not the frameworks.
- **The cost is per-candidate STRING work**, not the graph store: `Language::from_wire` (string→enum
  parse) + a path `split` **for every candidate of every ref** (millions).
- **The fix is interning** (the rust-analyzer/rustc pattern): store `language` as an enum, intern
  paths/names into `u32` symbols → per-candidate scoring becomes integer ops.
- **The stack is right; the implementation was not.** Keep Rust, keep tree-sitter, keep SurrealDB
  (embedded). The losses vs the TS original were *our* code (deep-clones, string-parsing in the hot
  loop), not the language or the database.

---

## What was measured (the evidence)

### 1. Index-time breakdown (VS Code, 257 k nodes, release)

| phase | time | share | whose code |
|---|---:|---:|---|
| **resolve ladder** | 189 s | **48 %** | SeleneCode CPU |
| edge persist | 82 s | 21 % | SurrealDB writes |
| FTS build | ~55 s | 14 % | SurrealDB |
| parse + node write | 28 s | 7 % | mixed |

### 2. `classify` per-step profile (VS Code, summed CPU across ~10 rayon threads; ÷ threads ≈ 187 s wall)

| step | CPU-ms | note |
|---|---:|---|
| pre-filter | 938 | runs on all 1.25 M refs |
| frameworks loop | **47** | 11 frameworks × refs — negligible |
| import resolution | 8 782 | |
| **name matching** | **1 835 789** | **~98 % of the ladder** |

**1.16 M of 1.25 M refs reach the name matcher.** The cost lives in `match_reference` →
`apply_language_gate` (`Language::from_wire` per candidate) + `find_best_match` /
`path_proximity` (a `file_path.split('/')` per candidate).

### 3. Speed vs the reference (release, best-of-2)

| corpus | SurrealDB (ours) | lbug (ours) | CodeGraph (TS, SQLite) |
|---|---:|---:|---:|
| codegraph-src (TS, 162 f) | 1.6 s | 3.4 s | 2.4 s |
| selene-crates (Rust, 344 f) | 1.1 s | 4.1 s | 2.9 s |
| django (Python, 931 f) | 6.4 s | 8.3 s | 5.8 s |
| VS Code (257 k nodes) | 400 s | — | 202 s |

We beat CodeGraph on small/medium, lose ~2× on VS Code — and that 2× is the name-matcher ladder.

### 4. RAM (peak RSS)

| corpus | SurrealDB | lbug | CodeGraph |
|---|---:|---:|---:|
| django | 1.35 GiB | 1.19 GiB | 0.45 GiB |
| VS Code | 4.58 GiB | ~4.7 GiB | 1.44 GiB |

**RAM is NOT the database.** Measured-and-eliminated: block cache, write buffers, Kuzu buffer pool,
parse stacks, rayon threads, the allocator (already mimalloc), data size. Both heavy engines land at
the same ~1.3 GiB on small/medium → it is a constant working-set cost of the **eager in-memory
resolve** (the eager node index + the 976 k / 1.25 M-ref queue, held in RAM *for speed*). Extraction
streaming was implemented + measured + reverted: it does NOT cut the peak (the peak is at resolve,
not parse).

### Dead ends measured this session (do NOT re-run)

- The 5× eager-index node clone → fixed (Arc); did not move peak RSS.
- Capping RocksDB block cache / write buffers → helps VS Code modestly, nothing on small/medium.
- Capping the Kuzu buffer pool → 96–512 MiB all ~1.12 GiB; 64 MiB CRASHES.
- Allocator tuning (mimalloc purge) → inert.
- Streaming the extraction pipeline → correct, gates green, but 0 peak-RAM win, +14 % time.
- **The `Arc<Node>` ladder refactor → correct (gates green), but 0 ladder speedup: the cost was
  scoring, not cloning.** Kept, because it's idiomatic and correct, but it is not the lever.
- The LadybugDB migration → complete, correct, but slower + not lighter. Feature-gated dead end.

---

## What remains — the road to the final optimized version

Ordered by measured leverage. Each is gated by the **tolerance-0 parity gate** (graph must stay
byte-identical) and the dispatch gate.

### ① Name-match interning — the #1 lever (attacks the measured 98 %)

The scoring loop does string work per candidate, millions of times. Turn it into integer ops:

1. **`Node.language: String` → a `Language` enum field.** Kills `Language::from_wire(&c.language)` in
   `apply_language_gate` (called per candidate). This is a **`Node` schema / wire-contract change** —
   the biggest risk (serde output + the DB schema + the extraction contract). Do it deliberately,
   with the parity gate after every step.
2. **Intern `file_path` (and `name`/`qualified_name`) into a `Symbol(u32)`** (an interner: `HashMap<&str,
   u32>` + arena, the matklad/rust-analyzer pattern). Then `path_proximity`'s per-candidate
   `split('/')` and the `==` comparisons become pre-split / integer-compare. Pre-split each path once
   at intern time; the scorer walks segment-ids.
3. **Bonus: interning cuts RAM too** — code has massively duplicated identifiers (every `self`, every
   common method name, every path repeated across nodes); interning stores each once. This is the
   only lever that hits BOTH the name-match speed AND the ~1 GiB resolve RAM.

Expected: the ladder (189 s) collapses toward CodeGraph's whole-run time; VS Code index approaches
parity. This is where Rust finally beats TS (rustc/rust-analyzer do exactly this).

### ② Write path — BEGIN/COMMIT transaction batching (the 82 s persist)

Nothing in `selene-db` uses explicit transactions, so every INSERT auto-commits (a RocksDB fsync
each). Wrap the bulk edge writes in `BEGIN TRANSACTION … COMMIT TRANSACTION` (few large transactions:
correct, no cross-transaction conflict, few fsyncs). Replaces the crash-prone concurrent path AND the
serialize gate. Medium effort; addresses 21 % of the large-repo time. DB stays SurrealDB.

### ③ RAM — only if parity with CodeGraph (0.45 GiB) becomes a hard requirement

The ~1 GiB is the eager in-memory resolve. Two paths, both trade something:
- **Interning (①)** shaves it for free (string dedup) — do this first and re-measure.
- If still needed: **stream the resolve** (LRU caches + lazy node reads like CodeGraph, drop the
  eager full index and the in-memory ref queue). This reverses the core speed decision — it trades
  the speed the eager structures buy. It is a product choice (speed OR RAM), not a bug.

### ④ FTS — the django floor (~4.5 s of its 6.4 s)

SurrealDB's 4-index FULLTEXT build is the small/medium floor. Options: fewer FTS indexes, or defer/
skip FTS when `search` isn't used. Low priority (small absolute cost).

---

## The right stack (verdict)

| layer | choice | why |
|---|---|---|
| language | **Rust** (keep) | the losses were implementation (clones, string-parse in hot loop), not the language — rustc/rust-analyzer prove Rust wins here with interning |
| parser | **tree-sitter** (keep) | deterministic, fast, 40+ languages; same as CodeGraph/KiroGraph/Graphify |
| store | **SurrealDB embedded** (keep) | fastest of the embedded engines measured; native graph traversal + hybrid vector/FTS. lbug is slower + not lighter (dead end). SQLite only if RAM-parity is mandated, and it costs the native traversal + a streaming rewrite |
| hot-path technique | **interning + Arc-sharing** (ADD) | the missing piece — the rust-analyzer/rustc pattern; the measured fix for the name-matcher 98 % |
| allocator | **mimalloc** (already, via surrealdb) | keep |

**Bottom line: the app is on the right stack. The final optimized version is one focused change away
— interning the name matcher — aimed, for the first time this session, at a MEASURED cost.**

---

## Method note (why this doc exists)

Six performance hypotheses were tried and measured this session; five were wrong until `classify` was
profiled step-by-step. **The lesson: instrument first, fix second.** Every future perf change here
should start by measuring where the time actually goes — the intuition-from-architecture was wrong
~3× on both speed and RAM. The profiling counters live in `resolver.rs` (`NS_*`), logged by the batch
loop; keep them.
