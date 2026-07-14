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
| SeleneCode (`.`) | 12,123* | 5,069 | <500** | **1.2 s** | correct (resolve_pending → … → Edge) | ✅ PASS |
| CodeGraph (`../codegraph`) | ~500 | ~4,900 | <500 | **1.4 s** | correct (handleMessage → handleToolsCall → handleExplore) | ✅ PASS |
| **VS Code (`../vscode`)** | **12,123** | **349,737** | **≥5000** | **38–224 s** | **WRONG — 1 of 4 symbols, 0 files shown, off-topic flow** | ❌ **FAIL** |

\* SeleneCode's file count is inflated by its own indexed fixture corpora. \** budget still resolves to 1 in practice.

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
| SeleneCode | 12,123* | 5,069 | ~10 s |
| CodeGraph | ~500 | ~4,900 | ~4 s |
| VS Code | 12,123 | 349,737 | **672 s (11.2 min)** |
