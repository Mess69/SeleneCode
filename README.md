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

## Status (2026-07-17)

**Usable end-to-end on real projects**: `selene init` builds the graph and installs git hooks that
keep it fresh; `selene install` wires the MCP server into your agents; the CLI answers
callers/callees/impact/explore questions directly; `selene viz` renders the graph as an interactive
HTML galaxy.

| | |
|---|---|
| **Indexing** | 12 languages (TS/TSX, JS/JSX, Python, Rust, Go, Java, Kotlin, C, C++, C#, PHP, Ruby), deterministic |
| **Speed vs CodeGraph (TS)** | **faster on all three benchmark corpora**: codegraph-src 0.77×, selene-crates 0.77×, django (3 011 files) 0.96× — best-of-2, cold, same machine. (VS Code scale: not re-run since the optimizations) |
| **RAM** | codegraph-src **467 MB** (parity with TS), selene-crates **~600 MB** (1.9× lighter than TS), django **2.0 GiB** (TS: 0.6 GiB — the remaining gap, tracked in the [roadmap](docs/plans/2026-07-16-optimization-roadmap.md)) |
| **Freshness** | `selene sync` re-indexes only touched files; git hooks (installed by `init`) sync after commit/merge/checkout; the daemon watches while an agent is connected |
| **Correctness gates** | TS↔Rust resolution parity at **tolerance 0** (edge identity), dispatch-coverage on whole flows, 13 byte-pinned extraction snapshots |
| **Known limits** | VS Code-scale (250k+ nodes) is indexable but not yet re-benchmarked after the latest optimizations; semantic relevance when a query's words diverge from the code's ([details](docs/benchmarks/2026-07-phase5-dogfood.md), `selene embed` is the optional answer) |

The full optimization history — every measured (and disproved) theory — is in
[`docs/plans/2026-07-16-optimization-roadmap.md`](docs/plans/2026-07-16-optimization-roadmap.md).

---

## Quick start — two commands, total

```bash
# 1. ONCE PER MACHINE — no Rust, no Node, nothing to compile:
curl -fsSL https://raw.githubusercontent.com/Mess69/SeleneCode/main/scripts/install.sh | sh
#    (grabs the prebuilt static binary for your OS/arch, checksum-verified;
#     falls back to building from source inside a checkout. `selene upgrade`
#     updates it in place later, `selene upgrade --check` just looks.)

# 2. ONCE PER PROJECT: index it + wire it into Claude Code, in one go
cd /path/to/your/project
selene install
#   no index here yet — running `selene init` first…
#   done: 19061 nodes, 46946 edges
#   installed 3 git sync hook(s) (post-commit/merge/checkout)
#   claude   created ./.mcp.json
#   Restart the agent (or reload its MCP servers) to pick up selene.
```

**Restart Claude Code — that's it.** Ask it a structural question ("who calls X?", "how does a
request become a DB write?") and it answers from the graph through the `selene_explore` MCP tool
instead of burning tokens on `Read`/`Grep`. `selene install -t auto` wires every agent detected on
your machine (Cursor, Codex, opencode, …) instead of just Claude Code.

From then on the index maintains itself: the git hooks re-sync on commit/merge/checkout, and the
daemon watches for file changes while an agent is connected. Manual controls, if you ever want them:

```bash
selene status             # what's in the graph
selene sync               # incremental refresh — only touched files
selene index              # full rebuild from scratch
selene purge              # remove EVERYTHING selene added to this project, one shot
```

`selene purge` is the clean exit: it stops the daemon, deletes `.selene/`, strips the selene block
from the git hooks (your own hook content stays), cleans the `.git/info/exclude` entry, removes
`selene-graph.html`, and takes selene out of the project's MCP configs (a `.mcp.json` that lists
your other servers is preserved). Your source files are never touched. Add `--global-mcp` to also
strip selene from the global agent configs (`~/.claude.json`, …). The finer-grained pieces exist
too: `selene uninit` (index + hooks only), `selene uninstall` (MCP configs only).

### Ask questions from the terminal (no agent needed)

```bash
selene explore "how does a request become a database write"   # flow answer: spine + numbered source
selene query UserSerializer          # find symbols by name
selene node validate_password        # one symbol: source + caller/callee trail
selene callers save                  # who calls this?
selene callees dispatch              # what does this call?
selene impact AuthMiddleware         # blast radius if this changes
selene affected src/db/models.py     # files whose graph depends on these files
selene report                        # write GRAPH_REPORT.md: hubs, clusters, cycles, orphans
selene insights                      # structural summary: betweenness bottlenecks, clusters, cycles
selene export --format graphml       # full graph to stdout: json | jsonl | graphml (Gephi/yEd) | dot
selene diff HEAD~5                   # what changed in the GRAPH since a revision (no checkout)
selene memory                        # what explore was asked before (session memory; --clear)
selene query --raw "SELECT ..."      # read-only SurrealQL for power users (mutations refused)
```

**Documents are part of the graph** (2026-08-18): markdown/txt/rst — and .docx/.pdf
via their extracted text — index as `Document`/`Section` nodes whose code-spans,
paths and links become `mentions` edges bound by the resolver. A rationale-shaped
question ("why was X chosen") surfaces the documentation section that answers it,
verbatim, zero Read — and nothing ever leaves your machine (no LLM, no API: the
parse is deterministic, the optional semantic layer is local ONNX).

Every `explore` answer ends with its measured **token economy** — e.g. *"this answer ≈ 5k tokens;
the 75 files it distills total ≈ 237k tokens — **52× less**"* — computed from the indexed file
sizes, so the saving is a measurement, not a slogan.

---

## The visual mode — `selene viz`

Render the whole code graph as a **self-contained interactive HTML map** (zero dependencies,
one file, works offline):

```bash
selene viz --open                    # writes ./selene-graph.html and opens it
```

The page opens on an **architecture map**: one named node per module (directory group), sized by
symbol count, edges weighted by how many calls cross between them. Test/vendored/generated code is
filtered out up front (the header says how much was hidden). Click a module to drill into its
symbols; the `Symbols` button shows the whole galaxy.

The **`Clusters`** button switches to **call-graph communities** (Louvain, computed in Rust,
deterministic): the map physically separates into named constellations — each galaxy is **named by
its hub symbol** and pulled to its own region, colors follow who *calls* whom, not where files sit.
The legend is a per-cluster show/hide filter (click a cluster to hide it, "— all clusters —" to
reset); hovering a row shows the dominant directory. The header lists the **god-nodes** (the 5
most-connected symbols, clickable — with their in/out split) and the **rare bridges** (cross-module
dependencies carried by a single edge).

### Live mode — watch your agent build the graph

```bash
selene viz --watch --open            # serves the map on a local port, updates live
```

Leave this open while Claude Code (or any agent) works on the project: every new function, class
or module **bursts into the map with a supernova animation** seconds after it is written — the
index updates through the daemon's file watcher, and the page polls and animates the difference
(a toast narrates: `✨ +3 functions · +1 class`). Works with or without a running daemon: when your
agent is connected, reads go through the daemon's socket, no lock conflict. Ctrl-C stops it.

The header also shows the project's live footprint — `index 42 MB · RAM 310 MB (daemon)` with a
rolling sparkline: the index's size on disk, and the RAM of whichever process is actually holding
the graph (the daemon while your agent works, else the viz server itself), refreshed every poll.

Options:

| flag | effect |
|---|---|
| `--watch` | live mode: serve on a local port, animate index changes in real time |
| `-o, --out <FILE>` | output path (default `./selene-graph.html`) |
| `--max-nodes <N>` | cap the rendered nodes, most-connected first (default 2000 — keeps the page light on big repos) |
| `--all-kinds` | also render the low-signal kinds (file / import / variable / parameter) that are dropped by default |
| `-p, --path <DIR>` | project directory (default `.`) |

Colors are node kinds; edge types are the 12 relationship kinds (calls, imports, extends, …).
For a first look at an unfamiliar codebase, `viz --open` is the fastest map you can get.

---

## Wire it into your agents (MCP)

One command — the installer detects the agents on your machine and writes their MCP config
(Claude Code, Cursor, Codex, opencode, hermes, …):

```bash
selene install                       # default: Claude Code, project-local config
selene install -t auto               # every agent detected on this machine
selene install -t claude,cursor      # explicit list
selene install --print-config        # show the JSON it would write, touch nothing
selene uninstall                     # remove it again
```

Then ask the agent a flow question — it calls `selene_explore` and answers from the graph instead
of reading files. (`selene serve --mcp` is the underlying server; agents launch it themselves, you
never run it by hand.)

<details>
<summary>Manual MCP config (if you prefer to wire it yourself)</summary>

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
Use the **absolute path** of the binary — a config naming an unrunnable command fails silently.
</details>

### Optional: semantic search

```bash
cargo build --release -p selene --features semantic-search
selene embed        # embeds every symbol locally (ONNX, offline) — `keypress` then finds `keybinding`
```

The lexical index works without it; `embed` adds meaning-based recall on top.

---

## Tuning (all optional)

| env var | effect |
|---|---|
| `RUST_LOG=info` | per-phase timings + profiling counters on stderr (`RUST_LOG=selene::index=info` for just the index lines) |
| `SELENE_SYNC_NEVER=1` | skip per-commit WAL fsyncs during `index` (~5–10% faster). Crash-safe on clean exit; a mid-index crash just means re-running `index` |
| `SELENE_PARSE_WORKERS=n` | parse-pool size (default: cores−1, capped at 8) |
| `SURREAL_ROCKSDB_BLOCK_CACHE_SIZE` (bytes) | RocksDB cache+memtable budget. Default **768 MiB** — measured cliff: below ~768 MiB the memtable charges starve the read cache and the run slows 3× |
| `SURREAL_ROCKSDB_WRITE_BUFFER_SIZE` / `_MAX_WRITE_BUFFER_NUMBER` | write buffers (default 128 MiB × 4) |

These `SURREAL_*` variables work in the embedded engine because of the one-line vendored SDK patch
in [`vendor/surrealdb`](vendor/surrealdb) — the stock crates.io SDK silently ignores them (see the
`SELENE PATCH` note in `vendor/surrealdb/src/engine/local/native.rs`).

Housekeeping: `selene daemon` lists/manages running daemons, `selene unlock` clears a stale
app-level lock, `selene uninit` removes `.selene/` from a project, `selene telemetry status|on|off`.

---

## Distribution & releases

One true **static binary** per platform — no bundled Node runtime (CodeGraph ships ~50 MB of
vendored Node per platform; `selene` is a single ~20 MB executable that starts in milliseconds).

- **Releases are built by [`dist`](https://github.com/axodotdev/cargo-dist)** (the tool `uv` and
  `atuin` ship with) from [`dist-workspace.toml`](dist-workspace.toml): tag `vX.Y.Z`, push, and CI
  builds `{aarch64,x86_64}-apple-darwin` + `{aarch64,x86_64}-unknown-linux-gnu` **natively** (no
  cross-compiling — RocksDB's C++ build is the classic musl/zig casualty; glibc-only is exactly
  SurrealDB's own shipping posture), publishes tarballs + per-asset `sha256` + the generated
  `selene-installer.sh`.
- **`selene upgrade`** self-updates from GitHub Releases (axoupdater — the `uv self update`
  engine): in-place and checksum-verified for installer installs, an honest refusal + the exact
  commands for source builds. `--check` reports without touching anything; `selene upgrade 0.2.0`
  pins.
- **`cargo binstall selene`** works off the same release assets; the crates.io compile fallback is
  deliberately disabled (it would silently drop the vendored SurrealDB patch).
- Until the repo is published on GitHub (set `repository` in `Cargo.toml` to the real slug), the
  installer's source-build fallback keeps everything working from a checkout.

---

## How it uses the stack

SeleneCode is not a transpile of the TypeScript build — it leans on what a Rust + SurrealDB + Tokio
stack does that TS + SQLite could not. Every number below is measured (see `docs/benchmarks/` and
the [roadmap addenda](docs/plans/2026-07-16-optimization-roadmap.md)):

- **Native tree-sitter, no WASM.** Grammars are linked in; the WASM worker pool / parser-reset / OOM-
  retry layer is deleted, not ported.
- **In-memory resolution over an eager index.** The symbol table is one scan, groups are handed out
  as shared slices (one refcount bump — was 59 M Arc clones per medium run), and the reference queue
  never round-trips through disk.
- **ASCII regex engines for receiver inference.** Unicode `\w`/`\b` DFA tables were ~750 MiB of peak
  RSS (≈524 KiB per compiled pattern); the ASCII rewrite matches the original JS semantics and
  collapsed them. Hoisting pattern compilation out of the per-line scan took django's name-match
  from 46 s to 6 s of CPU; the shared-slice group handouts below took it to 3.3 s.
- **Tokio-concurrent, transaction-batched writes** with conflict-retry on every bulk writer, FTS
  built `CONCURRENTLY` and overlapped with resolution.
- **mimalloc / jemalloc** (SurrealDB's `allocator` feature — mimalloc on Apple Silicon, jemalloc on x86) + laptop-sized RocksDB budgets by default.

---

## Architecture

A Cargo workspace of focused crates (see the PRD, §3):

| crate | role | state |
|---|---|---|
| `selene-core` | shared types: `Node`/`Edge` (22 kinds / 12 kinds), `Language`, the wire contract | ✅ |
| `selene-db` | `GraphStore` trait + embedded **SurrealDB** (RocksDB on disk) + FTS | ✅ |
| `selene-extract` | native tree-sitter extraction, Rayon fan-out, ordered commit, incremental re-index | ✅ |
| `selene-resolve` | imports, name matching, 11 framework resolvers, dynamic-dispatch synthesis | ✅ |
| `selene-graph` | traversal (callers/callees/impact/path) + `QueryManager` | ✅ |
| `selene-context` | `ContextBuilder` — the relevance pipeline, the Flow spine, the output the agent reads | ✅ |
| `selene-mcp` | MCP server (rmcp): tools, `isError` discipline, input caps, server-instructions | ✅ |
| `selene-sync` | file watcher (notify) + git-hook helpers | ✅ |
| `selene-installer` | multi-agent installer: MCP config writers | ✅ |
| `selene-cli` | CLI (clap), daemon, viz, telemetry, upgrade | ✅ |
| `selene` | the single binary | ✅ |
| `vendor/surrealdb` | crates.io SDK + one line: embedded engine reads `SURREAL_*` env config | ✅ |

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
- **Optimization roadmap + measured history** → [`docs/plans/2026-07-16-optimization-roadmap.md`](docs/plans/2026-07-16-optimization-roadmap.md).
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

**Source-available under [PolyForm Noncommercial 1.0.0](LICENSE)** (2026-08-20; supersedes the
earlier MIT/Apache intent). The code is open to read, study, modify and share — **any
noncommercial use is permitted** (personal projects, research, education, nonprofits,
government). **Commercial use requires a separate license** from the maintainer.

SurrealDB (BSL 1.1 — free to embed) sits behind the `GraphStore` trait; `vendor/surrealdb`
retains SurrealDB's own license (see `vendor/surrealdb/LICENSE`).
