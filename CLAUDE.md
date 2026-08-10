# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

SeleneCode is **local-first code intelligence, in Rust** — the Rust port of [CodeGraph](../codegraph). It parses any supported codebase with tree-sitter, stores symbols/edges/files in an **embedded graph database**, and exposes a knowledge graph to AI agents over **MCP**. Per-project data lives in `.selene/`. Extraction is **deterministic** (AST-derived, never LLM-summarized). The goal is a **single static binary** that serves as installer, indexer, and MCP server.

**Status: usable end-to-end.** `selene-core` (the data model), `selene-db` (Phase 1 — `GraphStore` + SurrealDB embedded), `selene-extract` (Phase 2 — tree-sitter extraction over the 12 v0 languages), `selene-resolve` (Phase 3 — the `resolve_one` ladder, imports, the name matcher, 11 framework resolvers, 4 dynamic-dispatch synthesizer channels + the Django ORM descriptor), `selene-graph`, `selene-context` (Phase 4), `selene-mcp` (Phase 5), **and the Phase 6+7 surface — `selene-cli` (~22 subcommands), `selene-sync` (git hooks + daemon watcher), `selene-installer` (8 agents)** — are implemented and binary-tested. Onboarding is one command per project (`selene install` → auto-init + hooks + MCP config; see README).

**Perf status (2026-07-17, supersedes the ⛔ of 07-14): SeleneCode is FASTER than the CodeGraph TS build on all three benchmark corpora** (codegraph-src 0.77×, selene-crates 0.77×, django 0.96× — `docs/plans/2026-07-16-optimization-roadmap.md`, addenda 1–4: the regex-hoist ladder fix, the ASCII-regex RAM fix, txn-batched retried writes, live RocksDB caps via the vendored SDK patch). The remaining gaps, both tracked in the roadmap: **RAM on medium repos** (django 2.0 GiB vs TS 0.6 GiB) and **VS Code scale not yet re-measured** since the optimizations (was 2× slower). Task 19 (`isError` discipline) and the Task 20 dogfood gate are built and run; the open large-repo limitation is semantic recall, not tooling (`docs/benchmarks/2026-07-phase5-dogfood.md`). The write path was re-argued with measurements (roadmap addendum 3): the persist floor is engine execution, not fsyncs — the SurrealQL-max decision stands. Phase 3 is held by **two gates**: a TS↔Rust resolution-parity gate on edge *identity* (tolerance 0) and a dispatch-coverage gate asserting whole *flows* — see `crates/selene-resolve/src/lib.rs`. **Start from `RESUME.md` at the repo root** — it is the single handoff doc. The full target architecture is the PRD: `docs/specs/2026-07-11-rust-graph-db-migration-design.md`. Read it before designing anything — it is the source of truth for the crate boundaries, the DB decision, and the invariants below.

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

**⚠ Disk discipline — `target/` is a known trap here.** Cargo never garbage-collects, and this
workspace (SurrealDB embedded + RocksDB + 12 tree-sitter grammars + ~100 test binaries) has bloated
`target/` past **150 GB twice**, once filling the disk (which masquerades as a memory crisis:
swap can't grow → `fork()` fails — RESUME.md §6). **Before or after any heavy build/test run, check
`du -sh target`; over ~30 GB, run `rm -rf target/debug`** (regenerable; keep `target/release`).
A PreToolUse hook in `.claude/settings.json` also warns on any `cargo` command when free disk
drops under 100 GB.

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
- `selene-extract` — tree-sitter extraction over **natively-linked** grammars (the WASM layer — worker pool, parser resets, OOM retries — is deleted, not ported), rayon fan-out with an **ordered** DB commit, the scan pipeline (git fast path + FS fallback), and incremental re-index. Emits **zero cross-file edges**: anything beyond the file leaves as an `UnresolvedReference` for Phase 3. Its `lib.rs` carries the public-interface ledger, the deferrals, and the known parity deviations.
- `selene-resolve` — Phase 3, **implemented**. Binds every cross-file reference: the
  `resolve_one` ladder (order *is* behavior), import + name matching, the framework
  registry (11 v0 frameworks, data-driven — adding one is a file plus a registry row),
  and the dynamic-dispatch synthesizers. Route nodes keep **hashed** ids like every
  other node; their semantics live in indexed fields (`route_method`/`route_path`/
  `framework`) and are queried, never parsed out of an id. Its `lib.rs` carries the
  public-interface ledger, the deferrals, the two gates, and the deviation-ledger
  pointer (`tests/fixtures/dispatch/deviations.toml` is the single authority).
- `selene-graph` (traversal/`QueryManager`), `selene-context` (the `explore` answer — relevance, the
  Flow spine, budgets) and `selene-mcp` (the MCP surface) — **implemented**. `selene-context`'s
  `relevance.rs` carries the pass ledger, **and the record of what has been measured and REVERTED**:
  read it before touching ranking, or you will retry a dead approach. The seed picker's decisive
  signal is **directional** — concepts spanned via *outgoing* calls, which separates an orchestrator
  from plumbing (`out=0, in=huge`) structurally rather than by weighting.
- `selene-sync` (git hooks + worktree watcher), `selene-installer` (8 agents, 4 config formats),
  `selene-cli` (clap surface: init/install/sync/status/query/explore/viz/daemon/…) — **built and
  binary-tested** (Phases 6 + 7).

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
