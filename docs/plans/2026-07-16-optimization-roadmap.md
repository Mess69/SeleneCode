# SeleneCode — the optimization roadmap to the final, right-stacked version

**Date: 2026-07-16. Status: ① EXECUTED and re-measured — see the addendum at the bottom.
The ladder bottleneck is FIXED (django name-match 46.4 s → 6.0 s CPU), but the fix was NOT
what this doc predicted: §TL;DR's attribution was wrong one level down. Read the addendum
before trusting the analysis below.**

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

---

# ADDENDUM (2026-07-16, later): ① executed — and the doc's own attribution was wrong

The lesson above struck again, one level down. This doc said the ladder cost was
"`Language::from_wire` + a path `split` per candidate". Both were implemented
(the `Language` enum on `Node`/`UnresolvedRef`; zero-alloc `path_proximity`) —
**and the django ladder did not move** (47.6 → 46.2 s CPU). Two more levels of
counters (`NS_M_*` per strategy, then `NS_MM_*`/`NS_INFER_*` inside the method
matcher) pinned the real cost:

**`match_method_call` was 99% of the name matcher, and ~all of it was the
receiver-inference backward line scan — `capture()` re-probed the global
mutex'd regex LRU (a `String` alloc + lock per pattern PER LINE), ~4.9 M probes
per django index, contended across every rayon thread (~18 µs/line).**

The fix (`receiver.rs`): compile the patterns ONCE per reference
(`compile_all` + `capture_compiled`), plus a substring pre-gate (every pattern
embeds the literal receiver, so `line.contains(receiver)` is a sound skip).
Same treatment for the PHP-property and C++ scans. Behavior-identical: both
gates green, graph byte-count identical (61 838 / 197 168 / 137 928 on django).

## Measured after (django, full repo 3 011 files, cold, same machine)

| metric | before (baseline this session) | after ① + fixes |
|---|---:|---:|
| name-match CPU | 46 391 ms | **6 039 ms (7.7×)** |
| infer line-scan CPU | 43 961 ms | **1 677 ms (26×)** |
| ladder wall (`ms_ladder`) | ~5 s | **1.8 s** |
| index total | 26.96 s | **22.65 s** |
| peak RSS | 3.26 GiB | 3.16 GiB (unchanged — RAM is ③, not ①) |

## Head-to-head vs CodeGraph after ① (2026-07-16, `scripts/bench-vs-codegraph.sh`, best-of-3, cold, source-only)

| corpus | selene | codegraph TS | speed gap | selene RSS | TS RSS |
|---|---:|---:|---:|---:|---:|
| codegraph-src (TS, 162 f) | 1.79 s | 2.28 s | **0.78×** | 1.38 GiB | 0.45 GiB |
| selene-crates (Rust) | 2.28 s | 3.03 s | **0.75×** | 1.77 GiB | 1.14 GiB |
| django (Python, full 3 011 f) | 21.8 s | 24.0 s | **0.91×** | 3.19 GiB | 0.58 GiB |

**SeleneCode is now faster than CodeGraph on ALL THREE corpora — including the
Python corpus, where it lost 1.10× before ①** (django here is the FULL repo incl.
tests, not the old 931-file subset; same copy for both tools, so the gap is fair
while the absolute times are not comparable to the old table). **RAM is still
3–5.5× worse** — untouched by ①, exactly as predicted: it is the eager in-RAM
resolve (③), the one lever this pass deliberately did not pull.

**VS Code has NOT been re-measured** (deliberately — resource constraint); expect
the 189 s ladder share to collapse but the 82 s persist (②) and RAM (③) to stand.
That is the next measurement to run.

## What ① bought and what it did not

- The `Language` enum + zero-alloc scoring are kept: correct, wire-byte-identical
  (13 snapshots pin it), and they remove real per-candidate work — they were just
  not the wall on django. The wall may sit elsewhere per corpus family
  (python's `self.method()` density is what made receiver inference dominate).
- **Full `Symbol(u32)` path interning (§①.2) is now unjustified on time** — exact/
  fuzzy/gate strategies total ~250 ms CPU on django after the fix. Its remaining
  case is RAM (③): `file_path`/`name` duplication across 61 k nodes. Re-argue it
  against ③'s streaming option with a VS Code measurement, not from this doc.
- New top costs on django after the ladder fix: **persist 7.4 s wall (②),
  synthesis 4.8 s, bulk node write ~4.7 s** — ② is now the #1 lever, as the
  large-repo table always said.

The `NS_M_*`/`NS_MM_*`/`NS_INFER_*` counters and the eager-handout counters
(`N_EAGER_LOOKUPS`/`N_EAGER_ARCS_CLONED` — 648 k lookups / 59 M Arc clones per
django run, a candidate for ② work) are kept, same as the `NS_*` set.

---

# ADDENDUM 2 (2026-07-16, later still): ③ RAM — attacked, and the attribution fell AGAIN

§4's claim ("RAM is a constant working-set cost of the eager in-memory resolve")
was **wrong**. Phase-RSS logging (`rss_mib` on every phase line, via
`selene_core::peak_rss_mib`) showed the eager index + full ref queue resident at
only **264 MiB** on codegraph-src — the jump to 1.3 GiB happened inside the
batch loop. dhat at t-gmax then named it exactly:

**~750 MiB of the peak was COMPILED REGEXES.** 1 228 receiver-inference
patterns × ~524 KiB each of `regex-automata` one-pass DFA transitions — the
price of Unicode `\w`/`\b` — held live in the 2 048-slot `PATTERN_CACHE` for
the whole run. (Two red herrings, for the record: vmmap shows mimalloc's
VM-tag-100 regions as "IOAccelerator", which looks like a GPU leak and is not;
and mimalloc purge knobs are inert because the peak is live bytes, not
allocator retention.)

Fix (`receiver.rs`): a dual-engine `Pattern` — every pattern ASCII-rewritten
(`\w` → `[0-9A-Za-z_]`, `\b` → `(?-u:\b)`) on the plain `regex` crate;
lookarounds (Lua) stay on `fancy_regex` (which cannot disable Unicode at all).
ASCII is the parity-faithful semantics — the TS build's JavaScript `RegExp`
`\w`/`\b` are ASCII. Gates green, graphs byte-count identical.

## RAM after (cold, `/usr/bin/time -l`, vs CodeGraph TS)

| corpus | before | after | CodeGraph |
|---|---:|---:|---:|
| codegraph-src | 1.38 GiB | **462 MB (−67%)** | 446 MB — **parity** |
| selene-crates | 1.77 GiB | **609 MB (−66%)** | 1.14 GiB — **we are 1.9× lighter** |
| django (3 011 f) | 3.19 GiB | **2.48 GiB (−22%)** | 0.58 GiB — still 4.3× |

Speed unchanged (codegraph-src 1.8 s, django 22.1 s).

## What remains of ③ (django's 2.48 GiB), measured + researched

1. **RocksDB (~0.7 GiB, the libc-malloc side of vmmap).** Researched: the
   embedded 3.2 SDK ships `ConfigMap::empty()` — block cache defaults to
   `sysmem/2 − 1 GiB` (15 GiB allowance on a 32 GiB Mac), write buffers
   128 MiB × 8, and **`SURREAL_ROCKSDB_*` env vars are dead in the SDK** (only
   the server binary reads them; SDK regression vs 2.x). Fix requires
   `surrealdb_core::kvs::Datastore::builder().with_config(...)` instead of
   `Surreal::new::<RocksDb>` in selene-db, or a one-line `[patch]` of the SDK's
   `run_router` (upstream-PR-worthy). Fold into the ② write-path work.
2. **The at-scale resolve data + batch-loop churn high-water** (the rest).
   Next levers if django RAM parity is demanded: the eager-index group-handout
   dedup (648 k lookups / 59 M Arc clones), `Box<str>`/interned fields on
   `Node`/`UnresolvedRef`, and only then the streaming-resolve trade §③ always
   named. Re-attribute with dhat (`cargo build -p selene --features dhat-heap`,
   TEMP-DHAT notes in git history) before pulling any of them.

---

# ADDENDUM 3 (2026-07-17): ② the write path — executed, and the fsync theory fell too

This doc's ② said the persist cost was "SurrealDB's RocksDB fsyncs on every
transaction commit". Researched against the vendored 3.2.1 sources, then
implemented, then measured — the MECHANISM was confirmed and the COST theory
was wrong at django scale:

**Confirmed mechanics** (all file:line-verified in surrealdb-core 3.2.1):
one `.query()` with N statements = N kvs transactions; textual
`BEGIN…COMMIT` = one; `?sync=never` skips the WAL flush but clean shutdown
still fsyncs (same durability class as CodeGraph's SQLite WAL+NORMAL); this
RocksDB build never pays macOS `F_FULLFSYNC` (fsync-to-drive-cache only);
grouped commit only amortizes with ≥12 concurrent committers.

**Implemented** (commit e53fd85): one transaction per write chunk everywhere
(upsert_files 295→146 ms), `INSERT RELATION IGNORE` replacing the per-chunk
SELECT dedup round trip, conflict-retry on EVERY bulk writer (fixes the
"Resource busy" abort class — the VS Code failure mode — measured live under
sync=never), and opt-in `SELENE_SYNC_NEVER=1` (~1.5 s wall on django).

**Measured truth: django's ~7 s edge persist did not move** under txn
batching, IGNORE, or sync=never. It is per-record `INSERT RELATION` engine
execution (~50 µs/edge: save points, SCHEMAFULL validation, graph pointers).
The write-path levers that remain are engine-internal (or the PRD §5.2
re-argument). At VS Code scale the commit-count reduction and the retry
hardening should still matter — the serialize gate stacked per-statement
commits sequentially there; re-measure on VS Code before concluding ② is
exhausted.

**django end-state this session: 26.96 s → ~21-22 s (CodeGraph TS: 24.0 s).**
Remaining django profile: edges 7-8 s (engine), synthesis ~4.5-5 s, FTS
~4.5 s (concurrent), node bulk 2.4 s, ctx warms ~1.4 s, ladder 1.7 s.
