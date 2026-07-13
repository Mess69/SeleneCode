# Phases 4 + 5 — `selene-graph` + `selene-context` + `selene-mcp`: the agent-facing product — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** turn a resolved graph in a database into a product an **AI agent actually uses**.
Phase 4 builds the query + context layer (`selene-graph`, `selene-context`): the
`QueryManager` over `GraphStore`, the `ContextBuilder`, the markdown/JSON formatters, the
explore **budgets**, and the `build_flow_from_named_symbols` heuristics. Phase 5 puts an
**MCP server** (`rmcp` 2.2, stdio) in front of them — `explore` (PRIMARY), `node`
(SECONDARY), `search`, `callers`, `callees`, `impact`, `files` — plus the two minimal binary
subcommands (`selene index`, `selene serve --mcp`) that make the whole path executable.

**They are planned together on purpose.** Phase 5 is the *only* consumer of Phase 4. Planning
them apart is exactly the seam that has already cost this project four **inert-seam** bugs
(see Global Constraints). The unit of value is the whole path:

```
selene-db (GraphStore)  →  selene-graph (QueryManager: symbols, adjacency, source)
                                    ↓
                          selene-context (ContextBuilder, budgets, flow, ranking, render)
                                    ↓
                          selene-mcp (rmcp tools, isError discipline, instructions)
                                    ↓
                          selene (bin: `index`, `serve --mcp`)  →  the agent
```

**Phase 4 gate (Task 13):** insta snapshots of `explore`/`node` output match the TS shapes on
a shared fixture corpus — **and content, not only shape**, is asserted (see Task 13; a
shape-only snapshot gate stays green while the content is empty, which is how Phase 2 nearly
shipped a hollow walker).

**Phase 5 gate (Task 20) — THE milestone:** the **real binary** (`selene index` then
`selene serve --mcp`) answers a real flow question on a real repo with **zero Read/Grep**,
measured, not assumed. Dogfood target: this repo (SeleneCode) and `../codegraph`.

**Tech Stack:** `selene-core` (wire types), `selene-db` (the `GraphStore` **trait** — never
`SurrealStore` in a library's dependency graph; `SurrealStore` is `[dev-dependencies]` for
integration tests and a `[dependencies]` of the **binary** only), `selene-resolve`
(`resolve_and_persist_batched` — Phase 4's entry point into a resolved graph),
`selene-extract` (the `Indexer`), `rmcp` **2.2.x** (`transport-io` / stdio, `#[tool]` /
`#[tool_router]` / `#[tool_handler]`, `ServerInfo.instructions`), `schemars` (tool input
schemas, pulled by rmcp), `tokio` (runtime), `regex` + `std::sync::LazyLock` (the pattern
tables), `indexmap` (insertion-ordered grouped output — TS relies on `Map` iteration order),
`serde` / `serde_json`, `thiserror` 2, `clap` 4.6 derive (bin only), `anyhow` (bin only);
dev: `insta`, `tempfile`, `tokio` (multi-thread test runtime), `selene-db`'s `SurrealStore`.

**Reference (in priority order):**
- `docs/reference/from-codegraph/maps/mcp-context.md` — **THE parity contract for both
  phases.** Every constant, tier table, marker string and isError rule this plan quotes is
  copied from it **verbatim**. A task should never need to open the map to execute; it must
  open it when this plan is ambiguous, and it is the authority when they disagree.
- `docs/reference/from-codegraph/design/adaptive-explore-sizing.md` — why the sizing gate is
  shaped the way it is, and its six **dead ends** (do not re-attempt them).
- `docs/reference/from-codegraph/design/agent-codegraph-adoption.md` — why `explore` is
  Read-*equivalent* (P1: file-view Read parity shipped; a Read-deny hook was **rejected** —
  do not port one) and why the handshake + `tools/list` must be **decoupled from opening the
  index** (P2).
- `docs/plans/2026-07-12-selenecode-roadmap.md` — Phases 4/5, tech pins, and
  **"Contracts that must never drift"**.
- `crates/selene-db/src/store.rs` — the `GraphStore` trait (the traversals and the FTS
  candidate fetch are already in-DB; Phase 4 is *thin* over them by design).
- `crates/selene-resolve/src/lib.rs` — the crate docs: the pass order,
  `resolve_and_persist_batched`, the offset-0 batch loop, the keyed delete, and the
  **four inert seams** this plan is written to avoid repeating.
- `docs/specs/2026-07-11-rust-graph-db-migration-design.md` — PRD §3 (crate roles), §6
  (surfaces), **§8.2 (the invariants below)**.
- `docs/reference/rust-ecosystem-2026-07.md` §2 — the `rmcp` 2.2.x pins and its API-churn
  warning ("copy patterns from the repo's current examples, not old blog posts").
- TS parity source `../codegraph`: consult **ONLY** the specific file a task names
  (`src/mcp/tools.ts`, `src/context/index.ts`, `src/context/formatter.ts`,
  `src/mcp/server-instructions.ts`, `src/mcp/dynamic-boundaries.ts`) — never at large.

---

## Global Constraints (bind every task)

- **Sufficiency / anti-Read is the product.** The value is that an agent answers a
  structural/flow question with a few fast tool calls and **zero Read/Grep**. A flow question
  resolves in **1 explore call on small repos, scaling to 3–5 on large**. Every design choice
  is judged by one question: *does the answer stop the agent from reading?* An output that is
  correct but sends the agent to `Read` has failed.
- **Explore budget stays monotonic with repo size.** A larger repo tier **never** gets a
  smaller per-file output budget (`maxCharsPerFile`) than a smaller one. This is a
  machine-checked invariant (Task 8), not a style note.
- **`isError` is RESERVED.** Only `Error::PathRefusal` (security), input-validation failures,
  disabled/unknown tool names, and **genuine malfunctions** set `isError: true`. Every
  expected/recoverable condition — not indexed, symbol not found, no results, file not in
  index, offset past end, ambiguous file — returns **success-shaped guidance**. One or two
  `isError` responses early and an agent abandons the tool for the rest of the session; this
  is the single most load-bearing rule in Phase 5.
- **Single source of tool guidance.** The MCP `server-instructions`
  (`ServerInfo.instructions`) are the **one** place agent-facing guidance lives. Do not
  duplicate advice into tool descriptions, README, or CLI help — a second copy drifts and the
  drifted copy is the one the agent reads.
- **Dynamic-dispatch coverage is end-to-end or not at all** (PRD §8.2, inherited from
  Phase 3). A half-bridged flow reveals a hop the agent then Reads — worse than no bridge.
  Phase 4 *renders* those bridges (the Flow section, the `dynamic:` arrows); it must never
  render a partial one as if it were complete.
- **Never tell the agent to Read.** No output string in `selene-context` or `selene-mcp` may
  suggest Read/Grep as the way to see indexed code — the sole exceptions are the **staleness**
  and **degraded** banners (where Read genuinely *is* the right move) and the not-indexed
  guidance. Truncation notes say "run another `selene_explore` with the specific names — do
  NOT Read these files."
- **Wire fidelity is a contract.** Tool names/descriptions/schemas, the annotations object,
  the instructions strings, the banner texts, the `<n>\t<line>` numbering (no padding,
  trailing empty line kept), the `**\`` file-section prefix, and the
  `[[selene-explore-summary]]` sentinel + post-truncation substitution are byte-for-byte
  contracts. Tests assert the bytes, not the intent.
- **Determinism.** Same graph + same query ⇒ byte-identical output. No wall-clock, no
  `HashMap` iteration order in anything that reaches output (`IndexMap`/`BTreeMap` where
  order is observable; TS relies on `Map` insertion order in grouped output — port that as
  `IndexMap`). Float sort comparators keep the TS epsilon ties. `to_locale_string()` has no
  Rust equivalent: **pin one thousands-separator formatting** (`1,234`) in a helper + a test.
- **Errors are collected, never thrown.** A failure inside one section of an explore response
  degrades that section, never the tool call. `Result::Err` out of `selene-graph` /
  `selene-context` is a genuine store malfunction only.
- **No `unwrap`/`expect` outside `#[cfg(test)]`.** House idiom: `#[allow(clippy::unwrap_used)]`
  only on a `Regex::new` over a compile-time literal, with (a) a justification comment and
  (b) a test that exercises the regex. Prefer `LazyLock<Regex>` for the static tables.
- **`S: GraphStore`-generic.** `GraphStore` is not `dyn`-safe (RPITIT) — thread the type
  parameter through `QueryManager<S>`, `ContextBuilder<S>`, `SeleneMcp<S>`. Never
  `Box<dyn GraphStore>`. (If a tool handler ever *needs* erasure, `trait-variant` is the
  sanctioned escape, per the `GraphStore` docs — do not re-litigate `async fn` in traits.)
- **Env vars carry the `SELENE_` prefix.** Ported knobs: `SELENE_MCP_TOOLS`,
  `SELENE_EXPLORE_LINENUMS`, `SELENE_ADAPTIVE_EXPLORE`, `SELENE_RANK_NO_MULTITERM`,
  `SELENE_MCP_DEBUG`. **Dropped, with the daemon (Phase 6):** `*_NO_DAEMON`,
  `*_DAEMON_INTERNAL`, `*_CATCHUP_GATE_TIMEOUT_MS`, `*_STARTUP_HANDSHAKE_TIMEOUT_MS`,
  `*_PPID_POLL_MS`, `*_QUERY_POOL_SIZE`, `*_DAEMON_IDLE_TIMEOUT_MS`. `*_WASM_RELAUNCHED` and
  `*_NO_UPDATE_CHECK` are obsolete/Phase-8.
- **Tasks are completable by a fresh subagent in one session.** Each names its Files and
  Interfaces, is **TDD** (write the ported contract test first, watch it fail, then
  implement), and ends in **one conventional commit**. `cargo fmt && cargo clippy
  --all-targets && cargo test` green before every commit.

### The lesson this plan is written against

Four seams in this project shipped with green unit tests and **no production caller**: import
resolution (dead for 3 commits — *all* cross-file resolution, every language), the four
project singletons, all five synthesizers, and the batch driver itself. Every one was caught
by a gate against an independent baseline; **not one** was caught by a unit test — because

> **a test double injects what a stub fails to load, and a seam returning "nothing found" is
> indistinguishable from a seam that works and found nothing.**

Consequences that are binding on this plan, not advisory:

1. **The binary is wired in Task 14, not Task 19.** `selene serve --mcp` exists before a
   single tool handler does, so every handler lands into a **live production path**. A tool
   that is written but never listed, or listed but never dispatched, is the same bug class.
2. **Every gate drives the production entry point against a real store.** Task 13 builds its
   fixture graph by running the **real** `Indexer` + `resolve_and_persist_batched` — never by
   hand-inserting nodes. Task 20 runs the **real binary** over a **real repo**.
3. **A "no results" assertion is not a test.** Any test whose passing state is an empty
   `Vec`/empty string must be paired with a positive control on the same fixture that proves
   the pipeline can produce a non-empty result at all.

---

## File structure

```
crates/selene-graph/
  Cargo.toml                selene-core, selene-db (trait), indexmap, serde/serde_json,
                            thiserror; dev: insta, tempfile, tokio, selene-db/SurrealStore,
                            selene-extract, selene-resolve
  src/lib.rs                [T2 creates / T13 polishes] crate docs, ledger, re-exports
  src/error.rs              [T2] GraphError (thiserror) wrapping selene_db::Error
  src/query.rs              [T2] QueryManager<S>: stats, files, file_dependents, project meta
  src/symbols.rs            [T3] find_all_symbols, find_symbol_matches, matches_symbol,
                            grouping by (file_path, qualified_name), RUST_PATH_PREFIXES
  src/adjacency.rs          [T4] callers/callees/impact/path wrappers + kind whitelists
  src/source.rs             [T4] get_code(node), file text access, validate_path_within_root,
                            line-numbering (`<n>\t<line>`), config-leaf key-only rendering

crates/selene-context/
  Cargo.toml                selene-core, selene-db (trait), selene-graph, regex, indexmap,
                            serde/serde_json, thiserror; dev: insta, tempfile, tokio,
                            selene-db/SurrealStore, selene-extract, selene-resolve
  src/lib.rs                [T5 creates / T13 polishes] crate docs, ledger, re-exports
  src/error.rs              [T5] ContextError
  src/stopwords.rs          [T5] the ~100-word stopword list, extract_symbols_from_query
  src/relevance.rs          [T5/T6] find_relevant_context scoring pipeline → Subgraph
  src/builder.rs            [T7] ContextBuilder<S>: build_context, get_code, call paths
  src/formatter.rs          [T7] format_context_as_markdown / _as_json, LOW_CONFIDENCE_MARKER
  src/budgets.rs            [T8] explore_budget, ExploreOutputBudget, explore_output_budget,
                            normalize_query_spelling, truncate_to_ceiling, truncate_output
  src/flow.rs               [T9] build_flow_from_named_symbols (seeding, BFS, spine, dyn links)
  src/boundaries.rs         [T10] scan_dynamic_dispatch, blank_string_contents,
                            build_dynamic_boundaries, polymorphic boundaries
  src/explore/mod.rs        [T11 creates] the handle_explore pipeline (12 steps, ordered)
                            ⚠ SHARED SEAM — see sequencing
  src/explore/rank.rs       [T11] glue, file scoring, RWR (compute_graph_relevance),
                            change-surface rescue, relevance gate, sort
  src/explore/render.rs     [T12] whole-file rule, clustering, adaptive skeletonization,
                            focused view, gap markers, blast radius
  src/node_view.rs          [T12] the `node` tool's data+render half (file-mode Read parity,
                            symbol-mode details/trail/outline). ⚠ PHASE 4, not Phase 5: the
                            Phase 4 gate (T13) snapshots it, and a gate cannot snapshot a
                            component that does not exist yet. Task 17 is the thin MCP
                            handler over it.

crates/selene-mcp/
  Cargo.toml                rmcp 2.2 (transport-io), schemars, tokio, selene-core, selene-db
                            (trait), selene-graph, selene-context, serde/serde_json, thiserror;
                            dev: insta, tempfile, selene-db/SurrealStore, selene-extract,
                            selene-resolve
  src/lib.rs                [T14 creates] crate docs, ledger, re-exports, serve_stdio
  src/instructions.rs       [T14] SERVER_INSTRUCTIONS + SERVER_INSTRUCTIONS_NO_ROOT_INDEX
                            (verbatim; the ONE place agent guidance lives)
  src/server.rs             [T14 creates] SeleneMcp<S>: ServerHandler, get_info, tool_router
                            ⚠ SHARED SEAM — see sequencing
  src/tools.rs              [T15] the 7 tool defs, schemas, annotations, visibility gating
  src/handlers/explore.rs   [T16] the explore tool handler
  src/handlers/node.rs      [T17] the node tool handler
  src/handlers/query.rs     [T18] search / callers / callees / impact / files / status
  src/errors.rs             [T19] isError classification, input caps, not-indexed guidance
  src/banners.rs            [T19] staleness / degraded / worktree-mismatch banner formatting

crates/selene/src/main.rs   [T14] clap: `selene index [path]`, `selene serve --mcp [--path]`
                            (the FULL 22-subcommand CLI stays Phase 6)

tests (per crate, integration):
  selene-graph/tests/query_test.rs, symbols_test.rs, adjacency_test.rs, source_test.rs
  selene-context/tests/relevance_test.rs, budgets_test.rs, flow_test.rs, boundaries_test.rs,
    explore_rank_test.rs, explore_render_test.rs, context_format_test.rs
  selene-context/tests/explore_snapshot_gate.rs      [T13] THE PHASE 4 GATE
  selene-context/tests/snapshots/                    [T13] insta snapshots
  selene-mcp/tests/tools_test.rs, unindexed_test.rs, initialize_test.rs, iserror_test.rs
  selene-mcp/tests/dogfood_gate.rs                   [T20] THE PHASE 5 GATE (the milestone)
  tests/fixtures/context/<project>/…                 [T13 owns; T5–T12 contribute projects]
  docs/benchmarks/2026-07-phase45-explore.md         [T13/T20] gate results
```

---

## ⚠ Task sequencing — the shared seams

Files touched by more than one task are listed here. Tasks that touch the same file are
**strictly sequential** — never dispatch two of them to parallel subagents or worktrees.
(Phase 2 lost a day to five agents colliding on one dispatch ladder; Phase 3's table is why
Phase 3 didn't.)

| Shared file | Tasks that modify it | Rule |
|---|---|---|
| `selene-graph/src/lib.rs` | **2** (creates), 3, 4, 13 | Append-only, one `mod`/`pub use` line per task; 13 does the facade+ledger pass. |
| `selene-graph/src/query.rs` | **2** (creates: `QueryManager<S>` + stats/files), **3** (symbol methods delegate), **4** (adjacency/source delegate) | STRICTLY SEQUENTIAL 2 → 3 → 4. `QueryManager` is the single struct all three extend; 2 lays down the impl block and the method stubs with `// TODO(Task N)`. |
| `selene-context/src/lib.rs` | **5** (creates), 6–13 — **and nothing after 13** | Append-only, one line per task; **13 does the facade+ledger pass and it must be the LAST task that touches this crate**. That is why the node view is Task 12 (Phase 4) and not Task 17: a Phase-5 task adding a module here would leave 13's ledger stale by construction. If a later task ever needs to touch `selene-context`, the ledger pass moves with it. |
| `selene-context/src/relevance.rs` | **5** (creates: symbol extraction + scoring passes 1–4), **6** (passes 5–11 + BFS + trims) | STRICTLY SEQUENTIAL 5 → 6. 5 lays the full ordered pass list down with stubbed steps. |
| `selene-context/src/explore/mod.rs` | **11** (creates the 12-step pipeline with every step present as a named stub), **12** (fills step 11: rendering), **9/10** (steps 10's flow + boundary calls are *called from* here — 11 wires them) | STRICTLY SEQUENTIAL 9 → 10 → 11 → 12. 11 never re-orders the pipeline; 12 fills exactly one step. |
| `selene-context/src/budgets.rs` | **8** (creates), **12** (reads only — never adds a tier) | 8 must land before 11/12 can size anything. |
| `selene-mcp/src/lib.rs` | **14** (creates), 15–19 | Append-only; 19 does the facade+ledger pass. |
| `selene-mcp/src/server.rs` | **14** (creates: `SeleneMcp<S>`, `get_info`, empty `#[tool_router]`), **15** (the 7 tool declarations), **16/17/18** (each fills its handler's body via `src/handlers/*`), **19** (wraps dispatch with the error/banner layer) | STRICTLY SEQUENTIAL 14 → 15 → {16, 17, 18} → 19. **16/17/18 each add one `#[tool]` method** to the one router — running two in parallel merges into a lost method. Prefer sequential; if parallelized, they must be re-serialized at merge. |
| `crates/selene/src/main.rs` | **14** (creates: `index` + `serve --mcp`), **20** (gate drives it — read-only) | 14 blocks 20. Phase 6 owns every other subcommand — do not add one here. |
| `crates/selene-db/src/store.rs` (+ `store_impl.rs`) | **2** (ONLY if the spike, Task 1, finds a missing primitive) | Any `GraphStore` addition is a **wire-contract change**: re-run `cargo test -p selene-db -p selene-extract -p selene-resolve` before the commit. Default expectation is **zero** additions — the trait was designed for these consumers. |
| `tests/fixtures/context/` | **13** owns the tree + its manifest; **5–12** contribute projects | A task adds a project directory; only 13 adds a manifest row + snapshot. **ONE corpus.** Two corpora would mean two truths and a gate certifying the wrong one. |

**Parallelizable after their blocker lands:** T5/T6's `stopwords.rs`, T8's `budgets.rs`, T9's
`flow.rs`, T10's `boundaries.rs`, T12's `node_view.rs` (independent of T12's `render.rs` — they
share no state, which is why T12 may be committed as two commits) are each a **fresh file** —
only their one-line hook into a shared pipeline is sequential. T16/T17/T18's `handlers/*.rs` files are
task-private; only their `#[tool]` method registration in `server.rs` collides.

**⚠ Layering.** `selene-context` depends on `selene-graph`; `selene-mcp` depends on both.
**No reverse edge, ever** — a `selene-graph` that reaches into `selene-context` for a budget
is the cycle that ends with the budget logic smeared across three crates. `selene-mcp` owns
**only**: tool schemas, dispatch, error classification, banners, instructions. Every
ranking/flow/budget/render decision lives in `selene-context` as a pure function over the
graph API (map §Rust port notes: "the flow builder, RWR, budgets, and cluster assembly could
live in `selene-context` … with `selene-mcp` owning only schemas, dispatch, banners, and
error classification").

---

## Deliberately deferred (each with its phase and its reason)

Naming these here is what stops a task from half-porting one.

- **The daemon, the proxy, the worker query-pool, the PPID/liveness watchdogs, the
  startup-handshake timeout, the socket-path candidate walk → Phase 6** (`selene-cli` +
  daemon). They are Node-process-model workarounds; a single static Rust binary with tokio
  collapses most of `daemon.ts`/`proxy.ts`/`query-pool.ts`. Phase 5 ships **direct stdio
  mode only** — which is what the gate needs. Keep the socket-path candidate walk (#997
  ExFAT/WSL2 fallback) and the version-hello handshake **for Phase 6**, do not port them now.
- **The file watcher, the staleness/degraded state, the catch-up gate → Phase 6**
  (`selene-sync`). Task 19 **does** port the banner *formatting* + the substring-matching
  logic against a `PendingFiles` provider trait whose Phase-5 impl returns empty — but that
  provider is exactly the inert-seam shape, so Task 19 must ship it with a test that feeds a
  **non-empty** fake and asserts the banner bytes. The wiring to a real watcher is Phase 6's.
- **`selene_status` as an 8th tool → Phase 6** (maintainer ruling, 2026-07-13 — **not** an open
  question). The map lists 8 tool defs; the roadmap scopes Phase 5 to **7** (explore, node,
  search, callers, callees, impact, files). `status` reports daemon/journal/watcher state that
  does not exist until Phase 6. The not-indexed case is carried by **success-shaped guidance in
  every tool** (Task 19), which is where it belongs — no tool is added to deliver a message the
  other seven already deliver.
- **The update-notice append to `instructions`** (`initializeInstructions(base, notice)`) →
  Phase 8 (with telemetry/upgrade). Port the function shape, pass `None`.
- **`format_subgraph_tree`** (`formatter.ts`) — the map flags it as **unused by MCP** (CLI/
  legacy only). **Do not port it in Phase 4.** Phase 6 ports it if the CLI needs it.
- **Roots protocol (`roots/list`), `workspaceFolders`, monorepo `projectPath` reach-through
  → Phase 6.** Phase 5 resolves the project from `--path` / cwd via the nearest `.selene/`
  walk-up. The **`projectPath` tool argument and its schema stay** (Task 15) — the
  no-default-project instructions variant depends on it — but the reach-through *cache* and
  the per-call re-walk (#926/#925) are Phase 6.
- **HTTP/SSE transport → post-v1.** stdio only.

---

## Coordination points — ALL FOUR RATIFIED (maintainer, 2026-07-13). Do NOT re-open.

No open questions remain. A task that finds itself wanting to revisit one of these is
misreading the plan; the answer is here.

1. **Tool visibility follows TS exactly: `explore` is the ONLY default-visible tool.** All seven
   are implemented and callable; six are hidden behind `SELENE_MCP_TOOLS`. The map is the parity
   contract, and TS ships this way for a measured reason: an agent facing seven tools reaches for
   the wrong one, and the product bet is that **`explore` answers in one call**. Task 15 carries
   the mechanism *and the reason*. (Note the corroboration: the verbatim instructions text says
   *"There is a single tool"* — it is only true under this default. The two move together.)
2. **No eighth tool. `selene_status` stays deferred to Phase 6.** The not-indexed case is carried
   by success-shaped guidance in **every** tool (Task 19) — not by a tool that exists to announce
   it.
3. **Branding: port the instructions verbatim, modulo a mechanical rename table.**
   `SERVER_INSTRUCTIONS` is ported **verbatim in structure and wording**, applying exactly this
   table and **nothing else** — not a word of guidance, not a reordering, not an "improvement":

   | TS | Selene |
   |---|---|
   | `Codegraph` / `CodeGraph` / `codegraph` (prose) | `Selene` / `selene` |
   | `codegraph_<tool>` | `selene_<tool>` |
   | `.codegraph/` | `.selene/` |
   | `codegraph init` | `selene index` |
   | `[[codegraph-explore-summary]]` | `[[selene-explore-summary]]` |
   | `a SQLite knowledge graph` | `an embedded knowledge graph` (the one **factual** fix) |

   **Why this is a rule and not a preference:** the server-instructions are the single source of
   agent-facing guidance and they were **tuned against real agent behavior**. A well-meant
   rewrite is exactly how that tuning is lost — silently, with every test still green, and the
   only symptom is an agent that starts reaching for `Read` again. Task 14 keeps the rename as a
   **test-asserted table** (the TS original lives in a fixture; the test applies the pairs and
   asserts byte-equality with `SERVER_INSTRUCTIONS`), so the diff against TS stays reviewable
   line by line, forever.
4. **Dogfood repos: THREE, and the third is ≥5000 files — NOT optional.** Measured 2026-07-13,
   `../codegraph` is 311 source files and SeleneCode 165, so **both sit in the `<500` tier**. A
   two-repo gate would drive `explore_budget == 1` and the small output tiers **only** — the
   ≥5000-file tiers (where the relationship/completeness meta-text switches on) and the **"3–5
   calls on a large repo"** half of the sufficiency invariant would be *unit-tested but never
   driven*. **That is precisely the inert-seam class this project has paid for four times: a code
   path that compiles, tests green, and is never exercised by anything real.** A gate that only
   drives the easy tier tests the easy half of the product. Task 20 therefore carries a third
   repo (**Django or VS Code** — both already in the TS A/B corpus, so the question set is
   reusable) with its own `must_contain_symbols`, its own zero-Read assertions, and the "3–5
   calls" bound **measured there**, not assumed from a unit test.

---

## Tasks

<!-- Task bodies follow. Each is one commit. Phase 4 = Tasks 1–13; Phase 5 = Tasks 14–20. -->

### Task 1: Spike — de-risk `rmcp` 2.2, the `GraphStore` query surface, and the fixture rig

**Files:** Create: `crates/selene-mcp/tests/spike_rmcp.rs`,
`crates/selene-graph/tests/spike_store_surface.rs`. Modify: root `Cargo.toml`
(`[workspace.dependencies]`: `rmcp` 2.2 with features, `schemars`), `crates/selene-mcp/Cargo.toml`.

**Interfaces:** none — throwaway knowledge, kept as two smoke tests. **Every finding is
written into a comment block at the top of the spike file** and, where it changes a later
task, **into this plan** (edit the task; do not silently diverge).

Front-loaded because Phases 4 and 5 are built on two things nobody has checked: an SDK the
roadmap explicitly flags for API churn ("2.x had breaking API churn from the 0.x/1.x era —
copy patterns from the repo's current examples, not old blog posts"), and a `GraphStore` trait
that was designed for these consumers but never *driven* by one.

- [ ] **`rmcp` 2.2: prove the whole handshake, from the real crate.** Add the dep, then write
  a test that stands a trivial `ServerHandler` up over stdio (or the in-memory duplex the SDK
  provides) and drives `initialize` → `tools/list` → `tools/call`. Record, in the comment
  block, the **exact** shapes of: `ServerHandler::get_info()` → `ServerInfo` (does
  `instructions: Option<String>` exist as documented?); `#[tool_router]` / `#[tool_handler]` /
  `#[tool]` (what do they expand to — an inherent method? a trait impl? what is the receiver?);
  how a tool's **input schema** is derived (`schemars` version and derive requirements); how a
  tool returns **text content** and how it sets an **error result** (`CallToolResult::error`?
  an `is_error` field? an `Err` return?); the `protocol_version` constant the SDK sends.
- [ ] **The isError question, answered against the SDK — this is the #1 risk.** Our hardest
  invariant is that a recoverable condition returns a **success-shaped** result. Determine
  concretely: if a `#[tool]` method returns `Err(...)`, does rmcp map it to a JSON-RPC
  **error** (`-32603`) or to a `CallToolResult { is_error: true }`? They are **not** the same
  wire shape and an agent treats them differently. Write down the exact call that produces
  `{content:[{type:'text',text}], isError:false}` and the exact call that produces
  `isError:true` — Task 19 is built on this and cannot be written until it is known. If rmcp
  cannot express one of the two, **record it and surface it** before Task 14.
- [ ] **Tool listing without an open index (adoption doc P2).** Confirm `tools/list` can be
  answered from a static table with **no** store handle — i.e. `get_info()` and the tool router
  do not require the server's generic `S` to have opened a database. If the SDK forces the
  handler to be constructed with its state up front, record the workaround (an
  `Option<Store>`-shaped state) — Task 14 needs it, because **tools must be listed at an
  un-indexed root** (#964) and the handshake must answer **before** any heavy init (#172).
- [ ] **`GraphStore` surface audit — what the handlers need vs. what exists.** Against
  `crates/selene-db/src/store.rs`, map every method the map's §Public interface lists
  (`getStats`, `searchNodes`, `getNodesByName`, `getNodesByNamePrefix`, `getNodesInFile`,
  `getNode`, `getCallers`/`getCallees`, `getIncomingEdges`/`getOutgoingEdges`,
  `getImpactRadius`, `getCode`, `getChildren`, `getFiles`, `getFileDependents`,
  `getProjectRoot`, `getProjectNameTokens`) onto a trait method — or onto **"composed in
  `selene-graph`"**, or onto **"MISSING"**. Write the table into the spike comment. Known
  suspects to resolve explicitly:
  - **`getFiles()` → `{path, language, nodeCount}[]`.** `all_files()` returns `FileRecord`s —
    does a `FileRecord` carry a node count? If not: compose it (one `get_nodes_by_file` per
    file is O(files) round-trips — **measure it on a 5k-file index** before accepting it) or
    add a store method. Record the cost.
  - **`getProjectNameTokens()`** (explore's PascalCase overload bias, #720 excludes project-name
    tokens) — where do project name tokens come from? (`.selene/` dir name? `Cargo.toml`?
    `package.json`?) Decide and record; Task 11 consumes it.
  - **`getCode(nodeId)`** — the source slice comes from **disk**, not the DB (the DB has no
    body text). Confirm `Node` carries `start_line`/`end_line`/`file_path` sufficient to slice,
    and that `end_line` is inclusive. Task 4 depends on the answer.
  - **RWR needs an undirected adjacency over `{calls, references, extends, implements,
    overrides, instantiates, returns, type_of, imports}` on an already-bounded node set** —
    `edges_between(ids, kinds)` gives exactly that in one call. Confirm it returns **parallel
    edges** (it does per its docs) and that 200 ids is a sane batch. Time it.
- [ ] **The fixture rig — prove the real pipeline runs in a test.** In
  `spike_store_surface.rs`, build a temp `SurrealStore`, run the **real** `selene_extract::Indexer`
  over a tiny 3-file fixture, then the **real** `selene_resolve::resolve_and_persist_batched`,
  and assert the graph is non-empty (nodes > 0 **and** cross-file edges > 0). This exact rig is
  what Tasks 13 and 20 are built on; if it is awkward, factor it now into a shared
  `tests/common/mod.rs` helper (`index_fixture(dir) -> SurrealStore`) and say so in the plan.
  ⚠ The cross-file-edge assertion is the point: a rig that indexes but never resolves produces
  a graph where explore finds symbols and **no flow** — green tests, dead product.
- [ ] **Perf sanity.** Time `index_fixture` on this repo (`crates/`, ~200 files) end-to-end.
  If it is > 30 s, Task 13/20's gates need a cached index and this plan needs to say so.
- [ ] Commit: `chore(mcp): spike rmcp 2.2 API shape + GraphStore query-surface audit`

### Task 2: `selene-graph` — crate skeleton + `QueryManager<S>`: stats, files, project meta

**Files:** Create: `crates/selene-graph/src/{error.rs, query.rs}`; rewrite
`crates/selene-graph/src/lib.rs`; `crates/selene-graph/Cargo.toml`;
`crates/selene-graph/tests/query_test.rs`, `crates/selene-graph/tests/common/mod.rs` (the
shared fixture rig from Task 1). ⚠ `src/query.rs` is a shared seam — this task **creates** it
and lays down the full `impl` block with **every** later method present as a named stub.

**Interfaces:**
```rust
// error.rs
#[derive(Debug, thiserror::Error)]
pub enum GraphError { Store(#[from] selene_db::Error), PathRefusal { path: String }, Io(..) }
pub type Result<T> = std::result::Result<T, GraphError>;

// query.rs — THE query surface every upper layer talks to. Thin: traversal is already in SurrealQL.
pub struct QueryManager<S: GraphStore> { store: S, root: PathBuf }
impl<S: GraphStore> QueryManager<S> {
    pub fn new(store: S, root: PathBuf) -> Self;
    pub fn root(&self) -> &Path;
    pub async fn stats(&self) -> Result<GraphStats>;                    // delegates
    pub async fn file_count(&self) -> Result<u64>;                      // drives the budgets
    pub async fn files(&self) -> Result<Vec<FileInfo>>;                 // {path, language, node_count}
    pub async fn file_dependents(&self, path: &str) -> Result<Vec<String>>;
    pub async fn file_dependencies(&self, path: &str) -> Result<Vec<String>>;
    pub async fn project_name_tokens(&self) -> Result<Vec<String>>;     // #720 (see Task 1)
    pub async fn is_indexed(&self) -> Result<bool>;                     // file_count > 0
    // stubs filled by Task 3 (symbols) and Task 4 (adjacency/source) — see sequencing
}
pub struct FileInfo { pub path: String, pub language: String, pub node_count: u64 }
```

- [ ] **`is_indexed` is the not-indexed seam.** It must distinguish "no `.selene/` at all"
  (the caller's walk-up already failed — that is `selene-mcp`'s job) from "`.selene/` exists,
  zero files" (this method: `Ok(false)`). Both are **success-shaped** at the tool layer; never
  return `Err`.
- [ ] **`files()`** — per Task 1's finding. If it composes node counts client-side, cap the
  work: sort by path, and document the cost in the method docs.
- [ ] **`project_name_tokens()`** — per Task 1's decision. Tokenize the project name on
  `[-_ .]` and camelCase boundaries; lowercase; dedupe; deterministic order.
- [ ] **Path normalization (#426).** Every path-taking method normalizes: strip a leading
  `./`, treat `/` and `.` and `""` as "the root", convert `\` to `/`. Port the four cases from
  `mcp-files-path-normalization.test.ts` as unit tests here (the *filter* lives in the `files`
  tool, but the normalizer belongs at this layer so every caller shares it).
- [ ] TDD, **against a real store** (the Task 1 rig, `tests/common/mod.rs`): index a 3-file
  fixture; assert `file_count == 3`, `files()` returns them sorted with correct languages and
  **non-zero** node counts, `file_dependents` of a file that another imports is non-empty
  (positive control — this is the assertion that proves the rig actually resolved), and
  `is_indexed()` is `false` on a fresh empty store, `true` after indexing.
- [ ] Commit: `feat(graph): QueryManager over GraphStore — stats, files, project metadata`

### Task 3: `selene-graph` — symbol resolution: `find_all_symbols`, `find_symbol_matches`, grouping

**Files:** Modify: `src/query.rs` (fill the symbol stubs — **strictly after Task 2**). Create:
`src/symbols.rs`, `tests/symbols_test.rs`.

**Interfaces:**
```rust
// symbols.rs — the two DIFFERENT symbol lookups. They are deliberately not unified.
pub const RUST_PATH_PREFIXES: [&str; 3] = ["crate", "super", "self"];
pub struct SymbolGroup { pub file_path: String, pub qualified_name: String, pub nodes: Vec<Node> }
impl<S: GraphStore> QueryManager<S> {
    /// callers/callees/impact path (map §callers/callees/impact).
    pub async fn find_all_symbols(&self, name: &str) -> Result<Vec<Node>>;
    /// node-mode path (map §handleNode). Qualified-with-no-exact-match returns [] (#173).
    pub async fn find_symbol_matches(&self, symbol: &str) -> Result<Vec<Node>>;
    pub async fn group_by_definition(&self, nodes: Vec<Node>) -> Vec<SymbolGroup>;  // (#764)
}
pub fn matches_symbol(node: &Node, query: &str) -> bool;
```

**These two functions differ, and the difference is intentional (map §Suspicious/dead code) —
port it, do not "fix" it.** `find_all_symbols`'s `exact_matches.len() <= 1` branch silently
falls back to `results[0]` (a fuzzy hit) even for a **qualified** miss; `find_symbol_matches`
does **not** (#173 fixed node-mode only). Keep the divergence and **write a test that pins
it**, with a comment citing this line — otherwise the next reader "unifies" them and changes
callers/callees behavior.

- [ ] **`find_all_symbols`** (map §callers/callees/impact): FTS `limit 50`; **colon-fallback**
  — if the query contains `:` and FTS came up empty, re-search by the tail after the last
  `:`/`::`; the nix option-path special case (`^[a-z][\w'-]*(?:\.[\w'-]+)+$`) is **wave-2** —
  leave the branch as a comment naming Phase 8.
- [ ] **`find_symbol_matches`** (map §handleNode): a **bare** name uses
  `get_nodes_by_name` — a **full enumeration**, deliberately *not* FTS, because FTS's cut
  drops overloads (the map's note: "full enumeration beats FTS cut, tokio `poll`") — with
  generated files sorted **last**. A **qualified** name (contains `.` or `::`) uses FTS then
  filters with `matches_symbol`; **if no exact match, return `[]`** (#173 — do not fall back).
- [ ] **`matches_symbol`**: suffix match on the `::`-joined qualified name, **or** file-path
  segment containment, with `RUST_PATH_PREFIXES` (`crate`/`super`/`self`) stripped from the
  query first.
- [ ] **`group_by_definition`** (#764): group by `(file_path, qualified_name)`; groups keep
  **first-seen order** (`IndexMap`) — grouped output ordering is observable.
- [ ] TDD against the real store: a repo with the **same method name in 3 classes** →
  `find_all_symbols("handle")` returns all 3, `group_by_definition` yields 3 groups in stable
  order; `find_symbol_matches("Foo.handle")` returns exactly 1; `find_symbol_matches("Nope.gone")`
  returns `[]` **and not a fuzzy hit** (#173); `find_all_symbols("Nope.gone")` **does** fall
  back (the pinned divergence). A generated file's symbol sorts last.
- [ ] Commit: `feat(graph): symbol resolution — find_all_symbols, find_symbol_matches, grouping`

### Task 4: `selene-graph` — adjacency + source access (`get_code`, Read-parity line numbering)

**Files:** Modify: `src/query.rs` (fill the adjacency/source stubs — **strictly after Task 3**).
Create: `src/adjacency.rs`, `src/source.rs`, `tests/adjacency_test.rs`, `tests/source_test.rs`.

**Interfaces:**
```rust
// adjacency.rs — thin wrappers; the traversal is SurrealQL's (GraphStore docs).
impl<S: GraphStore> QueryManager<S> {
    pub async fn callers(&self, id: &str, depth: u32) -> Result<Vec<NeighborEntry>>;
    pub async fn callees(&self, id: &str, depth: u32) -> Result<Vec<NeighborEntry>>;
    pub async fn impact(&self, id: &str, depth: u32) -> Result<Subgraph>;
    pub async fn find_path(&self, from: &str, to: &str, kinds: &[EdgeKind])
        -> Result<Option<Vec<(Node, Option<Edge>)>>>;
    pub async fn type_hierarchy(&self, id: &str) -> Result<Subgraph>;
    pub async fn children(&self, id: &str) -> Result<Vec<Node>>;
    pub async fn outgoing(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>>;
    pub async fn incoming(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<NeighborEntry>>;
}
// source.rs — the ONLY place this workspace reads source text off disk for output.
pub const CONFIG_LEAF_LANGUAGES: &[&str];       // #383 — key-only rendering
pub fn validate_path_within_root(root: &Path, path: &str) -> Result<PathBuf>; // #527
pub fn number_lines(text: &str, start_line: usize) -> String;   // `<n>\t<line>`, NO padding
impl<S: GraphStore> QueryManager<S> {
    pub async fn get_code(&self, node_id: &str) -> Result<Option<String>>;      // slice by lines
    pub async fn read_file_slice(&self, path: &str, offset: usize, limit: usize)
        -> Result<FileSlice>;                                                   // Read parity
}
pub struct FileSlice { pub path: String, pub text: String, pub total_lines: usize,
                       pub truncated: bool }
```

- [ ] **Clamps, ported verbatim** (map §callers/callees/impact): `limit` clamped **1–100**;
  impact `depth` clamped **1–10**, **default 2**. Clamping, not rejecting — an out-of-range
  limit is not an error.
- [ ] **`validate_path_within_root` (#527) is the ONE `isError` source in Phase 4.** It
  canonicalizes and refuses any path escaping the root (`..`, absolute, symlink out) →
  `GraphError::PathRefusal`. **Every** disk read in this workspace goes through it. Test the
  refusals: `../../etc/passwd`, an absolute path, a symlink pointing out of the root.
- [ ] **Read parity is byte-for-byte** (`node-file-view.test.ts`, 9 cases): line numbering is
  `` format!("{n}\t{line}") `` with **no padding** (assert the exact string
  `1000\t  const v998` — the four-digit number is *not* right-aligned against the shorter
  ones), and a **trailing empty line is kept**. `offset`/`limit` semantics identical to the
  agent's `Read` tool (1-based offset; `limit` lines; offset past end → **success-shaped**
  message, not an error). Default `limit` = **2000** lines, char budget **38 000**
  (`CHAR_BUDGET`). ⚠ The map notes `CHAR_BUDGET=38000` predates the 24K/25K host
  externalization cap, so file-view results **can** exceed the inline cap — that is **known and
  accepted** for Read parity. Do not "fix" it to 24K; write the comment.
- [ ] **Config-leaf languages (#383)** render **keys only**, never values — the secret guard.
  A `.env`/`json`/`yaml`/`toml`/`properties` leaf node's body is replaced by its key list.
  Test with a fixture containing `API_KEY=sk-live-abc` and assert the value **never** appears
  in any output.
- [ ] **`get_code`** slices `[start_line, end_line]` inclusive off disk (per Task 1's finding),
  through `validate_path_within_root`. A node whose file has since been deleted → `Ok(None)`,
  never `Err`.
- [ ] TDD against the real store + real files (positive control first: `get_code` on a known
  function returns its **actual body text**, not an empty string — an empty-string pass is the
  inert-seam signature).
- [ ] Commit: `feat(graph): adjacency wrappers + source access with Read-parity numbering`

### Task 5: `selene-context` — skeleton, query-symbol extraction, and the scoring pipeline (passes 1–4)

**Files:** Create: `crates/selene-context/{Cargo.toml, src/error.rs, src/stopwords.rs,
src/relevance.rs}`; rewrite `src/lib.rs`; `tests/relevance_test.rs`. ⚠ `src/relevance.rs` is a
shared seam — this task **creates** it and lays the **full ordered pass list** down, with every
later pass present as a named stub (`// pass N: … TODO(Task 6)`). Task 6 fills them and
**never re-orders**.

**Interfaces:**
```rust
// stopwords.rs
pub const STOPWORDS: &[&str];   // the ~100-word list (map §ContextBuilder.findRelevantContext)
pub fn extract_symbols_from_query(q: &str) -> Vec<String>;  // camelCase/snake_case/SCREAMING/
                                                            // acronym/dotted/lowercase≥3, minus stopwords
// relevance.rs
pub const HIGH_VALUE_NODE_KINDS: &[NodeKind];   // excludes import/export/parameter/…
pub struct FindOptions { pub search_limit: usize, pub traversal_depth: u32, pub max_nodes: usize,
                         pub min_score: f64, pub node_kinds: Vec<NodeKind> }
impl Default for FindOptions { /* search_limit:3, traversal_depth:1, max_nodes:20, min_score:0.3,
                                  node_kinds: HIGH_VALUE_NODE_KINDS */ }
pub struct ScoredNode { pub node: Node, pub score: f64, pub term_hits: usize,
                        pub distinctive: bool }
pub struct RelevantContext { pub subgraph: Subgraph, pub confidence: Confidence }
pub enum Confidence { High, Low }
pub(crate) async fn score_candidates<S: GraphStore>(q: &QueryManager<S>, terms: &[String],
    opts: &FindOptions) -> Result<Vec<ScoredNode>>;   // passes 1–4 (this task)
```

**The ordered pass list (map §`ContextBuilder.findRelevantContext`) — ORDER IS BEHAVIOR.**
Lay all 11 down now; this task implements 1–4.

1. **Symbol extraction** from the query (this task).
2. **Exact-name lookup** with a **co-location boost**: `+20` per *extra* co-named symbol in the
   same file (this task).
3. **TitleCase prefix search** over definition kinds: `+15 + brevity` (this task).
4. **Per-term FTS** with a **multi-term boost**: `+5` per extra hit; **test-file dampen ×0.3**;
   **dominant-file core-dir boost `+25`** when the dominant file's edge count is ≥ **3×** the
   next (this task).
5. Term-group co-occurrence rerank *(Task 6)*.
6. CamelCase-boundary LIKE matches *(Task 6)*.
7. Compound ≥2-term LIKE matches *(Task 6)*.
8. Sort → slice `search_limit * 3` → min-score filter → resolve imports→definitions → cap to
   `search_limit` roots *(Task 6)*.
9. Confidence: **low iff** ≥2 terms **and** no result with 2 term hits or a distinctive name
   *(Task 6)*.
10. Type-hierarchy expansion (budget `max_nodes/4`, **2 passes** for siblings) *(Task 6)*.
11. BFS both directions per root (limit `max_nodes/roots`) → trim to `max_nodes` (roots +
    neighbors prioritized) → per-file cap `max(5, ceil(max_nodes*0.2))` → non-prod cap
    `max(3, ceil(max_nodes*0.15))` → edge recovery via `edges_between` over
    `{calls, extends, implements, references, overrides}` *(Task 6)*.

- [ ] **`extract_symbols_from_query`**: the six patterns (camelCase, snake_case, SCREAMING,
  acronym, dotted, lowercase ≥3 chars), minus the stopword list. Copy `STOPWORDS` **verbatim**
  from `../codegraph/src/context/index.ts` (the one sanctioned read). A stopword-only query
  yields **zero** terms — and that must be a **success-shaped** empty result upstream, not an
  error.
- [ ] **"brevity" and "pathScore" are shared scoring helpers** used by several passes — define
  them once here (`fn brevity(name: &str) -> f64`, `fn path_score(path: &str) -> f64`) and
  port their exact formulas from the TS; every later pass reuses them.
- [ ] **Determinism**: candidate lists are insertion-ordered (`IndexMap`); sorts are stable and
  tie-break on `(score desc, file_path, start_line, name)` so equal scores never reorder
  between runs.
- [ ] TDD (unit, against a small **real** store): each of passes 2–4 in isolation with a
  fixture that makes exactly that pass fire; the co-location `+20`, the multi-term `+5`, the
  test-dampen `×0.3` and the dominant-file `+25` are asserted **numerically**, not by ordering
  (an ordering assertion passes even when a weight is 10× wrong).
- [ ] Commit: `feat(context): query-symbol extraction + relevance scoring passes 1–4`

### Task 6: `selene-context` — `find_relevant_context`: rerank, LIKE passes, confidence, expansion, BFS, trims

**Files:** Modify: `src/relevance.rs` (fill passes 5–11 — **strictly after Task 5**). Create:
`tests/relevance_expansion_test.rs`.

**Interfaces:**
```rust
impl<S: GraphStore> ContextBuilder<S> {   // declared here, constructed in Task 7
    pub async fn find_relevant_context(&self, query: &str, opts: &FindOptions)
        -> Result<RelevantContext>;
}
```

- [ ] **Pass 5 — term-group co-occurrence rerank** (the subtlest weight in the pipeline; copy
  it exactly): ≥2 groups → `× (1 + 0.5n)`; **distinctive** exact matches are **exempt**;
  **common-word** exact matches → `× 0.3`; everything else → `× 0.6`.
- [ ] **Pass 6 — CamelCase-boundary LIKE**: `limit 200`, score `8 + brevity + path_score`,
  then scaled `× (1 + term_count) + 30 * (term_count - 1)`.
- [ ] **Pass 7 — compound ≥2-term LIKE**: score `10 + 20*(terms-1) + path_score + brevity`.
- [ ] **Pass 8 — the cut**: sort → slice `search_limit * 3` → `min_score` filter → **resolve
  imports to definitions** (an `import` node is never a root; follow it to what it imports) →
  cap to `search_limit` roots.
- [ ] **Pass 9 — confidence**: `Low` **iff** the query has ≥2 terms **and** no surviving result
  has 2 term hits **or** a distinctive name. `Low` is what raises `LOW_CONFIDENCE_MARKER`
  (Task 7) — it is a *signal*, never an error.
- [ ] **Pass 10 — type-hierarchy expansion**: budget `max_nodes / 4`; **two** passes so
  *siblings* (not just ancestors) are pulled in. Use `QueryManager::type_hierarchy`.
- [ ] **Pass 11 — BFS + trims + edge recovery**, in this order: BFS **both directions** per root
  (per-root limit `max_nodes / roots`) → trim to `max_nodes` prioritizing **roots then
  neighbors** → per-file cap `max(5, ceil(max_nodes * 0.2))` → non-prod cap
  `max(3, ceil(max_nodes * 0.15))` → **edge recovery**: one `edges_between(surviving_ids,
  {calls, extends, implements, references, overrides})` call. ⚠ Edge recovery is not optional
  polish — the trims delete nodes, and without it the surviving subgraph keeps edges pointing
  at nodes that are gone, which renders as a broken flow.
- [ ] **Explore's own defaults differ from `buildContext`'s** and both are contracts:
  `find_relevant_context` from **explore** is called with `{search_limit: 8, traversal_depth: 3,
  max_nodes: 200, min_score: 0.2}`; `build_context`'s defaults are `{max_nodes: 20,
  max_code_blocks: 5, max_code_block_size: 1500, search_limit: 3, traversal_depth: 1,
  min_score: 0.3}`. Pin both in tests.
- [ ] TDD: a 2-term query where one term is common and one distinctive → the distinctive one
  survives the ×0.3 (pass 5); a query matching only via CamelCase boundary (pass 6) returns a
  hit; the per-file cap actually caps (fixture: 40 symbols in one file, `max_nodes: 20`);
  **edge recovery restores an edge the trim would have orphaned** (positive control: assert the
  subgraph's edge count is > 0 and every edge's endpoints are both present).
- [ ] Commit: `feat(context): find_relevant_context — rerank, LIKE passes, expansion, BFS, trims`

### Task 7: `selene-context` — `ContextBuilder`, formatters, call paths, low-confidence handoff

**Files:** Create: `src/builder.rs`, `src/formatter.rs`, `tests/context_format_test.rs`.
Modify: `src/lib.rs` (one `pub use` line).

**Interfaces:**
```rust
// builder.rs
pub struct ContextBuilder<S: GraphStore> { q: QueryManager<S> }
impl<S: GraphStore> ContextBuilder<S> {
    pub fn new(q: QueryManager<S>) -> Self;
    pub fn query(&self) -> &QueryManager<S>;          // explore/node reach the graph through this
    pub async fn build_context(&self, input: &TaskInput, opts: &BuildOptions)
        -> Result<TaskContext>;
    pub async fn get_code(&self, node_id: &str) -> Result<Option<String>>;   // → QueryManager
}
pub struct TaskInput { pub query: String, pub files: Vec<String> }
pub struct TaskContext { pub subgraph: Subgraph, pub entry_points: Vec<Node>,
    pub related: Vec<Node>, pub code_blocks: Vec<CodeBlock>, pub call_paths: Vec<CallPath>,
    pub confidence: Confidence }
// formatter.rs
pub const LOW_CONFIDENCE_MARKER: &str = "### ⚠️ Low-confidence match";
pub fn format_context_as_markdown(c: &TaskContext) -> String;
pub fn format_context_as_json(c: &TaskContext) -> String;
```

- [ ] **Markdown section headers are a contract** (map §Wire/contract surfaces): `## Code
  Context`, `### Entry Points`, `### Related Symbols` (**≤10**), `### Code`, `## Call paths`,
  and `LOW_CONFIDENCE_MARKER` verbatim (it is a dependency-free **sentinel** shared with the
  MCP layer — keep it in its own tiny module so `selene-mcp` can match on it without pulling
  the formatter).
- [ ] **`## Call paths`** (DFS over the subgraph's `calls` edges): `MAX_HOPS = 6`, budget
  **2000** chars, chains of **≥3 nodes** with **≥2 roots**, keep **≤3** non-subpath chains,
  and synthesized hops annotated `` →[callback via `x` @file:line] ``. The annotation comes from
  `metadata.synthesizedBy` — the Phase 3 contract.
- [ ] **`format_subgraph_tree` is NOT ported** (map §Suspicious/dead code: unused by MCP). Write
  the one-line reason in `formatter.rs`'s module docs so the next reader doesn't "restore" it.
- [ ] TDD: markdown shape (headers present, `Related Symbols` capped at 10); JSON round-trips
  (`serde_json::from_str::<TaskContext>` of the output equals the input); a low-confidence
  result emits the marker **byte-for-byte**; a call path with a synthesized hop renders the
  `→[callback via …]` annotation; **truncation** at `max_code_block_size` keeps the block
  parseable.
- [ ] Commit: `feat(context): ContextBuilder + markdown/JSON formatters + call paths`

### Task 8: `selene-context` — the explore **budgets** (the monotonicity invariant lives here)

**Files:** Create: `src/budgets.rs`, `tests/budgets_test.rs`. Modify: `src/lib.rs`.
This task is **independent** of Tasks 5–7 and may run in parallel with them. It **blocks**
Tasks 11 and 12.

**Interfaces:**
```rust
pub fn explore_budget(file_count: u64) -> u32;   // the CALL budget (how many explores to make)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExploreOutputBudget {
    pub max_output_chars: usize, pub default_max_files: usize, pub max_chars_per_file: usize,
    pub gap_threshold: usize, pub max_symbols_in_file_header: usize,
    pub max_edges_per_relationship_kind: usize, pub include_relationships: bool,
    pub include_additional_files: bool, pub include_completeness_signal: bool,
    pub include_budget_note: bool, pub exclude_low_value_files: bool,
}
pub fn explore_output_budget(file_count: u64) -> ExploreOutputBudget;
pub fn normalize_query_spelling(query: &str) -> String;
pub fn truncate_to_ceiling(text: &str, b: &ExploreOutputBudget) -> String;  // the final hard cap
pub fn truncate_output(text: &str) -> String;                               // generic, 15_000
pub const MAX_OUTPUT_LENGTH: usize = 15_000;
pub const FILE_SECTION_PREFIX: &str = "**`";   // the unique greppable truncation boundary
```

**`explore_budget(file_count)` — copy verbatim:** `<500 → 1`, `<5000 → 2`, `<15000 → 3`,
`<25000 → 4`, else `5`.

**`explore_output_budget(file_count)` — the tier table, copied verbatim from
`maps/mcp-context.md` §Budgets:**

| tier | max_output_chars | default_max_files | max_chars_per_file | gap_threshold | max_symbols_in_file_header | max_edges_per_rel_kind | relationships / additional / completeness / budget_note | exclude_low_value_files |
|---|---|---|---|---|---|---|---|---|
| `<150` | 13000 | 4 | 3800 | 7 | 5 | 4 | **all false** | true |
| `<500` | 18000 | 5 | 3800 | 8 | 6 | 6 | **all false** | true |
| `<5000` | 24000 | 8 | 6500 | 12 | 10 | 10 | **all true** | false |
| `<15000` | 24000 | 8 | 7000 | 15 | 15 | 15 | **all true** | false |
| `≥15000` | 24000 | 8 | 7000 | 15 | 15 | 15 | **all true** | false |

- [ ] **The monotonicity invariant, machine-checked.** A test sweeps file counts `0..30_000`
  and asserts `max_chars_per_file` is **non-decreasing** — a larger tier never gets a smaller
  per-file budget. Also assert `max_output_chars` is non-decreasing. This test is the invariant;
  it is not a nice-to-have. (Why the caps sit at ~24K: the host **externalizes** inline results
  above ~25K — a bigger budget doesn't reach the agent, it just becomes a file the agent has to
  open. That is the anti-Read invariant biting from the other side. Write that in the docs.)
- [ ] **Boundary off-by-ones** (`explore-output-budget.test.ts`): assert at **exactly** 149/150,
  499/500, 4999/5000, 14999/15000, 24999/25000 — for **both** functions. Tier boundaries are `<`,
  not `≤`.
- [ ] **The final hard ceiling** (`truncate_to_ceiling`): `min(round(max_output_chars * 1.5),
  25_000)`. Cut at the **last `\n**\`` file-section boundary** if that boundary lies **past 50%**
  of the ceiling (otherwise a hard cut), then append **verbatim**:
  `... (output truncated to budget; the source above is complete and verbatim — treat it as
  already Read. For any area not covered, run another selene_explore with the specific names —
  do NOT Read these files.)`
  ⚠ Note what that sentence does: it truncates **without** sending the agent to Read. Any
  rewording that suggests Read violates a Global Constraint.
- [ ] **`truncate_output`** (the generic one): `MAX_OUTPUT_LENGTH = 15_000`, cut at the last
  newline **if it lies past 80%** of the cap.
- [ ] **`normalize_query_spelling`** — two regexes, ported exactly:
  - `fn/3` → `fn`: `\b([A-Za-z_][\w@]*)\/(\d{1,3})(?=$|[\s,()[\]/])` (the Erlang/Elixir arity
    spelling).
  - `mod:fn` → `mod.fn`: `(^|[\s,()[\]])(?!(?:kind|lang|language|path|name):)([a-z_][\w@]*):([A-Za-z_][\w@]*)(?=$|[\s,()[\]])`
    — note the **negative lookahead** protecting the field prefixes (`kind:`, `lang:`, `path:`,
    `name:`). The `regex` crate has **no lookahead**: either use `fancy-regex` (already a
    workspace dep from Phase 3) or restructure as a match-then-reject. Whichever — a test must
    cover `kind:function` (unchanged) and the Lua `mod:fn` case (rewritten).
- [ ] TDD: the whole tier table asserted **value by value** (a table test over all 5 tiers × 11
  fields — 55 assertions; a "shape" test here is worthless), the monotonicity sweep, the
  boundary off-by-ones, the ceiling cut at a file boundary vs. a hard cut, and the two
  normalization regexes incl. the field-prefix and Lua cases.
- [ ] Commit: `feat(context): explore budgets — tiers, monotonicity, truncation ceiling`

### Task 9: `selene-context` — `build_flow_from_named_symbols` (the Flow section, and the spine)

**Files:** Create: `src/flow.rs`, `tests/flow_test.rs`. Modify: `src/lib.rs`.
Fresh file; may run in parallel with Tasks 5–8. **Blocks Task 11** (which calls it) and
**Task 12** (whose adaptive gate reads its output).

**This is the single highest-value function in the product.** It is what turns "here is some
source" into "here is the path from X to Y, including the dynamic-dispatch hops grep can't
follow" — the thing an agent cannot get from `Read`. Everything else is retrieval; this is
the answer.

**Interfaces:**
```rust
pub struct Flow {
    pub text: String,                       // the rendered "**Flow (…)**" section, or empty
    pub path_node_ids: IndexSet<String>,    // THE SPINE — Task 12's adaptive gate reads it
    pub named_node_ids: IndexSet<String>,   // every callable the agent named (superset of spine)
    pub unique_named_node_ids: IndexSet<String>,   // tokens with ≤3 defs (drives spares)
    pub spine_call_sites: HashMap<String, u32>,    // spine node → line of its call to the next hop
}
pub async fn build_flow_from_named_symbols<S: GraphStore>(
    q: &QueryManager<S>, query: &str, subgraph: &Subgraph) -> Result<Flow>;
```

**Constants — verbatim (map §`handleExplore` step 10):** `MAX_HOPS = 7`; frontier cap
**1500**; **`MAX_BRIDGE = 1`** (at most **1** consecutive *unnamed* hop); seeds ≤ **8**;
`named` cap **40**; ≤ **6** defs per token; `dynNamed` cap **12**, per-token **4**; tokens
resolved ≤ **16**; renders only if the chain has **≥3 nodes**; dynamic-dispatch links ≤ **6**.

- [ ] **Token resolution + the co-naming disambiguation.** Resolve ≤16 query tokens. An
  **ambiguous simple name** (>3 callable defs) is kept **only if** its container segment (the
  2nd-last segment of the qualified name split on `::` or `.`) appears in the query's segment
  pool. This is what stops `execute` (110 defs) from seeding the world.
- [ ] **`MAX_BRIDGE = 1` is the whole reason the flow is usable — port it exactly.** BFS over
  `calls` edges from ≤8 seeds, accepting only **named sinks**, allowing **at most one
  consecutive unnamed hop**. Without the cap, the BFS wanders a god-function's fan-out and the
  "flow" becomes noise. Longest chain wins.
- [ ] **`spine_call_sites`** maps each spine node → the **line of its call to the next hop**.
  Task 12 windows god-methods around exactly this line; without it, an oversized spine cluster
  has no anchor and renders the wrong 28 lines.
- [ ] **`dyn_named`**: non-callable `{constant, variable, field, property}` endpoints that carry
  a **heuristic** edge (Phase 3's `provenance: heuristic` + `metadata.synthesizedBy`) — cap 12,
  per-token 4. These are the RTK-constant-style endpoints (#687).
- [ ] **Dynamic-dispatch links**: ≤6 heuristic edges incident to named/dynNamed nodes,
  **skipping hops already on the chain**. Render arrows `   ↓ calls` and `   ↓ dynamic: …`.
- [ ] **Synth edge labels are a wire contract** (read from `metadata.synthesizedBy`):
  `callback`, `event-emitter`, `react-render`, `jsx-render`, `vue-handler`, `interface-impl`,
  `closure-collection`, `fn-pointer-dispatch`, `goframe-route`, plus the generic fallback
  `{kind.replace('-', ' ')} (dynamic dispatch)`. Compact form:
  `` dynamic: callback via `x` @file:line ``. (Phase 3 ships **four** channels — `callback`,
  `event-emitter`, `react-render`, `jsx-render` — plus the Django-ORM *resolver*; the other
  labels are Phase 8's. Port the **full** label table anyway: it is a lookup, and a missing
  label renders as garbage the day a channel lands.)
- [ ] **Section format (verbatim):** header `**Flow (call path among the symbols you queried)**`,
  numbered steps `{i}. name (file:line)`, arrows `   ↓ calls` / `   ↓ dynamic: …`.
- [ ] **Port the `named.size() > 40` break AS-IS** (map §Suspicious/dead code): it exits the
  **token loop** mid-way, so later tokens are silently unresolved. It is the shipped behavior.
  Write the comment; do not "fix" it — and pin it with a test so a future reader can't quietly
  change flow output.
- [ ] TDD, **against a real resolved graph** (the Task 1 rig — a flow test on a hand-built
  subgraph proves nothing about dispatch): a 4-hop static call chain renders with 4 numbered
  steps; a chain requiring **one** unnamed bridge renders; a chain requiring **two**
  consecutive unnamed hops does **not** (the `MAX_BRIDGE` assertion — and this is the test that
  fails loudly if someone "improves" the BFS); a chain crossing a **synthesized `callback`
  edge** renders the `↓ dynamic: callback via …` arrow (**the anti-Read payoff, asserted**); a
  2-node chain renders **nothing** (the ≥3 rule); an ambiguous token (`execute`, 5 defs) seeds
  only when co-named.
- [ ] Commit: `feat(context): build_flow_from_named_symbols — spine, bridge cap, dynamic hops`

### Task 10: `selene-context` — dynamic + polymorphic boundaries (the "where does it go" notes)

**Files:** Create: `src/boundaries.rs`, `tests/boundaries_test.rs`. Modify: `src/lib.rs`.
Fresh file; parallelizable. **Blocks Task 11.**

Purpose: when the flow **dead-ends at a dynamic call site** the graph can't bridge, explore
must still tell the agent *where the dispatch goes* — otherwise the agent Reads. This is the
"partial coverage is worse than none" invariant's escape valve: a boundary **note** is honest
about the edge of the graph; a missing hop is not.

**Interfaces:**
```rust
pub struct BoundaryMatch { pub form: String, pub label: String, pub snippet: String,
    pub line: u32, pub key: Option<String>, pub key_is_type: bool, pub more_sites: bool }
pub fn blank_string_contents(text: &str) -> String;   // blank strings+comments, KEEP offsets
pub fn scan_dynamic_dispatch(body: &str, language: &str, file_start_line: u32)
    -> Vec<BoundaryMatch>;
pub async fn build_dynamic_boundaries<S: GraphStore>(..) -> Result<Vec<String>>;   // the notes
pub async fn build_polymorphic_boundaries<S: GraphStore>(..) -> Result<Vec<String>>;
```

**Constants — verbatim (map §`handleExplore` step 10):** dynamic — `MAX_NOTES = 4`,
`MAX_SCAN = 8` bodies, `MAX_TOTAL_CHARS = 200_000`; fires **only for uncovered tokens**; scans
the **dead end first**, then unique-named-first; `boundary_candidates` probes `key`,
`on{Cap}`, `handle{Cap}`, `{key}Handler`, `handle_{key}` (for **type** keys:
`{key}Handler`, `key`), then **FTS limit 12**, a normalized-containment filter, **≤4 shown**;
handler methods match `/^(handle|handleAsync|execute|executeAsync|consume|consumeAsync|run|__invoke)$/i`.
Polymorphic — `POLY_MIN_FAMILY = 8`, `MIN_SUPPORT = 2`, `SAMPLE = 40`, `MAX_NOTES = 3`,
`MIN_IMPL = 8`, **ranked by the true graph-wide implementer count, not the sample frequency**.

- [ ] **`blank_string_contents` must preserve byte offsets** (it blanks string/comment
  *contents* in place so the regexes below can't match inside a string literal, while line
  numbers stay correct). It reuses the same idea as `selene_resolve::strip_comments_for_regex`
  — check whether that function is directly reusable (it is `pub`); if it is, **depend on it**
  rather than writing a second comment stripper. Two strippers = two behaviors = a bug.
- [ ] **`scan_dynamic_dispatch`** — the per-language form regexes (`../codegraph/src/mcp/
  dynamic-boundaries.ts` is the one sanctioned read; port its form table). Every regex runs
  **only over blanked text**. Cheap `contains` pre-gate before each expensive regex (#1235).
- [ ] **Polymorphic boundaries rank by true implementer count.** The sample (40) is how you
  *find* the family; the **graph-wide count** is how you rank it. Ranking by sample frequency
  makes the most-sampled family win instead of the biggest one — a subtle, plausible bug that
  no shape test catches. Assert it: a fixture where the sampled-most family is **not** the
  largest.
- [ ] TDD: string/comment blanking (a `dispatch(x)` inside a string literal is **not** matched,
  and the line numbers of everything after it are **unchanged**); each form regex has a positive
  and a negative case; boundary candidate probing finds `onSubmit` from key `submit`;
  `MAX_NOTES`/`MAX_SCAN`/`MAX_TOTAL_CHARS` all actually cap; the polymorphic ranking test above.
- [ ] Commit: `feat(context): dynamic + polymorphic dispatch boundaries`

### Task 11: `selene-context` — the `explore` pipeline: glue, file scoring, RWR ranking, blast radius

**Files:** Create: `src/explore/mod.rs`, `src/explore/rank.rs`, `tests/explore_rank_test.rs`.
Modify: `src/lib.rs`. ⚠ `src/explore/mod.rs` is the **top shared seam of Phase 4**: this task
**creates** it and lays the **12-step pipeline** down with every step present as a named call
(step 11, rendering, is a stub → `// TODO(Task 12)`). Task 12 fills exactly that step and
**never re-orders**. Requires Tasks 6, 8, 9, 10.

**Interfaces:**
```rust
pub struct ExploreInput { pub query: String, pub max_files: Option<usize>,
                          pub include_code: bool }
pub struct ExploreOutput { pub text: String }     // the whole tool response body
pub async fn handle_explore<S: GraphStore>(cb: &ContextBuilder<S>, input: &ExploreInput)
    -> Result<ExploreOutput>;
// rank.rs
pub(crate) fn compute_graph_relevance(adj: &Adjacency, seeds: &[String]) -> HashMap<String, f64>;
pub(crate) const RWR_ALPHA: f64 = 0.25;
pub(crate) const RWR_ITERATIONS: usize = 25;
```

**The 12-step pipeline (map §`handleExplore`) — ORDER IS BEHAVIOR. Lay all 12 down.**

1. `normalize_query_spelling` (Task 8) → `find_relevant_context(query, {search_limit: 8,
   traversal_depth: 3, max_nodes: 200, min_score: 0.2})`. **Empty ⇒ success-shaped**
   `No relevant code found for "{query}"` — never an error, never a suggestion to Read.
2. **Glue**: callers/callees of root nodes **in already-surfaced files**, cap **60**.
3. **Named-symbol seeding** (this task): tokenize on `[\s,()[\]]+`; strip file extensions;
   keep tokens ≥3 chars matching `^[A-Za-z_$][\w$]*(?:(?:::|\.)[\w$]+)*$`; **max 16**. Per
   token: qualified → `find_all_symbols`, bare → `get_nodes_by_name` (**full enumeration**,
   deliberately not FTS). Filter to `CALLABLE = {method, function, component, constructor}` and
   **non-test** paths (`/(^|\/)(tests?|specs?|__tests__|testdata|mocks?|fixtures?)\//i` or
   `\.(test|spec)\.[a-z]+$`); sort substantive-first.
   **NL-stopword guard:** a bare lowercase word seeds **only when co-named** (another query
   token is a symbol in the same file); a **shape-precise** token (contains `[._$]`, `::`, `/`,
   or is camelCase, or leads with a capital) seeds **unconditionally**.
   **≤3 defs** → all picked; the tier is the most-substantive **+** co-named defs with callers
   ≥ **25%** of max. **>3 defs** → keep overloads whose file or qualified name contains a
   **PascalCase** query token (`^[A-Z][A-Za-z0-9]{3,}`, **project-name tokens excluded** —
   #720, `QueryManager::project_name_tokens`), cap **4**; else the single most-substantive.
4. **File scoring**: named seed **+50**, entry **+10**, connected-to-entry **+3**, else **+1**.
   **Skip** import/export nodes and **config-leaf** nodes (#383 secret guard). Keep files with
   score **≥3**.
5. **Test/low-value hard-exclude on ALL tiers** unless the query matches
   `\b(test|tests|testing|spec|verify|verifies)\b/i` — **and only if ≥2 non-test files remain**.
   `is_low_value` regexes: `/\/(tests?|__tests?__|spec)\//`, `_test\.go$`, `test_.*\.py$`,
   `_spec\.rb$`, `\.(test|spec)\.[jt]sx?$`, `\bicons?\b`, `\bi18n\b`, …
   (Why *all* tiers: `adaptive-explore-sizing.md` refinement 3 — one test file ate 2.3 KB of
   Django's 28 KB budget.)
6. **RWR ranking** (`compute_graph_relevance`): **undirected** adjacency over
   `{calls, references, extends, implements, overrides, instantiates, returns, type_of, imports}`
   (one `edges_between` call over the bounded node set), restart **α = 0.25** to seeds,
   **25** power iterations, **dangling mass is kept** (not redistributed). **Central files** =
   top-2 by mass with **≥1** term hit.
7. **Change-surface rescue (#1064)**: signature types (`references|type_of|returns` edges) of
   tier seeds whose file is **buried** (graph mass < `max_graph × 0.06` **AND** term hits < 2)
   are **injected**, score capped at **45**, **force-kept** and tiered.
8. **Relevance gate**: keep a file iff mass ≥ `max_graph × 0.06` **OR** central **OR** entry
   **OR** change-surface **OR** ≥2 distinct term hits. **Never prune below 2 files.**
9. **Sort**: named-seed files first → **corroborated** (entry/central **and** ≥2 term hits;
   disabled by `SELENE_RANK_NO_MULTITERM=1`) → graph mass (epsilon `max_graph × 0.01`) → term
   hits → low-value last → generated last → score → node count.
10. **Flow** (Task 9's `build_flow_from_named_symbols`) + **boundaries** (Task 10). This task
    **wires** them; it does not reimplement them.
11. **Source rendering** — *stub here* → **Task 12**.
12. **Header + blast radius + truncation**: the header sentinel `[[selene-explore-summary]]` is
    emitted first, then **replaced after truncation** with `Found N symbols across M files.`
    counted from the **surviving** sections (#1046 — count what survived, not what was
    gathered). Blast-radius section: `ROOT_CAP = 5`, `FILE_CAP = 4`, and
    `⚠️ no covering tests found` when there are none. Then `truncate_to_ceiling` (Task 8).
    **Never tells the agent to Read.**

- [ ] **Leading strings, verbatim:** `**Exploration: {query}**`, and the source preamble
  `> The code below is the **verbatim, current on-disk source** …`. File section header is
  `` **`path`** — {suffix} `` (**`FILE_SECTION_PREFIX = "**\`"`** — a unique greppable
  truncation boundary; **no ATX headings anywhere** in explore output, #778 — a `#` heading
  breaks the truncation boundary scan).
- [ ] **RWR is a pure function over an already-bounded subgraph** — do not push it into
  SurrealQL. It runs over ≤200 nodes; it belongs in code (map §Rust port notes says exactly
  this). Assert convergence determinism: same input ⇒ bit-identical mass map.
- [ ] TDD, against a **real** indexed fixture: named-seed scoring (+50 dominates); the
  NL-stopword collision case (`explore-nl-stopword-collision.test.ts` — a query word that is
  also a symbol name in an unrelated file does **not** seed unless co-named); corroboration
  ranking (`explore-corroboration-ranking.test.ts`); the relevance gate never prunes below 2
  files; blast radius shows `⚠️ no covering tests found` on a repo with no tests; the `#1046`
  header counts **surviving** sections after a forced truncation (fixture: enough files to
  overflow the ceiling — assert the count matches what's actually in the text).
- [ ] Commit: `feat(context): explore pipeline — seeding, file scoring, RWR ranking, blast radius`

### Task 12: `selene-context` — source rendering (clustering, adaptive skeletonization) **+ the node view**

**Files:** Create: `src/explore/render.rs`, `src/node_view.rs`, `tests/explore_render_test.rs`,
`tests/node_view_test.rs`. Modify: `src/explore/mod.rs` (fill **step 11 only** — strictly after
Task 11), `src/lib.rs`.

⚠ **The node view lives in Phase 4, not Phase 5.** It is the second half of what the Phase 4
gate (Task 13) snapshots — `explore` **and** `node` — and a gate cannot snapshot a component
that lands four tasks later; the executor would stub it and snapshot nothing, which is
precisely the Phase-2 failure this plan is written against. Phase 5's Task 17 is then a **thin
MCP handler** over what this task builds. The two halves also share every rendering primitive
(line numbering, config-leaf key-only, `validate_path_within_root` — all from Task 4), so
splitting them across phases would fork them.

**If this task feels too large for one session, split it at the file boundary** —
`render.rs` (explore) and `node_view.rs` (node) are independent modules with no shared state —
and commit twice, `feat(context): source rendering …` then `feat(context): node view …`. Both
must land **before** Task 13. What must **not** happen is the node view slipping into Phase 5.

**This is where the budget is actually spent**, and where `adaptive-explore-sizing.md` earned
its numbers (OkHttp/Django flipped from *costlier than grep* to ~14–17% cheaper, median **0
reads**). Read that doc's **"Dead ends"** section before starting — six approaches are already
disproven, two of them plausible enough to re-invent.

**Interfaces:**
```rust
pub(crate) struct RenderCtx<'a> { pub budget: &'a ExploreOutputBudget, pub flow: &'a Flow,
    pub remaining: usize, /* … */ }
pub(crate) async fn render_file<S: GraphStore>(..) -> Result<String>;
pub(crate) const MIN_SIBLINGS: usize = 3;
pub(crate) const WHOLE_FILE_MAX_LINES_CENTRAL: usize = 280;
pub(crate) const WHOLE_FILE_MAX_LINES_OTHER: usize = 220;
pub(crate) const OVERSIZE_SPINE_LINES: usize = 200;
pub(crate) const SPINE_WINDOW: usize = 28;
pub(crate) const GAP_MARKER: &str = "\n\n... (gap) ...\n\n";   // language-neutral
pub(crate) const ENVELOPE_KINDS: &[NodeKind];  // file, module, class, struct, interface, enum,
                                               // namespace, protocol, trait, component
```

- [ ] **Budget-90% soft stop for *incidental* files.** A **necessary** file (it defines the
  entry, a spine node, or a unique-named symbol) renders **past** the cap; an incidental one
  stops at 90%.
- [ ] **The adaptive gate — all four conditions, in order** (`SELENE_ADAPTIVE_EXPLORE`, default
  **on**). A file is skeletonized **iff**: (1) a **spine exists**; (2) the file is **off** the
  spine; (3) it is a **polymorphic sibling** (its class `implements`/`extends` a supertype with
  **≥3** implementers — from **real** `implements`/`extends` edges, **not** synth edges, and
  **cached**); (4) it is **not spared** — where **spared = the agent named a (near-)unique
  callable in it**, **UNLESS** the file itself **defines** a ≥3-impl supertype (the family-file
  **override**).
  ⚠ Two of these are inverted-looking and were each a measured regression:
  - **Uniqueness-aware spare** (refinement 3): only a **unique** named callable spares a file.
    `as_sql` has **110 defs** — naming it must not keep every backend variant full.
    Use `Flow::unique_named_node_ids` (Task 9), not `named_node_ids`.
  - **Defining a supertype is an OVERRIDE, not a spare** (dead end #5): sparing family files
    regressed Django to **9% costlier**. A base+subclasses file is huge and Read-anyway;
    skeletonizing it **frees** budget for the siblings.
- [ ] **Per-symbol focused view** (refinement 2 — whole-file skeletons were too coarse): in a
  collapsed file, emit **full bodies** by priority — spine (0), unique-named (1), family-base
  co-named (2) — greedily under `body_cap = max_chars_per_file * 1.5`; everything else as
  **signature lines** (scan **≤4 lines forward** past decorators/annotations to find the line
  that actually names the symbol; `SIG_MAX = max(12, max_symbols_in_file_header * 2)`).
- [ ] **Whole-file rule**: ≤ `WHOLE_FILE_MAX_LINES` (central **280** / other **220**) **and**
  ≤ the char cap (central `min(remaining, max_chars_per_file * 1.5)`, other
  `max_chars_per_file * 3`) → dump the **whole file**.
- [ ] **Else clustering**: build ranges from nodes (**skip envelope containers** spanning >50%
  of the file — `ENVELOPE_KINDS`) plus edge-source lines (importance **2**). Importance: entry
  **10**, named **9**, glue **6**, connected **3**, else **1**. Merge ranges within
  `gap_threshold`. Rank **spine-first** → max-importance → density → score → span. Select under
  `file_budget = min(max_chars_per_file, remaining)`; **spine clusters** may run up to
  `SPINE_CEILING = min(max_chars_per_file * 2.5, remaining)`; **always take the top-1** cluster.
  **Named-cluster survival** (refinement 4): inject agent-named method defs into a file's
  clusters even when the gather missed them, rank them at importance **9**, and cap selection at
  `min(per-file, remaining-total)` so a high-importance named cluster is not source-order
  trimmed.
- [ ] **Oversize spine cluster** (> `OVERSIZE_SPINE_LINES = 200` lines) is **windowed** to
  `SPINE_WINDOW = 28` lines around **the call site** (`Flow::spine_call_sites` — Task 9) plus a
  ≤5-line signature head. This is what makes a god-method readable instead of budget-eating.
- [ ] **Context padding** 3 lines; gap marker `GAP_MARKER` (**language-neutral** — no `// …`);
  line numbers `{n}\t{line}` (`SELENE_EXPLORE_LINENUMS=0` disables).
- [ ] TDD — port `adaptive-explore-sizing.test.ts` (**7 cases**) plus the render contracts:
  the named-callable **spare** (an OkHttp-shaped fixture: `RealCall` off-spine, trips the
  sibling signal via a 9-impl mixin, agent named a unique callable in it → **kept full**); the
  supertype-family **override** (a Django-`compiler.py`-shaped fixture: named **and** defines a
  ≥3-impl supertype → **skeletonized**); a 1:1 interface→impl pair (**not** a sibling → stays
  full — `MIN_SIBLINGS = 3` matters); an off-spine 3-impl sibling → skeletonized; the whole-file
  rule at exactly 220/221 lines; an oversize spine cluster windows to 28 lines **around the call
  site** (assert the window contains the call line); the gap marker is language-neutral; line
  numbers on/off.
  ⚠ **Every skeleton test must also assert the file's *symbols* are still listed in the
  header** — a skeleton that hides what's in the file sends the agent to Read, which is the
  regression this whole mechanism exists to prevent.
**The node view — interfaces:**
```rust
// src/node_view.rs — the `node` tool's data+render half. selene-mcp owns dispatch ONLY.
pub const CONTAINER_NODE_KINDS: &[NodeKind];   // class, struct, interface, trait, protocol,
                                               // enum, namespace, module
pub const TRAIL_CAP: usize = 12;
pub const BODY_BUDGET: usize = 12_000;
pub const HARD_CAP: usize = 16;     // max bodies packed in a multi-match view
pub const LIST_CAP: usize = 20;     // max listed candidates
pub struct NodeArgs { pub symbol: Option<String>, pub file: Option<String>,
    pub line: Option<u32>, pub include_code: bool, pub offset: Option<usize>,
    pub limit: Option<usize> }
pub async fn node_view<S: GraphStore>(cb: &ContextBuilder<S>, args: &NodeArgs) -> Result<String>;
```

- [ ] **File mode** (`file` **without** `symbol`) is **byte-for-byte Read parity**
  (`node-file-view.test.ts`, 9 cases — port all 9). Resolution: exact → suffix → substring;
  **ambiguous** → list ≤25 candidates (**success-shaped**). `offset`/`limit` semantics identical
  to `Read`; **offset past end** → success-shaped message. Config-leaf languages return **keys
  only** (#383). Every read goes through `validate_path_within_root` (#527) — the **one**
  `isError` source here. The slicing itself is Task 4's `read_file_slice`; do not re-implement it.
- [ ] **Symbol mode**: `find_symbol_matches` (Task 3) → `file`/`line` narrowing (a `line`
  prefers the **containing** definition, else the **nearest start**) → **1 match** → details +
  optional body; a **container** kind (`CONTAINER_NODE_KINDS`) returns a **member outline**
  instead of a body; plus the **trail** (`TRAIL_CAP = 12` callees/callers with `file:line`,
  **synth edges annotated** with their `synthesizedBy` label — the Phase 3 contract). **N
  matches** with `include_code` → **all** bodies packed under `BODY_BUDGET = 12_000` /
  `HARD_CAP = 16`.
- [ ] **Node-mode output markers** (verbatim): node lists render as
  `- name (kind) - file:line — via label`.
- [ ] TDD (node view): the 9 Read-parity cases **including** `1000\t  const v998` (unpadded) and
  the trailing-newline case; ambiguous-file listing is success-shaped; a container returns an
  outline, **not** a 2000-line body; the trail annotates a synthesized hop; a `line` inside a
  method picks the **method**, not its class.
- [ ] Commit: `feat(context): source rendering — clustering, whole-file rule, adaptive sizing`
      (and, if split per the note above, a second: `feat(context): node view — Read-parity file
      mode + symbol mode`)

### Task 13: **PHASE 4 GATE** — insta snapshots of explore/node output vs the TS shapes

**Files:** Create: `crates/selene-context/tests/explore_snapshot_gate.rs`,
`crates/selene-context/tests/snapshots/` (insta), `tests/fixtures/context/` (the **one**
corpus) + `tests/fixtures/context/projects.toml` (the manifest),
`docs/benchmarks/2026-07-phase45-explore.md`. Modify: `crates/selene-graph/src/lib.rs`,
`crates/selene-context/src/lib.rs` (the facade + public-interface **ledger** pass, mirroring
`selene-extract`/`selene-resolve`: every item in the map's §Public interface is either ported
or **explicitly deferred with its phase and reason**).

**The gate must fail loudly on WRONG CONTENT, not only on a changed shape.** Phase 2's lesson:
a snapshot that only pins structure stays green while the content is empty or wrong. So the
gate has **three** independent halves, and all three must pass.

- [ ] **Half 1 — the corpus is built by the PRODUCTION pipeline.** `projects.toml` lists each
  fixture project `{name, path, query}`. For each: run the **real** `selene_extract::Indexer`
  → the **real** `selene_resolve::resolve_and_persist_batched` → a real `SurrealStore`. **No
  hand-built graphs. No test-composed pipeline.** (This is the whole lesson of the four inert
  seams: a pipeline the test composes itself proves the library works; only driving the
  production entry point proves the product runs.) Reuse Task 1's `index_fixture` rig.
  Corpus ≥ **6** projects, covering: a small TS/React app, a Python/Django app, a Go service, a
  Rust crate, a Java/Spring app, and **one repo with a synthesized dispatch bridge** (reuse a
  `tests/fixtures/resolve/synth-*` project from Phase 3 — the flow must cross a `callback` or
  `jsx-render` edge).
- [ ] **Half 2 — the insta snapshots** (`insta::assert_snapshot!`) of the **full text** of
  `handle_explore` (Task 11/12) **and** `node_view` (Task 12 — it is in Phase 4 precisely so
  this gate can snapshot it; a gate cannot snapshot a component that lands in a later phase)
  for each project's query. Reviewed once by a human,
  then frozen. Shape *and* content: the snapshot contains the actual **source lines**, so a
  regression that empties a section shows up as a diff, not as a silently-passing shape.
- [ ] **Half 3 — the CONTENT ASSERTIONS, which is what makes the gate a gate.** Independent of
  the snapshots (a human can accept a bad snapshot; they cannot accept these), assert **on
  every project**:
  - `explore` output is **non-empty** and contains **≥1 `**\`` file section** (the positive
    control — the assertion that would have caught every one of the four inert seams).
  - Every file section contains **real source lines**, matched as `^\d+\t` — not just a header.
  - **≥1 project's** output contains the **`**Flow (call path among the symbols you queried)**`**
    section with **≥3 numbered steps** — and for the synth project, that flow contains a
    **`↓ dynamic:`** arrow. **This is the anti-Read payoff, asserted end-to-end.** If the
    resolver, the flow builder, or the render silently degrade, this line fails.
  - The **blast-radius** section is present.
  - `total_chars <= min(round(max_output_chars * 1.5), 25_000)` for the project's tier — the
    ceiling is respected on **real** output, not just in a unit test.
  - **No output string** matches `/\bRead\b|\bgrep\b/i` except inside the sanctioned banner /
    not-indexed texts (a machine-checked form of the "never tells the agent to Read"
    constraint).
  - **No secret leaks**: a fixture with `API_KEY=sk-live-…` in a config leaf → the value
    appears **nowhere** in any output (#383).
- [ ] **Budget parity vs TS.** For each project, record `file_count` → `explore_budget` and the
  full `ExploreOutputBudget` in `docs/benchmarks/2026-07-phase45-explore.md`, alongside the
  measured output size. This is the table Phase 9's A/B rerun compares against.
- [ ] **The ledger pass.** `selene-graph/src/lib.rs` and `selene-context/src/lib.rs` get the
  crate-doc treatment the other crates have: role + PRD section, the **public-interface ledger**
  (map §Public interface item → Rust item, or "deferred → Phase N, because …"), the invariants
  (anti-Read, monotonicity, never-say-Read), and a **"the seams this crate could have shipped
  inert"** note naming the positive controls that prove otherwise.
- [ ] Commit: `test(context): PHASE 4 GATE — explore/node snapshots + content assertions on real fixtures`

---

<!-- PHASE 5 — selene-mcp. Task 14 wires the REAL BINARY first, on purpose. -->

### Task 14: `selene-mcp` — rmcp server, server-instructions, **and the real binary** (`index`, `serve --mcp`)

**Files:** Create: `crates/selene-mcp/{Cargo.toml, src/instructions.rs, src/server.rs}`;
rewrite `crates/selene-mcp/src/lib.rs`; **rewrite `crates/selene/src/main.rs`** (clap);
`crates/selene-mcp/tests/initialize_test.rs`. ⚠ `src/server.rs` is Phase 5's shared seam: this
task creates it with an **empty** `#[tool_router]`; Tasks 15–19 fill it, strictly sequentially.

**The binary is wired FIRST, before a single tool handler exists.** That is deliberate: four
seams in this project shipped green-tested and unreachable. From this commit on, every handler
lands into a **live production path** that `selene serve --mcp` actually runs — and the
smoke check below is what proves it.

**Interfaces:**
```rust
// lib.rs
pub async fn serve_stdio<S: GraphStore + Clone + 'static>(state: ServerState<S>)
    -> anyhow::Result<()>;
pub struct ServerState<S: GraphStore> {
    pub project: Option<ProjectHandle<S>>,   // None = no .selene/ at the root (#964) — the
                                             // tools are STILL listed and STILL callable
    pub root_hint: PathBuf,
}
pub struct ProjectHandle<S: GraphStore> { pub root: PathBuf, pub ctx: ContextBuilder<S>,
                                          pub file_count: u64 }

// ⚠ ToolOutcome is DEFINED HERE, in Task 14 — not in Task 19, which merely classifies INTO it.
// Every handler (Tasks 16/17/18) returns it, so it must exist before the first handler lands;
// three executors each inventing their own is a three-way merge that eats two of them.
pub enum ToolOutcome {
    Text(String),         // success-shaped — isError:false. THE DEFAULT for every miss.
    Refusal(String),      // isError:true, no retry note — PathRefusal only.
    Malfunction(String),  // isError:true + the retry-once note.
}
/// Map a ToolOutcome onto the rmcp call-result shape found by the Task 1 spike.
/// Task 14 ships it with the two isError arms wired and a test pinning BOTH wire shapes;
/// Task 19 adds the classification rules (input caps, not-indexed guidance, banners) that
/// decide which arm a given condition takes. Nothing else in the crate constructs an rmcp
/// error directly.
pub fn to_call_result(o: ToolOutcome) -> CallToolResult;

// server.rs
pub struct SeleneMcp<S: GraphStore> { state: ServerState<S>, tool_router: ToolRouter<Self> }
impl<S: GraphStore + …> ServerHandler for SeleneMcp<S> { fn get_info(&self) -> ServerInfo { … } }
// instructions.rs
pub const SERVER_INSTRUCTIONS: &str;                 // root IS indexed
pub const SERVER_INSTRUCTIONS_NO_ROOT_INDEX: &str;   // root has no .selene/
pub fn initialize_instructions(base: &str, notice: Option<&str>) -> String;  // notice = None
                                                                             // until Phase 8
```

**`main.rs` — exactly two subcommands (clap 4.6 derive). The other 20 are Phase 6.**
```
selene index [PATH]            # Indexer + resolve_and_persist_batched → .selene/
selene serve --mcp [--path P]  # stdio MCP server
```
- `index`: `scan_directory` → `Indexer::index_all` → `resolve_and_persist_batched` → print
  counts. `indicatif` progress is **Phase 6** — a plain line per phase is enough here.
- `serve --mcp`: walk **up** from `--path`/cwd for the nearest `.selene/`. Found → open the
  store, build `ProjectHandle`, use `SERVER_INSTRUCTIONS`. **Not** found → `project: None` and
  `SERVER_INSTRUCTIONS_NO_ROOT_INDEX` — **the server still starts and still lists every tool**
  (#964). It does **not** index anything: *indexing is the user's decision* (a hard rule from
  the instructions text — the server must never index on its own).
- `anyhow` in the bin, `thiserror` in the libs. No daemon, no proxy, no watchdogs (Phase 6).

- [ ] **The handshake answers BEFORE any heavy init (#172).** `get_info()` must not open the
  store, run a query, or block. The store is opened once, up front, by `main.rs` — and if that
  is slow on a big index, it happens **before** the transport starts, not inside `initialize`.
  Per Task 1's finding, confirm rmcp's construction order allows this; if it does not, hold the
  store behind a `OnceCell` and open it lazily on the **first tool call**, never in `get_info`.
- [ ] **`PROTOCOL_VERSION`**: rmcp 2.2 sends its own (per Task 1). TS pinned `'2024-11-05'`.
  Take the SDK's default and **write the version into the test** so a silent SDK bump is
  visible. Do not hand-roll the JSON-RPC layer — that is what the SDK is for.
💡 **The instructions text and the tool-visibility ruling corroborate each other** — note that
the verbatim text below literally says *"There is a single tool, `selene_explore`"*. That
sentence is **only true** under the ratified explore-only default surface (Task 15). If someone
later widens the default set, this text becomes a lie to the agent — which is the single-source
-of-guidance invariant failing in the most damaging possible way. The two must move together.

- [ ] **`SERVER_INSTRUCTIONS` — port verbatim, modulo the RATIFIED rename table** (Coordination
  Point 3, maintainer 2026-07-13). The text below **is** the TS original with that table already
  applied. It is the **one** place agent guidance lives, and it was **tuned against real agent
  behavior**: do not paraphrase, do not reorder, do not "improve", do not duplicate elsewhere.
  A rewrite here fails silently — every test stays green and the only symptom is an agent that
  starts reaching for `Read` again.

````
# Selene — code intelligence over an indexed knowledge graph

Selene is an embedded knowledge graph of every symbol, edge, and file in
the workspace — pre-computed structure you would otherwise re-derive by
reading files (cached intelligence: thousands of parse/trace decisions you
don't pay to re-reason each run). Reads are sub-millisecond; the index lags
writes by ~1s through the file watcher. Reach for it BEFORE *and* while
writing or editing code — not just for questions: one call returns the
verbatim source PLUS who calls it and what it affects, so you edit with the
blast radius in view. More accurate context, in far fewer tokens and
round-trips than reading files yourself.

## One tool: selene_explore — use it instead of reading files

There is a single tool, `selene_explore`, and it is Read-equivalent. It
takes either a natural-language question or a bag of symbol/file names and
returns the **verbatim, line-numbered source** of the relevant symbols
grouped by file — the same `<n>\t<line>` shape `Read` gives you, safe to
`Edit` from — PLUS the call path among them (including dynamic-dispatch hops
like callbacks, React re-render, and JSX children that grep can't follow) and
a blast-radius summary of what depends on them.

Whether you're answering "how does X work" or implementing a change (fixing a
bug, adding a feature), call `selene_explore` before you Read. ONE call
usually answers the whole question. Selene IS the pre-built search index —
so running your own grep + read loop, or delegating the lookup to a separate
file-reading sub-task/agent, repeats work selene already did and costs more
for the same answer. A direct selene answer is typically one to a few
calls; a grep/read exploration is dozens.

## How to query

- **Almost any question — "how does X work", architecture, a bug, "what/where is X", or surveying an area** → `selene_explore` with a natural-language question or the relevant names. ONE capped call returns the verbatim source grouped by file; most often the ONLY call you need.
- **"How does X reach/become Y? / the flow / the path from X to Y"** → `selene_explore`, naming the symbols that span the flow (e.g. `mutateElement renderScene`) — it surfaces the call path among them, riding dynamic-dispatch hops, and returns their source.
- **Reading or editing a file/symbol you can name** → put its name or file path in the `selene_explore` query — it returns that current line-numbered source (safe to `Edit` from) with the call path and blast radius attached, so you don't Read it separately. For an overloaded name it returns every matching definition's body in one call.
- **Need more?** Call `selene_explore` again with more specific names — treat the source it returns as already Read.

## Anti-patterns

- **Trust selene's results — don't re-verify them with grep.** They come from a full AST parse; re-checking with grep is slower, less accurate, and wastes context.
- **Don't grep or Read first** to find or understand indexed code — ONE `selene_explore` returns the relevant symbols' source together in a single round-trip. Reach for raw `Read`/`Grep` only to confirm a specific detail selene didn't cover, or for what selene doesn't index (configs, docs).
- **Don't reconstruct a flow by hand** — name the endpoints in one `selene_explore` and it surfaces the path between them, dynamic-dispatch hops included.
- **After editing, check the staleness banner.** When a tool response starts with "⚠️ Some files referenced below were edited since the last index sync…", the listed files are pending re-index — Read those specific files for accurate content. Every file NOT in that banner is fresh, so still trust selene. A different, rarer banner — "⚠️ Selene auto-sync is DISABLED…" — means live watching stopped entirely (the whole index is frozen, not just a few files); until it's resolved, Read files directly to confirm anything that may have changed.

## Limitations

- If a tool reports a project isn't indexed (no `.selene/`), stop calling selene tools for that project for the rest of the session and use your built-in tools there instead. Indexing is the user's decision — mention they can run `selene index` if it comes up, but don't run it yourself.
- Index lags file writes by ~1 second.
- Cross-file resolution is best-effort name matching; ambiguous calls may return multiple candidates.
- No live correctness validation — that's still the compiler / test suite / linter's job. Selene supplements those with structural context they don't have.
````

- [ ] **`SERVER_INSTRUCTIONS_NO_ROOT_INDEX` — same treatment:**

````
# Selene — available (per-project; pass projectPath)

Selene is an embedded knowledge graph of a codebase's symbols, edges, and
files: one `selene_explore` call returns the verbatim, line-numbered source
of the relevant symbols PLUS the call paths between them and a blast-radius
summary — replacing a grep + Read loop with one round-trip.

This server started somewhere with no `.selene/` of its own, so there is no
default project — but the tools are available and work **per project**:

- To query a project that HAS a `.selene/` index (e.g. a service inside a
  monorepo, or a second repo), pass its path as `projectPath` to
  `selene_explore` (and any other selene tool). Selene resolves the
  nearest `.selene/` at or above that path and answers from it — for as many
  projects as you like in one session.
- For a project with no `.selene/`, use your built-in tools (Read/Grep/Glob)
  for that project. Indexing is the user's decision — don't run it yourself, but
  if it comes up they can run `selene index` in a project to enable selene
  there (a new index is picked up live, no restart).
````

⚠ Note the second text promises **`projectPath` reach-through**, which is **Phase 6**. Task 15
still ships the `projectPath` **argument and schema** (so the promise is not a lie about the
*surface*), and Task 19's not-indexed guidance is honest about what happens today. If the
maintainer prefers, trim the monorepo bullet until Phase 6 — flag it, don't decide it alone.

- [ ] **The rename is a test-asserted table**, not a hand-edit: a test holds the TS original
  (as a fixture file) and the rename pairs, applies them, and asserts the result **equals**
  `SERVER_INSTRUCTIONS` byte-for-byte. That way the diff from the TS original stays reviewable
  forever, and an accidental paraphrase fails the build.
- [ ] **`ToolOutcome` + `to_call_result` ship in THIS task**, with a test that pins **both**
  wire shapes against the real SDK: `Text` ⇒ `{content:[{type:'text',…}], isError:false}`,
  `Refusal`/`Malfunction` ⇒ `isError:true` (per the Task 1 spike — if rmcp cannot express one
  of the two shapes, that is a blocker, not a workaround). Tasks 16/17/18 consume this enum;
  Task 19 fills in *which condition maps to which arm*.
- [ ] **Smoke check — this is the anti-inert-seam proof for Phase 5, and it must be in the
  commit.** `tests/initialize_test.rs`: start the **real** `serve_stdio` over an in-memory /
  piped transport, send `initialize`, assert (a) the response arrives **before** any store
  query, (b) `instructions` is `SERVER_INSTRUCTIONS` when a `.selene/` exists and
  `SERVER_INSTRUCTIONS_NO_ROOT_INDEX` when it does not, (c) `tools/list` answers at an
  **un-indexed** root (#964), (d) `resources/list`, `resources/templates/list` and
  `prompts/list` return **empty lists**, not `-32601` (#621 — client probes must not see a
  method-not-found), and (e) `ping` → `{}`.
- [ ] Commit: `feat(mcp): rmcp stdio server + server-instructions + selene index/serve binary`

### Task 15: `selene-mcp` — the tool surface: 7 definitions, schemas, annotations, visibility gating

**Files:** Create: `src/tools.rs`, `tests/tools_test.rs`. Modify: `src/server.rs` (declare the
7 `#[tool]` methods, bodies delegating to `todo!()`-free **stubs** that return a
success-shaped "not implemented" — never a panic, never an `isError`). Strictly after Task 14.

**Interfaces:**
```rust
pub const TOOL_NAMES: [&str; 7] = ["selene_explore", "selene_node", "selene_search",
    "selene_callers", "selene_callees", "selene_impact", "selene_files"];
/// ⚠ TS parity, ratified by the maintainer (2026-07-13): the default-visible surface is
/// **`explore` ALONE**. All seven tools are IMPLEMENTED and callable; six are hidden by default.
pub const DEFAULT_MCP_TOOLS: &[&str] = &["selene_explore"];
pub const TINY_REPO_FILE_THRESHOLD: u64 = 500;
pub const TINY_REPO_TOOLS: [&str; 3] = ["selene_explore", "selene_search", "selene_node"];
pub fn static_tools() -> Vec<ToolDefinition>;  // NO engine, NO store — the P2 contract
pub fn visible_tools(state: &ServerState<..>) -> Vec<ToolDefinition>;
```

- [ ] **The annotations object, on EVERY tool** (#1018): `{readOnlyHint: true,
  destructiveHint: false, idempotentHint: true, openWorldHint: false}`. It must **survive** the
  schema clone and the dynamic-description rewrite — that is precisely what
  `mcp-tool-annotations.test.ts` catches, so port that test.
- [ ] **The default surface is `explore` ALONE — and the REASON must be written into the code,
  not just the behavior** (maintainer ruling, 2026-07-13; TS ships `DEFAULT_MCP_TOOLS =
  {'explore'}`). All seven tools are implemented, schema'd, annotated and dispatchable; six are
  simply not *listed* by default. Why: **an agent facing seven tools reaches for the wrong
  one** — it calls `search`, gets names, then Reads the files, and the entire product bet
  (one `explore` call returns the source *and* the flow *and* the blast radius, so no Read
  happens) is lost to a plausible-looking tool menu. A hidden tool costs nothing; a wrong tool
  choice costs the whole session. Write that paragraph into `tools.rs`'s module docs.
- [ ] **`SELENE_MCP_TOOLS`** (comma-separated short names, a `selene_` prefix is stripped if
  present) is the reveal mechanism: it **replaces** the default set entirely — it does not add
  to it. This is how the other six become reachable (and how Tasks 17/18's tests reach them).
- [ ] **Tiny-repo gate**: with a project open and `file_count < TINY_REPO_FILE_THRESHOLD`
  (**500**), the active set is **intersected** with `TINY_REPO_TOOLS`
  (`{explore, search, node}`) — so it can only ever *narrow* a set widened by
  `SELENE_MCP_TOOLS`, never widen the default. Port the intersection semantics exactly; the
  gate and the default are two different mechanisms and conflating them changes what an agent
  sees on a 400-file repo.
- [ ] **A call to a hidden tool is `isError: true`** ("disabled tool" — Task 19's list). Hidden
  ≠ absent: the dispatch arm exists, and refusing it loudly is what keeps a
  `SELENE_MCP_TOOLS` typo from silently degrading to "tool does nothing".
- [ ] **Explore's description is DYNAMIC**: append
  `" Budget: make at most {budget} calls for this project ({file_count} files indexed)."` —
  `{budget}` from `explore_budget(file_count)` (Task 8), `{file_count}` **thousands-separated**
  (`1,234`) by the pinned helper (there is no `toLocaleString` in Rust — Global Constraints).
- [ ] **No default project (#993)**: every tool exposing `projectPath` gets it added to
  `required` — a **pure schema clone**, never a mutation of the static table (the static table
  is shared and must stay `&'static`).
- [ ] **`static_tools()` needs no store** (adoption doc P2): `tools/list` must be answerable
  instantly, decoupled from opening the index. Test it by calling `static_tools()` with **no
  store constructed at all**.
- [ ] **Tool descriptions** are the *short* pointers; the **guidance lives in the instructions**
  (Global Constraints). Each description says what the tool returns and when to reach for it —
  and `explore` is marked **PRIMARY**, `node` **SECONDARY**, in one line each.
- [ ] TDD: **only `selene_explore` is listed by default** (the ruling — assert the list has
  length 1); `SELENE_MCP_TOOLS=explore,node,search` lists exactly 3; the tiny-repo gate
  **intersects** (with `SELENE_MCP_TOOLS` naming all 7 on a 400-file repo, the listed set is
  `{explore, search, node}`) and does **not** fire at exactly 500 files; every listed tool
  carries the annotations, and they survive the schema clone + the dynamic description; the
  `projectPath`-required variant when `project: None`; the dynamic budget line for a 3,000-file
  project reads `Budget: make at most 2 calls for this project (3,000 files indexed).`;
  `static_tools()` works with **no store constructed**.
- [ ] Commit: `feat(mcp): tool surface — 7 definitions, annotations, visibility gating`

### Task 16: `selene-mcp` — the `explore` handler (PRIMARY)

**Files:** Create: `src/handlers/explore.rs`, `src/handlers/mod.rs`. Modify: `src/server.rs`
(fill the `selene_explore` `#[tool]` body only). Strictly after Task 15.

**Interfaces:**
```rust
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ExploreArgs { pub query: String, pub max_files: Option<usize>,
                         pub project_path: Option<String> }
pub async fn handle_explore<S: GraphStore>(state: &ServerState<S>, args: ExploreArgs)
    -> ToolOutcome;   // ⚠ ToolOutcome is TASK 14's — do not redefine it here.
```

The handler is **thin**: validate → resolve the project → call
`selene_context::explore::handle_explore` → wrap. Every ranking/budget/render decision already
happened in Phase 4 (Global Constraints: layering). If this file grows past ~150 lines,
something belongs in `selene-context`.

- [ ] **`ToolOutcome` is the isError seam** (Task 19 turns it into rmcp's shape, per Task 1's
  finding). `Text` → success-shaped, **always** — including `No relevant code found for
  "{query}"`. `Refusal` → `isError` (path refusal only). `Malfunction` → `isError` + the
  retry-once note. A handler **never** constructs an rmcp error directly.
- [ ] **Not indexed** (`state.project == None`, or a `projectPath` with no `.selene/`) → the
  long **success-shaped guidance** text (Task 19 owns the exact strings). Never `isError`.
- [ ] **`max_files`** defaults to the tier's `default_max_files` and is clamped to it as a
  ceiling — a caller cannot ask for more than the budget allows.
- [ ] **`ExploreInput::include_code` has no tool argument — and that is deliberate.** The MCP
  schema (`ExploreArgs`) exposes only `query` / `max_files` / `project_path`; the handler
  **always** passes `include_code: true`. Explore *is* the verbatim source (the instructions
  promise exactly that), so a caller-suppressible body would let an agent turn the one
  anti-Read tool into a name-lister and then Read the files. The field stays on `ExploreInput`
  for tests and for `selene-cli` (Phase 6), not for the wire. Task 11's `ExploreInput` therefore
  documents `include_code: bool` with **default `true`**.
- [ ] TDD **through the real server** (not by calling the function directly — that is the inert
  seam again): drive `tools/call` with `{"name":"selene_explore","arguments":{"query":"…"}}`
  over the real transport against a real indexed fixture, and assert the response body contains
  a `**\`` file section with `^\d+\t` source lines. Then a **negative**: an unindexed root
  returns `isError: false` with guidance text.
- [ ] Commit: `feat(mcp): explore tool handler (PRIMARY)`

### Task 17: `selene-mcp` — the `node` tool handler (SECONDARY): the thin dispatch over Task 12's node view

**Files:** Create: `src/handlers/node.rs`, `tests/node_tool_test.rs`. Modify: `src/server.rs`
(fill `selene_node` only). After Task 15; independent of Task 16 **except** for the `server.rs`
seam. **The data+render half is Task 12's** (`selene_context::node_view`) — it is Phase 4, it is
gated by Task 13, and this task must **not** reimplement any of it.

**Interfaces:**
```rust
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct NodeToolArgs { pub symbol: Option<String>, pub file: Option<String>,
    pub line: Option<u32>, pub include_code: Option<bool>, pub offset: Option<usize>,
    pub limit: Option<usize>, pub project_path: Option<String> }
pub async fn handle_node<S: GraphStore>(state: &ServerState<S>, args: NodeToolArgs)
    -> ToolOutcome;   // ToolOutcome is Task 14's
```

Thin: validate → resolve the project → map `NodeToolArgs` onto `node_view::NodeArgs` → call
`selene_context::node_view::node_view` → wrap in `ToolOutcome`. If this file grows past ~120
lines, something belongs in `selene-context`.

- [ ] **Neither `symbol` nor `file`** given → **success-shaped** guidance naming both
  (`Provide either a "symbol" or a "file"…`), never `isError`.
- [ ] **`include_code`** defaults to **`true`** (the anti-Read invariant: a `node` call that
  returns metadata without source sends the agent straight to `Read`). `offset`/`limit` default
  to `None` → Task 4's Read-parity defaults (`limit` = 2000 lines).
- [ ] **A `GraphError::PathRefusal` out of the node view maps to `ToolOutcome::Refusal`** — the
  only `isError` this tool can produce. Every other condition (ambiguous file, no match, offset
  past end) arrives as `Ok(text)` and stays success-shaped.
- [ ] TDD **through the real server** (a direct function call proves nothing about the wiring —
  the inert-seam rule): drive `tools/call` with `{"name":"selene_node","arguments":{"file":"…"}}`
  over the real transport and assert `^\d+\t` numbered source lines in the body; a `..`-escaping
  path returns `isError: true`; a non-existent symbol returns `isError: false` with guidance.
  ⚠ `selene_node` is **not** default-visible (Task 15's TS-parity gating) — the test must set
  `SELENE_MCP_TOOLS=explore,node` (or the tiny-repo surface) to reach it, and **that fact is
  itself worth one assertion**: calling a hidden tool returns `isError: true` (disabled tool).
- [ ] Commit: `feat(mcp): node tool handler (SECONDARY) over the Phase-4 node view`

### Task 18: `selene-mcp` — `search`, `callers`, `callees`, `impact`, `files`

**Files:** Create: `src/handlers/query.rs`, `tests/query_tools_test.rs`. Modify: `src/server.rs`
(fill the remaining `#[tool]` bodies). After Task 15; the `server.rs` seam serializes it against
16/17.

- [ ] **Shared shape for callers/callees/impact** (map §callers/callees/impact): resolve via
  `find_all_symbols` → **group** into distinct definitions by `(file_path, qualified_name)`
  (#764) → optional `file` narrowing (**a miss adds a note and shows all** — never an error) →
  **per-definition sections when >1 group** → limits clamped **1–100**, impact `depth` clamped
  **1–10**, default **2**.
- [ ] **Output markers, verbatim:** `**Search Results (N found)**`;
  `**Impact: "{symbol}" affects N symbols**`; node lists `- name (kind) - file:line — via label`.
  Misses are **success-shaped**: `No results found for "{q}"`,
  `Symbol "{s}" not found in the codebase`, `No callers found for "{s}"` /
  `No callees found for "{s}"`, `No files indexed. Run \`selene index\` first.`
- [ ] **`files`**: path filter with the #426 normalization (`/`, `.`, `./`, and a backslash path
  all behave) — port `mcp-files-path-normalization.test.ts`. Uses `QueryManager::files()`.
- [ ] **NO eighth tool. `status` is NOT in Phase 5** (maintainer ruling, 2026-07-13): the
  roadmap scopes this phase to **seven**, and most of what `status` reports (journal mode,
  watcher state, daemon) does not exist until Phase 6. The **not-indexed** case — the one thing
  that tempted a status tool into existence — is handled where it belongs: as **success-shaped
  guidance returned by every tool** (Task 19). Adding a tool to carry a message that every
  other tool already carries is scope creep at exactly the gate where scope creep is most
  tempting. Task 19's guidance text therefore **must not** point at a `selene_status` tool.
- [ ] TDD: an overloaded symbol yields **per-definition sections**; a `file` narrowing miss
  yields a **note plus all groups** (not an error, not an empty result); every "no results" path
  is `isError: false`; the 4 path-normalization cases; `impact` depth clamps at 0 → 1 and
  11 → 10. ⚠ These five tools are **hidden by default** (Task 15's ruling) — every test here
  sets `SELENE_MCP_TOOLS` to expose them, and one asserts that **without** it, the call is
  refused as a disabled tool.
- [ ] Commit: `feat(mcp): search/callers/callees/impact/files handlers`

### Task 19: `selene-mcp` — **the `isError` discipline**, input caps, not-indexed guidance, banners

**Files:** Create: `src/errors.rs`, `src/banners.rs`, `tests/iserror_test.rs`,
`tests/unindexed_test.rs`. Modify: `src/server.rs` (wrap dispatch), `src/lib.rs` (facade +
ledger pass). **Last of the MCP tasks — it is the layer every handler passes through.**

**This task is the one that decides whether an agent keeps using the tool.** One or two
`isError` responses early and the agent abandons selene for the session. Everything below is
the map's §isError contract, which is a **hard invariant** (PRD §8.2), not a preference.

**Interfaces:**
```rust
// ⚠ `ToolOutcome` + `to_call_result` already exist — Task 14 defined them. This task adds the
// CLASSIFICATION (which condition takes which arm), the input caps, the guidance texts, and the
// banner layer that wraps dispatch. Do not redefine the enum.
pub const MAX_INPUT_LENGTH: usize = 10_000;   // free-form strings
pub const MAX_PATH_LENGTH: usize = 4_096;     // path-likes
pub fn classify(e: GraphError) -> ToolOutcome;             // the one place errors become outcomes
pub trait PendingFiles: Send + Sync { fn pending(&self) -> Vec<PendingFile>;
                                      fn degraded_reason(&self) -> Option<String>; }
pub struct NoWatcher;   // Phase 5's impl — returns empty. ⚠ INERT-SEAM SHAPE. See below.
pub fn format_stale_banner(stale: &[PendingFile]) -> String;
pub fn format_stale_footer(stale: &[PendingFile]) -> String;
pub fn format_degraded_banner(reason: Option<&str>) -> String;
```

**`isError: true` — the COMPLETE list. Nothing else, ever:**
1. **Path refusal** (`GraphError::PathRefusal`, #527) — a sensitive/escaping path. No retry note.
2. **Input validation**: `"Error: {name} must be a non-empty string"`; a free-form string over
   `MAX_INPUT_LENGTH` (**10 000**); a path-like over `MAX_PATH_LENGTH` (**4 096**).
3. **Disabled-tool call** and **unknown tool name**.
4. **Genuine malfunction** — and it carries the note verbatim: `"Tool execution failed: {msg}.
   This is an internal selene error — retry the call once; if it persists, continue without
   selene for this task."`

**Success-shaped — ALL of these, no exceptions:** not indexed (both the no-default and the
explicit-`projectPath` variants); `No results found for "{q}"`;
`Symbol "{s}" not found in the codebase`; `No callers/callees found for "{s}"`;
`No relevant code found for "{q}"`; `` No files indexed. Run `selene index` first. ``;
file-not-matched; ambiguous-file lists; offset-past-end.

- [ ] **The not-indexed guidance text** is long and deliberate (TS's `getCodeGraph`): it tells
  the agent to **stop calling selene for that project this session** and use its built-in tools
  — because a tool that keeps saying "not indexed" is worse than one that says "stop asking me".
  It must also say **indexing is the user's decision** (do not offer to run it). Port both
  variants (no-default vs explicit path). ⚠ **It must NOT reference a `selene_status` tool** —
  there is none in Phase 5 (ratified). This guidance is returned by **every** tool, which is
  exactly why no tool needs to exist to deliver it.
- [ ] **⚠ `NoWatcher` is textbook inert-seam shape** — a provider that returns empty, feeding a
  banner nobody can see. It is allowed **only** because Phase 6 will replace it, and **only**
  with this mitigation, which is not optional: the banner tests inject a **non-empty fake**
  `PendingFiles` and assert the banner bytes. Without that, Phase 6 wires a real watcher into a
  formatter nobody ever ran.
- [ ] **Banner texts, verbatim.** Staleness banner opens
  `⚠️ Some files referenced below were edited since the last index sync — their selene entries
  may be stale:` with lines `  - {path} (edited {ms}ms ago, {indexing in progress|pending sync})`
  and the tail `For accurate content of those specific files, Read them directly. The rest of
  this response is fresh.` Footer: `(Note: N file(s) elsewhere in this project are pending index
  sync but were not referenced above: …)` — **max 5** plus `…and N more`. Degraded banner:
  `⚠️ Selene auto-sync is DISABLED — live file watching stopped, so the index is frozen and any
  file edited since then is stale here. Read files directly to confirm current content before
  relying on it.` plus an optional `  Reason: {reason}` line.
  (These are the **only** sanctioned "Read" instructions in the product — Global Constraints —
  because here Read genuinely *is* correct.)
- [ ] **Staleness matching is substring-based**: a pending file's path is matched by
  **substring** against the response body; matched paths get the per-file banner, the rest go to
  the footer (max 5). Port the TS behavior, including its looseness.
- [ ] TDD (`mcp-unindexed.test.ts` is **the** policy test — port it whole): every
  success-shaped condition above returns `isError: false` **through the real server**; each of
  the 4 `isError` conditions returns `isError: true`; a 10 001-char query is rejected, 10 000 is
  accepted; a path refusal is `isError` **without** a retry note; a malfunction **has** one; the
  banner bytes with an injected non-empty `PendingFiles`; the instructions variant flips with
  `.selene/` presence.
- [ ] **Ledger pass** on `selene-mcp/src/lib.rs`: role + PRD section, the public-interface
  ledger (map §Public interface → Rust item, or "deferred → Phase N, because …" — the daemon,
  proxy, query-pool, watchdogs, roots, transports all land here as **explicit** deferrals), the
  invariants, and the `isError` contract restated where a maintainer will actually read it.
- [ ] Commit: `feat(mcp): isError discipline, input caps, not-indexed guidance, staleness banners`

### Task 20: **PHASE 5 GATE — THE MILESTONE.** `selene index && selene serve --mcp` answers a real flow question with **zero Read/Grep**

**Files:** Create: `crates/selene-mcp/tests/dogfood_gate.rs`, `scripts/dogfood.sh`,
`tests/fixtures/dogfood/questions.toml`, `docs/benchmarks/2026-07-phase5-dogfood.md`.
Modify: root `README.md` (the "it works" section). Requires **every** prior task.

This is the vertical-slice milestone the whole roadmap has been walking toward: **the real
binary, on a real repo, answering a real question, with the agent never opening a file.** It is
not a unit test with a mock; it is not a snapshot. Two halves, and **both must be green** —
because the TS build learned (adaptive-explore `Dead ends` #6) that a deterministic probe and a
real agent **form different queries, surface different files, and disagree**. The probe said
"Django: 0 skeletons, reads flat"; the real agent Read the file back.

---

**Half A — the deterministic sufficiency gate (runs in CI, `cargo test`).**

Objective, hermetic, and it is what a subagent can actually finish in one session.

`questions.toml` — each row is a **flow question** plus the **facts that answer it**:
```toml
[[question]]
repo      = "."                       # SeleneCode itself
query     = "how does an unresolved reference become a graph edge"
# The answer REQUIRES these symbols. If explore's output doesn't contain them, an agent
# cannot answer without reading — that is the whole definition of the gate.
must_contain_symbols = ["resolve_and_persist_batched", "resolve_one", "create_edges",
                        "insert_edges"]
must_contain_flow    = true           # the **Flow …** section, ≥3 numbered steps
must_contain_files   = ["crates/selene-resolve/src/batch.rs",
                        "crates/selene-resolve/src/resolver.rs"]
max_explore_calls    = 1              # small repo ⇒ explore_budget == 1

[[question]]
repo      = "../codegraph"            # the TS parity source: a 72k-LOC real repo (311 src files)
query     = "how does an MCP tools/call request reach handleExplore"
must_contain_symbols = ["MCPSession", "ToolHandler", "execute", "handleExplore"]
must_contain_flow    = true
must_contain_files   = ["src/mcp/session.ts", "src/mcp/tools.ts"]
max_explore_calls    = 1

# ⚠ THE LARGE-TIER ROW — RATIFIED, NOT OPTIONAL (Coordination Point 4).
# Both repos above are <500 files. Without this row the gate drives explore_budget == 1 and the
# small output tiers ONLY: the ≥5000-file tiers and the "3–5 calls on a large repo" half of the
# sufficiency invariant would be unit-tested and NEVER DRIVEN — the inert-seam class, again.
[[question]]
repo      = "../django"               # or ../vscode — both are in the TS A/B corpus
query     = "how does a QuerySet become SQL"
must_contain_symbols = ["QuerySet", "_fetch_all", "SQLCompiler", "execute_sql", "as_sql"]
must_contain_flow    = true
must_contain_files   = ["django/db/models/query.py", "django/db/models/sql/compiler.py"]
max_explore_calls    = 3              # the LARGE-repo bound — MEASURED here, not assumed
tier_assertions      = true           # see the large-tier checklist item below
```

- [ ] **Drive the PRODUCTION binary, not the library.** The test shells out:
  `cargo run -p selene -- index <repo>` then `cargo run -p selene -- serve --mcp --path <repo>`,
  speaks **real MCP over the child's stdio** (`initialize` → `tools/call selene_explore`), and
  asserts on the **response bytes**. No in-process shortcut. If the binary can't do it, the
  product can't do it. *(Everything before this task can be green while the binary is broken —
  that is exactly the seam this gate closes.)*
- [ ] **The sufficiency assertions**, per question row: every `must_contain_symbols` entry
  appears **as a rendered definition** (its name in a file-section header **and** its body's
  source lines present — not a passing mention inside another function); every
  `must_contain_files` appears as a `**\`` section; the **Flow** section exists with **≥3**
  numbered steps; the blast-radius section exists; output length ≤ the tier ceiling; and
  **zero** occurrences of Read/Grep advice outside the sanctioned banners.
- [ ] **The negative control** (without it, Half A is not a test): the **same** assertions run
  against a **deliberately weak** query (a single stopword, e.g. `"the"`), and must **fail to
  find** the flow — proving the assertions can distinguish a real answer from output that merely
  exists. A gate that would pass on garbage certifies nothing.
- [ ] **`explore_budget` is respected**: the number of explore calls needed is ≤
  `max_explore_calls`. **Measured 2026-07-13, so it is not the executor's problem:**
  `../codegraph` = **496 tracked / 311 source files**; SeleneCode = **165 `.rs`** (plus the
  fixture corpora, which *are* indexed — they are real source). **Both are therefore in the
  `<500` tier ⇒ `explore_budget == 1`**, and `max_explore_calls = 1` is correct for both rows.
  Re-measure with `selene index`'s own file count at gate time and record it (the fixture trees
  grow; if either repo crosses 500, the row's budget becomes 2 — the invariant is *budget scales
  with repo size*, not *one call always*).
- [ ] **⚠ THE LARGE-TIER RUN IS NOT OPTIONAL** (ratified — Coordination Point 4). The two small
  repos exercise `explore_budget == 1`, the `<500` output tier, and the tiny-repo tool gate
  (which fires below 500 files, narrowing the surface to `{explore, search, node}`). They
  **never** exercise the ≥5000 tiers or the multi-call budget. The third repo does, and it
  carries its **own** `must_contain_symbols`, its **own** zero-Read assertions (Half B), and:
  - **The tier is verified against the real index, not assumed.** Assert
    `file_count >= 5000` after `selene index` (Django is ~2.8k source files in some layouts —
    **if the chosen repo indexes below 5000, it is the WRONG REPO for this row**: swap to VS Code
    and record the measured count. Do not soften the row to fit the repo; that inverts the whole
    point of the gate).
  - **The tier's meta-text is DRIVEN**: at ≥5000 files `include_relationships`,
    `include_additional_files`, `include_completeness_signal` and `include_budget_note` are all
    **true** — so the output must actually **contain** the relationship section, the
    additional-files list, the completeness signal and the budget note. On the small repos they
    are all **false** and must be **absent**. That pair of assertions is the only thing standing
    between "the tier table is implemented" and "the tier table works" — four output features
    that no other test in this plan ever renders.
  - **The "3–5 calls" bound is MEASURED, not assumed**: record the number of `selene_explore`
    calls the real agent made (Half B) and assert it is **≤ `explore_budget(file_count)`** and
    **≥1**. This is the *only* place the sufficiency invariant's "scaling to 3–5 on large repos"
    half is ever exercised against a real agent.
  - **`max_chars_per_file` monotonicity, observed end-to-end**: the large repo's per-file
    rendered budget must be **≥** the small repos' (Task 8's invariant, now proven on real
    output rather than in a unit test).

---

**Half B — the real-agent zero-Read run (`scripts/dogfood.sh`, run manually; results
committed to `docs/benchmarks/`).**

This is the half that is **not** self-deception. It measures what an actual agent does.

- [ ] **Setup**: register the built binary as an MCP server for a headless agent session
  (`claude mcp add selene -- <target>/debug/selene serve --mcp --path <repo>`, or the equivalent
  `.mcp.json`), in a **scratch copy** of the repo so no state is shared with this session. Run
  over **all three** repos (two small + the ratified ≥5000-file one). Build with
  `--release` for the large repo and **index it once**, reusing the `.selene/` across the 3 runs
  — a debug-build index of a 5k-file repo is the one place this gate can become genuinely slow,
  and it is a fixture cost, not a measurement.
- [ ] **Run**: headless, streaming the tool-use log —
  `claude -p "<the question from questions.toml>" --output-format stream-json` — with **the same
  question text** Half A used. Run **n = 3 per repo × 3 repos = 9 runs** (agent runs vary; one
  run proves nothing, and the ≥2-of-3 rule is **per repo** — a large-tier failure is not
  averaged away by two small-repo passes).
  ⚠ **The large repo is where this half earns its keep.** If it is Django, note that Django is
  the exact repo where the TS build's deterministic probe **lied** (`adaptive-explore-sizing.md`
  dead end #6: the probe said "0 skeletons, reads flat"; the real agent skeletonized
  `compiler.py` and Read it straight back). Half A on Django can be green while Half B is red —
  that is not a flaw in the gate, that **is** the gate.
- [ ] **Verify zero-Read MECHANICALLY, not by reading the transcript.** Parse the
  `stream-json` output for `tool_use` blocks and **count** them by name. The gate is:
  - `Read` count == **0**, `Grep` count == **0**, `Glob` count == **0**, and **no Task/agent
    delegation** that would read files on the agent's behalf (the instructions explicitly call
    that out as an anti-pattern — count `Task` too);
  - `selene_explore` count ≤ `explore_budget(file_count)` for that repo;
  - the final answer text **names the `must_contain_symbols`** — i.e. it actually answered,
    rather than answering vacuously with zero tool calls of any kind. ⚠ **A run that reads
    nothing because it answered nothing is a FAILURE, not a pass** — this assertion is what
    separates the two, and it is the one a rushed implementation will forget.
  - **⚠ A run PASSES only if it satisfies BOTH halves — zero-Read AND answered — and the
    ≥2-of-3 rule is over PASSING RUNS, not over each criterion separately.** Scoring the two
    criteria independently is a silent false-green: a zero-Read-but-empty run and a
    read-everything-but-correct run would score "2/3 zero-Read, 2/3 answered" and the gate would
    go green on **zero** runs that actually did the thing. Evaluate per run, then count.
  - **Median across the 3 runs** is what's recorded; a single outlier run does not fail the gate,
    but **≥2 of 3 runs must PASS** in the conjunctive sense above.
- [ ] **Record the result** in `docs/benchmarks/2026-07-phase5-dogfood.md`: per repo — file
  count, **tier**, `explore_budget`, explore calls made, Read/Grep/Glob/Task counts per run,
  output chars, wall-clock, and the **verbatim answer** of the median run. **All three repos in
  one table, tier column included** — that table is what makes the budget invariant *visible*
  (small tier → 1 call, large tier → up to 5) instead of merely asserted, and it is the baseline
  Phase 9's A/B rerun compares against.
- [ ] **If Half B fails, the phase is not done — and the failure is the finding.** Do not
  "fix" it by weakening the question. Diagnose *what the agent went to Read*, and that names the
  gap: a missing flow hop (→ a Phase 3 synthesizer gap, or the `MAX_BRIDGE` cap), a file the
  ranking buried (→ Task 11), a body the renderer skeletonized that it shouldn't have
  (→ Task 12's spare/override — this is the exact regression `adaptive-explore-sizing.md`
  documents, twice). **Write the diagnosis into the benchmark doc even if you then fix it** —
  that record is worth more than the green checkmark.
- [ ] **README**: a short "Try it" section — `cargo install --path crates/selene`,
  `selene index`, register the MCP server, ask a flow question. This is the first commit where
  that sentence is **true**; it is the point of the phase.
- [ ] Commit: `test(mcp): PHASE 5 GATE — dogfood, real binary answers a flow question with zero Read`

---

## Definition of done (both phases)

- [ ] Tasks 1–20 committed, `cargo fmt && cargo clippy --all-targets && cargo test` green.
- [ ] **Phase 4 gate** (Task 13): explore/node snapshots + content assertions green on a
      ≥6-project corpus built by the **production** index→resolve pipeline.
- [ ] **Phase 5 gate** (Task 20): Half A green in CI; Half B recorded in
      `docs/benchmarks/2026-07-phase5-dogfood.md` with **≥2 of 3 runs PASSING** (zero
      Read/Grep/Glob/Task **and** answered — conjunctive, per run) on **all three** dogfood repos
      — including the **≥5000-file** one, whose ≥5000 tier meta-text and multi-call budget are
      **driven**, not merely unit-tested.
- [ ] The three crate `lib.rs` ledgers name every deferred item **with its phase and its reason**.
- [ ] `docs/plans/2026-07-12-selenecode-roadmap.md` Phase 4/5 rows updated to reflect reality
      (the maps are the source of truth for parity; the roadmap for status).
- [ ] **No open coordination points remain** — all four were ratified 2026-07-13 (explore-only
      default surface; no `status` tool; the verbatim-instructions rename table; three dogfood
      repos incl. the mandatory ≥5000-file one). A task that wants to re-open one is misreading
      the plan.
