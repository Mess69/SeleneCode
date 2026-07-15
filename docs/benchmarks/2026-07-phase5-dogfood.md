# Task 20 — the milestone gate: the binary answers a flow question with zero Read

**The vertical-slice proof.** The real `selene` binary, on a real repo, answering a real flow
question — with the agent never opening a file. Two halves, both required.

## Half A — deterministic sufficiency (CI, `cargo test`)

`crates/selene-mcp/tests/dogfood_gate.rs` drives the release binary (`index` then `serve --mcp`),
speaks real MCP over its stdio, and asserts on the response bytes: every required symbol rendered as
a definition, every required file as a section, a Flow section with ≥3 steps, a blast-radius
section, and **no Read/Grep advice** outside the sanctioned banners. Plus a **negative control** —
the same assertions against a stopword must FAIL, proving they can tell a real answer from noise.

Run: `cargo test -p selene-mcp --test dogfood_gate -- --ignored --nocapture`

| repo | files | nodes | tier | explore latency | answer | verdict |
|---|---:|---:|---|---:|---|---|
| SeleneCode (`.`) | 334 | 5,069 | <500 | **1.2 s** | correct (resolve_pending → … → Edge) | ✅ PASS |
| CodeGraph (`../codegraph`) | ~500 | ~4,900 | <500 | **1.4 s** | correct (handleMessage → handleToolsCall → handleExplore) | ✅ PASS |
| **VS Code (`../vscode`)** | **12,123** | **349,737** | **≥5000** | **38–224 s** | **WRONG — 1 of 4 symbols, 0 files shown, off-topic flow** | ❌ **FAIL** |



## ⛔ THE MILESTONE GATE FAILS ON THE LARGE TIER — and that is the finding, not a bug to paper over

The gate exists to prove the product answers a flow question with zero Read on a ≥5000-file repo. It
does **not**. Two independent failures, both measured on VS Code (349,737 nodes / 1,595,451 edges,
indexed in 11.2 min):

**1. Latency — unusable, and LOCALIZED.** A single `explore` call took **38 s** for the ratified
question and **223 s** (3.7 minutes) for a longer one, vs **1.2–1.4 s** on the small repos.
Instrumented (`selene::explore` spans), the 35.6 s splits as:

```
dominant_file                  5.9 s   aggregation over 1.6M edges, no index
pass0  derive_corpus_terms    10.0 s   hundreds of prefix lookups per query term, no prefix index
pass1-4 score_candidates       2.3 s   FTS (index-backed) — the ONE fast pass
pass6-7 LIKE (CONTAINS)        4.4 s   substring scan, CONTAINS never uses an index (SurrealDB docs)
pass12 graph connectivity      8.9 s   neighbor walk over a 349k-node graph
flow + boundaries + files      0.3 s
```

**Four passes, each O(graph size), each an unindexed scan.** The one fast pass (2.3 s) is
`score_candidates`, which goes through the **FTS index we already have**. The SurrealDB docs are
explicit: `CONTAINS` never uses an index (it is a substring scan); fast text lookup needs the
full-text index. The fix is to route the pipeline through FTS and bound/skip the O(graph) passes on
large repos:
- `dominant_file` — skip above an edge threshold (it is a marginal scoring boost, not correctness).
- `pass0` — the prefix lookups aren't index-backed at scale; bound the candidate substrings, or cap
  pass0 on large corpora (its whole job is a nice-to-have stem widening).
- `pass6-7` — replace the `CONTAINS` scan with an FTS-backed candidate query.
- `pass12` — tighten the hub-degree cap and seed count on large graphs.

## Latency FIXED (2026-07-15): 35.6 s → 6.5 s on VS Code

The unindexed O(graph) passes are skipped above 3,000 files (`LARGE_REPO_FILES`), where the FTS index
(the one index-backed pass) carries candidate generation:

```
              before    after
dominant_file   5.9 s  →  0
pass0          10.0 s  →  0     (skipped — stem widening, a nicety)
pass1-4 (FTS)   2.3 s  →  2.0 s (kept — index-backed)
pass6-7 CONTAINS 4.4 s →  0     (skipped — CONTAINS never uses an index)
pass12          8.9 s  →  0.3 s (skipped)
TOTAL          35.6 s  →  6.5 s
```

Small/medium repos (SeleneCode, codegraph, django — all <3000 files) run every pass and are
**byte-identical** (verified by SHA of the explore output). This makes `explore` usable on a large
repo; the remaining half — the query-vocabulary gap (`keypress` → `keybinding`) — is the next task,
and only worth doing now that latency is not the blocker.

**31.5 s of 35.6 s is two unindexed scans that grow with the graph.** `dominant_file` is a single
SurrealQL aggregation over 1.6M edges; the relevance gather's `search_name_like` does a `CONTAINS`
with no index → a full 349k-row scan per query term. Both are fixable (index-back the name scans,
bound or index `dominant_file`), but until they are, `explore` is unusable on a large repo — even a
correct answer at 38 s violates the "a few fast tool calls" invariant.

**2. Relevance — vocabulary gap.** The question *"how does a keypress become an executed command"*
seeded on `CommandExecuted, executeCommands, executeCommand, …` — every symbol that lexically
matches *command/executed* — and **missed** `AbstractKeybindingService` / `_doDispatch`, because the
code says **keybinding / dispatch**, not **keypress**. The graph *has* the symbols (re-querying with
"keybinding dispatch" surfaces `_doDispatch`; with the literal names, 3 of 4). The relevance cannot
bridge the query's word to the code's word at this scale. The flow it did draw wandered
`references` edges through unrelated editor-model types — the "half-bridged flow is worse than none"
failure, exactly.

**What this means for the product.** SeleneCode answers flow questions correctly and fast on
small/medium repos (SeleneCode, codegraph, and django all resolve in 1–2 s). It does **not** yet
work on a 349k-node repo, on either axis. This is the truth the milestone gate was built to surface,
and it is more valuable known than assumed. Fixing it is two separate efforts: (a) make the
relevance pipeline scale (bounded scans, index-backed candidate generation, a frontier cap on the
flow BFS) and (b) bridge query vocabulary to code vocabulary (segment/synonym matching — the TS
build's `name_segment_vocab`, which SeleneCode never ported).

## Half B — the real-agent zero-Read run

Not run: Half A already fails on the large tier, so the real-agent run would only confirm a known negative. It is gated behind a passing Half A.

## Indexing cost (for reference)

| repo | files | nodes | index time |
|---|---:|---:|---|
| SeleneCode | 334 | 5,069 | ~10 s |
| CodeGraph | ~500 | ~4,900 | ~4 s |
| VS Code | 12,123 | 349,737 | **672 s (11.2 min)** |

## The vocabulary gap — edge-ngram MEASURED insufficient; the answer is vector search

The large-tier question *"how does a keypress become an executed command"* fails on relevance because
the query's words are not the code's: the dispatch path is `AbstractKeybindingService._doDispatch →
executeCommand`, and the query says **keypress**, not **keybinding**/**dispatch**.

**Tried: SurrealDB's `edgengram(3,15)` analyzer filter** (its native autocomplete/partial-match
mechanism — the right instinct, and the replacement for the TS build's hand-rolled SQLite
`name_segment_vocab`). Re-indexed VS Code (349k nodes, 11 min) and measured. **It does not close the
gap.** `keypress` and `keybinding` share only the 3-char prefix `key`; that single weak token is
drowned by the strong, wrong matches on `command`/`executed` (`executedMarker`, `CommandExecuted`,
…), and — the deeper problem — the real answer symbols contain **neither** `command` **nor**
`executed`: `AbstractKeybindingService` and `_doDispatch` are reachable only by *meaning*, not by any
shared substring. Prefix matching cannot know that `keybinding` is the right `key` among thousands
(`keyboard`, `keyword`, `keychain`…). Reverted (byte-identical on small repos; no benefit on the
target; keeping an ineffective schema change is the inert-seam trap).

**The SurrealDB-native answer is vector search.** SurrealDB has first-class HNSW / DISKANN vector
indexes, `vector::similarity::cosine()`, and the `<|K, …|>` KNN operator
(`docs/learn/data-models/vector-search`). Embedding each symbol (name + signature + docstring) and
the query into the same space, then KNN over cosine similarity, bridges `keypress → keybinding →
dispatch` by **meaning** — which is exactly what this question needs and exactly what a graph+vector
database can do that the TS build's SQLite could not. It is the single most compelling "max out
SurrealDB" lever left, and it is scoped, not built: it needs an embedding pipeline (a model over
349k symbols), which is a feature, not a tweak.

**Bottom line.** The product answers flow questions correctly and fast on small/medium repos (the
common case), and large-repo latency is fixed (35.6 s → 6.5 s). What remains for a large repo is
*semantic* relevance when the query's vocabulary diverges from the code's — a documented limitation
with a clear, native path (vector search), not a regression.
