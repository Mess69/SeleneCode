# SeleneCode

**Local-first code intelligence, in Rust.**

SeleneCode parses any supported codebase with tree-sitter, stores symbols / edges / files in an
**embedded graph database** (SurrealDB), and serves that knowledge graph to AI agents (Claude Code,
Cursor, Codex, opencode, …) over **MCP**. Per-project data lives in `.selene/`. Extraction is
**deterministic** — derived from the AST, never LLM-summarized.

The point: an agent answers a structural or flow question — *"how does a request become a database
write?"* — with **one fast tool call and zero `Read`/`Grep`**. The graph is built once; every agent
that opens the repo reuses it instead of re-reading files.

SeleneCode is the Rust port of [CodeGraph](../codegraph) (TypeScript + SQLite), rebuilt on a
graph-native stack: a single static binary, natively-linked tree-sitter, SurrealDB + Tokio.

---

## Status (2026-07-15)

**It works, and it is fast.** `selene index` builds the graph; `selene serve --mcp` answers flow
questions over MCP; the answer includes the call-path spine, verbatim numbered source, and a blast
radius — enough to answer without opening a file.

| | |
|---|---|
| **Indexing** | 12 languages (TS, JS, Python, Rust, Go, Java, Kotlin, C, C++, PHP, Ruby, …), deterministic |
| **Speed** | django (931 files, 19k nodes): **index ~11 s, explore ~1–2 s**. **1.4–1.9× of the CodeGraph TS build** — see [the benchmark](docs/benchmarks/2026-07-14-rust-vs-ts-speed.md) |
| **`explore`** | answers flow questions correctly on small/medium repos; the milestone gate (`selene-mcp/tests/dogfood_gate.rs`) drives the real binary end-to-end |
| **Large repos** | VS Code (349k nodes): indexing works; `explore` is **~2 s steady-state** (a persistent `serve` pays a ~9 s warm-up once per session). One open limitation — semantic relevance when the query's words diverge from the code's ([details](docs/benchmarks/2026-07-phase5-dogfood.md)) |
| **Not built yet** | the CLI beyond `index`/`serve`/`status` (`sync`), the file watcher, the daemon, and `selene install` (MCP config is wired by hand today) |

Honest limitations and the roadmap are in [`RESUME.md`](RESUME.md).

---

## Quick start

```bash
# 1. Build the release binary (the debug build is ~2.4× slower — always use --release)
cargo build --release -p selene

# 2. Index a project (writes ./.selene/)
./target/release/selene index /path/to/your/repo

#    Per-phase timings on stderr:
RUST_LOG=selene::index=info ./target/release/selene index /path/to/your/repo

# 3. See what's in the graph
./target/release/selene status /path/to/your/repo
#   /path/to/your/repo
#     files:  931
#     nodes:  19061
#     edges:  46946
#     languages: python (931)
#     node kinds: function 8402, method 3211, …
```

### Wire it into Claude Code (MCP)

Until `selene install` lands, add the server by hand. In your project's `.mcp.json` (or Claude
Code's MCP config), point at the **absolute path** of the binary — a static binary is not guaranteed
on `PATH`, and a config naming an unrunnable command fails silently:

```json
{
  "mcpServers": {
    "selene": {
      "command": "/abs/path/to/target/release/selene",
      "args": ["serve", "--mcp", "--path", "/abs/path/to/your/repo"]
    }
  }
}
```

Then ask the agent a flow question. It calls `selene_explore` and answers from the graph.

### Try it directly over MCP stdio

```bash
./scripts/ask.sh "how does an unresolved reference become a graph edge"
```

`ask.sh` drives the real binary over real MCP against a dogfood copy — the only evidence that counts
here (unit tests pass on planted fixtures while the real answer can be wrong; run the binary).

---

## How it uses the stack

SeleneCode is not a transpile of the TypeScript build — it leans on what a Rust + SurrealDB + Tokio
stack does that TS + SQLite could not. Every number below is measured (see `docs/benchmarks/`):

- **Native tree-sitter, no WASM.** Grammars are linked in; the WASM worker pool / parser-reset / OOM-
  retry layer is deleted, not ported. Parsing 931 Python files is **0.27 s** — 0.4 % of a run.
- **Tokio-concurrent writes.** The store is written with `buffer_unordered` / bounded `try_join_all`,
  not one query at a time. `insert_nodes` **3.4 s → 0.8 s**, `insert_edges` **1.5 s → 0.6 s**.
  SurrealDB reaches 300k ops/s with 128 concurrent clients; a serial caller sees none of it.
- **FTS index, not `CONTAINS` scans.** SurrealDB's docs are explicit that `CONTAINS` never uses an
  index. On a 349k-node repo, routing candidate generation through the FULLTEXT index (built
  `CONCURRENTLY`, overlapped with resolution via `tokio::join!`) instead of unindexed substring scans
  took `explore` **35.6 s → 6.5 s**.
- **In-memory resolution.** The resolver's symbol table is loaded once (`all_nodes`) rather than
  queried per reference — 32,524 blocking point-lookups became one 127 ms scan. And the unresolved-
  reference queue is kept in memory instead of round-tripped through the disk between two phases of
  the same process.
- **`allocator` feature (mimalloc).** SurrealDB's own embedded-Rust guidance; not on by default, and
  a `default-features = false` dependency never gets it. Worth ~10 %.

The full arc — **61 s → 11 s on django in one day, 10.7× behind TS → 1.9×** — is in
[`docs/benchmarks/2026-07-14-rust-vs-ts-speed.md`](docs/benchmarks/2026-07-14-rust-vs-ts-speed.md).

The next stack lever, scoped and native: **vector search** (SurrealDB HNSW / cosine KNN) for
semantic relevance — bridging a query's words to the code's by meaning, which prefix/FTS matching
cannot ([why](docs/benchmarks/2026-07-phase5-dogfood.md)).

---

## Architecture

A Cargo workspace of focused crates (see the PRD, §3):

| crate | role | state |
|---|---|---|
| `selene-core` | shared types: `Node`/`Edge` (22 kinds / 12 kinds), `Provenance`, the wire contract | ✅ |
| `selene-db` | `GraphStore` trait + embedded **SurrealDB** (RocksDB on disk) + FTS | ✅ |
| `selene-extract` | native tree-sitter extraction, Rayon fan-out, ordered commit, incremental re-index | ✅ |
| `selene-resolve` | imports, name matching, 11 framework resolvers, dynamic-dispatch synthesis | ✅ |
| `selene-graph` | traversal (callers/callees/impact/path) + `QueryManager` | ✅ |
| `selene-context` | `ContextBuilder` — the relevance pipeline, the Flow spine, the output the agent reads | ✅ |
| `selene-mcp` | MCP server (rmcp): tools, `isError` discipline, input caps, server-instructions | ✅ |
| `selene-sync` | file watcher (notify) + git-hook helpers | ⬜ stub |
| `selene-installer` | multi-agent installer: MCP config writers | ⬜ stub |
| `selene-cli` | CLI (clap), daemon, telemetry, upgrade | ⬜ stub |
| `selene` | the single binary (`index`, `serve`, `status`) | ✅ |

**Decision (2026-07-12):** SurrealQL-max — traversal is pushed into SurrealQL (recursive
`.{1..n}(->calls->fn)`, shortest-path); the permissive fallback backend was dropped. `selene-db` is
the only crate that touches the database; everything above depends on the `GraphStore` trait.

---

## Build, test, lint

```bash
cargo build --release -p selene            # the binary
cargo test -p selene-context               # a single crate's tests
cargo test -p selene-resolve --test resolution_parity_gate --test dispatch_coverage_gate
                                           # the gates that compare edge identity vs TS, tolerance 0
cargo test -p selene-mcp --test dogfood_gate -- --ignored --nocapture
                                           # the milestone gate: the real binary answers zero-Read
cargo clippy --all-targets
cargo fmt
```

The toolchain is pinned in `rust-toolchain.toml`. `rustfmt.toml` sets `max_width = 100`.

---

## Documentation

- **How to resume / current state** → [`RESUME.md`](RESUME.md) — the single handoff doc.
- **Working guide for contributors and agents** → [`CLAUDE.md`](CLAUDE.md).
- **Benchmarks** → [`docs/benchmarks/`](docs/benchmarks/) — the Rust-vs-TS speed arc, the write-path
  findings, the milestone gate.
- **PRD (target architecture)** → [`docs/specs/2026-07-11-rust-graph-db-migration-design.md`](docs/specs/2026-07-11-rust-graph-db-migration-design.md).
- **Ported reference** (design + subsystem maps from the CodeGraph TS build) →
  [`docs/reference/from-codegraph/`](docs/reference/from-codegraph/).

---

## Data model

`selene-core` is the shared contract. Each of the 22 node kinds becomes a graph node; each of the 12
edge kinds a typed relationship carrying `provenance` (`tree-sitter` / `scip` / `heuristic`).
Synthesized (dynamic-dispatch) edges are tagged `heuristic` with `synthesizedBy` / `registeredAt` in
their metadata.

## Licensing

Intended: **MIT OR Apache-2.0** (permissive / OSI). SurrealDB (BSL 1.1 — free to embed) sits behind
the `GraphStore` trait; the previously-planned permissive fallback backend was dropped when the
SurrealQL-max decision was ratified.
