# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

SeleneCode is **local-first code intelligence, in Rust** — the Rust port of [CodeGraph](../codegraph). It parses any supported codebase with tree-sitter, stores symbols/edges/files in an **embedded graph database**, and exposes a knowledge graph to AI agents over **MCP**. Per-project data lives in `.selene/`. Extraction is **deterministic** (AST-derived, never LLM-summarized). The goal is a **single static binary** that serves as installer, indexer, and MCP server.

**Status: scaffold.** `selene-core` (the data model) is implemented and tested; the other layer crates are stubs. The full target architecture is the PRD: `docs/specs/2026-07-11-rust-graph-db-migration-design.md`. Read it before designing anything — it is the source of truth for the crate boundaries, the DB decision, and the invariants below.

## Build, Test, Run

```bash
cargo build                 # build the workspace
cargo test                  # all tests (selene-core: data-model roundtrip tests)
cargo run -p selene         # run the scaffold binary
cargo clippy --all-targets  # lint
cargo fmt                   # format (rustfmt.toml: max_width 100)

cargo test -p selene-core   # single crate
cargo test -p selene-core kind_counts   # single test by name
```

Toolchain is pinned in `rust-toolchain.toml` (stable + rustfmt + clippy).

## Architecture — the crate workspace

Layered pipeline (mirrors CodeGraph):

```
files → selene-extract (tree-sitter) → selene-db (nodes/edges/files)
              ↓
       selene-resolve (imports, name-matching, frameworks, dyn-dispatch synthesis)
              ↓
       selene-graph (callers, callees, impact, path)
              ↓
       selene-context (markdown/JSON for AI consumption)
              ↓
       selene-mcp / selene-cli (surfaces)  → selene (bin)
```

- `selene-core` — shared types. `NodeKind` (22) / `EdgeKind` (12) are exhaustive enums; their `as_str()` and serde output are the wire contract and must not drift. Also `Provenance`, `Visibility`, `Node`, `Edge`, `Error`.
- `selene-db` — everything DB is behind a **`GraphStore` trait** (a seam for tests/mocking, not a portability layer). Sole backend: **SurrealDB embedded**. **Decision (2026-07-12):** SurrealQL-max — traversal logic is pushed into SurrealQL (recursive `.{1..n}(->calls->fn)`, shortest-path); the permissive fallback (IndraDB/redb + Tantivy) from PRD §5.2 is **dropped**, and the PRD §5.4 spike is resolved accordingly.
- `selene-extract`, `selene-resolve`, `selene-graph`, `selene-context`, `selene-mcp`, `selene-sync`, `selene-installer`, `selene-cli` — stubs; each crate's `lib.rs` names its role + PRD section.

Shared third-party deps and their versions are declared once in the root `[workspace.dependencies]`; crates opt in with `dep.workspace = true`.

## Invariants — do not regress (from the CodeGraph experience, PRD §8.2)

These are hard-won and carry over verbatim to SeleneCode:

- **Sufficiency / anti-Read.** The product's value is that an agent answers a structural/flow question with a few fast tool calls and **zero Read/Grep**. A flow question should resolve in **1 explore call on small repos, scaling to 3–5 on large**. Every change is judged by: does the answer stop the agent from reading?
- **Explore budget stays monotonic with repo size.** Larger repos never get a smaller per-file output budget than smaller ones.
- **`isError` is reserved.** Only `Error::PathRefusal` (security) — and genuine malfunctions — surface as `isError`. Every expected/recoverable condition (not indexed, symbol not found, file not in index) returns **success-shaped guidance**. One or two `isError` responses early and an agent abandons the tool.
- **Dynamic-dispatch coverage must be end-to-end.** Partial coverage is *worse* than none — a half-bridged flow reveals a hop the agent then reads to finish. Close the flow, then re-measure.
- **Deterministic extraction.** Derived from the AST, never LLM-summarized.
- **Single source of tool guidance.** The MCP `server-instructions` are the one place agent-facing guidance lives.

## Roadmap & plans

The build order lives in `docs/plans/2026-07-12-selenecode-roadmap.md` (phases 0–9, locked
decisions, tech pins, contract list). Each phase gets a just-in-time detailed plan in
`docs/plans/` before implementation. The PRD §5.4 spike is **resolved** (SurrealQL-max, no
fallback backend); the Phase 1 benchmark gate (PRD §5.3) still applies.

## Reference

- `docs/reference/from-codegraph/maps/` — **subsystem maps of the TS parity source**
  (exact contracts, constants, algorithms per subsystem). Consult the relevant map
  *before* reimplementing anything, instead of re-reading the TS source at large.
- `docs/reference/rust-ecosystem-2026-07.md` — crate versions/status pins (July 2026).
- `docs/reference/from-codegraph/design/` + `benchmarks/` — language-agnostic design +
  benchmark notes ported from CodeGraph TS (dynamic-dispatch coverage playbook,
  callback/value-reference edge synthesis, chained-call resolution, adaptive explore
  sizing, A/B methodology). Consult before building the resolver/synthesizer and the
  explore budget.
