# SeleneCode

**Local-first code intelligence, in Rust.**

SeleneCode parses any supported codebase with tree-sitter, stores symbols / edges / files in an **embedded graph database**, and exposes a knowledge graph to AI agents (Claude Code, Cursor, Codex, opencode, …) over **MCP**. Per-project data lives in `.selene/`. Extraction is **deterministic** — derived from the AST, not LLM-summarized.

SeleneCode is the Rust port of [CodeGraph](../codegraph): same value proposition (fast structural/flow answers with zero Read/Grep), rebuilt for a single static binary, native tree-sitter, and a graph-native data model.

> **Status: scaffold.** The workspace, the data model (`selene-core`), and the docs are in place; the layer crates are stubs. The target architecture is fully specified in the PRD below.

## Documentation

- **PRD (target architecture)** → [`docs/specs/2026-07-11-rust-graph-db-migration-design.md`](docs/specs/2026-07-11-rust-graph-db-migration-design.md)
- **Ported reference** (language-agnostic design + benchmark notes from the CodeGraph TS implementation) → [`docs/reference/from-codegraph/`](docs/reference/from-codegraph/)
- **Working guide for contributors / agents** → [`CLAUDE.md`](CLAUDE.md)

## Architecture

A Cargo workspace of focused crates (see PRD §3):

| Crate | Responsibility |
|---|---|
| `selene-core` | Domain types: `NodeKind` (22), `EdgeKind` (12), `Provenance`, `Node`, `Edge`, errors. **Implemented.** |
| `selene-db` | `GraphStore` trait + embedded SurrealDB backend + FTS (permissive fallback: IndraDB/redb + Tantivy) |
| `selene-extract` | Native tree-sitter extraction + standalone extractors; Rayon parallelism |
| `selene-resolve` | Reference/import/name resolution, frameworks, dynamic-dispatch synthesizers |
| `selene-graph` | Traversal (BFS/DFS, impact radius, path-finding) + query manager |
| `selene-context` | `ContextBuilder` + markdown/JSON formatter |
| `selene-mcp` | MCP server (rmcp): tools, transport, server-instructions |
| `selene-sync` | File watcher (notify) + git-hook helpers |
| `selene-installer` | Multi-agent installer: targets + registry + config writers |
| `selene-cli` | CLI (clap), daemon, telemetry, upgrade |
| `selene` | Single static binary that wires it all together |

## Build & run

```bash
cargo build              # build the workspace
cargo test               # run all tests (selene-core has the data-model tests)
cargo run -p selene      # run the scaffold binary
cargo clippy --all-targets
cargo fmt
```

## Data model

`selene-core` is the shared contract. Each of the 22 node kinds becomes a graph
node; each of the 12 edge kinds a typed relationship carrying `provenance`
(`tree-sitter` / `scip` / `heuristic`). Synthesized (dynamic-dispatch) edges are
tagged `heuristic` with `synthesizedBy` / `registeredAt` in their metadata.

## Licensing

Intended license: **MIT OR Apache-2.0** (permissive / OSI). Add `LICENSE-MIT`
and `LICENSE-APACHE` before publishing. The one DB dependency with a non-OSI
license (SurrealDB, BSL 1.1 — free to embed) sits behind the `GraphStore` trait
with a fully-permissive fallback; see PRD §5.
