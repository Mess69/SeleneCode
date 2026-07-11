# SeleneCode Master Roadmap

**Date:** 2026-07-12 · **Status:** active · **Owner:** maintainer + Claude
**Goal:** a ready-to-use `selene` binary with full CodeGraph feature parity, improved by
Rust and SurrealDB-native graph queries.

This is the *navigation* document: phase ordering, locked decisions, tech pins, and
working method. Each phase gets its own detailed implementation plan (written just-in-time
in `docs/plans/`) following the bite-sized TDD task format. This roadmap never contains
task-level code.

## Inputs (read before planning any phase)

- PRD (target architecture): `docs/specs/2026-07-11-rust-graph-db-migration-design.md`
- **Subsystem maps of the TS parity source** (exact contracts, constants, algorithms):
  `docs/reference/from-codegraph/maps/*.md` — consult the relevant map instead of
  re-reading the TS source at large. TS source: `../codegraph` (72k LOC, 162 files).
- Ecosystem research (crate versions, July 2026): `docs/reference/rust-ecosystem-2026-07.md`
- Design playbooks: `docs/reference/from-codegraph/design/*.md`

## Locked decisions (user, 2026-07-12 — supersede PRD open questions)

1. **SurrealQL-max.** Traversal logic goes *into* SurrealDB (recursive traversal
   `node:x.{1..n}(->calls->fn)`, shortest-path, kind/provenance filters). The `GraphStore`
   trait remains as a clean seam for tests/mocking, but its API is designed
   SurrealDB-shaped — no lowest-common-denominator primitives.
2. **No permissive fallback backend.** IndraDB/redb/Tantivy path is dropped. Full SurrealDB.
3. **v0 language wave (~13 grammars):** TypeScript, TSX, JavaScript, Python, Rust, Go,
   Java, Kotlin, C, C++, C#, PHP, Ruby. Everything else in wave 2.
4. **v1 = complete local binary** (index, sync/watch, daemon, full CLI, MCP server,
   8 installer targets). Cross-compilation, npm shim, Homebrew: post-v1 distribution phase.

## Technology pins (from `rust-ecosystem-2026-07.md`)

| Area | Choice |
|---|---|
| Toolchain | Rust **1.97.0** pinned in `rust-toolchain.toml`, **edition 2024**, MSRV = 1.97 |
| Storage | `surrealdb` **3.2.x** embedded. Both `kv-surrealkv` (pure Rust, default for dev/tests) and `kv-rocksdb` behind cargo features; Phase 1 bench picks the default. Pin minor version (3.x optimizer churn — issues #6800/#4767) |
| FTS | SurrealDB native (`DEFINE ANALYZER` + `DEFINE INDEX … FULLTEXT ANALYZER … BM25 HIGHLIGHTS` — 3.0 renamed SEARCH→FULLTEXT) |
| MCP | `rmcp` **2.2.x**, stdio transport, `#[tool]`/`#[tool_router]` macros, `ServerInfo.instructions` = single source of agent guidance |
| Parsing | `tree-sitter` **0.26.x** + per-language grammar crates (all 29 exist maintained; use `tree-sitter-kotlin-ng`, `tree-sitter-toml-ng`, `tree-sitter-sequel` for SQL). Grammar crates pinned exact (`=x.y.z`) + golden tests per language |
| Parallelism | `rayon` for parse fan-out; `tokio` runtime for surrealdb/rmcp; bridge via `spawn_blocking` — never block tokio workers with rayon |
| CLI | `clap` 4.6 derive |
| Watch | `notify` 8.2 + `notify-debouncer-full` |
| Config edits | `toml_edit` 0.25, `jsonc-parser` 0.33 (dprint) |
| UX | `indicatif` 0.18 + `crossterm` 0.29 |
| Tests | `insta` (snapshots), `tempfile`, `criterion` (benches) |
| Errors | `thiserror` 2 in libs, `anyhow` only in bin |

## Contracts that must never drift (checked by tests, from the maps)

- `NodeKind` (22) / `EdgeKind` (12) wire strings — already in `selene-core`.
- **Node id:** `"<kind>:" + hex(sha256("{filePath}:{kind}:{name}:{line}"))[..32]`.
- File content hash: sha256 hex of file text.
- `EXTRACTION_VERSION` (TS is at 24): any output-shape change bumps it; mismatch ⇒
  "re-index recommended", never a hard error.
- Explore budgets: exact tiers of `getExploreBudget`/`getExploreOutputBudget`
  (see `maps/mcp-context.md`); monotonicity invariant (bigger repo tier never gets a
  smaller per-file budget).
- `isError` reserved for `PathRefusal` + genuine malfunction; everything else
  success-shaped guidance (see `selene-core::Error` docs).
- Extraction errors are collected, never thrown: partial results + `ExtractionError`.
- Dynamic-dispatch bridges end-to-end or not at all; heuristic edges carry
  `provenance:'heuristic'` + `metadata.synthesizedBy`/`registeredAt`.

## Phases

Each phase = detailed plan → subagent-driven TDD execution → conventional commit per
feature → `cargo fmt && cargo clippy --all-targets && cargo test` green before every
commit. Task list mirror lives in the session task tracker (#2–#11).

### Phase 0 — Workspace foundations *(task #2)*
Edition 2024 + rust 1.97 pins, `[workspace.lints]` (incl. `clippy::unwrap_used` warn in
libs), dep version refresh from pins table, `logs/` gitignore, CI (GitHub Actions:
fmt/clippy/test), CLAUDE.md refresh (decisions). Extend `selene-core` only with types
proven shared (project layout `.selene/`, config shape) — YAGNI otherwise.
**Gate:** CI green on a clean clone.

### Phase 1 — `selene-db`: GraphStore + SurrealDB embedded *(task #3)*
Schema (nodes/edges/files + FTS index), bulk upserts (batched transactions),
single-file delete cascade, SurrealQL traversal queries (callers/callees 1-hop,
impact radius depth-N, path-finding, filters by kind/provenance), unresolved-refs
storage. **Gate (PRD §5.3):** criterion bench — bulk-load ≥ TS indexing throughput on a
synthetic 100k-node graph; deep traversal (depth 5+) latency ≤ applicative BFS baseline;
results recorded in `docs/benchmarks/`. Backend default (surrealkv vs rocksdb) decided here.

### Phase 2 — `selene-extract`: tree-sitter core + v0 wave *(task #4)*
Generic AST-walker engine (the TS `TreeSitterExtractor` equivalent), extractor-config
model per language, node-id/qualified-name/docstring helpers, scan pipeline (git fast
path via `git ls-files`, ScopeIgnore semantics, embedded-repo discovery, generated-file
detection), Rayon parallel parse, incremental single-file re-index. v0 languages with
per-language golden/insta tests mirroring TS fixtures. Function-ref capture specs
(`FN_REF_SPECS`) for v0 languages.
**Gate:** node/edge counts on shared fixtures match TS build (tolerance documented).

### Phase 3 — `selene-resolve` *(task #5)*
ReferenceResolver pass ordering, import-resolver per v0 ecosystem (+ tsconfig aliases,
cargo workspace globs), name-matcher (exact scoring/tie-breaks per `maps/resolution.md`),
chained-call resolution via `return_type`, then v0-relevant frameworks (Express,
FastAPI/Django/Flask, Spring, Gin, Axum, ASP.NET, Laravel, Rails, React Router, Cargo)
and all 5 synthesizers (callback/observer, EventEmitter, React re-render, JSX child,
Django ORM). **Gate:** dispatch-coverage fixtures resolve end-to-end (no half-bridged flow).

### Phase 4 — `selene-graph` + `selene-context` *(task #6)*
QueryManager over GraphStore (thin — traversal already in SurrealQL), ContextBuilder +
markdown/JSON formatters, `buildFlowFromNamedSymbols` heuristics, explore budgets.
**Gate:** insta snapshots of explore/node outputs match TS shapes on fixtures.

### Phase 5 — `selene-mcp` *(task #7)* → **vertical-slice milestone**
rmcp server: explore (PRIMARY), node (SECONDARY), search, callers, callees, impact,
files; server-instructions verbatim port; isError discipline; tool surface exposed even
unindexed. **Gate:** `selene index && selene serve --mcp` answers a real flow question
on a real repo with 0 Read/Grep — dogfood on codegraph or SeleneCode itself.

### Phase 6 — `selene-cli` + daemon + `selene-sync` *(task #8)*
All 22 subcommands (flags/output/exit codes per `maps/cli-daemon-sync.md`), daemon
(socket protocol, lockfile arbitration, refcount+idle timeout, PPID watchdog, stdio
proxy), FileWatcher (debounce, degrade policy, WSL2 policy), git hooks, prompt-hook,
terminal UI. **Gate:** daemon lifecycle integration tests (spawn/reuse/idle-exit/takeover).

### Phase 7 — `selene-installer` *(task #9)*
8 targets, registry + `--target auto|all|none|<id>`, toml_edit/jsonc surgical writes,
marker strips. **Gate:** all ~97 contract tests ported and green (idempotence, neighbor
preservation, reversible uninstall, byte-equal re-run ⇒ `unchanged`).

### Phase 8 — Wave 2 + transverse *(task #10)*
Remaining tree-sitter languages (swift, scala, dart, lua/luau, r, objc, cobol, vbnet,
erlang, solidity, terraform, arkts, nix, pascal, cfscript/cfquery, …) + 8 standalone
extractors (svelte, vue, astro, liquid, razor, dfm, mybatis, cfml), remaining frameworks
(SvelteKit, Vue/Nuxt, Vapor, …), telemetry, upgrade, project-config, directory/path
refusal rules, fatal handler semantics.

### Phase 9 — Parity validation + A/B + v1 polish *(task #11)*
Full-repo node/edge diff vs TS build, explore-budget equivalence, A/B benchmark rerun
(`docs/reference/from-codegraph/benchmarks/`), perf targets (indexing < TS, query ≤ TS,
cold-start < Node), README/docs. **Gate = v1:** PRD §9 success criteria checked off.

## Rust practices applied throughout

- Workspace lints table; `#![forbid(unsafe_code)]` where possible (`unsafe` only if a
  grammar binding forces it); no `unwrap`/`expect` in library crates (surfaces map to
  the success-shaped-guidance invariant); `thiserror` enums per crate wrapping
  `selene_core::Error` semantics.
- Every crate: doc comment stating role + PRD section; public API documented; unit tests
  colocated, integration tests in `tests/`, snapshots via insta, benches via criterion.
- Determinism: extraction output ordering stable (sort before store) so snapshots and
  parity diffs are byte-stable; no wall-clock in extraction output except `updated_at`.
- Conventional commits, one feature per commit; plans and maps updated when reality
  diverges (maps are the source of truth for parity questions).

## Risks (carried from PRD §8 + research)

- SurrealDB 3.x optimizer churn → pinned minor + criterion bench in CI (Phase 1 gate).
- RocksDB build weight → surrealkv default in dev; decision at Phase 1 gate.
- rmcp 2.x API churn → copy patterns from current repo examples only.
- Grammar quality tails (dart, sql) → exact pins + golden tests per language.
- Daemon platform edge cases (Windows named pipes, WSL2) → port the TS policy tables
  verbatim (`maps/cli-daemon-sync.md`), integration-test the POSIX paths in CI.
