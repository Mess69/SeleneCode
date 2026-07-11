# Rust Crate Ecosystem Research — SeleneCode Port (July 2026)

All versions verified against the crates.io API on 2026-07-12; claims cited inline. Current stable Rust: **1.97.0** (2026-07-09).

---

## 1. SurrealDB embedded (`surrealdb` 3.2.1)

**Latest stable:** `surrealdb` **3.2.1** (published 2026-07-10, very active). `surrealkv` backend crate at **0.21.2** (2026-05-12, pre-1.0). `rocksdb` (rust-rocksdb) at **0.24.0**.

**Opening an embedded DB** — via `engine::local` with a feature flag ([docs.rs/surrealdb engine::local](https://docs.rs/surrealdb/latest/surrealdb/engine/local/index.html)):

```rust
// features = ["kv-rocksdb"]  or  ["kv-surrealkv"]  or  ["kv-mem"]
let db = Surreal::new::<RocksDb>(".selene/db").await?;   // or ::<SurrealKv>, ::<Mem>
db.use_ns("selene").use_db("graph").await?;
```

`engine::any` (`Surreal<Any>`) lets you pick `mem://`, `rocksdb://path`, `surrealkv://path` from a connection string at runtime — useful for tests vs prod ([Surreal\<Any\> blog](https://surrealdb.com/blog/introducing-surrealany-dynamic-support-for-any-engine-in-rust)). Also available: `kv-mem`, `kv-indxdb` (wasm), `kv-tikv`.

**Graph edges:** `RELATE from->edge->to SET ...` / `RELATE ... CONTENT {...}` creates edge tables with `in`/`out` fields ([RELATE docs](https://surrealdb.com/docs/surrealql/statements/relate)).

**Variable-depth traversal:** SurrealQL has native recursive traversal since 2.1 — `node:x.{1..5}(->calls->fn)`, with `@` self-reference and destructuring to collect fields at each depth, e.g. `SELECT @.{1..5}.{ id, callees: ->calls->fn.* }`. Depth bound 1–256; open ranges (`{1..}`) stop at the first empty frontier. Shortest-path support exists (`{..+shortest=...}`) ([graph traversal/recursion blog](https://surrealdb.com/blog/data-analysis-using-graph-traversal-recursion-and-shortest-path), [graph traversal docs](https://surrealdb.com/docs/learn/data-models/graph/graph-traversal)). This directly answers PRD §5.4: callers/callees/impact are expressible in one SurrealQL statement — but none of it is portable to IndraDB, so keep `GraphStore` primitives at the "expand frontier by edge-kind" level and let the Surreal backend fuse them into SurrealQL internally.

**Native FTS:** yes — `DEFINE ANALYZER` (tokenizers + filters) + `DEFINE INDEX ... FULLTEXT ANALYZER a BM25 HIGHLIGHTS`, queried with the match operator and `search::score`/`search::highlight`. **Syntax break in 3.0:** the clause was renamed `SEARCH ANALYZER` → `FULLTEXT ANALYZER`; 3.x FTS is more concurrent and supports `OR` ([DEFINE INDEX docs](https://surrealdb.com/docs/surrealql/statements/define/indexes), [FTS model docs](https://surrealdb.com/docs/surrealdb/models/full-text-search)).

**Known embedded perf issues (red flags):**
- [#4767](https://github.com/surrealdb/surrealdb/issues/4767) — embedded engine measurably slower than the standalone server on aggregate scans (17s vs 5s over 69K records reported).
- [#6800](https://github.com/surrealdb/surrealdb/issues/6800) — 3.0 regressions (simple WHERE up to 2000x slower on RocksDB/Windows in one report; ORDER BY/index-only-scan inconsistencies). Opened Jan 2026, closed via PR #7018 — but it signals an optimizer still in flux across 3.x minors. **Pin the minor version and keep a query benchmark in CI** (Phase 1 bench gate is the right call).

**Binary size / build:** RocksDB is a large C++ dep — multi-minute compiles, needs clang/libclang, "trickier on Windows" ([SurrealDB embedding docs](https://surrealdb.com/docs/surrealdb/embedding/rust), [code-maven writeup](https://rust.code-maven.com/surrealdb-embedded-with-rocksdb)). Expect a `kv-rocksdb` binary in the ~60–130MB range (the full surreal server ships >100MB); `kv-surrealkv` is pure Rust (LSM, versioned) and meaningfully smaller and cross-compiles trivially ([surrealkv repo](https://github.com/surrealdb/surrealkv)) — but it's 0.21.x pre-1.0 and younger in production exposure.

**License:** `surrealdb`/`surrealdb-core` crates are **BSL 1.1** ("non-standard" on crates.io). Additional Use Grant explicitly permits **embedding in your applications**, redistribution, and internal service use; only offering SurrealDB itself as a DBaaS is restricted; each version converts to Apache 2.0 after 4 years (current code: 2030-01-01) ([license FAQ](https://surrealdb.com/license), [license repo](https://github.com/surrealdb/license)). `surrealkv` itself is Apache-2.0. **SeleneCode's embedding use is squarely permitted**, but BSL is not OSI-open-source — note it in your own LICENSE/NOTICE.

**Verdict:** Use `surrealdb 3.2.x` embedded. Default **kv-rocksdb** for perf-proven storage on the primary path; gate it behind the `GraphStore` trait and benchmark **kv-surrealkv** as the size/cross-compile-friendly option — the SDK makes the two backends API-identical, so this is a feature-flag decision you can defer. Budget CI time for RocksDB compiles and watch 3.x optimizer churn.

---

## 2. rmcp (official Rust MCP SDK)

**Latest stable:** **2.2.0** (2026-07-08). Official SDK under [modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk); very active (86 releases, commits through July 2026).

- **stdio transport:** first-class — `serve(transport::stdio())` / `transport-io` feature; also `TokioChildProcess` for the client side.
- **Tool declaration:** `#[tool]` on methods + `#[tool_router]`/`#[tool_handler]` macros; JSON schemas derived via `schemars`. Clean fit for the selene-mcp tool set.
- **server-instructions:** supported — `ServerHandler::get_info()` returns `ServerInfo { protocol_version, capabilities, server_info, instructions: Option<String> }`. This is exactly where the single-source agent guidance invariant lives ([docs.rs/rmcp](https://docs.rs/rmcp)).
- **Streaming/HTTP:** `transport-streamable-http-server` feature (axum-based `StreamableHttpService`); SSE legacy also present ([Shuttle guide](https://www.shuttle.dev/blog/2025/10/29/stream-http-mcp)). Protocol tracks the `2025-11-25` spec revision.
- **Red flags:** 2.x had breaking API churn from the 0.x/1.x era — copy patterns from the repo's current examples, not old blog posts. Requires tokio (you already need it for surrealdb).

**Verdict:** Use **rmcp 2.2.x**, stdio transport for v1. No credible alternative — it's the official SDK and healthy.

---

## 3. tree-sitter + grammar coverage

**Core:** `tree-sitter` **0.26.10** (2026-06-28). **ABI compatibility is a solved problem now:** grammar crates depend on [`tree-sitter-language`](https://crates.io/crates/tree-sitter-language) (`LanguageFn`), decoupled from the core version; core 0.26 accepts language ABI **13–15** (`MIN_COMPATIBLE_LANGUAGE_VERSION = 13`, [docs.rs](https://docs.rs/tree-sitter/latest/tree_sitter/constant.MIN_COMPATIBLE_LANGUAGE_VERSION.html)). I verified via crates.io dependency metadata that the crates below list `tree-sitter` only as a **dev-dependency** — their runtime dep is `tree-sitter-language ^0.1`, so they all load on core 0.26.

| Language | Crate | Version | Notes |
|---|---|---|---|
| typescript | `tree-sitter-typescript` | 0.23.2 | official; TS grammar fn |
| tsx | `tree-sitter-typescript` | 0.23.2 | same crate, `LANGUAGE_TSX` |
| javascript | `tree-sitter-javascript` | 0.25.0 | official |
| python | `tree-sitter-python` | 0.25.0 | official |
| rust | `tree-sitter-rust` | 0.24.2 | official, updated 2026-03 |
| go | `tree-sitter-go` | 0.25.0 | official |
| java | `tree-sitter-java` | 0.23.5 | official |
| kotlin | `tree-sitter-kotlin-ng` | 1.1.0 | **use -ng** (tree-sitter-grammars); legacy `tree-sitter-kotlin` 0.3.8 pins core `>=0.21,<0.23` — dead end |
| c | `tree-sitter-c` | 0.24.2 | official, updated 2026-04 |
| cpp | `tree-sitter-cpp` | 0.23.4 | official |
| c-sharp | `tree-sitter-c-sharp` | 0.23.5 | official, updated 2026-04 |
| php | `tree-sitter-php` | 0.24.2 | official; php + php_only fns |
| ruby | `tree-sitter-ruby` | 0.23.1 | official |
| swift | `tree-sitter-swift` | 0.7.3 | alex-pinkus grammar, updated 2026-06, active |
| scala | `tree-sitter-scala` | 0.26.0 | official, updated 2026-04 |
| bash | `tree-sitter-bash` | 0.25.1 | official |
| css | `tree-sitter-css` | 0.25.0 | official |
| html | `tree-sitter-html` | 0.23.2 | official |
| json | `tree-sitter-json` | 0.24.8 | official |
| yaml | `tree-sitter-yaml` | 0.7.2 | tree-sitter-grammars |
| toml | `tree-sitter-toml-ng` | 0.7.0 | **use -ng**; original `tree-sitter-toml` 0.20 (2022) links ancient core — dead |
| sql | `tree-sitter-sequel` | 0.3.11 | DerekStride grammar published as crate (updated 2025-10); `tree-sitter-sql` 0.0.2 (2021) is abandoned |
| lua | `tree-sitter-lua` | 0.5.0 | tree-sitter-grammars, updated 2026-02 |
| elixir | `tree-sitter-elixir` | 0.3.5 | elixir-lang official, updated 2026-03 |
| dart | `tree-sitter-dart` | 0.2.0 | UserNobody14 grammar, updated 2026-04; historically laggy — watch quality |
| haskell | `tree-sitter-haskell` | 0.23.1 | official |
| ocaml | `tree-sitter-ocaml` | 0.25.0 | official, updated 2026-05 |
| zig | `tree-sitter-zig` | 1.1.2 | tree-sitter-grammars |
| objective-c | `tree-sitter-objc` | 3.0.2 | amaanq/tree-sitter-grammars |

**Verdict:** **No vendoring required — all 29 targets have a maintained crate.** Weakest links: `tree-sitter-dart` (single-maintainer grammar) and `tree-sitter-sequel` (SQL dialects are inherently partial) — pin exact versions and keep golden-file extraction tests per language so grammar bumps can't silently shift node kinds. Pick core `0.26.x`; the `tree-sitter-language` indirection means grammar bumps and core bumps are independent.

---

## 4. Supporting crates

| Crate | Version (verified) | Status / notes |
|---|---|---|
| `notify` | 8.2.0 (2026-05) | healthy (notify-rs); pair with `notify-debouncer-full` for selene-sync |
| `clap` | 4.6.1 (2026-04) | de facto standard; derive API |
| `rayon` | 1.12.0 (2026-04) | healthy; keep off the tokio runtime threads (use `spawn_blocking` bridge) |
| `toml_edit` | 0.25.12 (2026-05, TOML spec 1.1) | healthy (toml-rs) |
| `jsonc-parser` | 0.33.0 (2026-07) | dprint, active — good for tsconfig.json |
| `indicatif` | 0.18.6 (2026-07) | healthy |
| `crossterm` | 0.29.0 (2025-04) | slower cadence but standard; fine |
| `insta` | 1.48.0 (2026-06) | healthy — ideal for context-output snapshots |
| `tempfile` | 3.27.0 | healthy |
| `thiserror` | 2.0.18 (2026-01) | use 2.x everywhere |
| `tantivy` | 0.26.1 (2026-04) | maintained under quickwit-oss (post-Datadog acquisition, still releasing). Solid plan-B FTS |
| `redb` | 4.1.0 (2026-04) | healthy, pure Rust, 4.x stable format |
| `indradb` | 5.0.0 (2025-08) | **yellow flag:** not archived (2.4K stars) but effectively single-maintainer; two commit bursts in 2025, nothing since v5.0.0 (2025-08-16) per [GitHub](https://github.com/indradb/indradb) |

**Verdict:** All green except the fallback store. IndraDB is alive-but-dormant — acceptable only because the fallback path is secondary. Given `GraphStore` is your own trait, seriously consider **redb + tantivy with a hand-rolled adjacency layout** instead of IndraDB: fewer moving parts, both deps are first-class maintained, and PRD §5.4 already forces portable traversal primitives.

---

## 5. Rust 2026 workspace practices

- **Toolchain:** stable **1.97.0** ([release blog](https://blog.rust-lang.org/2026/07/09/Rust-1.97.0/)); pin in `rust-toolchain.toml` and set `rust-version` (MSRV) in the workspace.
- **Edition:** **2024** is current — there is no 2026 edition (3-year cadence; next lands ~2027). Use `edition = "2024"` + `[workspace.package]` inheritance.
- **Lints:** use the `[workspace.lints]` table (stable since 1.74), inherited per-crate with `[lints] workspace = true`. Sensible baseline: `rust: { unsafe_code = "warn", rust_2018_idioms = "warn", unused_qualifications = "warn" }`, `clippy: { all = "warn", pedantic = "warn" }` with targeted `allow`s; add `clippy::unwrap_used = "warn"` in lib crates given the "success-shaped guidance, never crash" invariant.
- **Release tooling:** **`dist` (cargo-dist) v0.32.0 (2026-05-22)** is still the best fit and still shipping — repo pushed 2026-07-07 ([releases](https://github.com/axodotdev/cargo-dist/releases)) — **but** axo the company is gone (axo.dev domain is parked/for sale), so it's community-momentum maintenance now. It generates GitHub Actions release matrices, shell/powershell installers, and an **npm installer** out of the box. Fallback if it stalls: plain GH Actions matrix + `taiki-e/upload-rust-binary-action`.
- **npm shim (biome/esbuild pattern):** one meta-package with a tiny JS launcher + `optionalDependencies` on per-platform packages (`@selene/cli-darwin-arm64`, …) carrying `os`/`cpu` fields; postinstall-free resolution at runtime. `dist`'s npm installer emits this shape; hand-rolling it is ~200 lines if you drop dist.
- **musl static linking with RocksDB — real gotchas** ([rust-rocksdb #635](https://github.com/rust-rocksdb/rust-rocksdb/issues/635), [#440](https://github.com/rust-rocksdb/rust-rocksdb/issues/440), [#174](https://github.com/rust-rocksdb/rust-rocksdb/issues/174)): bindgen/libclang can't find musl headers (`stdarg.h` errors), C++ runtime link failures (`__dso_handle`, glibc-versioned symbols leaking in), and cross-from-macOS is worst. Working recipe: build in a musl container (`cross` with a custom image or `messense/rust-musl-cross`), `ROCKSDB_STATIC=1`, static libstdc++, and `-C target-feature=+crt-static`. **Pragmatic recommendation:** ship musl builds only if you keep the `kv-surrealkv` (pure-Rust) backend for Linux-static targets, and ship glibc (gnu) builds for the RocksDB flavor — this is exactly the flexibility the `GraphStore`/feature-flag split buys you. Alpine/NixOS users are the musl audience; everyone else is fine on gnu with a modest glibc floor.

**Verdict:** Rust 1.97 + edition 2024 + workspace lints table; `dist` 0.32 for releases with the npm-shim installer; treat "single static binary" as per-target — fully static musl only for the pure-Rust storage backend, glibc dynamic-libc for RocksDB builds.

---

### Sources
- https://docs.rs/surrealdb/latest/surrealdb/engine/local/index.html · https://surrealdb.com/docs/surrealql/statements/relate · https://surrealdb.com/blog/data-analysis-using-graph-traversal-recursion-and-shortest-path · https://surrealdb.com/docs/surrealql/statements/define/indexes · https://surrealdb.com/docs/surrealdb/models/full-text-search · https://surrealdb.com/license · https://github.com/surrealdb/license · https://github.com/surrealdb/surrealdb/issues/6800 · https://github.com/surrealdb/surrealdb/issues/4767 · https://github.com/surrealdb/surrealkv · https://surrealdb.com/docs/surrealdb/embedding/rust
- https://github.com/modelcontextprotocol/rust-sdk · https://docs.rs/rmcp · https://www.shuttle.dev/blog/2025/10/29/stream-http-mcp
- https://docs.rs/tree-sitter · https://docs.rs/tree-sitter/latest/tree_sitter/constant.MIN_COMPATIBLE_LANGUAGE_VERSION.html · https://crates.io/crates/tree-sitter-language · crates.io API (versions + dependency kinds, queried 2026-07-12)
- https://github.com/indradb/indradb · GitHub API (pushed_at 2025-08-16)
- https://blog.rust-lang.org/2026/07/09/Rust-1.97.0/ · https://github.com/axodotdev/cargo-dist/releases · https://github.com/rust-rocksdb/rust-rocksdb/issues/635 · https://github.com/rust-rocksdb/rust-rocksdb/issues/440 · https://github.com/rust-rocksdb/rust-rocksdb/issues/174
