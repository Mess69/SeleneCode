# Phase 3 — `selene-resolve`: cross-file resolution, frameworks, dispatch synthesis — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **This plan is written in three parts.** Part A (this file) owns the shared front-matter
> — goal, architecture, global constraints, the whole crate's file structure, and the
> task-sequencing contract — plus Tasks 1–10 (the resolution core). Part B continues at
> Task 11 (frameworks + synthesizers). Part C closes the phase (batching/persistence, the
> pass driver, sync integration, the dispatch-coverage gate, facade). The Global
> Constraints and the File structure below bind **all three parts**.

**Goal:** the `selene-resolve` crate — everything CodeGraph does between "extraction wrote
nodes + `unresolved_refs`" and "the graph has cross-file edges". Phase 2 deliberately emits
**zero cross-file edges**: every reference beyond the file left as an `UnresolvedRef`
(`crates/selene-extract/src/lib.rs`, "Extraction emits no cross-file edges"). Phase 3 binds
them: a `ReferenceResolver` with a fixed pass order, import resolution per v0 ecosystem, a
scored name-matcher, chained-call resolution via `return_type`, function-ref resolution,
the v0-relevant framework resolvers, and all 5 dynamic-dispatch synthesizers.
**Gate (Part C):** dispatch-coverage fixtures resolve **end-to-end** — no half-bridged flow.

**Architecture:** `selene-resolve` sits between `selene-extract` and `selene-graph`:

```
selene-extract → UnresolvedRef rows (pending)  ─┐
                                                ├→ ReferenceResolver ─→ Edge (calls/references/
selene-db (GraphStore: nodes, edges, refs)     ─┘      (this crate)         imports/implements/…)
```

The resolver is **generic over `S: GraphStore`** — never tied to `SurrealStore`. The TS
`ResolutionContext` (an interface of ~20 data-access closures) becomes a **`ResolutionContext`
trait** whose store-backed implementation (`StoreContext<S>`) is the only thing that talks to
`selene-db`; every strategy function takes `&C: ResolutionContext`, so the whole matcher is
unit-testable against an in-memory fake with zero DB. Filesystem reads (file text, tsconfig,
`go.mod`, `package.json`) are also context methods — the resolver never touches `std::fs`
directly outside `StoreContext`.

**Ordering is behavior.** The `resolve_one` ladder, the `match_reference` strategy order, the
≥0.9 return-immediately threshold, first-wins ties, `preferred_fqn` before
`prefer_call_site_file`, backward-then-forward scans — all of it is observable in edge output.
Port it as a **fixed pipeline, not a rules engine** (`maps/resolution.md` §Rust port notes).

**Tech Stack:** `selene-core` (wire types), `selene-db` (the `GraphStore` trait — the crate
depends on the trait, and on `SurrealStore` only in `[dev-dependencies]` for integration
tests), `regex` (pre-compiled templates + `regex::escape` for per-ref receiver patterns),
`fancy-regex` (ONLY for the Lua/Luau negative-lookahead receiver pattern — the `regex` crate
has no lookahead), `lru` (the `LRUCache` port), `serde_json` (edge metadata, `candidates`),
`json5` / `jsonc-parser` (tsconfig JSONC tolerance, ohpm `oh-package.json5`), `thiserror` 2;
dev: `insta`, `tempfile`, `tokio` (multi-thread test runtime). Rayon is **not** used here —
resolution is a single ordered pipeline whose determinism contract (first-wins ties,
insertion-ordered candidate lists) is easier to keep sequential; revisit only with a bench.

**Reference (in priority order):**
- `docs/reference/from-codegraph/maps/resolution.md` — **THE parity contract** for Parts A
  and C (§Pass ordering, §Batching, §Caches, §`resolveOne` pipeline, §`matchReference`
  strategy order, §Confidence/scoring constants, §Receiver-type inference, §Import resolution
  per ecosystem, §Edge creation, §Wire/contract surfaces, §Rust port notes). Every constant
  this plan quotes is copied from it **verbatim** — a task never needs to open the map to
  execute, but must open it when this plan is ambiguous.
- `docs/reference/from-codegraph/maps/frameworks-synthesis.md` — the parity contract for
  Part B.
- `docs/reference/from-codegraph/design/chained-call-resolution.md` — the 3-part mechanism,
  the 13 covered languages, and **why TypeScript/Luau were evaluated and skipped**.
- `docs/reference/from-codegraph/design/function-ref-capture.md` — the capture half shipped
  in Phase 2 (`FN_REF_SPECS`); its §Precision rules 3, 5, 9, 10 are the **resolution** half
  and are the contract for Task 10.
- `docs/reference/from-codegraph/design/dynamic-dispatch-coverage-playbook.md` +
  `callback-edge-synthesis.md` — Part B/C.
- TS parity source `../codegraph` (relative to repo root): consult **ONLY** the specific file
  a task names (e.g. `src/resolution/name-matcher.ts` lines 1092–1237), or a named
  describe-block of `__tests__/resolution.test.ts` — never at large.

## Global Constraints (bind Parts A, B and C)

- **Dynamic-dispatch coverage is end-to-end or not at all.** Partial coverage is *worse* than
  none — a half-bridged flow reveals a hop the agent then has to Read, which is exactly the
  behavior the product exists to eliminate. Hard invariant (PRD §8.2, CLAUDE.md). If a
  synthesizer can only close half a flow, it does not ship; close the flow, then re-measure.
- **Validated inference — no edge beats a wrong edge.** Every type guess ends in
  `resolve_method_on_type`: the method must actually exist on the inferred type (or a
  supertype it conforms to), else **no edge**. Path-shaped refs (PHP includes, COBOL
  copybooks, Nix paths, Terraform) **never** fall through to symbol name-matching (#660).
  Ubiquitous names **decline** rather than guess (`AMBIGUOUS_NAME_CEILING`, #999). Regressing
  any of these produces wrong edges, which are worse than missing ones.
- **Provenance.** AST-derived/resolved edges carry `provenance: Provenance::TreeSitter`
  (`"tree-sitter"`); every heuristic/synthesized edge carries `provenance:
  Provenance::Heuristic` **plus** `metadata.synthesizedBy` and `metadata.registeredAt`
  (Part B). Never restring an enum — always `selene_core::{NodeKind, EdgeKind,
  Provenance}::as_str()`.
- **Determinism.** Same input ⇒ same edges, in the same order. Candidate lists are
  insertion-ordered; ties are first-wins; sort before persisting where the source order is not
  inherently stable. **No wall-clock in output** — the sole exception is `Node.updated_at`
  (Phase 2's) and the heuristic-edge `metadata.registeredAt` (Part B), which is a *stamped*
  value, not read from the clock during matching. Two resolver runs over one graph produce
  byte-identical edge sets.
- **Errors are collected, never thrown; `isError` is reserved.** A resolver failure degrades
  one reference, never the pass. `Result::Err` is for a genuine store malfunction only
  (`selene_db::Error`); "not found", "ambiguous", "no candidates" are `Ok(None)`.
- **No `unwrap`/`expect` outside `#[cfg(test)]`.** House idiom: `#[allow(clippy::unwrap_used)]`
  is permitted **only** on a `Regex::new` over a compile-time literal, and only with (a) a
  justification comment and (b) a test that exercises the regex (so a bad literal fails a test,
  not production). Prefer `std::sync::LazyLock<Regex>` for the static pattern tables.
- **The resolver is generic over `S: GraphStore`.** No `selene-db::SurrealStore` in the
  library's dependency graph (dev-dependencies only). `GraphStore` is not `dyn`-safe (RPITIT),
  so thread the type parameter — never `Box<dyn GraphStore>`.
- **`ResolvedRef.original.reference_name` MUST equal the stored row's `reference_name`.**
  The keyed delete (`GraphStore::delete_resolved`) matches on it; a resolver that returns a
  *mutated* name no-ops the delete, the offset-0 batch loop re-reads the same rows forever, and
  the run explodes (the observed TS failure: 5M edges / 1.4 GB, #760). The Go bare-name chain
  fallback in Task 9 is the specific place this bites — it resolves a *synthetic* ref but must
  return the **original** one. Part C's non-progress guard is the backstop, not the fix.
- **Confidence 0.9 is the return-immediately threshold** in `resolve_one`; the final pick is
  **max-confidence, first-wins on ties** (a `reduce` that keeps the earlier candidate on
  equality). Do not "improve" this into a scoring blend.
- **`resolvedBy` wire strings** (persisted in edge metadata, read by explore/node output and
  by edge resurrection): `exact-match | import | qualified-name | framework | fuzzy |
  instance-method | file-path | function-ref`. Part B adds no new values; the batched pass adds
  the **stats** key `callback-synthesis` (a stats key, not a `resolvedBy`).
- **Edge metadata keys** are a contract: `confidence`, `resolvedBy`, `refName` (the ORIGINAL
  `reference_name` — the #1240 resurrection contract, read by
  `GraphStore::replace_file_extraction`), `refKind` (**only** when kind promotion changed the
  kind), `fnRef: true` (function_ref only). Part B adds `synthesizedBy`/`registeredAt` for
  heuristic edges.
- **`function_ref` is an internal reference kind, never an `EdgeKind`.** It resolves into an
  edge of kind `references` with `metadata.fnRef = true`.
- **Env vars** carry the `SELENE_` prefix: `SELENE_RESOLVER_CACHE_SIZE` (positive int, default
  `DEFAULT_CACHE_LIMIT = 5000`), `SELENE_AMBIGUOUS_NAME_CEILING` (default 500). TS's
  `CODEGRAPH_SYNTH_TIMINGS` (timing logs) is dropped — use `tracing` spans.
- **Cooperative yield is dropped.** `cooperative-yield.ts` (`maybeYield`, 250 ms budget) is a
  Node event-loop artifact serving a liveness watchdog we do not have. Resolution runs off the
  async runtime (`spawn_blocking`-shaped, like Phase 2's extraction); document the drop in
  `lib.rs`. Do **not** map it to `tokio::task::yield_now`.
- **Tasks must be completable by a fresh subagent in one session.** Each task states its Files
  and Interfaces, is TDD (write the ported contract test first, watch it fail, then implement),
  and ends in **one conventional commit**. `cargo fmt && cargo clippy --all-targets && cargo
  test -p selene-resolve` green before every commit.

## File structure (all under `crates/selene-resolve/` unless noted)

Ownership: **[A]** = Part A (this file, Tasks 1–10) · **[B]** = Part B (frameworks +
synthesizers) · **[C]** = Part C (batching, passes, sync, gate, facade).

```
Cargo.toml                  [A] selene-core, selene-db (trait), regex, fancy-regex, lru,
                                serde_json, json5/jsonc-parser, thiserror; dev: insta,
                                tempfile, tokio, selene-db/SurrealStore, selene-extract
src/lib.rs                  [A creates / C polishes] crate docs, ledger, re-exports
src/error.rs                [A] ResolveError (thiserror) wrapping selene_db::Error
src/types.rs                [A] ResolvedRef, ResolvedBy, ResolutionResult, ResolutionStats,
                                ImportMapping, ReExport
src/context.rs              [A] ResolutionContext trait + StoreContext<S: GraphStore>
                                (the ONLY module that touches selene-db or std::fs)
src/cache.rs                [A] LRU wrappers + cache-limit policy (DEFAULT_CACHE_LIMIT)
src/families.rs             [A] LANGUAGE_FAMILY, same_language_family, gate_language,
                                gate_framework_language
src/builtins.rs             [A] JS_BUILT_INS, REACT_HOOKS, PYTHON_BUILT_INS/TYPES/METHODS,
                                GO_STDLIB_PACKAGES, GO_BUILT_INS, C_BUILT_INS, CPP_BUILT_INS
src/resolver.rs             [A T3 creates] ReferenceResolver: resolve_one ladder, deferral
                                queues, create_edges.  ⚠ SHARED SEAM — see sequencing below
src/imports/mappings.rs     [A] extract_import_mappings, extract_re_exports (regex, per lang)
src/imports/aliases.rs      [A] tsconfig/jsconfig paths (JSONC-tolerant), apply_aliases
src/imports/workspace.rs    [A] npm/yarn/bun/pnpm workspaces + ohpm file: deps
src/imports/go_module.rs    [A] go.mod module directive
src/imports/cpp_includes.rs [A] compile_commands.json -I scan + heuristic include dirs
src/imports/mod.rs          [A T5 creates] resolve_import_path, resolve_via_import,
                                resolve_jvm_import, find_exported_symbol (re-export chase)
src/matcher/mod.rs          [A T7 creates] match_reference dispatcher (strategy order)
                                ⚠ SHARED SEAM — see sequencing below
src/matcher/scoring.rs      [A] find_best_match weights, prefer_call_site_file,
                                pick_closest_file_node, pick_closest_jvm_candidate
src/matcher/names.rs        [A] match_by_file_path / _qualified_name / _exact_name / fuzzy
src/matcher/receiver.rs     [A] infer_cpp_receiver_type, infer_local_receiver_type (the
                                per-language regex tables), infer_java_field_receiver_type
src/matcher/method.rs       [A] match_method_call, resolve_method_on_type (validated)
src/matcher/chains.rs       [A] match_cpp_call_chain / _scoped_ / _dotted_ + CHAIN_LANGUAGES
src/matcher/fnref.rs        [A] match_function_ref, resolve_this_member_fn_ref
src/passes.rs               [A T9/T10] resolve_chained_calls_via_conformance,
                                resolve_deferred_this_member_refs (drains the T9/T10 queues)
src/strip_comments.rs       [B] stripCommentsForRegex (frameworks + synthesizers + MCP)
src/frameworks/mod.rs       [B] FrameworkResolver trait, registry, detect_frameworks
src/frameworks/*.rs         [B] express, fastapi/django/flask, spring, gin, axum, aspnet,
                                laravel, rails, react_router, cargo_workspace
src/synth/mod.rs            [B] synthesizer registry + shared registration-site helpers
src/synth/*.rs              [B] callback/observer, event_emitter, react_rerender, jsx_child,
                                django_orm
src/batch.rs                [C] resolve_all / resolve_and_persist / _batched (PERSIST_CHUNK,
                                offset-0 loop, non-progress guard), warm_caches
src/sync.rs                 [C] scoped re-resolve by changed files, failed-ref retry,
                                orphan sweep
tests/context_fake.rs       [A T2] the in-memory ResolutionContext fake (shared test rig —
                                every later task's unit tests build on it)
tests/imports_*.rs          [A] per-ecosystem import tests
tests/matcher_*.rs          [A] name-matcher / receiver / chains / fnref contract tests
tests/frameworks_*.rs       [B]
tests/synth_*.rs            [B]
tests/resolve_e2e.rs        [C] extract → resolve → assert edges, on a real SurrealStore
tests/dispatch_gate.rs      [C] THE Phase 3 gate (end-to-end dispatch coverage)
docs/benchmarks/2026-07-phase3-dispatch-coverage.md   [C] gate results
```

Nothing in `selene-core` or `selene-db` changes in Part A **except** the one open question
below (`delete_resolved` key arity), which is a maintainer decision, not a task's to take.

## ⚠ Task sequencing — the shared seams

Phase 2 learned this the hard way: five agents editing the walker's dispatch ladder in
parallel worktrees collided on every merge. Files touched by more than one task are listed
here; tasks that touch the same file are **strictly sequential** — never dispatch two of them
to parallel subagents or worktrees.

| Shared file | Tasks that modify it | Rule |
|---|---|---|
| `src/resolver.rs` (the `resolve_one` ladder) | **A3** (creates: ladder + built-in filter + pre-filter + `create_edges`), **A6** (steps 5–8: JVM import, Razor, import branch), **A9** (step 11: chain deferral), **A10** (step 4: `function_ref` branch), **B** (step 7: the frameworks loop), **C** (batching/passes wiring) | STRICTLY SEQUENTIAL in that order. Each task inserts into a **named, pre-stubbed** ladder step that A3 lays down as a `// step N:` comment + a no-op call — never re-orders the ladder. |
| `src/matcher/mod.rs` (the `match_reference` dispatcher) | **A7** (creates: strategy order, steps 0/1/3/4), **A8** (step 2 `match_method_call`), **A9** (steps 1b/1c/1d chains), **A10** (the `function_ref` short-circuit) | STRICTLY SEQUENTIAL. Same rule: A7 lays the full ladder down with stubbed steps; later tasks fill one step each. |
| `src/passes.rs` | **A9** (chain conformance drain), **A10** (this-member drain) | Sequential (A9 → A10). |
| `src/types.rs`, `src/context.rs` | **A2** (creates), **B** (may add framework-facing context methods) | B appends only; never reshapes A2's types. |
| `src/imports/mod.rs` | **A5** (creates: `resolve_import_path`), **A6** (`resolve_via_import` branches) | Sequential (A5 → A6). |
| `src/lib.rs` | every task (one `mod`/`pub use` line) | Append-only, one line per task; Part C does the final facade pass. |

Everything else is task-private and **parallelizable**: A4's four import-input modules
(`mappings`/`aliases`/`workspace`/`go_module`) are independent of A5/A6 and of each other;
A7's `scoring.rs`/`names.rs`, A8's `receiver.rs`/`method.rs`, A9's `chains.rs`, A10's
`fnref.rs` are each a fresh file (only their one-line hook into the shared dispatcher is
sequential). Part B's framework and synthesizer files are all task-private after B's
registry task lands.

---

## Tasks

<!-- Part A: Tasks 1–10 (resolution core). Part B continues at Task 11. -->

### Task 1: Spike — de-risk the resolver's assumptions (GraphStore seam, regex, caches)

**Files:** Create: `crates/selene-resolve/Cargo.toml` (deps per Tech Stack),
`crates/selene-resolve/tests/spike_seam.rs`. Modify: root `Cargo.toml`
(`[workspace.dependencies]`: add `fancy-regex`, `lru`, `json5` — **none are in the roadmap
pins table**; pin them and note the addition in the commit body).

**Interfaces:** none — throwaway knowledge, kept as a smoke test. **Every finding gets
written into a comment block at the top of `spike_seam.rs`** and, where it changes a later
task, into this plan (edit the task, do not silently diverge).

This task exists because Parts A/B/C are all built on assumptions about a trait
(`GraphStore`) and a TS source (`src/resolution/*`) that no one has yet checked against each
other. Every item below has been observed to be *plausibly* wrong.

- [ ] **The `delete_resolved` / `mark_failed` key arity — the #1 risk.** The TS lifecycle keys
  a processed row by `fromNodeId + referenceName + referenceKind` (map §Wire/contract
  surfaces). `GraphStore::delete_resolved(&[(String, String)])` and `mark_failed` take a
  **2-tuple** `(from_node_id, reference_name)` — no kind. Verify against
  `crates/selene-db/src/store.rs` + its `unresolved.rs` impl what a 2-part key actually
  deletes, then write a test with **two pending refs from the same node with the same name but
  different kinds** (the real case: Phase 2 emits both a `calls` ref and a `function_ref` for
  `foo` from one function — see `fnref.rs`) and record whether resolving one drains the other.
  If it does, that is a **silent recall bug** (the second ref never resolves, and the
  orphan-sweep pending count still reaches 0, so nothing detects it). Do NOT fix it here —
  record the finding, and surface the maintainer decision (extend the trait key to 3-part vs.
  accept the collision) named in "Open coordination points".
- [ ] **`get_method_matches` has no store primitive.** The TS `ResolutionContext.getMethodMatches`
  (memo key `` `${language} ${type}::${method}` ``) backs `resolveMethodOnType`, which needs
  "method nodes, same language, `qualified_name == type::method` OR ending with
  `::type::method`". The trait offers `get_nodes_by_name` and `get_nodes_by_qualified_name`
  (exact only) — there is **no suffix query**. Confirm the intended Rust shape:
  `get_nodes_by_name(method)` → filter in-resolver on `qualified_name` + language + kind,
  memoized in an LRU. Measure it on a realistic name (`get`, `handle`) against a store loaded
  from a real repo index — if a hot method name returns thousands of nodes, record the cost and
  whether `AMBIGUOUS_NAME_CEILING` already bounds it.
- [ ] **`get_supertypes` shape.** TS takes a *name*; the design doc's #756 note records that
  the name-keyed version produced a cross-class wrong edge on rails and was replaced by a
  **node-anchored** walk. Confirm the Rust version is buildable from the trait as:
  `outgoing(node_id, &[EdgeKind::Implements, EdgeKind::Extends], None)` → supertype nodes →
  `children(supertype_id)` (the `contains` edge) for member lookup. Prove it with a two-file
  fixture (class + superclass in different files) through a real `SurrealStore`.
- [ ] **`all_node_names` vs the TS streaming `iterateNodeNames`.** The warm-cache pass builds
  `known_names: HashSet<String>` (used by the `has_any_possible_match` pre-filter on every
  ref). The trait returns a whole `Vec<String>`. Measure it on the largest index available
  (index `../codegraph` or SeleneCode itself with `selene-extract`'s `Indexer`): record the
  count and the peak RSS delta. If it is fine (expected: hundreds of thousands of short
  strings), the warm-cache is a straight `HashSet` and the yielding variant is dropped.
- [ ] **Regex portability.** Confirm the three JS-specific behaviors the map's §Rust port notes
  call out, each with a failing-then-passing assertion: (a) `CHAIN_SHAPE = /^(.+)\(\)\.(\w+)$/`
  — JS's greedy `(.+)` binds to the **LAST** `().`; verify the `regex` crate's greedy semantics
  match on `A().b().c` (expect inner = `A().b`); (b) `String::replace('*', x)` in
  `apply_aliases` replaces only the FIRST `*` — Rust's `str::replace` replaces ALL, so use
  `replacen(.., 1)`; (c) the Lua/Luau receiver pattern's negative lookahead
  `(?![\w.]|\s*[({"'\[])` needs `fancy-regex` — confirm `fancy-regex` compiles it and the
  `regex` crate does not.
- [ ] **Per-ref regex compilation cost.** The receiver-inference tables build patterns *per
  reference* from an escaped receiver name (`new RegExp` in TS). Bench naive
  `Regex::new(&format!(...))` per ref vs a small `LruCache<String, Regex>` keyed by the built
  pattern; record the numbers. If naive compilation dominates (expected), Task 8 uses the cache.
- [ ] **JSONC tolerance.** Parse a real-world `tsconfig.json` with comments *and* trailing
  commas (write one) through the chosen crate (`json5` vs `jsonc-parser`) and confirm
  `compilerOptions.paths` survives — if tolerance is weaker than TS's `stripJsonc` +
  trailing-comma removal, **alias loading silently vanishes** and every aliased import
  regresses to unresolved. Pick the crate here; Task 4 uses the pick.
- [ ] Commit: `feat(resolve): spike — GraphStore seam, regex portability, cache sizing`

### Task 2: Crate skeleton — types, `ResolutionContext` seam, caches, language families

**Files:** Create: `src/lib.rs`, `src/error.rs`, `src/types.rs`, `src/context.rs`,
`src/cache.rs`, `src/families.rs`, `tests/context_fake.rs`.

**Interfaces (the contract — `maps/resolution.md` §Public interface, §types.ts):**
```rust
// types.rs — UnresolvedRef itself is REUSED from selene_core (do not redefine).
pub enum ResolvedBy { ExactMatch, Import, QualifiedName, Framework, Fuzzy,
    InstanceMethod, FilePath, FunctionRef }
impl ResolvedBy { pub fn as_str(&self) -> &'static str; }   // exact-match | import |
    // qualified-name | framework | fuzzy | instance-method | file-path | function-ref
pub struct ResolvedRef { pub original: UnresolvedRef, pub target_node_id: String,
    pub confidence: f64, pub resolved_by: ResolvedBy }
pub struct ResolutionStats { pub total: usize, pub resolved: usize, pub unresolved: usize,
    pub by_method: BTreeMap<String, usize> }   // BTreeMap: deterministic output order
pub struct ResolutionResult { pub resolved: Vec<ResolvedRef>,
    pub unresolved: Vec<UnresolvedRef>, pub stats: ResolutionStats }
pub struct ImportMapping { pub local_name: String, pub exported_name: String,
    pub source: String, pub is_default: bool, pub is_namespace: bool,
    pub resolved_path: Option<String> }
pub enum ReExport { Named { exported_name: String, original_name: String, source: String },
    Wildcard { source: String } }

// context.rs — the TS ResolutionContext interface, as a trait.
pub trait ResolutionContext {
    // graph reads (GraphStore-backed)
    fn nodes_in_file(&self, path: &str) -> Vec<Node>;
    fn nodes_by_name(&self, name: &str) -> Vec<Node>;
    fn nodes_by_lower_name(&self, lower: &str) -> Vec<Node>;
    fn nodes_by_qualified_name(&self, qn: &str) -> Vec<Node>;
    fn nodes_by_kind(&self, kind: NodeKind) -> Vec<Node>;
    fn node_by_id(&self, id: &str) -> Option<Node>;
    fn count_nodes_named(&self, name: &str) -> u64;          // the #999 ceiling check
    fn method_matches(&self, language: &str, ty: &str, method: &str) -> Vec<Node>;  // memoized
    fn supertypes(&self, node_id: &str) -> Vec<Node>;        // NODE-anchored (see Task 1)
    fn members_of(&self, node_id: &str) -> Vec<Node>;        // `contains` children
    // filesystem reads
    fn project_root(&self) -> &Path;
    fn file_exists(&self, path: &str) -> bool;
    fn read_file(&self, path: &str) -> Option<String>;       // LRU-cached
    fn file_lines(&self, path: &str) -> Option<Arc<Vec<String>>>;  // LRU-cached (#1122)
    fn all_files(&self) -> &[String];
    fn list_directories(&self, path: &str) -> Vec<String>;
    // lazily-computed project singletons (None = absent, computed once per resolver life)
    fn import_mappings(&self, path: &str) -> Arc<Vec<ImportMapping>>;   // LRU-cached
    fn re_exports(&self, path: &str) -> Arc<Vec<ReExport>>;             // LRU-cached
    fn project_aliases(&self) -> Option<&AliasMap>;
    fn go_module(&self) -> Option<&GoModule>;
    fn workspace_packages(&self) -> Option<&WorkspacePackages>;
    fn cpp_include_dirs(&self) -> &[String];
    // the warm caches
    fn known_files(&self) -> &HashSet<String>;
    fn known_names(&self) -> &HashSet<String>;
}
pub struct StoreContext<S: GraphStore> { /* store, root, LRU caches, singletons */ }
impl<S: GraphStore> ResolutionContext for StoreContext<S> { .. }

// families.rs
pub fn same_language_family(a: &str, b: &str) -> bool;
pub fn is_known_language_family(l: &str) -> bool;
pub fn crosses_known_family(a: &str, b: &str) -> bool;
```

**The sync/async seam (decide here, once):** `ResolutionContext`'s methods are **synchronous**
— the whole matcher is sync, ordered, single-threaded. `StoreContext` is built by an `async`
constructor that pre-loads the warm caches, and its per-call reads bridge to the async store
via a `tokio::runtime::Handle::block_on` **only when the resolver is itself running inside
`spawn_blocking`** (which Part C guarantees, exactly as Phase 2's orchestrator does at its DB
seam). Document this in `context.rs` module docs: an async `ResolutionContext` would make every
strategy function async, infect the whole matcher, and buy nothing — resolution is CPU-bound
over a warm cache, not I/O-bound.

- [ ] `families.rs` — `LANGUAGE_FAMILY` verbatim (map §resolveOne pipeline, Language gates):
  `java|kotlin|scala → jvm`; `swift|objc → apple`; `typescript|tsx|javascript|jsx|arkts → web`;
  `c|cpp → c`; `csharp|razor → dotnet`; **everything else is its own singleton family**.
  `same_language_family(a,b)` = `a == b || family(a) == family(b)` (both known);
  `crosses_known_family(a,b)` = both are in a KNOWN family and the families differ (a language
  with no family entry never "crosses").
- [ ] `cache.rs` — `DEFAULT_CACHE_LIMIT = 5000`, overridable by `SELENE_RESOLVER_CACHE_SIZE`
  (positive int only; garbage ⇒ default, never an error). **Content-bearing caches** (file text,
  split lines) get `max(64, limit / 5)` (map §Caches — they hold whole files, so they get a
  fifth of the entry budget). The `lru` crate; `get` refreshes recency, `put` evicts oldest.
  Caches to declare (all per-resolver-instance): `node_cache` (file→nodes), `file_cache`
  (file→content|None), `import_mapping_cache`, `re_export_cache`, `name_cache`,
  `lower_name_cache`, `qualified_name_cache`, `file_lines_cache`, `method_match_cache`
  (key `` `{language} {type}::{method}` ``). `nodes_by_kind_cache` is a **plain HashMap** (≈24
  kinds, never evicted — #1180). **Dead code, do NOT port:** `import-resolver.ts`'s
  module-level `importMappingCache` (declared, cleared, never written — the real cache is the
  resolver's LRU). The module-level `cppIncludeDirCache` and the COBOL copybook `WeakMap`
  become plain resolver/context fields.
- [ ] `tests/context_fake.rs` — a `FakeContext` implementing `ResolutionContext` over
  in-memory `Vec<Node>` + a `HashMap<String, String>` of file contents, with a builder
  (`FakeContext::new().with_node(..).with_file(..)`). **Every later task's unit tests are
  written against this** — a matcher test must never need a SurrealDB. Make it `pub` from a
  `#[cfg(test)]`-gated module or a `tests/common/` include so tasks 3–10 can reuse it.
- [ ] TDD: family-gate truth table (jvm↔jvm same, jvm↔web crosses, python↔ruby neither same
  nor crossing — both are singletons: `same_language_family("python","ruby") == false` AND
  `crosses_known_family("python","ruby") == false`, which is exactly why unfamilied languages
  survive the `imports` gate); LRU eviction + recency refresh; `SELENE_RESOLVER_CACHE_SIZE`
  parsing incl. garbage input.
- [ ] Commit: `feat(resolve): crate skeleton — types, ResolutionContext seam, caches, families`

### Task 3: `resolve_one` ladder — built-in filters, fast pre-filter, `create_edges`

**Files:** Create: `src/builtins.rs`, `src/resolver.rs`. Tests: `tests/resolver_ladder_test.rs`.
⚠ `src/resolver.rs` is the plan's #1 shared seam — this task **creates** it and lays the full
12-step ladder down with every not-yet-implemented step present as a named stub. Later tasks
fill exactly one step each and **never re-order**.

**Interfaces:**
```rust
pub struct ReferenceResolver<C: ResolutionContext> {
    ctx: C,
    frameworks: Vec<Box<dyn FrameworkResolver>>,   // Part B fills; empty here
    deferred_chain_refs: Vec<UnresolvedRef>,       // Task 9 fills
    deferred_this_member_refs: Vec<UnresolvedRef>, // Task 10 fills
}
impl<C: ResolutionContext> ReferenceResolver<C> {
    pub fn new(ctx: C) -> Self;
    pub fn resolve_one(&mut self, r: &UnresolvedRef) -> Option<ResolvedRef>;
    pub fn create_edges(&self, resolved: &[ResolvedRef]) -> Vec<Edge>;
    pub fn detected_frameworks(&self) -> Vec<String>;   // Part B
}
```

**The `resolve_one` ladder (map §`resolveOne` pipeline — THE order is the contract).** Lay all
12 steps down now, in this order, each as a `// step N:` comment + its call (stubbed steps
return `None` and carry a `// TODO(Task N)` naming the task that fills them):

1. **Built-in/external filter** (`is_built_in_or_external`) → early `None`. *(this task)*
2. **CFML component-path inheritance** — wave-2 language; **not in v0**. Leave the step as a
   comment naming Phase 8; do not stub a call.
3. **Fast pre-filter** (`has_any_possible_match` ∪ `matches_any_import` ∪ any framework
   `claims_reference`) → skip unless one hits. *(this task; the framework arm is Part B's)*
4. `function_ref` dedicated path. *(Task 10)*
5. `resolve_jvm_import`. *(Task 6)*
6. Razor `@using` — **wave 2**, comment only.
7. **Frameworks loop** (`gate_framework_language`); conf ≥ **0.9** returns immediately, else
   pushes a candidate. *(Part B)*
8. `resolve_via_import` (`gate_language`); ≥ 0.9 returns immediately, else candidate. *(Task 6)*
9. **Path-only refs** — `is_php_include_path_ref` (php + `imports` + name contains `/` or `.`),
   COBOL/Nix/Terraform are wave-2 (comment only) → **return the best candidate so far, or
   `None`. NEVER fall through to name-matching** (#660: a wrong edge is worse than none).
   *(the PHP arm is Task 6's; lay the branch here)*
10. `match_reference` (`gate_language`). *(Task 7)*
11. **Deferral** for the conformance passes when no candidate. *(Task 9)*
12. Return the **highest-confidence** candidate, first-wins on ties.

- [ ] **Language gates** (`families.rs` consumers, spelled here because they gate the ladder):
  `gate_language` — for `references`/`function_ref` refs, drop a target unless
  `same_language_family(target.language, ref.language)`; for `imports` refs, drop if
  `crosses_known_family`. `gate_framework_language` — applies **only** to `references`/`imports`
  refs, drops if `crosses_known_family` (deliberately preserving `calls` and config↔code
  bridges).
- [ ] **`builtins.rs` — copy the exact sets verbatim** from `../codegraph/src/resolution/index.ts`
  **lines 71–196** (the one sanctioned read of that file; the map says "port verbatim"). v0-relevant
  arms of `is_built_in_or_external`:
  - **JS/TS** (`typescript|tsx|javascript|jsx`): `JS_BUILT_INS`; any name starting `console.`,
    `Math.`, `JSON.`; `REACT_HOOKS`.
  - **Python**: `PYTHON_BUILT_INS`; a dotted name whose **receiver** ∈ `PYTHON_BUILT_IN_TYPES`
    or whose **member** ∈ `PYTHON_BUILT_IN_METHODS` — **unless** the receiver is capitalized and
    ∈ `known_names` (a user class shadowing a builtin type wins); a **bare** builtin-method name
    only when it is NOT ∈ `known_names`.
  - **Go**: `GO_STDLIB_PACKAGES` (the receiver before the first `.`) and `GO_BUILT_INS`.
  - **C/C++**: the `std::` prefix is filtered **unconditionally**; `C_BUILT_INS`/`CPP_BUILT_INS`
    are filtered **only when `!has_any_possible_match(name)`** — i.e. **user shadowing wins**.
  - ArkTS/Pascal arms are wave-2 (skip, with a comment).
- [ ] **`has_any_possible_match(name)`** (map §resolveOne step 3) — the cheap existence probe
  against `ctx.known_names()`. It tries, in order: the direct name; around a `.` — the receiver,
  the member, the **capitalized** receiver, the last-dot tail; around `::` — the receiver, the
  member, the last-`::` tail; around a single `:` (not `::`) and around `$` — member / receiver /
  capitalized receiver; after the last `/` — the filename. Plus `matches_any_import(ref)`: any
  import mapping whose `local_name == name` or where `name.starts_with(&format!("{local_name}."))`.
- [ ] **`create_edges`** (map §Edge creation — small, self-contained, and Part C depends on it):
  `kind = reference_kind`, **except** the three promotions: `function_ref` → **`references`**;
  `extends` → **`implements`** when the target is an interface/protocol and the source is not;
  `calls` → **`instantiates`** when the target is a class/struct. `metadata` (serde_json):
  `confidence`, `resolvedBy` (`ResolvedBy::as_str()`), `refName` = **`original.reference_name`**
  (the resurrection contract, #1240), `refKind` = the original kind **only when a promotion
  changed it**, `fnRef: true` only for `function_ref`. `provenance: Provenance::TreeSitter`;
  `line`/`column` from the ref. Target kinds come from a single batched `ctx.node_by_id` sweep —
  do not query per edge.
- [ ] TDD (all against `FakeContext`): built-in filter matrix — `console.log` (JS) filtered;
  Python `dict.get` filtered but `MyDict.get` kept when `MyDict` ∈ known_names; a bare `items`
  filtered unless a user `items` exists; Go `fmt.Println` filtered; C++ `std::sort` filtered
  while a user-defined `printf` (shadowing `C_BUILT_INS`) is **kept**. Pre-filter: a name with
  no possible match short-circuits before any strategy runs (assert via a strategy-call
  counter). `create_edges`: all three promotions, the `refKind`-only-on-promotion rule, `fnRef`
  only for function_ref, `refName` == the ORIGINAL name.
- [ ] Commit: `feat(resolve): resolve_one ladder — built-in filters, pre-filter, edge creation`

### Task 4: Import inputs — mappings, tsconfig aliases, workspace packages, `go.mod`

**Files:** Create: `src/imports/mappings.rs`, `src/imports/aliases.rs`,
`src/imports/workspace.rs`, `src/imports/go_module.rs`. Tests:
`tests/imports_inputs_test.rs`. (These four modules are **independent of each other and of
Tasks 5/6** — the one task in Part A that can be split across parallel agents if desired.)

**Interfaces:**
```rust
// mappings.rs
pub fn extract_import_mappings(file_path: &str, content: &str, language: &str)
    -> Vec<ImportMapping>;
pub fn extract_re_exports(content: &str, barrel_path: &str) -> Vec<ReExport>;
// aliases.rs
pub struct AliasPattern { pub prefix: String, pub suffix: String, pub has_wildcard: bool,
    pub replacements: Vec<String> }
pub struct AliasMap { pub base_url: PathBuf /* absolute */, pub patterns: Vec<AliasPattern> }
pub fn load_project_aliases(project_root: &Path) -> Option<AliasMap>;
pub fn apply_aliases(import_path: &str, aliases: &AliasMap, project_root: &Path) -> Vec<String>;
// workspace.rs
pub struct WorkspacePackages { pub by_name: HashMap<String, String>,
    pub entry_by_name: HashMap<String, String> }
pub fn load_workspace_packages(project_root: &Path) -> Option<WorkspacePackages>;
pub fn resolve_workspace_import(import_path: &str, ws: &WorkspacePackages) -> Option<String>;
// go_module.rs
pub struct GoModule { pub module_path: String, pub root_dir: String }
pub fn load_go_module(project_root: &Path) -> Option<GoModule>;
```

- [ ] **Import-mapping extraction** (map §Import resolution, "Import-mapping extraction" —
  regexes over RAW content, per language):
  - **JS/TS/JSX/TSX**: ES6 —
    `import\s+(?:(\w+)\s*,?\s*)?(?:\{([^}]+)\})?\s*(?:(\*)\s+as\s+(\w+))?\s*from\s*['"]([^'"]+)['"]`
    — plus `require()` destructuring. Named specifiers split on `,`, honoring `X as Y`
    (`exported_name` = X, `local_name` = Y). Default import ⇒ `is_default`; `* as ns` ⇒
    `is_namespace`.
  - **Python**: `from X import Y` (per-name, honoring `as`) + `^import X (as A)?`
    (`is_namespace`, `local_name` = the alias or the **full dotted path**).
  - **Go**: single + block imports; `is_namespace: true`; `local_name` = the alias, else the
    **last path segment**.
  - **Java/Kotlin**: `^\s*import\s+(static\s+)?([\w.]+(?:\.\*)?)\s*;` **after a comment strip**;
    **wildcards are skipped** (a `.*` import yields no mapping).
  - **PHP**: `use FQN (as Alias);`
  - **C/C++**: `^\s*#\s*include\s+[<"]([^>"]+)[>"]` — `is_namespace: true`, `local_name` = the
    basename with the header extension removed.
  - Ruby has **no** import mappings (`require` is path-shaped; handled — or deliberately not —
    in Task 5). Note the absence in a comment rather than inventing one.
- [ ] **Re-exports** (`extract_re_exports`): `export\s*\*(?:\s+as\s+\w+)?\s*from\s*['"]…['"]` and
  `export\s*\{([^}]+)\}\s*from\s*['"]…['"]`, both **after** a string-aware `strip_js_comments`
  scanner (a regex that eats `//` inside a string literal corrupts the barrel). **Keyed to the
  BARREL's own extension** (`/\.(?:d\.ts|[cm]?tsx?|[cm]?jsx?|ets)$/i` ⇒ treat as typescript),
  **not** the consumer's language (#629) — that is why `barrel_path`, not `language`, is the
  parameter.
- [ ] **`aliases.rs`**: read `tsconfig.json`, then `jsconfig.json` (first hit wins). JSONC
  tolerance = comment strip (string-aware) + trailing-comma removal, via the crate Task 1 picked
  — **if tolerance is weaker than TS's, aliases silently vanish and every aliased import
  regresses**; test with a commented, trailing-comma'd tsconfig. `base_url` defaults to `"."`.
  Patterns sorted **longer-prefix-first, literal-before-wildcard**. `apply_aliases` fills the
  single `*` (**`replacen(.., 1)` — Rust's `str::replace` would replace every `*`**, Task 1
  finding (b)), resolves against the absolute `base_url`, and **drops any candidate that escapes
  the project root**. `tsconfig` `extends` chains are a **documented non-feature** — preserve the
  limitation, note it in the module docs.
- [ ] **`workspace.rs`**: `package.json` `workspaces` (both the array and the `{packages: []}`
  forms) + `pnpm-workspace.yaml` (a minimal line parser — do not add a YAML dep). One-level `*`
  glob expansion, skipping dotdirs and `node_modules`; **first declaration wins**. ohpm
  (`oh-package.json5`) is ArkTS — **wave 2**; leave `entry_by_name` in the struct (Task 5/6 read
  it) but populate it only from the npm `main` field, and note the ohpm BFS as deferred.
  `resolve_workspace_import`: **longest matching package name**; a bare name resolves to the
  declared entry file if there is one, else to the package dir + subpath.
- [ ] **`go_module.rs`**: parse the `module` directive out of `go.mod` → `{module_path, root_dir}`.
  **Nested `go.mod`s (Go workspaces) are a documented non-feature** — preserve the limitation.
- [ ] TDD (tempfile trees): JS default + named + `X as Y` + namespace + `require` destructuring;
  Python `from`/`import`/`as`; Go aliased + block imports; JVM wildcard skipped and `import
  static` flagged; a 3-hop re-export chain and a **renamed** re-export; a barrel whose extension
  (not the consumer's language) drives parsing (#629); tsconfig aliases present / absent /
  JSONC-with-comments-and-trailing-commas; an alias replacement containing **two** `*` (only the
  first is filled); an alias escaping the root (dropped); pnpm workspace + a `*` glob;
  `go.mod` present/absent.
- [ ] Commit: `feat(resolve): import inputs — mappings, tsconfig aliases, workspaces, go.mod`

### Task 5: `resolve_import_path` — module specifier → file, per v0 ecosystem

**Files:** Create: `src/imports/mod.rs` (this task lays it down; Task 6 appends to it),
`src/imports/cpp_includes.rs`. Tests: `tests/imports_path_test.rs`.

**Interfaces:**
```rust
pub fn resolve_import_path(import_path: &str, from_file: &str, language: &str,
    ctx: &impl ResolutionContext) -> Option<String>;   // → a repo-relative file path
pub fn is_external_import(import_path: &str, language: &str,
    ctx: &impl ResolutionContext) -> bool;
// cpp_includes.rs
pub fn load_cpp_include_dirs(project_root: &Path) -> Vec<String>;
```

- [ ] **`EXTENSION_RESOLUTION`** — the string-**appended** suffix table (map §Import resolution;
  note the `/index.*` entries are suffixes too, not a separate mechanism). v0 rows, verbatim:
  - `typescript`: `['.ts','.tsx','.d.ts','.js','.jsx','/index.ts','/index.tsx','/index.js']`
  - `tsx`, `jsx`: same shape as their base language (copy the TS/JS rows).
  - `javascript`: `['.js','.jsx','.mjs','.cjs','/index.js','/index.jsx']`
  - `python`: `['.py','/__init__.py']` · `go`: `['.go']` · `rust`: `['.rs','/mod.rs']`
  - `java`: `['.java']` · `kotlin`: `['.kt']` · `c`: `['.h','.c']`
  - `cpp`: `['.h','.hpp','.hxx','.cpp','.cc','.cxx']`
  - `csharp`: `['.cs']` · `php`: `['.php']` · `ruby`: `['.rb']`
  (Wave-2 rows — arkts/svelte/vue/astro/objc/nix — are in the map; port them **only** if free.)
- [ ] **`is_external_import`** (an external specifier resolves to `None`, and the ref is dropped,
  not guessed):
  - **JS/TS**: the node builtins `[fs, path, os, crypto, http, https, url, util, events,
    stream, child_process, buffer]`; **escapes**: a specifier matching a workspace package name,
    or matching a tsconfig alias prefix, is **NOT** external; otherwise a **bare** specifier is
    external unless it starts with `@/`, `~/`, or `src/`.
  - **Python**: the stdlib **first segment** `[os, sys, json, re, math, datetime, collections,
    typing, pathlib, logging]`.
  - **Go**: local **iff** the specifier `== module_path`, or starts with `module_path + "/"`, or
    contains `/internal/`; otherwise external.
  - **C/C++**: the stdlib header set (including the `.h`-stripped form, so `<stdio.h>` and
    `<cstdio>` both filter).
- [ ] **`resolve_import_path` order** (map §Import resolution): COBOL copybooks first (**wave
  2** — skip, comment) → `is_external_import` ⇒ `None` → **relative** (`.`-prefixed; Python's
  dotted-relative form is translated first: **N leading dots = N−1 `../`**, and the remaining
  dots become `/`) → **aliased**: `apply_aliases` (tsconfig) → workspace rewrite → the
  hard-coded fallback map `{'@/'→'src/', '~/'→'src/', '@src/'→'src/', 'src/'→'src/',
  '@app/'→'app/', 'app/'→'app/'}` → the direct path. Each candidate is then extension-resolved
  against `ctx.known_files()` (the first hit in `EXTENSION_RESOLUTION` order wins — **order is
  the contract**, e.g. TS prefers `.ts` over `/index.ts`).
- [ ] **C/C++ last resort** — the `-I` dir scan (`cpp_includes.rs`): look for
  `compile_commands.json` at `[., build, cmake-build-debug, cmake-build-release, out]`; parse
  `-I<d>`, `-I d`, and `-isystem d` with a **mini shlex** (quoted paths with spaces are real).
  Heuristic fallback when no compile DB: the top-level dirs `[include, src, lib, api, inc]` plus
  any top-level dir containing a file matching `/\.(h|hpp|hxx|hh)$/i`. Result cached on the
  context (the TS module-level `cppIncludeDirCache` becomes resolver state — map §Rust port notes).
- [ ] TDD (tempfile trees + `FakeContext`): a relative import (`./foo` → `foo.ts`); an index
  import (`./foo` → `foo/index.ts` when no `foo.ts`); extension-order precedence (`.ts` beats
  `/index.ts`); a Python dotted-relative (`from ...pkg.mod import X` from `a/b/c.py` →
  `pkg/mod.py`); a Python absolute dotted module; Go local vs external (`internal/` local,
  `github.com/x/y` external, own-module-path local); a JS bare specifier external, but the same
  specifier **non**-external once it names a workspace package; an aliased import beating a
  same-named file; C `#include "sibling.h"` (same dir), `<subdir/x.hpp>`, a `-I` dir from
  `compile_commands.json`, and a system header (`<stdio.h>` → `None`).
- [ ] Commit: `feat(resolve): module-specifier → file resolution per v0 ecosystem`

### Task 6: `resolve_via_import` — per-ecosystem branches, re-export chasing, JVM FQN

**Files:** Modify: `src/imports/mod.rs` (append), `src/resolver.rs` (**fill ladder steps 5, 8,
9** — the shared seam; A5 must be merged first). Tests: `tests/imports_via_test.rs`.

**Interfaces:**
```rust
pub fn resolve_via_import(r: &UnresolvedRef, ctx: &impl ResolutionContext) -> Option<ResolvedRef>;
pub fn resolve_jvm_import(r: &UnresolvedRef, ctx: &impl ResolutionContext) -> Option<ResolvedRef>;
pub fn is_php_include_path_ref(r: &UnresolvedRef) -> bool;
pub const REEXPORT_MAX_DEPTH: usize = 8;
```

**Import-result confidences (contract — map §Confidence/scoring):** generic **0.9**; C/C++
same-dir sibling include **0.92**; Python module-member **0.85**; JVM FQN **0.95**; Lua require
**0.9** (wave 2); CFML relative **0.95** (wave 2). `resolved_by: ResolvedBy::Import` for all of
them (JVM included).

- [ ] **`resolve_via_import` branch order** (map §resolveViaImport branch order — **the order is
  the contract**; the first branch that owns the ref wins, and the "no fallthrough" branches
  return without consulting the later ones):
  1. **C/C++ `imports`**: same-dir sibling include → **0.92**; otherwise the resolved path → its
     file node → **0.9**.
  2. **COBOL** — wave 2 (comment; it is a *no-fallthrough* branch).
  3. **PHP include path**: resolve relative to the **includer**, retry with the extension; file
     node → **0.9**. **No fallthrough** (#660).
  4. **Nix / Terraform** — wave 2 (comment; also no-fallthrough).
  5. **Go cross-package** (`pkg.Member`): the receiver resolves through an alias → the in-module
     import → `pkg_dir` = the import source minus the module path; a candidate qualifies **only
     if** it is an **exported** Go node whose **immediate parent dir equals `pkg_dir`** → **0.9**.
  6. **Java/Kotlin**: a `Foo.bar` or a bare `Foo` matching an import → the FQN becomes a path
     suffix (`com/example/Foo.java|.kt`); the member is looked up by name **filtered by that file
     suffix**. The `import static` form uses the owner-path variant. → **0.9**.
  7. **Python module-member** (`certs.where`): via the binding — a namespace import maps to its
     source, a named import to `source(.)local_name`; the **member is the first segment**;
     accepted kinds are top-level `function | class | variable | constant` — **never `method`**
     → **0.85**. Plus the absolute dotted module form (`import a.b.c` → the file `a/b/c.py` or
     `a/b/c/__init__.py`, suffix-matched) → **0.9**.
  8. **Rust `A::B::C`**: the module prefix becomes a file — anchors: `crate` (walk **up ≤ 64
     dirs** to a `lib.rs`|`main.rs`), `self` (the own dir's `mod.rs`|`lib.rs`|`main.rs`, else
     `foo/`), `super` × N. A **bare** path tries **self-relative first, then crate-relative**.
     Each segment resolves as `<seg>.rs`, else `<seg>/mod.rs`. The leaf must be a
     `fn|struct|enum|trait|type_alias|constant|method|class|interface` node **in that file** →
     **0.9**. (Cargo **workspace member globs** are NOT here — they live in Part B's
     `cargo_workspace` framework resolver. Core Rust import support is the
     `crate::`/`self::`/`super::` walk + the `.rs`/`mod.rs` conventions **only** — map §Scope note.)
  9. **Whole-module import** → the file node (a TS/JS namespace or default import; a Python
     submodule) → **0.9**.
  10. **The generic loop**: for each mapping where `local_name == name` or
      `name.starts_with(&format!("{local_name}."))` — resolve the source path, then
      `find_exported_symbol`.
- [ ] **`find_exported_symbol`** (the re-export chase): depth cap **`REEXPORT_MAX_DEPTH = 8`** +
  a `visited` set (a barrel cycle must terminate, not recurse). Order: a **direct hit** first (a
  `default` import prefers a `component`-kind node, then an exported function/class — #629); then
  **named** re-exports (**following the rename**: `export { a as b } from './x'` means a ref to
  `b` resolves to `x`'s `a`); **wildcard** re-exports last.
- [ ] **Static-member descent** (#825), applied after a direct hit resolves to a container:
  containers are `{class, struct, interface, enum, trait, protocol}`; the member is the first
  segment after the receiver; look up `${container.qualified_name}::${member}` **filtered to the
  container's file**; a `calls` ref prefers a callable target → **0.9**.
- [ ] **`resolve_jvm_import`** (ladder step 5, ahead of the frameworks — java/kotlin only): an
  `imports` ref whose FQN maps to a `pkg::sym` qualified-name lookup → **0.95**. Multiple
  candidates → `pick_closest_jvm_candidate`: **max shared leading dir segments**; on a tie, prefer
  the node whose `decorators` contain `'expect'` (the Kotlin multiplatform `expect`/`actual` split
  — #314).
- [ ] **Ladder wiring** (`src/resolver.rs`): step 5 = `resolve_jvm_import`; step 8 =
  `resolve_via_import` (`gate_language`; **≥ 0.9 returns immediately**, else it becomes a
  candidate); step 9 = the path-only guard — `is_php_include_path_ref` (php + kind `imports` +
  the name contains `/` or `.`) returns the best candidate so far or `None` and **never falls
  through to name-matching**.
- [ ] TDD (port the named cases from `__tests__/resolution.test.ts`): relative + parent imports;
  JVM FQN resolution incl. **collision disambiguation** (#314) and the wildcard/unqualified/
  non-JVM/non-import **null** cases; Go cross-package + aliased imports (#388) and "stdlib stays
  external"; Python module-attribute (#578) and the never-a-method rule; the static-member
  descent (#825); re-export chains (3-hop, rename, bare-dir import, workspace-subpath barrel);
  C/C++ include resolution (same dir → 0.92, `.hpp`, subdir, `-I` dirs, multi-extension, a system
  header → null); PHP includes (#660: the shape predicate, a file→file edge, and **no
  mis-connect** — assert that a PHP include of `utils.php` produces no edge to a *symbol* named
  `utils`); Rust `crate::`/`self::`/`super::` paths and a `mod.rs` leaf.
- [ ] Commit: `feat(resolve): resolveViaImport — ecosystem branches, re-export chase, JVM FQN`

### Task 7: Name-matcher — dispatcher, file-path / qualified-name / exact-name / fuzzy + scoring

**Files:** Create: `src/matcher/mod.rs` (the dispatcher — **the second shared seam**; lay the
full strategy ladder down with stubs for the steps Tasks 8/9/10 fill), `src/matcher/scoring.rs`,
`src/matcher/names.rs`. Modify: `src/resolver.rs` (fill ladder **step 10**). Tests:
`tests/matcher_names_test.rs`.

**Interfaces:**
```rust
pub fn match_reference(r: &UnresolvedRef, ctx: &impl ResolutionContext) -> Option<ResolvedRef>;
pub fn match_by_file_path / match_by_qualified_name / match_by_exact_name / match_fuzzy(..)
    -> Option<ResolvedRef>;
pub fn prefer_call_site_file(nodes: &[Node], call_site_file: &str) -> Vec<Node>;
pub fn find_best_match(candidates: &[Node], r: &UnresolvedRef) -> Option<(Node, i32)>;
pub const AMBIGUOUS_NAME_CEILING: u64 = 500;   // env: SELENE_AMBIGUOUS_NAME_CEILING
```

**`match_reference` strategy order (map §matchReference strategy order — THE contract).** Lay
all of it down now; the bracketed task fills the stub:
- `function_ref` refs **short-circuit** to `match_function_ref` **only** — they never reach
  frameworks, the other strategies, or fuzzy. *(Task 10)*
- ArkTS leading-dot attrs / Erlang `implements` — **wave 2**; comment only.
- **(0)** `match_by_file_path` · **(1)** `match_by_qualified_name` · **(1b)** c/cpp
  `match_cpp_call_chain` *(Task 9)* · **(1c)** php/rust `match_scoped_call_chain` *(Task 9)* ·
  **(1d)** java/kotlin/csharp/go `match_dotted_call_chain` *(Task 9; swift/scala/dart/objc/pascal
  are wave 2)* · **(2)** `match_method_call` *(Task 8)* · **(3)** `match_by_exact_name` ·
  **(4)** `match_fuzzy`.

**Scoring constants — copied VERBATIM from map §Confidence/scoring constants. Do not drift.**

`match_by_file_path` — *only* runs when the name contains `/` **or** matches the extension regex
`/\.[A-Za-z][A-Za-z0-9]{0,3}$/`:
| case | confidence |
|---|---|
| exact `qualified_name` or `file_path` match | **0.95** |
| suffix match, disambiguated by `pick_closest_file_node` | **0.85** |
| a single file node exists | **0.70** |

`pick_closest_file_node`: build a **same-dir pool first**; score = `path_proximity` **+ 5 if
`same_language_family`**.

`match_by_qualified_name`:
| case | confidence |
|---|---|
| single exact | **0.95** |
| ambiguous-exact, same file | **0.95** |
| suffix partial (split the ref on `[:.]`; candidates are nodes named the **last part** whose `qualified_name.ends_with(reference_name)`; then `prefer_call_site_file`) | **0.85** |
For a `calls` ref, **drop `constant` nodes whose language is `yaml` or `properties`** (#1180).

`match_by_exact_name` — **excludes `kind == import` nodes** (#915):
| case | confidence |
|---|---|
| single candidate | **0.9** (cross-language: **0.5**) |
| `count_nodes_named(name) > AMBIGUOUS_NAME_CEILING` (**500**) | **decline — return `None`** (#999) |
| else `find_best_match` → proximity **≥ 30** | **0.7** |
| else | **0.4** |
> The cross-language single-candidate branch is mostly unreachable for `references` (the gate
> already filtered) but **live for `calls`** — keep it (map §Rust port notes).

`find_best_match` weights (an `i32` score; higher wins, **first-wins on ties**):
| signal | delta |
|---|---|
| same file | **+100** |
| directory proximity | **+15 per shared leading segment, capped at 80** |
| same language | **+50**; different language | **−80** |
| *(and: if any same-language candidate exists, cross-language candidates are skipped entirely)* | |
| `calls` ref → target kind `function`/`method` | **+25** |
| `instantiates` ref → target kind `class`/`struct`/`interface` | **+25** |
| `decorates` ref → target kind `function`/`method` | **+25**; → `class`/`interface` | **+15** |
| target `is_exported` | **+10** |
| same file, line distance | **+ `max(0, 20 − distance/10)`** |

`match_fuzzy`: a **lowercase** lookup (`nodes_by_lower_name`), kinds restricted to
`{function, method, class}`, language-gated, same-language preferred; **unique** → **0.5**
(cross-language **0.3**). Non-unique ⇒ `None`.

- [ ] Implement the dispatcher + the three name strategies + `find_best_match` /
  `prefer_call_site_file` / `pick_closest_file_node` exactly as tabled above. `prefer_call_site_file`
  returns the same-file subset when it is non-empty, else the input unchanged (it is a *filter*,
  not a sort — a same-file candidate always beats a cross-file one).
- [ ] `AMBIGUOUS_NAME_CEILING` uses `ctx.count_nodes_named(name)` (the store's counting
  primitive), **not** `nodes_by_name(..).len()` — the whole point of the ceiling is to decline
  **without** materializing 10k nodes. Env override `SELENE_AMBIGUOUS_NAME_CEILING`.
- [ ] Wire `src/resolver.rs` ladder **step 10**: `match_reference` under `gate_language`.
- [ ] TDD — port the named blocks of `__tests__/resolution.test.ts`: name-matcher basics
  (exact match; **cross-module confidence lowering**; same-module preference; qualified names);
  the **ubiquitous-name ceiling** (#999) triple — declines **above** the ceiling, a **same-file**
  match still resolves above it, and a count **just below** it is unchanged; **same-file
  preference** (#1079) via `prefer_call_site_file` and via qualified-name; **kind bias** for
  `instantiates` and `decorates`; #915 (an `import`-kind node is never an exact-name target);
  #1180 (a `calls` ref does not resolve to a yaml/properties `constant`); fuzzy uniqueness
  (two lowercase matches ⇒ no edge).
- [ ] Commit: `feat(resolve): name matcher — file-path/qualified/exact/fuzzy + scoring weights`

### Task 8: Method-call matching — receiver-type inference + validated `resolve_method_on_type`

**Files:** Create: `src/matcher/receiver.rs`, `src/matcher/method.rs`. Modify:
`src/matcher/mod.rs` (fill strategy **step 2** — sequential after Task 7). Tests:
`tests/matcher_method_test.rs`, `tests/matcher_receiver_test.rs`.

**Interfaces:**
```rust
pub fn match_method_call(r: &UnresolvedRef, ctx: &impl ResolutionContext) -> Option<ResolvedRef>;
pub fn resolve_method_on_type(type_name: &str, method: &str, r: &UnresolvedRef,
    ctx: &impl ResolutionContext, confidence: f64, resolved_by: ResolvedBy,
    preferred_fqn: Option<&str>, depth: u8) -> Option<ResolvedRef>;
pub fn infer_local_receiver_type(recv: &str, r: &UnresolvedRef, ctx: &impl ResolutionContext)
    -> Option<String>;
pub fn infer_cpp_receiver_type(..) -> Option<String>;
pub fn infer_java_field_receiver_type(..) -> Option<String>;
pub fn normalize_inferred_type_name(raw: &str) -> Option<String>;
```

**`resolve_method_on_type` is THE safety mechanism** (design doc §Edge cases: "Validation, not
guessing"). Every chain and every inferred receiver funnels through it, so a wrong type
inference yields **no edge, never a wrong one**:
- matches = **method**-kind nodes, **same language**, whose `qualified_name` **equals**
  `type::method` **or ends with** `::type::method` (memoized via `ctx.method_matches`).
- **empty ⇒ the conformance walk**: `ctx.supertypes(..)` (the union of `implements`/`extends`
  targets of same-named supertype-bearing kinds `{class, struct, interface, trait, protocol,
  enum}`), recursing while **`depth < 4`**.
- **multi-match tie-break, in this order**: `preferred_fqn` first — the file suffix
  `fqn.replace('.', "/") + (".kt" | ".java")` wins (#314); **then** `prefer_call_site_file`
  (#1079). *(`preferred_fqn` **before** `prefer_call_site_file` — map §Rust port notes lists
  this ordering as observable behavior.)*

**`match_method_call` receiver shapes:** dotted `/^([\w.]+)\.(\w+:?(?:\w+:)*)$/` (the trailing
`:` group is ObjC selectors — harmless in v0); `::` → `/^(\w+)::(\w+)$/`; PHP `this->prop.method`
(an **exclusive typed path** — if it matches, the other strategies are not tried); lua `receiver:method`
and R `receiver$method` are wave 2.

**`match_method_call` strategy order + confidences (verbatim):**
| # | strategy | confidence · `resolved_by` |
|---|---|---|
| 0 | **PHP** property typed-inference (exclusive) | **0.9** |
| 1 | **local receiver inference** (C++ has a dedicated inferrer; everything else shares one) → `resolve_method_on_type` — java/kotlin **pass the imported FQN** as `preferred_fqn` | **0.9** · `instance-method` |
| 2 | **Java/Kotlin field-signature** inference → `resolve_method_on_type` | **0.9** · `instance-method` |
| 3 | class-name in-file method (`prefer_call_site_file`, **same language**) | **0.85** · `qualified-name` |
| 4 | **capitalized receiver** (a type name used statically) | **0.8** · `instance-method` |
| 5 | method-name fallback — ceiling **500**; a **single same-language** candidate → **0.7**; else a camelCase **word-overlap** score (**+1** for same language), requiring `best_score >= 2`, `prefer_call_site_file` on ties → **0.65** | **0.7 / 0.65** · `instance-method` |

**Receiver-type inference (map §Receiver-type inference — port the regex tables VERBATIM from
`../codegraph/src/resolution/name-matcher.ts` lines 1092–1237; that line range is this task's one
sanctioned read):**
- **Scan direction and bounds**: scan **backward** from the call line to
  `enclosing_scope_start_line` (the *tightest* function/method containing the line). **Lines
  longer than 10 000 chars are skipped** (#1122 — a minified line otherwise pins a regex).
- **v0 per-language patterns** (the table is the contract; these are the shapes, copy the exact
  regexes): TS `= new T` and `: PascalCase`; Java/C#/Dart `Type recv[=;,)]`; **Rust** `let (mut)
  r … = &(mut) T` and `r : &(mut) T`; **Go** `:= &T{`, `var r *T`, and a PascalCase `r *T`
  param; **Ruby** `= T.new`; **Kotlin** (per the table); **PHP** — property-only patterns
  (`(private|protected|public|readonly|static|final)(\(set\))? ?Type $prop`, `$this->prop = new
  T`) plus a **second-chance** `$this->prop = $var` → then `$var`'s typed declaration **bounded
  by the enclosing `function` line**. Lua's anti-self-match lookahead
  `(?![\w.]|\s*[({"'\[])` (#1124) is **wave 2** but is the reason `fancy-regex` is a dependency —
  keep the dep and the note.
- **`this`/`self`-prefixed receivers** (PHP `this->`, CFML `variables.`/`this.`) **strip the
  prefix and scan the WHOLE file** (a forward sweep after the backward miss) — a field is
  declared above *or below* the method that uses it.
- **`infer_cpp_receiver_type`**: a backward line scan; the declarator regex is
  `` `([A-Za-z_][\w:]*(?:\s*<[^;=(){}]+>)?(?:\s*[*&]+)?)\s*\b{recv}\b\s*(?=[;=,)\[{(]|$)` `` (built
  per-ref from `regex::escape(recv)` — use the Task 1 regex cache). Normalization strips
  cv-qualifiers / `<>` / `[*&]`, takes the **last `::` segment**, and rejects `CPP_NON_TYPE_TOKENS`.
  **`auto` ⇒ initializer inference**: `new T`; a call/construction via `resolve_cpp_call_result_type`
  (`make_unique`/`make_shared<T>` → `T`; a **single-level** `recv.method` → the receiver's type,
  then that method's `return_type`); direct construction when a class of that name exists;
  **recursion cap `depth > 3`**. Fallback: sibling headers `.h`/`.hpp`/`.hxx`.
- **`infer_java_field_receiver_type`**: find the enclosing class **by line range** (the tightest
  by latest start); find a field node with the receiver's name inside that range; take the type
  from the node's `signature` (`"Type name"`): strip generics, `[]`, varargs, take the last
  `[.\s]` part, and **require an uppercase first char**.
- **`normalize_inferred_type_name`**: strip `<…>` and `[&*]`, take the last `[.:]` segment,
  reject `NON_TYPE_RECEIVER_TOKENS`. A rejected token ⇒ `None` ⇒ **no edge** (never a guess).
- [ ] TDD — port the **local receiver inference matrix** (#1108/#1125) across the v0 languages,
  plus: the **PHP property-receiver** suite (`$this->prop->method()` — typed property, promoted
  ctor param, and assignment inference — `__tests__/php-property-receiver-resolution.test.ts`);
  the **watchdog memo** cases (#1122: `method_matches` memoization, `file_lines`, and the
  **>10 000-char line is skipped** assertion); Java import disambiguation (#314:
  `preferred_fqn` beats same-file); same-file preference (#1079); and — **in every language
  block** — the mandatory safety test: **"creates NO edge when the type lacks the method"**.
- [ ] Commit: `feat(resolve): method-call matching — receiver inference + validated resolveMethodOnType`

### Task 9: Chained-call resolution via `return_type` + the conformance deferral

**Files:** Create: `src/matcher/chains.rs`, `src/passes.rs`. Modify: `src/matcher/mod.rs` (fill
strategy steps **1b/1c/1d** — sequential after Task 8), `src/resolver.rs` (fill ladder **step
11**, the deferral). Tests: `tests/matcher_chains_test.rs`.

**The mechanism** (design doc `chained-call-resolution.md` §The 3-part mechanism): Phase 2
already did parts 1 and 2 — it captured the factory's declared `return_type` on the node
(`Node.return_type`, "smart-pointer pointee unwrapped, `-> Self` ⇒ the marker string `self`")
and re-encoded a chained receiver as the marker string **`inner().method`** (the `().` marker
never appears in an ordinary ref). This task is **part 3: resolve AND VALIDATE** — infer the
receiver's type from what the inner call returns, then resolve the outer method **on that type**
through `resolve_method_on_type`, which validates that the method actually exists there.

**v0 coverage** (the design doc's 13 shipped languages ∩ the v0 wave): **C, C++** (`match_cpp_call_chain`,
`field_expression`), **PHP, Rust** (`match_scoped_call_chain`, `::`), **Java, Kotlin, C#, Go**
(`match_dotted_call_chain`, `.`). Swift/Scala/Dart/ObjC/Pascal are wave 2 — leave their language
entries in the gate lists (harmless: no such nodes exist yet) but do not fixture them.
**TypeScript is deliberately NOT covered** — it was fully implemented in TS and **consciously not
shipped**: gradual typing means factory return types are inferred, not declared, so the re-encoded
chain cannot resolve and it **drops the bare-name edge the existing resolver already found**
(real-repo A/B: +0 added on typeorm *and* nest, −164 on nest — a recall regression). Do not
"finish" TypeScript here; if anyone re-opens it, the only viable path is reading *inferred*
return types (resolving `return new X()` in the factory body), a much larger change.

**Constants (verbatim):**
```rust
pub const CHAIN_SHAPE: &str = r"^(.+)\(\)\.(\w+)$";  // greedy (.+) binds the LAST "()."  ← Task 1(a)
pub const PHP_PROP_SHAPE: &str = r"^this->\w+\.\w+$";
// deferral gate (map §resolveOne step 11) — v0 members of CHAIN_LANGUAGES:
pub const CHAIN_LANGUAGES: &[&str] = &["java","kotlin","csharp","rust","go",
    /* wave 2: */ "swift","scala","dart","objc","pascal"];
pub const CONSTRUCTS_VIA_BARE_CALL: &[&str] = &["kotlin", /* wave 2: */ "swift","scala","dart","pascal"];
```
All three chain resolvers return **0.85** and funnel through `resolve_method_on_type` —
**an absent method ⇒ NO edge, never a wrong one.**

- [ ] **`match_cpp_call_chain`** (c/cpp) → **0.85**.
- [ ] **`match_scoped_call_chain`** (php/rust): the **inner must contain `::`**; a `self` return
  marker resolves to the **factory's own class** → **0.85**.
- [ ] **`match_dotted_call_chain`** (java/kotlin/csharp/go) → **0.85**, plus:
  - **Go bare `New().M`**: try the return-type path (**0.85**) first; on a miss, fall back to a
    bare-name match (`match_by_exact_name` ?? `match_fuzzy`) over a **synthetic** ref — but the
    `ResolvedRef` **MUST be returned with the ORIGINAL ref as `.original`**. This is the
    runaway contract (#760): a mutated `original.reference_name` no-ops the keyed delete, the
    offset-0 batch re-reads forever, and the run produced 5M edges / 1.4 GB. Write the test that
    asserts `resolved.original.reference_name == the stored row's name`.
  - **bare capitalized constructor receivers** are honored **only** for
    `CONSTRUCTS_VIA_BARE_CALL` languages.
  - ObjC `[X alloc]`-style (**0.8**) and Pascal `/^[TI]/` constructor (**0.8**) fallbacks are
    wave 2 — comment only.
- [ ] **Ladder step 11 — the deferral** (`src/resolver.rs`): when `resolve_one` produced **no**
  candidate, defer the ref for the conformance pass iff kind == `calls` **and** either
  (language ∈ `CHAIN_LANGUAGES` **and** the name matches `CHAIN_SHAPE`) **or** (language == php
  **and** the name matches `PHP_PROP_SHAPE`). Push onto `deferred_chain_refs`.
- [ ] **`src/passes.rs` — `resolve_chained_calls_via_conformance()`** (map §Pass ordering step 3,
  design doc §Conformance pass #754): drains `deferred_chain_refs` **after** the main pass has
  persisted edges, so `implements`/`extends` edges now exist and `ctx.supertypes(..)` can see an
  **inherited / default-interface / trait / embedded-struct** method. Re-runs the chain resolvers
  (whose `resolve_method_on_type` now finds the method on a supertype), persists the new edges,
  and returns the count. **In-memory lifetime coupling is deliberate**: the batched pass deletes
  (or fails) the rows before those edges exist, so the queue can only be drained **by the same
  resolver instance** — preserve it (or re-read `status = 'failed'` chain-shaped rows instead;
  do NOT silently drop the pass).
- [ ] TDD — port the **per-language chained-call blocks** of `__tests__/resolution.test.ts` for
  the v0 languages: C++ (#645), PHP (#608), Java, Kotlin, C#, Rust, Go (**including the
  variable-inner fallback, asserted to not explode the graph**). **Every language block MUST
  include the safety test: "creates NO edge when the type lacks the method"** — that test is what
  makes this mechanism safe to ship. Plus the conformance pass (#750): superclass,
  interface-default, trait, and embedded-struct cases, each with its safety counterpart.
- [ ] Commit: `feat(resolve): chained-call resolution via return_type + conformance pass`

### Task 10: Function-ref resolution — unique-or-drop, class-scoped `this.X`, overload refusal

**Files:** Create: `src/matcher/fnref.rs`. Modify: `src/matcher/mod.rs` (the `function_ref`
short-circuit), `src/resolver.rs` (fill ladder **step 4**), `src/passes.rs` (append the
this-member drain — sequential after Task 9). Tests: `tests/matcher_fnref_test.rs`.

Phase 2 shipped the **capture** half (`FN_REF_SPECS`, the same-file/imported-binding gate, the
`function_ref` reference kind). This task is the **resolution** half. The contract is
`design/function-ref-capture.md` §Precision rules **3, 5, 9, 10** and map §Confidence/scoring
(`matchFunctionRef`, `resolveThisMemberFnRef`).

**Interfaces:**
```rust
pub fn match_function_ref(r: &UnresolvedRef, ctx: &impl ResolutionContext) -> Option<ResolvedRef>;
pub fn resolve_this_member_fn_ref(r: &UnresolvedRef, ctx: &impl ResolutionContext)
    -> Option<ResolvedRef>;   // miss ⇒ push onto deferred_this_member_refs
pub const BARE_FN_ONLY: &[&str] = &["typescript","tsx","javascript","jsx","cpp","python","php",
    /* wave 2: */ "arkts"];
```

**Ladder step 4 (`src/resolver.rs`) — the dedicated `function_ref` path**, in this order:
a `this.`-prefixed name → `resolve_this_member_fn_ref` **only**; otherwise `resolve_via_import`
(**accepted only if the target's kind is `function` or `method`**) → then `match_function_ref`.
Both are language-gated. A `function_ref` **never reaches the frameworks loop or fuzzy** — no
fuzzy fallback, ever.

**`match_function_ref` rules (verbatim):**
- **`BARE_FN_ONLY` languages restrict a bare name to the `function` kind** (rule 3): in TS/JS/
  Python/PHP/C++ a bare identifier can never be a *method* value (methods need a receiver —
  `this.m` / `self.m`), and allowing method targets soaked up locals passed as arguments. Python's
  `self.m` capture shape keeps method targets through its own path; C#/Java/Kotlin keep method
  targets (method groups and method references are real method values).
- **`::`-qualified member pointers** (`&Cls::method`, `Cls::m`, `this::m`): a same-**family**
  function/method whose `qualified_name` **equals** the name or **ends with** `::name`; the
  same-file pool first; **cross-file only when UNIQUE** → **0.9**.
- **Same-file** match: the **earliest `start_line`** wins → **0.95** (unique) / **0.9** (overloads).
- **Cross-file**, unique → **0.8**. **Ambiguity yields NO edge** (rule 9 — never fuzzy, never a guess).
- **Self-registration excluded**: a candidate whose `id == r.from_node_id` is never a target (no
  self-loops).
- **Overload-family refusal** (rule 5): several same-named **methods** in one file plus a bare
  identifier ⇒ **refuse** — that is almost always a same-named parameter, not a method value. (The
  TS rule is written for Swift's implicit-self; the *refusal* half is language-agnostic and
  applies wherever a bare id could hit an overload family. Swift's implicit-self class-prefix
  matching itself is wave 2.)
- **Rule 10, the runaway invariant**: `match_function_ref` **always** returns `original: r` — the
  stored row — so the keyed delete drains the batch. Same contract as Task 9's Go fallback.

**`resolve_this_member_fn_ref` (TS/JS/Python `this.X` / `self.X`) → 0.95 · `function-ref`:**
- The **class scope** = the from-node's own `qualified_name` when its kind ∈ (supertype-bearing
  `{class, struct, interface, trait, protocol, enum}` ∪ `module`), else its `qualified_name` with
  the **last `::` segment stripped**.
- Candidates = `function`/`method` nodes named `${class_prefix}::${member}`, **same file**,
  **earliest `start_line`**. **No fallback of any kind** — an inherited or unknown member yields
  no edge *here*; a property (post-#808 field classification) yields no edge at all.
- A **miss defers**: push onto `deferred_this_member_refs` for the second pass.

**`src/passes.rs` — `resolve_deferred_this_member_refs()`** (map §Pass ordering step 4, #808):
a **NODE-anchored BFS** over `implements`/`extends` edges, **depth < 5**, with member lookup via
the `contains` edges (`ctx.members_of`), requiring `same_language_family` → **0.85**.
**Node-anchored, not name-keyed**: a name-keyed `supertypes("Engine")` unioned every rails
`Engine`'s parents and produced a cross-class wrong edge (design doc §Known limits); the node walk
eliminated it. Runs after edges persist, drains the queue, returns the count.

- [ ] TDD — the resolution-half cases of `design/function-ref-capture.md`: an imported binding
  resolves cross-file when unique; **two same-named functions in different files ⇒ NO edge**;
  a same-file overload pair ⇒ **0.9**, a unique same-file match ⇒ **0.95**; a bare id in TS
  **never** resolves to a `method` (rule 3) but does in C#; `&Cls::method` resolves **scoped to
  that class** and a `Decoy::handle` does not match a `KtHandlers::handle` ref; `this.onResize`
  hits the enclosing class's method while `this.fonts` (a property) yields nothing; an
  **inherited** `this.X` resolves only through the deferred pass (**0.85**), and the pass is
  node-anchored (the rails `Engine` cross-class case produces **no** edge); a self-referencing
  candidate is excluded; `original.reference_name` is always the stored row's name (rule 10).
- [ ] Commit: `feat(resolve): function-ref resolution — unique-or-drop, class-scoped this.X`

---

## Open coordination points (Part A — surfaced to the maintainer; do not silently resolve)

1. **`GraphStore::delete_resolved` / `mark_failed` key arity.** The trait keys a processed row by
   `(from_node_id, reference_name)`; the TS lifecycle keys it by
   `fromNodeId + referenceName + referenceKind`. Phase 2 can emit **both** a `calls` ref and a
   `function_ref` with the same name from the same node, so a 2-part key may drain both when one
   resolves — a silent recall loss that the pending-count orphan sweep cannot detect. **Task 1
   measures it; the fix (extend the trait key to 3-part, touching `selene-db`) is a maintainer
   decision.**
2. **`ResolutionContext` is sync, over an async `GraphStore`.** Task 2 pins the seam:
   warm-cache-first + `block_on` inside `spawn_blocking` (Part C guarantees the wrapper). If the
   Part C driver ever runs the resolver on a tokio worker directly, this deadlocks — flagged here
   so the two parts stay consistent.
3. **New workspace deps** not in the roadmap's pins table: `fancy-regex` (lookahead),
   `lru`, `json5`/`jsonc-parser` (Task 1 picks one). Added in Task 1; noted in its commit body.
4. **Wave-2 resolution surface deliberately skipped in v0** (each left as a named comment at its
   ladder position, never a silent omission): CFML component-path inheritance, Razor `@using`,
   COBOL copybooks, Nix path imports, Terraform, ArkTS leading-dot attrs, Erlang behaviours, and
   the Lua/R receiver shapes. They land with their languages in Phase 8.
5. **TypeScript chained-call resolution stays unshipped** (Task 9) — a *decision*, not a gap.
   Reopening it needs inferred-return-type support and a fresh A/B.

---

*Tasks continue in Part B (frameworks + synthesizers).*


---

# Phase 3 — Part B: frameworks + dynamic-dispatch synthesizers — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Numbering:** this part numbers its tasks **20–36**. Part A (`phase3-partA-core.md`) owns
1–19 (resolver core: pass ordering, imports, name-matcher, chained calls, fn-ref gating,
and the shared Global Constraints). The controller renumbers at assembly; internal
cross-references in this file use these numbers.

**Goal:** the dynamic-dispatch half of `selene-resolve` — a data-driven **framework
registry** (Express, React Router, Django, Flask, FastAPI, Spring, Gin, Axum/Actix, Cargo,
Laravel, Rails, ASP.NET) plus **all 5 dispatch synthesizers** (callback/field-observer,
EventEmitter, React re-render, JSX child, Django ORM). Together these bridge the calls
static tree-sitter extraction structurally cannot see: a route's handler, an emitter's
listener, a `setState` that reaches `render`, a `<Child/>` that is really a call.

**Reference (in priority order):**
- `docs/reference/from-codegraph/maps/frameworks-synth.md` — **THE parity contract.** Every
  regex, constant, confidence and id format below is copied from it verbatim. An executor
  should not need to open it; if a task looks under-specified, the map §Key algorithms row
  for that framework is the tiebreaker.
- `docs/reference/from-codegraph/design/dynamic-dispatch-coverage-playbook.md` — the
  methodology (§2 shape→mechanism table, §4 the per-framework loop, §5 validation).
- `docs/reference/from-codegraph/design/callback-edge-synthesis.md` — the as-built
  synthesizer design incl. its recorded divergences (pair by file+field, regex arg
  recovery, `heuristic` provenance, fan-out caps instead of confidence tiers).
- `docs/plans/2026-07-12-selenecode-roadmap.md` §Phase 3 — scope fence.

---

## THE INVARIANT (governs every task in this part)

> **Dynamic-dispatch coverage is end-to-end or not at all.** Partial coverage is *worse*
> than none: a half-bridged flow reveals a hop the agent then Reads to finish, which
> destroys the product's value (PRD §8.2; playbook §3b/§4-Step5; the map records that
> shipping `react-render` *without* `jsx-render` measurably **raised** agent reads).

Operationally, for every task below:

1. Each task states **"Flow closed ⇔ …"** — the full chain, entry point → handler body,
   that must resolve for the bridge to count.
2. Each task's fixture asserts the **WHOLE chain** with a graph traversal
   (`store.find_path(entry, terminal)` / `callees` transitively), **not** a single edge
   existence check. An assertion that only proves hop 1 exists is a failing test dressed
   as a passing one.
3. If a task can only close part of its chain, it does **not** ship the partial bridge:
   it lands the pass **disabled** (not registered in the pass list) with the gap recorded
   in `lib.rs`'s deferrals ledger, and the task is not marked done.
4. Task 27 is the global gate: every framework/synthesizer fixture in one corpus, each
   asserting an end-to-end path. Phase 3 is not complete until it is green.

---

## Assumed from Part A (reconcile at assembly)

Part A was not yet written when this part was authored. These are the interfaces Part B
**consumes**; if Part A names them differently, the controller renames here — the shapes,
not the spellings, are what Part B depends on.

```rust
// selene-resolve/src/types.rs  (Part A)
pub struct UnresolvedRef {            // hydrated from selene_core::UnresolvedReference
    pub from_node_id: String, pub reference_name: String,
    pub reference_kind: String,       // EdgeKind wire string | "function_ref"
    pub line: Option<u32>, pub column: Option<u32>,
    pub file_path: String, pub language: Language,
}
pub struct ResolvedRef {
    pub target_node_id: String, pub confidence: f32,
    pub resolved_by: ResolvedBy,      // ::Framework for every hit in this part
}

/// The read-only project view a resolver/synthesizer sees. Part A owns it.
/// Part B needs these members; add them to Part A's trait if absent.
pub trait ResolutionContext: Send + Sync {
    fn root(&self) -> &Path;
    fn file_exists(&self, rel: &str) -> bool;              // project-relative
    fn read_file(&self, rel: &str) -> Option<&str>;        // cached source text
    fn all_files(&self) -> &[FileRecord];                  // path + language
    fn list_directories(&self, rel: &str) -> Vec<String>;  // cargo glob walk
    fn languages(&self) -> &BTreeSet<Language>;            // distinct file languages
}
```

- Part A owns `resolve_one()`'s **strategy ladder**. Part B plugs in as **Strategy 1
  (frameworks)**: iterate the registry in order, first result with `confidence >= 0.9`
  short-circuits; otherwise the result competes on max-confidence with import (0.9/0.95)
  and name-matcher. **The exact confidence constants in this part are load-bearing** for
  that competition — do not round them.
- Part A owns the **`claims_reference` pre-filter**: a ref whose name matches no symbol, no
  import, and no framework `claims_reference` is dropped *before* `resolve()` runs. Tasks
  20 / 23 / 28 / 35 depend on this hook existing. Claimed names (whole part):
  `_iterable_class`, `*.urls`, `Controller@method`, `[\w/]+#\w+`, `*:prefix`.
- Part A owns `gate_framework_language` (drops `references`/`imports` results that cross
  two known language families; `calls` and config↔code bridges pass) and the rule that
  `function_ref` refs **never reach frameworks**.
- Part A owns the resolver's `GraphStore` write path and error collection.

---

## Global Constraints (delta — Part A's apply too)

- **Heuristic edges:** `provenance: Provenance::Heuristic` + `metadata.synthesizedBy`
  (+ `via` / `event` / `field` / `registeredAt: "file:line"` as specified per pass). These
  metadata keys are a **wire contract** — Phase 5's MCP layer reads them verbatim to render
  the wiring site in explore's Flow section. `metadata` is `serde_json::Value` with
  **camelCase** keys (`synthesizedBy`, `registeredAt`) — the TS spelling, kept.
- **Framework-resolved refs are NOT heuristic.** They are ordinary resolved refs
  (`resolved_by: Framework`, `provenance: TreeSitter` on the resulting edge). Only
  whole-graph synthesizer passes emit `Heuristic`. (The map calls out this asymmetry for
  Django ORM explicitly — see Task 26.)
- **Route ids are opaque; route semantics are indexed fields.** Never parse, never
  key-match, never string-build a route id — anywhere. Look routes up with
  `find_route(framework, method, path)` (Task 11). In the fixtures below, the shorthand
  **`route("express", "POST", "/users/login")`** means exactly that call; `route(fw, None,
  "/x")` is a path-only router (django/react). If you find yourself writing
  `format!("route:{file}:…")`, stop — that is the TS design we deliberately dropped.
- **Route nodes are emitted at EXTRACTION time**, not resolution time: the registry's
  `extract(file_path, content)` runs after language extraction in `selene-extract`, for
  every detected framework applicable to that file's language. This is the
  `selene-extract` ↔ `selene-resolve` seam (Task 11).
- **Data-driven registry, not `if lang == …`:** every framework is a `FrameworkResolver`
  impl registered in one ordered table. Adding a framework = adding a file + a registry
  row. No framework name may be hard-coded in the resolver core.
- **Determinism:** same input ⇒ same edges in the same order. Iterate `BTreeMap`/sorted
  vecs, never `HashMap` iteration order, anywhere output order can leak. No wall-clock in
  output.
- **Errors collected, never thrown:** every `detect`/`extract`/`resolve`/`post_extract`/
  synthesizer call site is wrapped; a throwing framework produces a warning and never a
  failed index.
- **No `unwrap`/`expect`** outside `#[cfg(test)]`. House idiom for compile-time-literal
  regexes: `static RE: LazyLock<Regex> = LazyLock::new(|| { #[allow(clippy::unwrap_used)] // literal, covered by test below
  Regex::new(r"…").unwrap() });` + a test that constructs it.
- **Wire enums via `as_str()`** — never restring a `NodeKind`/`EdgeKind`.
- **Precision doctrine (survives the port verbatim):** fan-out caps instead of type
  inference; **named handlers only**; ambiguous ⇒ **drop, never guess**
  (`candidates.len() != 1 → continue`); silent beats wrong.
- **Perf discipline (carried from #610/#1212/#1235):** never materialize all nodes of an
  unbounded kind — stream; **language-gate every synthesizer pass** on
  `ctx.languages()`; pre-gate every expensive regex scan with a cheap `contains()`;
  index line-of-offset once per file (binary search), never `slice().split()` per match;
  insert edges in **2000-row chunks**. (The TS cooperative-yield machinery is *not*
  ported — no event-loop watchdog in Rust — but the streaming/batching discipline is.)
- **Regex portability:** all patterns below are ECMAScript. The `regex` crate has no
  lookaround; where a pattern needs it, hand-roll a scan or use `fancy-regex` (called out
  per task). Balanced-paren spans are **string-aware manual scans**, not regex.

---

## File structure (all under `crates/selene-resolve/`)

```
src/frameworks/mod.rs        FrameworkResolver trait, FRAMEWORK_REGISTRY (ordered),
                             detect_frameworks, applicable_frameworks, register()
src/frameworks/routes.rs     RouteNode builder — the id/name/qualifiedName contract
src/frameworks/express.rs    Express/Fastify/Koa/Hapi
src/frameworks/react.rs      React Router v5/v6 + component/hook/context conventions
src/frameworks/python.rs     Django (urls + DRF + ORM descriptor), Flask, FastAPI
src/frameworks/java.rs       Spring (Java + Kotlin + yaml/properties config keys)
src/frameworks/go.rs         Gin/Echo/Fiber/Chi/net-http
src/frameworks/rust_fw.rs    Axum + Actix attribute/builder routes
src/frameworks/cargo.rs      Cargo workspace crate map (helper, not a resolver)
src/frameworks/laravel.rs    Laravel + FACADE_MAPPINGS
src/frameworks/ruby.rs       Rails (explicit + RESTful `resources` expansion)
src/frameworks/csharp.rs     ASP.NET attribute + minimal API
src/synth/mod.rs             SynthPass trait, run_synthesis() orchestrator, dedupe, chunk
src/synth/lineindex.rs       lazy newline index + binary search (line_at(offset))
src/synth/callback.rs        field-observer channels
src/synth/event_emitter.rs   string-keyed EventEmitter channels
src/synth/react.rs           react-render + jsx-render (ship together — Tasks 33+34)
src/strip_comments.rs        per-language comment/string blanking (space-preserving)
tests/fw_<name>_test.rs      per-framework extract/resolve contract tests
tests/synth_<name>_test.rs   per-synthesizer unit tests
tests/dispatch_gate.rs       Task 27 — the END-TO-END gate over all fixtures
tests/fixtures/dispatch/<framework>/…   the end-to-end fixture corpus
```

---

## Task index

| # | Title | Commit type |
|---|---|---|
| 20 | Framework registry, `FrameworkResolver` trait, route-node contract, strip-comments | `feat(resolve)` |
| 21 | Express — routes, inline-arrow handler bodies, middleware/controller/service | `feat(resolve)` |
| 22 | React Router — v5/v6 + data-router; component/hook/context conventions | `feat(resolve)` |
| 23 | Django — `path`/`re_path`/`url` + DRF `router.register`; view/model conventions | `feat(resolve)` |
| 24 | Flask + FastAPI — decorator route engine, Flask-RESTful, dependency conventions | `feat(resolve)` |
| 25 | Spring — Java+Kotlin routes, config-key nodes, `@Value` relaxed binding, DI | `feat(resolve)` |
| 26 | Gin/Go — any-receiver routes, handler/service/middleware conventions | `feat(resolve)` |
| 27 | Axum/Actix + Cargo workspace crate map | `feat(resolve)` |
| 28 | Laravel + Rails — `Controller@method` / `controller#action` precise claims | `feat(resolve)` |
| 29 | ASP.NET — `[Http*]` + class `[Route]` prefix, minimal API, DI suffixes | `feat(resolve)` |
| 30 | Synthesizer harness — `SynthPass`, streaming primitives, dedupe, chunked insert | `feat(resolve)` |
| 31 | Synthesizer 1/5 — callback / field-observer channels | `feat(resolve)` |
| 32 | Synthesizer 2/5 — EventEmitter (string-keyed) channels | `feat(resolve)` |
| 33 | Synthesizer 3/5 — React re-render (`setState` → `render`) | `feat(resolve)` |
| 34 | Synthesizer 4/5 — JSX child (`<Child/>` → component) **— ships with 33** | `feat(resolve)` |
| 35 | Synthesizer 5/5 — Django ORM descriptor (resolver-mechanism, `claimsReference`) | `feat(resolve)` |
| 36 | **Phase 3 gate** — end-to-end dispatch-coverage fixture corpus + gate test | `test(resolve)` |

⚠ **Sequencing:** Task 11 blocks 21–29 and 35. Task 21 blocks 31–34. Tasks 21–29 are
mutually independent (one file each) and may be dispatched in parallel. **Tasks 33 and 34
form ONE mergeable unit** — see Task 25. Task 27 is last.

---

### Task 11: Framework registry, `FrameworkResolver` trait, route-node contract, strip-comments

**Files:** Create: `src/frameworks/mod.rs`, `src/frameworks/routes.rs`,
`src/strip_comments.rs`. Modify: `src/lib.rs` (module decls + the public-interface ledger),
`crates/selene-extract/src/orchestrator.rs` (the extract-time seam — see below).
Tests: `tests/fw_registry_test.rs`, `tests/strip_comments_test.rs`.

**Interfaces (the contract):**
```rust
pub struct FrameworkExtraction { pub nodes: Vec<Node>, pub refs: Vec<UnresolvedReference> }

pub trait FrameworkResolver: Send + Sync {
    fn name(&self) -> &'static str;
    /// None = applies to all languages (only `vue` does that in TS; unused in v0).
    fn languages(&self) -> Option<&'static [Language]>;
    /// Project-level, evaluated ONCE at resolver init.
    fn detect(&self, ctx: &dyn ResolutionContext) -> bool;
    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef>;
    /// Opt a name past Part A's "no symbol exists" pre-filter. Default: false.
    fn claims_reference(&self, _name: &str) -> bool { false }
    /// Route/config node emission. Runs in EXTRACTION. Default: empty.
    fn extract(&self, _path: &str, _content: &str) -> FrameworkExtraction { Default::default() }
    /// Cross-file finalize after every index/sync. Returns MUTATED nodes (id +
    /// qualified_name preserved), persisted via update. Default: empty.
    fn post_extract(&self, _ctx: &dyn ResolutionContext) -> Vec<Node> { Vec::new() }
}

/// Ordered — registry order IS resolve() precedence. Sorted, never a HashMap.
pub fn all_framework_resolvers() -> &'static [&'static dyn FrameworkResolver];
pub fn framework_resolver(name: &str) -> Option<&'static dyn FrameworkResolver>;
pub fn detect_frameworks(ctx: &dyn ResolutionContext) -> Vec<&'static dyn FrameworkResolver>;
pub fn applicable_frameworks<'a>(detected: &[&'a dyn FrameworkResolver], l: Language)
    -> Vec<&'a dyn FrameworkResolver>;

// routes.rs — the route-node contract. The id is the ORDINARY hashed node id; route
// SEMANTICS live in indexed fields (maintainer decision, 2026-07-13 — see below).
pub struct RouteSpec<'a> {
    pub framework: &'a str,          // resolver name: "express", "django", …
    pub method: Option<&'a str>,     // uppercased verb; None for path-only routers
    pub path: &'a str,
    pub file: &'a str,
    pub line: u32,
}
pub fn route_node(spec: &RouteSpec) -> Node;

/// The ONE way to look a route up. Indexed SurrealQL — never id-string parsing.
/// Lives on GraphStore (see the schema step below); re-exported here for tests.
pub async fn find_route<S: GraphStore>(store: &S, framework: Option<&str>,
    method: Option<&str>, path: &str) -> Result<Vec<Node>>;

// strip_comments.rs — blanks comments AND string bodies with spaces, BYTE-wise, so
// match offsets stay line-stable (every extractor below is regex-over-stripped-source).
pub fn strip_comments_for_regex(content: &str, lang: Language) -> String;
```

**Route-node contract — REDESIGNED vs the TS source (maintainer decision, 2026-07-13).**

TS encoded a route's semantics *into its id string*
(`route:{file}:{line}:{METHOD}:{path}`) and key-matched on that string downstream. **We do
not.** Route ids stay **hashed like every other node** — Phase 2's
`"<kind>:" + hex(sha256("{file}:{kind}:{name}:{line}"))[..32]` contract, **no new
exception** (the only id exception in the system remains the literal `file:<path>`).
Instead the semantics become **first-class indexed fields**, and every downstream lookup is
an **indexed SurrealQL query** (`WHERE kind='route' AND routeMethod=$m AND routePath=$p`),
never id parsing. This is what the locked SurrealQL-max decision asks for: **ids stay
opaque, semantics become queryable.**

*Fields to add.* `file` and `line` are **already** on `Node` (`file_path`, `start_line`) —
do **not** duplicate them. The genuinely new fields are three:

| `selene-core` `Node` field | wire / SurrealDB | Notes |
|---|---|---|
| `route_method: Option<String>` | `routeMethod` | **uppercased** verb, or `"ANY"`. `None` for path-only routers (django `path()`, react). |
| `route_path: Option<String>` | `routePath` | the raw path/prefix as written. |
| `framework: Option<String>` | `framework` | the resolver `name()` that emitted it. |

*Name / qualified-name spellings are UNCHANGED from TS* — they are agent-visible strings
surfaced by explore, so they keep their exact shape:

| Framework | `name` | `qualified_name` |
|---|---|---|
| express / laravel / rails-explicit / spring / go / rust / csharp / flask / fastapi | `{METHOD} {path}` | `{file}::{METHOD}:{path}` |
| django `path()` / react | `{path}` (raw url string) | `{file}::route:{path}` |
| DRF viewset | `VIEWSET /{prefix}` | `{file}::VIEWSET:{prefix}` |
| laravel resource | `resource:{name}` | `{file}::RESOURCE:{name}` |
| any verb-less | `ANY {path}` | `{file}::ANY:{path}` |

`NodeKind::Route`. METHOD is **uppercased**; `name` uses exactly **one space**.

> **Why the hashed id is still unique — and why `name` is load-bearing for that.** The hash
> input is `(file, kind, name, start_line)`. Several frameworks emit **multiple routes from
> ONE line**: axum `.route("/x", get(h).post(h2))`, rails `resources :articles` (7 actions,
> one line), flask stacked `@x.route` decorators. Those collide on `(file, kind, line)` and
> are separated **only by `name`** — which embeds the verb (`GET /x` vs `POST /x`) and, for
> rails, the action path. So the `{METHOD} {path}` name spelling above is not cosmetic: it
> is what keeps route ids unique now that the id no longer carries the method. **Task 27's
> gate must include a same-line multi-route fixture** (axum chained verbs + rails
> `resources`) asserting *N distinct route nodes*, not N−1. Any future framework that can
> emit two routes with the same `(file, name, line)` must disambiguate via `name`.

**Consequence for Part C (parity gate) — the controller is relaying this:** parity against
the TS build must compare **semantic identity** — `(framework, method, path, file, line)`
— **never raw id spelling**. TS ids are literal strings; ours are hashes. A byte-diff of
route ids is guaranteed to fail and means nothing.

- [ ] TDD: `tests/fw_registry_test.rs` first — assert (a) registry order is stable across
  100 calls (determinism; a `HashMap` here would flap), (b) `applicable_frameworks` filters
  by language and a `languages() == None` resolver matches everything, (c) a resolver whose
  `detect()` panics is caught and excluded (errors collected, never thrown) — use a test-only
  resolver registered via a `#[cfg(test)]` seam.
- [ ] TDD: `tests/strip_comments_test.rs` — for each of ts/py/java/go/rust/php/rb/cs:
  a source with a commented-out route + a string containing `//`. Assert (a) the output has
  **byte-identical length** to the input, (b) every `\n` is preserved at the same byte
  offset, (c) a route regex finds 0 matches in the commented-out route. Non-ASCII bytes each
  become one space byte (per Phase 2's `pre_parse` rule). This test pins the TS suite's
  "extractors ignore commented-out routes" case for the whole part.
- [ ] Implement `strip_comments_for_regex`: a per-language table of (line-comment, block
  comment open/close, string delimiters incl. raw/triple forms). Blank the *bodies*, keep
  the delimiters and newlines. Hand-rolled scanner (no regex).
- [ ] **Route fields — `selene-core` + `selene-db` (do this BEFORE `route_node()`).** This
  task carries the cross-crate change; the controller does **not** need a separate
  dependency task, because Task 11 already blocks 21–29 and no other Part-B task edits
  these files.
  - `selene-core::Node`: add `route_method: Option<String>`, `route_path: Option<String>`,
    `framework: Option<String>` (all `#[serde(skip_serializing_if = "Option::is_none")]`,
    camelCase wire names `routeMethod` / `routePath` / `framework`). Non-route nodes leave
    them `None`. **Bump `EXTRACTION_VERSION`** — this is an output-shape change, which is
    exactly what the bump rule in its doc comment is for.
  - `selene-db::schema` — the `node` table is **`SCHEMAFULL`**, so unknown fields are
    rejected: the `DEFINE FIELD`s are mandatory, not optional.
    ```sql
    DEFINE FIELD IF NOT EXISTS routeMethod ON node TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS routePath   ON node TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS framework   ON node TYPE option<string>;
    DEFINE INDEX IF NOT EXISTS node_route  ON node FIELDS kind, routeMethod, routePath;
    DEFINE INDEX IF NOT EXISTS node_framework ON node FIELDS framework;
    ```
    (Composite `kind` first so the index also serves "all routes" and "all routes of a
    framework" scans.)
  - `GraphStore`: add `find_route(framework, method, path) -> Vec<Node>` backed by that
    index. Test it against a store holding 2 same-path routes with different verbs and
    assert the verb filter selects one.
- [ ] TDD the id-uniqueness consequence: a fixture emitting **two routes from one line**
  (`GET /x` + `POST /x`, same file, same line) → **two distinct node ids**. This is the test
  that catches a framework author who sets `name` to just the path.
- [ ] Implement the trait + a `LazyLock<Vec<&'static dyn FrameworkResolver>>` registry.
  Registry order: `express, react, django, flask, fastapi, spring, go, rust, laravel, rails,
  aspnet` (alphabetical-within-ecosystem is NOT the contract — first-match-wins order is;
  keep this list as the one place order is declared).
- [ ] Implement `route_node()` per the table. Unit-test one id per row.
- [ ] Wire the **extract-time seam**: in `selene-extract`'s orchestrator, after language
  extraction of a file, call `extract(path, content)` for every *detected* framework
  applicable to that file's language; append nodes to the result's nodes and refs to its
  unresolved refs. Errors → a per-file `ExtractionError` with severity `warning`, **never**
  fatal. Detection runs once per index, not per file. (This makes `selene-extract` depend on
  `selene-resolve`'s registry — if that creates a cycle, the trait + registry move to a
  `selene-core::frameworks` module; record whichever you pick in `lib.rs`.)
- [ ] Wire `run_post_extract(ctx)`: after every full index AND every incremental sync, call
  each detected framework's `post_extract`, persist mutated nodes via the store's node
  upsert, per-framework try/catch. (No v0 framework uses it — NestJS's RouterModule
  prefixing does — but the hook is part of the trait contract and Phase 8 needs it.)
- [ ] Record in `src/lib.rs` the public-interface ledger entry + the deferrals: frameworks
  NOT in v0 (NestJS, SvelteKit, Vue/Nuxt, Vapor, Astro, Play, GoFrame, Drupal, Terraform,
  CICS, Swift↔ObjC, React Native, Expo, Fabric) are **Phase 8**; the ~31 non-v0 synthesizer
  passes are Phase 8.
- [ ] Commit: `feat(resolve): framework registry, FrameworkResolver trait, route-node contract`

---

### Task 12: Express — routes, inline-arrow handler bodies, middleware/controller/service

**Files:** Create: `src/frameworks/express.rs`. Tests: `tests/fw_express_test.rs`,
`tests/fixtures/dispatch/express/`.

**Flow closed ⇔** `POST /users/login` (route node) → the handler → **the service function
the handler's body calls** (`login`). The route→handler hop alone is NOT the flow: the
dominant modern shape is an *inline arrow* handler that is not a node at all, so the flow
must land on the **body's calls**. (Playbook §7 Express: the inline-arrow hole was the
whole ballgame — realworld 19 / ghost 65 edges; before the fix the route connected to
**nothing**.)

**Detection signals.** `name = "express"`, `languages = [Typescript, Tsx, Javascript, Jsx]`.
Detect: `package.json` deps contain any of `express | fastify | koa | hapi`; **else** any
file whose path contains `routes` | `controllers` | `middleware` AND whose content includes
`express` / `app.get` / `router.get`.

**Extract** (files matching `\.(m?js|tsx?|cjs)$`, over comment-stripped source):
- Head regex: `\b(app|router)\.(get|post|put|patch|delete|all|use)\s*\(\s*['"]([^'"]+)['"]\s*,`
  — for `use`, the path must start with `/` (else it's `app.use(cors())`, not a route).
- Args = a **balanced-paren, string-aware manual scan** from the `(` (a regex `[^)]+` breaks
  on the arrow's own `)` — that WAS the bug).
- **If the args contain `=>`** → inline handler: scan the arrow body for
  `\b([A-Za-z_$][\w$]*)\s*\(` and emit **one `calls` ref per unique name** not in
  `RESERVED_CALLS`, attributed to the **route node**.
- **Else** → the last comma-separated arg's tail identifier → one `references` ref.

**`RESERVED_CALLS` (verbatim; the map notes TS lists `redirect` twice — dedupe, it's a set):**
`json, jsonp, send, sendStatus, sendFile, status, end, redirect, render, set, get, header,
type, format, attachment, download, cookie, clearCookie, append, location, vary, links,
accepts, is, next, then, catch, finally, resolve, reject, all, race, map, filter, forEach,
reduce, find, push, pop, slice, splice, includes, keys, values, entries, assign, parse,
stringify, log, error, warn, info, String, Number, Boolean, Array, Object, Date, Math, JSON,
Promise, require, fail`

**Resolve** (confidences are load-bearing — they compete with import/name-matcher):
| Pattern (all `/i`) | Kind | Confidence |
|---|---|---|
| `^auth$`, `^authenticate$`, `^authorization$`, `^validate`, `^sanitize`, `^rateLimit`, `^cors$`, `^helmet$`, `^logger$`, `^errorHandler$`, `^notFound$`, `Middleware$` | function \| method | **0.8** |
| `^(\w+)Controller\.(\w+)$` | method on that class | **0.85** |
| `^(\w+)(Service\|Helper\|Utils?)\.(\w+)$` | method on that class | **0.8** |

- [ ] TDD **end-to-end first** (`tests/fixtures/dispatch/express/`): an `app.ts` with
  `router.post('/users/login', async (req, res) => { const u = await login(req.body); res.json(u) })`
  and a `service.ts` exporting `login()` which calls `hashPassword()`. Index → resolve →
  assert `store.find_path(route("express", "POST", "/users/login").id, node_id_of("hashPassword"))`
  returns a path **≥ 2 hops** (route → login → hashPassword). Assert `res.json` produced
  **no** ref (RESERVED_CALLS). This test fails until the whole task is done — that is the
  point (invariant §2).
- [ ] Unit tests ported from the TS contract suite: inline handler, middleware chain
  (`router.get('/x', auth, handler)` → handler ref is the LAST arg), `XController.method`
  ref, `app.use(cors())` emits no route, `app.use('/api', r)` does, a commented-out route
  emits nothing (strip-comments).
- [ ] Implement the balanced-paren scanner (string-aware: `'`, `"`, `` ` ``, escapes) —
  put it in `src/frameworks/mod.rs` as `match_delim(src, open_idx) -> Option<Range>`;
  Tasks 22/27 reuse it.
- [ ] Implement extract + resolve. Line = 1-based line of the match offset via the shared
  line index (`synth/lineindex.rs` from Task 21 — if Task 21 hasn't landed, inline a local
  one and de-dup at Task 21).
- [ ] Commit: `feat(resolve): express framework — routes, inline-arrow handlers, DI conventions`

---

### Task 13: React Router — v5/v6 + data-router + Next.js file routes; component/hook/context conventions

**Files:** Create: `src/frameworks/react.rs`. Tests: `tests/fw_react_test.rs`,
`tests/fixtures/dispatch/react/`.

**Flow closed ⇔** `<Route path="/article/:slug" element={<Article/>}/>` (route node) →
`Article` (the component node) → **the hook/service `Article`'s body calls**
(`useArticle` → `fetchArticle`). Route→component alone leaves the agent reading the
component to find the data call. Note this task closes route→component; the
component→child hop is the **JSX-child synthesizer** (Task 25) — the two together are what
makes a React flow answerable, which is exactly why Task 25 exists.

**Detection signals.** `name = "react"`, `languages = [Javascript, Jsx, Typescript, Tsx]`
(tsx/jsx **must** be listed or `extract` never runs on the files that hold the routes).
Detect: `package.json` deps contain `react | next | react-native`; **else** any `.jsx`/`.tsx`
file exists.

**Extract — on RAW content, NOT comment-stripped** (react is one of the few; a `<Route>` in
a JSX comment `{/* … */}` is rare enough that the TS build accepted it — keep the deviation,
it is a compat contract):
- **(a) JSX routes.** For each `<Route\b`: look in a **400-char window** for `path="…"`
  (skip the route entirely if absent) and for `component={Comp}` **or** `element={<Comp`.
  → route node with `route_method: None` (path-only router), `name` = the path,
  `qualified_name` = `{file}::route:{path}`, `framework: "react"`. Ref kind `references` →
  `Comp`.
- **(b) Object data-router.** Only if the file mentions
  `createBrowserRouter | createHashRouter | createMemoryRouter | createRoutesFromElements`:
  for each `path: '…'`, look in a **300-char window** for `element: <Comp` or
  `Component: Comp`. Path `''` → `/`.
- **(c) Next.js file routes** *(maintainer decision 2026-07-13: IN scope for Phase 3 — the
  fixture corpus covers it for free)*. A file whose path has a `pages/` or `app/` **path
  segment**, with an `export default`, basename not starting `_`, not matching
  `*.config.*`, extension `tsx?|jsx?`:
  - `pages/…` → path = the sub-path with the `pages/` prefix, a trailing `index`, and the
    extension stripped; `[x]` → `:x`. Route emitted at **line 1**, ref → the
    default-exported component.
  - `app/…` → **only `page.*` files** (`page.tsx`, `page.jsx`, …).
  - ⚠ **Fix the TS bug, do not port it:** the TS build tested `filePath.includes('page.')`,
    which also matches `mypage.tsx`. Match the **basename** against `^page\.(tsx?|jsx?)$`.
    Record this as a deliberate deviation in `lib.rs` (it is a bug fix, so counts may differ
    from TS by design — tell Part C).
- **Deferred (Phase 8, do NOT build):** lazy data-routers
  (`path: paths.x.path, lazy: () => import()` — variable paths, the known frontier).

**Resolve:**
| Ref | Target | Confidence | Rule |
|---|---|---|---|
| PascalCase name, **only from `tsx`/`jsx` refs** | component \| function \| class | **0.8** | same-dir first → component dirs → **unique-only**; if still ambiguous return `None` and let the name-matcher decide (TS #764 — do not guess) |
| `use*` | function | **0.85** | hook dirs preferred (`/hooks/`, `/hook/`) |
| `*Context` \| `*Provider` | variable \| constant \| function | **0.8** | |

- [ ] TDD **end-to-end first**: fixture `App.tsx` with a v6 `<Route path="/article/:slug"
  element={<Article/>}/>`, `Article.tsx` whose component body calls `useArticle()`, and
  `hooks/useArticle.ts` calling `fetchArticle()`. Assert
  `find_path(route("react", None, "/article/:slug").id, fetchArticle)` connects (route →
  Article → useArticle → fetchArticle). Add a v5 fixture (`component={Article}`) and a
  `createBrowserRouter([...])` fixture asserting the same terminal.
- [ ] TDD Next.js **end-to-end**: `pages/articles/[slug].tsx` with a default-exported
  component calling `useArticle()` → assert `find_path(route("react", None,
  "/articles/:slug").id, fetchArticle)` connects. Plus `app/articles/page.tsx`. Plus the
  bug-fix guard: a file named `mypage.tsx` under `app/` emits **no** route.
- [ ] Unit tests: `<Route>` with no `path` emits nothing; a `path` >400 chars from its
  `element` does not pair (window bound); object-router only fires when the file mentions a
  `create*Router`; PascalCase ref from a `.ts` (non-tsx) file does **not** resolve via this
  resolver (the language gate); `_app.tsx` / `next.config.js` emit no routes.
- [ ] Implement. The 400/300-char windows are **byte windows over the raw source**, clamped
  to the file end.
- [ ] Commit: `feat(resolve): react framework — react-router v5/v6, data-router + next.js file routes`

---

### Task 14: Django — `path`/`re_path`/`url` + DRF `router.register`; view/model conventions

**Files:** Create: `src/frameworks/python.rs` (Django section; Task 15 appends Flask +
FastAPI to the same file, Task 26 appends the ORM descriptor). Tests:
`tests/fw_django_test.rs`, `tests/fixtures/dispatch/django/`.

⚠ **Shared file:** Tasks 23, 24 and 35 all edit `src/frameworks/python.rs`. Run them
**sequentially**, never as parallel subagents. (Alternative if you must parallelize: split
into `python/django.rs`, `python/flask.rs`, `python/fastapi.rs` behind a `python/mod.rs` —
allowed, record the deviation in `lib.rs`.)

**Flow closed ⇔** `path('articles/<slug>/', ArticleDetail.as_view())` → `ArticleDetail`
(the view class) → **the model/queryset call in its body** (`Article.objects.filter`).
Django's *other* flow (QuerySet → SQL compiler) is Task 26 and is a **separate** chain;
this task's chain ends at the view's own calls.

**Detection signals.** `name = "django"`, `languages = [Python]`. Detect: `django`
(case-insensitive) appears in `requirements.txt` / `setup.py` / `pyproject.toml`, **or**
`manage.py` exists.

**Extract** (over comment-stripped source):
- URLconf: `\b(path|re_path|url)\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+(?:\s*\([^)]*\))?)`
  → route node, `route_method: None` (path-only), `route_path` = the url string,
  `name` = the **raw url string**. Handler expr:
  - `include('x.y')` → ref kind **`imports`**, name `x.y`
  - else strip a trailing `.as_view(...)` / trailing call, take the **last dotted segment**
    → ref kind `references`.
- DRF: `\.register\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+)` — **only** when the 2nd arg
  matches `/View(Set)?$/` (the string first arg is what separates `router.register` from
  `admin.register(Model, Admin)`, whose first arg is a class). → `route_method: "VIEWSET"`,
  `route_path` = `/{prefix}`, name `VIEWSET /{prefix}`; prefix strips a leading `^` and a
  trailing `/?$`.

**`claims_reference`:** `*.urls` (dotted `include` targets). (`_iterable_class` is claimed in
Task 26.)

**Resolve:**
| Ref | Dirs | Kind | Confidence |
|---|---|---|---|
| `*Model` or `^[A-Z][a-z]+$` | `MODEL_DIRS = ["models", "app/models", "src/models"]` | class | **0.8** |
| `*View` \| `*ViewSet` | `VIEW_DIRS = ["views", "app/views", "src/views"]` | class \| function | **0.85** |
| `*Form` | (any) | class | **0.8** |

- [ ] TDD **end-to-end first**: fixture `urls.py` (`path('articles/<slug>/',
  ArticleDetail.as_view())`), `views.py` (`class ArticleDetail(APIView)` whose `get` calls
  `get_article()`), `services.py` (`get_article` calls `Article.objects.filter`). Assert
  `find_path(route("django", None, "articles/<slug>/").id, get_article)` connects. Add a DRF
  fixture (`router.register(r'articles', ArticleViewSet)`) asserting the route→ViewSet→body
  chain via `route("django", Some("VIEWSET"), "/articles")`.
- [ ] Unit tests ported from the TS suite: `as_view` stripped; dotted handler
  (`views.article_detail` → `article_detail`); `include('api.urls')` → an `imports` ref;
  `re_path`/`url` forms; a **non-urls** python file emits no routes; `admin.register(Model,
  Admin)` emits **no** viewset route (the class-first-arg guard).
- [ ] Commit: `feat(resolve): django framework — urlconf + DRF router routes, view/model conventions`

---

### Task 15: Flask + FastAPI — decorator route engine, Flask-RESTful, dependency conventions

**Files:** Modify: `src/frameworks/python.rs` (append the two resolvers) — **sequential
after Task 14**. Tests: `tests/fw_flask_test.rs`, `tests/fw_fastapi_test.rs`,
`tests/fixtures/dispatch/{flask,fastapi}/`.

**Flow closed ⇔** `GET /api/articles` (route) → the decorated `def` handler → **the
service function the handler body calls**. The decorator→`def` pairing is the whole
mechanism: get it wrong and the route points at nothing.

**Shared machinery — the "decorator route engine"** (both frameworks + Phase 8's others):
after matching a decorator at offset `o`, the handler is the **next**
`\n\s*(?:async\s+)?def\s+(\w+)` occurring after `o` — which **skips stacked decorators**
(`@app.route(...)` `@login_required` `def x()` → still `x`). Implement once,
`fn next_def_after(src: &str, offset: usize) -> Option<(String, u32)>`.

**Flask** (`name = "flask"`, `languages = [Python]`):
- Detect: `\bflask\b` (case-insensitive) in `requirements.txt` / `pyproject.toml` /
  `Pipfile` / `setup.py`; **else** scan the **first 50** files matching
  `(?:^|/)(app|application|main|wsgi|__init__)\.py$` for `\bFlask\s*\(` **plus** a flask
  import. (The subdir app-factory entrypoint case is why the second arm exists — a
  requirements-less repo was 0→19 routes.)
- Extract: `@(\w+)\.route\s*\(\s*['"]([^'"]*)['"](?:\s*,\s*methods\s*=\s*[[(]([^\])]+)[\])])?\s*\)`
  → default method **GET**; the method is the **first quoted token** of the list/tuple
  (a tuple `methods=('GET',)` was previously mislabeled — pin it). Route name
  `{METHOD} {path || '/'}`; `route_method` = the verb, `route_path` = the path.
- Flask-RESTful: `\.add\w*[Rr]esource\s*\(\s*(\w+)\s*,\s*((?:['"][^'"]+['"]\s*,?\s*)+)` →
  **one route per path**, method `ANY`, ref → the Resource class.
- Resolve: `*_bp` | `*_blueprint` → kind `variable`, **0.8**.

**FastAPI** (`name = "fastapi"`, `languages = [Python]`):
- Detect: `\bfastapi\b` (case-insensitive) in requirements/pyproject, **or** `FastAPI(`
  appears in `app.py` | `main.py` | `api.py`.
- Extract: `@(\w+)\.(get|post|put|patch|delete|options|head)\s*\(\s*['"]([^'"]*)['"]`
  — **empty path allowed** → name `/` (`@router.get("")` router-root routes are real and
  were a 100%-recall fix on a large corpus). Handler = `next_def_after`.
- Resolve: `*_router` | `router` → kind `variable` in a path containing `/routers/` |
  `/api/` | `/routes/` | `/endpoints/`, **0.8**; `get_*` | `Depends*` → function in
  `/dependencies/` | `/deps/` | `/core/`, **0.75**.
- **Builtin-name guard (do not lose this):** a handler named after a Python builtin/method
  (`index`, `get`, `update`, `count`, …) must NOT be filtered out as a builtin by Part A's
  name-matcher pre-filter. The route→handler ref is *framework-claimed*; ensure the
  ref survives. Add a regression test with a handler literally named `get`.

- [ ] TDD **end-to-end first** (both): Flask fixture with a **stacked** decorator
  (`@bp.route('/articles', methods=['POST'])` + `@login_required` + `def create():` calling
  `create_article()` in `services.py`); assert `find_path(route("flask", "POST",
  "/articles").id, create_article)`. FastAPI fixture with `@router.get("")` on a handler
  named `get` calling `list_articles()`; assert the same shape via `route("fastapi", "GET",
  "/")`. Both must connect route → handler → service.
- [ ] Unit tests: tuple `methods=('GET',)`; multi-line FastAPI decorator; intervening
  `@login_required`; two stacked `@x.route` lines on one handler → **two** route nodes, one
  handler; `add_resource(Api, '/a', '/b')` → 2 routes, both `ANY`, both → the class.
- [ ] Commit: `feat(resolve): flask + fastapi frameworks — decorator route engine, RESTful resources`

---

### Task 16: Spring — Java+Kotlin routes, config-key nodes, `@Value` relaxed binding, DI

**Files:** Create: `src/frameworks/java.rs`. Tests: `tests/fw_spring_test.rs`,
`tests/fixtures/dispatch/spring/`.

**Flow closed ⇔** `GET /articles/{slug}` (route) → the `@GetMapping` **method** → the
`@Autowired`/field-injected **service** it calls → (via Part A's name-matcher) the service
impl. Two hops are framework work here: the **bare-mapping + class-prefix join** (without
it, multi-method controllers — the dominant shape — have no routes at all: halo had 28
routes for 2,444 files) and the **config-key bridge**.

**Detection signals.** `name = "spring"`,
`languages = [Java, Kotlin, Yaml, Properties]` — the config languages **must** be listed or
`extract` never runs on `application.yml`, and the `@Value` bridge is then a half-bridged
flow (which the hard invariant forbids — this is precisely why the enum addition below was
authorized). Detect: `spring-boot | springframework` in `pom.xml` / `build.gradle` /
`build.gradle.kts`; **else** `@SpringBootApplication | @RestController | @Service |
@Repository` in any `.java`.

> ⚠ **Cross-crate step — sequencing.** This task adds `Yaml` and `Properties` to
> **`selene-extract`'s `Language` enum** (`src/language.rs`) as **file-level-only**
> languages (maintainer decision, 2026-07-13). `is_file_level_only()` already exists and
> already contains `{yaml, twig, properties}` per Phase 2's plan — **verify** whether the
> enum variants actually landed; if they did, this is a no-op and you only add the
> `EXTENSION_MAP` rows (`.yml`, `.yaml`, `.properties`). **This is the only Part-B task that
> touches `selene-extract/src/language.rs`** — no collision with Task 11 (which touches
> `orchestrator.rs`) or any other task. Do not run this concurrently with a Part-A task
> that edits the same file; confirm with the controller at dispatch.

**`claims_reference`:** names ending `:prefix` (i.e. `*:prefix` — the
`@ConfigurationProperties` bind refs).

**Extract — config files** (basename matches
`application|bootstrap(-<profile>)?\.(yml|yaml|properties)`):
- One `NodeKind::Constant` node per **LEAF** key. id `spring-config:{file}:{line}:{dotted}`,
  `qualified_name` = the dotted key.
- **The VALUE IS NEVER STORED** (secret redaction, TS #383). This is a security contract:
  add an explicit test asserting no `Node.signature`/`metadata` field of a config node
  contains the value text.

**Extract — `.java` / `.kt`:**
- Class-level `@RequestMapping("…")` → a **prefix**, not a route.
- Verb annotations `@(Get|Post|Put|Patch|Delete)Mapping\b\s*(\([^)]*\))?` — **bare allowed**
  (no parens). Path = `join_path(prefix, sub)` = `'/' + parts joined by '/'` (collapse
  duplicate slashes). Name `{VERB} {path}`.
- Method-level `@RequestMapping(method=RequestMethod.X)` → verb `X`, else `ANY`.
- Handler = within the **next 600 chars**: Kotlin `\bfun\s+(\w+)\s*\(`; Java
  `\b(?:public|private|protected)\s+[^;{=]*?\s+(\w+)\s*\(`.
- `@Value("${k[:default]}")` → a `Constant` bind node `spring-value:{file}:{line}:{k}` + a
  **`references`** ref named `k`. `@ConfigurationProperties(prefix="p")` → node
  `spring-cp:{file}:{line}:{p}` + a `references` ref named `p:prefix`.

**Resolve:**
- `*:prefix` refs → the **shortest** config constant whose canonical key starts with the
  canonical prefix. **0.85**.
- Dotted `references` refs from java/kotlin — **`calls` refs are NEVER resolved this way**
  (TS #1180: it was a perf catastrophe; the `references`-only gate is a hard contract, test
  it) → exact **canonical** match. Canonicalization (Spring *relaxed binding*):
  `key.to_lowercase().replace(['-', '_'], "")`. Confidence **0.9** if unique; **0.75** on a
  tie, broken by: base file (`application.yml`) over profile variants, then shorter
  basename.
- DI suffix conventions, with directory preferences:
  | Ref | Kind | Conf | Preferred dir infix |
  |---|---|---|---|
  | `*Service` | class \| interface | 0.85 | `/service/` |
  | `*Repository` | class \| interface | 0.85 | `/repository/` |
  | `*Controller` | class | 0.85 | `/controller/` |
  | PascalCase entity | class | 0.70 | `/entity/`, `/entities/`, `/model/`, `/models/`, `/domain/` |
  | `*Component` \| `*Config` | class | 0.80 | `/component/`, `/components/`, `/config/` |

- [ ] **First:** add `Yaml` / `Properties` to `selene-extract`'s `Language` enum +
  `EXTENSION_MAP` (`.yml`, `.yaml`, `.properties`) as **file-level-only** languages, if not
  already present. Test: `detect_language("application.yml")` → `Yaml`, and
  `is_file_level_only(Yaml)` → true (a yaml file yields a file node and **no** symbol
  nodes from the generic walker — only Spring's `extract()` adds the config constants).
- [ ] TDD **end-to-end first**: fixture `ArticleController.java` with class
  `@RequestMapping("/articles")` + a **bare** `@GetMapping` method `getBySlug` calling
  `articleService.findBySlug(...)`, plus `ArticleService.java`. Assert
  `find_path(route("spring", "GET", "/articles").id, ArticleService.findBySlug)` connects.
  Add the **Kotlin** twin (`fun getBySlug`) asserting the same.
- [ ] TDD the config bridge **end-to-end** (this is the hop the enum addition unlocks):
  `application.yml` containing `app: { cache-list: … }` + a class with
  `@Value("${app.cache-list}")`. Assert the `@Value` bind node **resolves to the config
  constant node** for `app.cacheList` (relaxed binding: `cache-list` ≡ `cacheList` ≡
  `cache_list`). Also assert a `calls` ref with a dotted name does **not** resolve to a
  config key (the #1180 `references`-only gate) — and that the constant node stores **no
  value text** (the #383 secret-redaction contract).
- [ ] Unit tests: class-prefix join; stacked annotations; `@RequestMapping(method=…)`;
  profile-variant tie-break; **config values never stored**.
- [ ] Commit: `feat(resolve): spring framework — java/kotlin routes, config keys, relaxed binding, DI`

---

### Task 17: Gin/Go — any-receiver routes, handler/service/middleware conventions

**Files:** Create: `src/frameworks/go.rs`. Tests: `tests/fw_go_test.rs`,
`tests/fixtures/dispatch/go/`.

**Flow closed ⇔** `POST /api/v1/articles` (route) → the named handler func → **the service
call in its body**. The load-bearing detail is **any receiver**: routes are registered on
*group* variables (`v1.GET(...)`, `PublicGroup.GET(...)`), not just `r`/`router` — the TS
build was missing *every* group-routed app until this was generalized (gin-vue-admin
4 → 259 routes).

**Detection signals.** `name = "go"`, `languages = [Go]`. Detect: `go.mod` is readable,
**or** any `.go` file exists. (Deliberately broad — the extract regex is the real gate.)

**Extract** (over comment-stripped source):
```
\b\w+\.(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Get|Post|Put|Patch|Delete|Handle|HandleFunc)
   \s*\(\s*"([^"]+)"\s*,\s*([^)]+)\)
```
- Receiver = **ANY identifier** (that is the fix). `Handle` | `HandleFunc` → method `ANY`.
- Ref = the **tail identifier** of the handler expr (`handlers.CreateArticle` →
  `CreateArticle`). This also covers **gorilla/mux** (`s.HandleFunc(...)`, subrouter vars)
  and chi/net-http; the `.Methods()` chain is ignored (label only).
- `route_method` = the verb (or `ANY`), `route_path` = the quoted path, `framework = "go"`.

**Resolve** (dir match requires the `/{dir}/` **infix**, not a prefix):
| Ref | Kind | Conf | Dirs |
|---|---|---|---|
| `*Handler` \| `Handle*` | function | 0.8 | `/handler/`, `/handlers/`, `/api/`, `/controller/` |
| `*Service` \| `*Repository` \| `*Store` | struct \| interface | 0.8 | `/service/`, `/repository/`, `/store/` |
| `*Middleware` \| `Auth*` \| `Log*` | function | 0.75 | `/middleware/` |
| PascalCase | struct | 0.7 | `/model/`, `/models/`, `/entity/` |

**Deferred (Phase 8, do NOT build):** the `gin-middleware-chain` synthesizer (`(*Context).Next`
→ every registered HandlerFunc), `go-implements` / `go-contains` pre-passes, GoFrame.
Note them in `lib.rs` — Gin's middleware chain is a **known open hop** in v0, and per the
invariant we say so rather than half-bridge it.

- [ ] TDD **end-to-end first**: fixture `router.go` with `v1 := r.Group("/api/v1")` and
  `v1.POST("/articles", handlers.CreateArticle)`; `handlers/article.go` with
  `func CreateArticle(c *gin.Context)` calling `service.Create(...)`; `service/article.go`.
  Assert `find_path(route("go", "POST", "/articles").id, service.Create)` connects. (Note: the group
  prefix is **not** prepended to the path — TS deviation, keep it; the route name is
  `POST /articles`. Record it in the deviations ledger.)
- [ ] Unit tests: any-receiver (`PublicGroup.GET`); `HandleFunc` → `ANY`; namespaced handler
  → tail identifier; a commented-out route emits nothing.
- [ ] Commit: `feat(resolve): go framework — any-receiver gin/mux/chi routes, handler conventions`

---

### Task 18: Axum/Actix + Cargo workspace crate map

**Files:** Create: `src/frameworks/rust_fw.rs`, `src/frameworks/cargo.rs`. Tests:
`tests/fw_rust_test.rs`, `tests/fw_cargo_test.rs`, `tests/fixtures/dispatch/axum/`.

⚠ **Overlap with Part A:** Part A's import-resolver also lists "cargo workspace globs". The
crate map lives **here** (`frameworks/cargo.rs`) and is a plain function Part A's import
resolver may call. If Part A also specifies it, the controller drops the duplicate — the
API below is the one to keep. **Open question for the maintainer** (see end of file).

**Flow closed ⇔** `.route("/articles", get(list).post(create))` → **both** `list` and
`create` handler fns → the service call in each body. Chained verbs are the whole point:
the TS build emitted only the *first* verb+handler, so `post(create)` was invisible
(realworld-axum 12 → 19 routes).

**Detection signals.** `name = "rust"`, `languages = [Rust]`. Detect: `Cargo.toml` exists.

**Extract** (over comment-stripped source) — three shapes:
- **(a) Attribute routes** (Actix/Rocket style):
  `#[(get|post|put|patch|delete|head|options)\s*\(\s*["']([^"']+)["'].*\)]` → route + the
  **next `fn` name**.
- **(b) Axum.** Find each `.route\s*\(`; take the **balanced-paren** args (reuse Task 12's
  `match_delim`); path = `^\s*"([^"]+)"\s*,`. Then for **each** chained
  `\b(get|post|put|patch|delete|head|options|trace)\s*\(\s*([A-Za-z_][\w:]*)` in the
  remaining args, emit **one route node per verb** (same line), ref = the **last `::`
  segment** of the handler (`api::v1::list` → `list`).
- **(c) Actix builder.** `web::resource("p")` + the following **≤500-char** chain (bounded
  at the next `web::resource`), scanning `web::(verb)\(\)\.to\((handler)\)`; a bare `.to(h)`
  with no verb → `ANY`. Also app-level `.route("p", web::verb().to(h))`.

**Resolve:**
| Ref | Kind | Conf |
|---|---|---|
| `*_handler` \| `handle_*` | function | 0.8 |
| `*Service` \| `*Repository` | struct \| trait | 0.8 |
| PascalCase | struct | 0.7 |
| `^[a-z_]+$` (a module) | file node | see below |

Module resolution order: `src/{n}.rs` → `src/{n}/mod.rs` (**local, conf 0.6**) → **cargo
workspace** `{crateDir}/src/lib.rs` \| `main.rs` (**conf 0.95** — deliberately beats the
name-matcher's self-file 0.7; a workspace crate reference must not lose to a local
same-named symbol).

**`frameworks/cargo.rs` — the crate map** (helper, not a `FrameworkResolver`):
```rust
/// crate-name AND crate_name (underscore alias) -> member directory. Cached per-context.
pub fn cargo_workspace_crate_map(ctx: &dyn ResolutionContext) -> &BTreeMap<String, String>;
```
- Parse the root `Cargo.toml` `[workspace] members` — a **hand-rolled**, escape-aware
  section/array/quote parser (do NOT pull in a TOML crate for parity: the TS parser is
  lenient in ways `toml` is not; if you do use `toml_edit`, prove equivalence on a malformed
  fixture and record the deviation).
- Expand globs by walking `ctx.list_directories()`; **skip** `target`, `node_modules`,
  `.git`, `dist`, `build` and any dot-dir; `MAX_GLOB_WALK_DEPTH = 5`.
- Read each member's `[package] name`; map **both** `crate-name` and `crate_name` → dir.
- Cache: a `OnceCell` on the resolver instance (the TS `WeakMap`-per-context).

- [ ] TDD **end-to-end first**: fixture with `main.rs` doing
  `.route("/articles", get(list_articles).post(create_article))`, `handlers.rs` with both
  fns each calling into `service.rs`. Assert **both**
  `find_path(route("rust", "GET", "/articles").id, service::list)` **and**
  `find_path(route("rust", "POST", "/articles").id, service::create)` connect. A test that
  only checks the GET route would pass on the *broken* single-verb behavior — this is the
  canonical example of why the fixture must assert the whole chain. **Also assert the two
  route nodes have distinct ids** (they share file+line; only `name` separates them — the
  hashed-id consequence from Task 11).
- [ ] TDD cargo: a workspace fixture (`members = ["crates/*"]`) with two member crates;
  assert both `my-crate` and `my_crate` map to `crates/my-crate`, that `target/` is not
  walked, and that depth > 5 is not walked.
- [ ] Unit tests: attribute route + next fn; actix `web::resource` chain incl. the 500-char
  bound; namespaced axum handler (`get(api::list)` → `list`).
- [ ] **Deferred (record, do not build):** actix `web::scope("/api")` prefix not prepended;
  anonymous closure handlers (`.to(|| async {})`).
- [ ] Commit: `feat(resolve): axum/actix framework routes + cargo workspace crate map`

---

### Task 19: Laravel + Rails — `Controller@method` / `controller#action` precise claims

**Files:** Create: `src/frameworks/laravel.rs`, `src/frameworks/ruby.rs`. Tests:
`tests/fw_laravel_test.rs`, `tests/fw_rails_test.rs`,
`tests/fixtures/dispatch/{laravel,rails}/`.

**Flow closed ⇔** route → **the right controller's** action → the model/service call in its
body. Both frameworks share the same hard-won lesson, which is why they are one task:
**the handler ref must carry the controller**, not the bare method name. A bare `index` ref
name-matches to *whichever* `index` the matcher finds first (every Laravel route resolved to
`ArticleController.index`). And both need `claims_reference`, because `ArticleController@index`
/ `articles#index` name **no declared symbol**, so Part A's pre-filter drops them before
`resolve()` ever runs. (Playbook §7 Rails: "the `claimsReference` pre-filter was the gotcha".)

**Laravel** (`name = "laravel"`, `languages = [Php]`):
- Detect: `artisan` exists **or** `app/Http/Kernel.php` exists.
- **`claims_reference`:** `^[A-Za-z_][A-Za-z0-9_]*Controller@\w+$`.
- Extract: `Route::(get|post|put|patch|delete|options|any)\(…\)` → route,
  `name = "{VERB} {path}"`. Handler expr:
  - `[Class::class, 'm']` → ref name **`Class@m`**
  - `'Ctrl@m'` → **`Ctrl@m`** (namespace-stripped: take the segment after the last `\`)
  - `Class::class` → `Class`
  - a closure → **no ref** (silent beats wrong)
  - `Route::(resource|apiResource)('name', Ctrl::class)` → `route_method: "RESOURCE"`,
    `route_path` = `{name}`, node name `resource:{name}`, ref kind **`imports`**.
- Resolve:
  - `Controller@method` → `app/Http/Controllers/{C}.php` → else **any class named `C` in a
    path containing `Controllers`**. Confidence **0.9** → short-circuits (returns immediately).
  - `Model::method` → `app/Models/{C}.php` then `app/{C}.php`; **method first, class
    fallback**. Confidence **0.85**.
  - **Facades/helpers return `None`** (they are external). `FACADE_MAPPINGS` (const, copy
    verbatim): `Auth → Illuminate\Auth\AuthManager`, and the same shape for `Cache, Config,
    DB, Event, File, Gate, Hash, Log, Mail, Queue, Redis, Request, Response, Route, Session,
    Storage, URL, Validator, View`. (Kept as data for Phase 8's `laravel-event` synthesizer;
    v0 uses it only to *recognize and skip* facades.)

**Rails** (`name = "rails"`, `languages = [Ruby]`):
- Detect: `Gemfile` contains `'rails'`, **or** any of `config/application.rb` /
  `app/controllers/application_controller.rb` / `config/routes.rb` exists.
- **`claims_reference`:** `^[\w/]+#\w+$`.
- Extract:
  - Explicit:
    `\b(get|post|put|patch|delete|match)\s+['"]([^'"]+)['"]\s*(?:,\s*to:\s*|=>\s*)['"]([\w/]+#\w+)['"]`
    → route + a `{c}#{a}` ref.
  - `resources :name` / `resource :name` (with `only:` / `except:` filters) → expand via
    **`RESTFUL_ROUTES` (verbatim)**:
    | action | verb | path |
    |---|---|---|
    | index | GET | `/{r}` |
    | create | POST | `/{r}` |
    | new | GET | `/{r}/new` |
    | show | GET | `/{r}/:id` |
    | edit | GET | `/{r}/:id/edit` |
    | update | PATCH | `/{r}/:id` |
    | destroy | DELETE | `/{r}/:id` |
    Singular `resource` **omits `index`** and **pluralizes** the controller.
    ⚠ **All 7 expanded routes come from ONE source line**, so they collide on
    `(file, kind, line)` and are separated only by `name`. Set the node `name` to
    `{VERB} {path}` (e.g. `GET /articles/:id`) — `index` and `create` share `/articles` but
    differ by verb; `show` and `edit` differ by path. Assert 7 **distinct** ids in the test.
  - `pluralize` / `camelize` are **deliberately naive — port as-is** (they are an id compat
    contract): pluralize = `y → ies`; `s|x|z|ch|sh → +es`; else `+s`.
- Resolve:
  - **Pattern 0** `c#a` → `app/controllers/{c}_controller.rb`, the method `a`; fallback the
    camelized `XController` class file. **0.85**. **Returns `None` on a miss — no
    fallthrough** (a miss must not degrade into a bare-name match; that is the bug being
    prevented).
  - Model `^[A-Z][a-zA-Z]+$` → snake-cased `app/models/…`, **0.8**.
  - `*Controller` **0.85**; `*Helper` → kind `module`, **0.8**; `*Service` | `*Job` **0.8**.

- [ ] TDD **end-to-end first** (both): Laravel fixture — `routes/api.php` with
  `Route::get('/articles', [ArticleController::class, 'index'])`, an
  `app/Http/Controllers/ArticleController.php` whose `index()` calls
  `$this->articleService->list()`, plus the service class. Assert
  `find_path(route("laravel", "GET", "/articles").id, ArticleService::list)` connects **and**
  that a *second* controller in the fixture also defining `index()` is **not** the target
  (the precision regression — assert the resolved target's file).
  Rails fixture — `config/routes.rb` with `resources :articles, only: [:index, :create]`,
  `app/controllers/articles_controller.rb` with both actions, `app/models/article.rb`.
  Assert the expansion yields exactly **2** routes, with **2 distinct ids** (same line!),
  and that `find_path(route("rails", "GET", "/articles").id, Article.recent)` connects
  through `ArticlesController#index`.
- [ ] Unit tests: Laravel tuple / `@` string / namespaced string / closure (no ref) /
  `Route::resource`; Rails `to:` and `=>` forms; `resource` (singular) omits index +
  pluralizes; `except:` filter; the naive pluralizer on `category → categories`.
- [ ] **Assert `claims_reference` is actually consulted**: a test where the ref name matches
  no declared symbol and the resolver still fires (this is the hop that silently deleted
  every Rails route in the TS build).
- [ ] Commit: `feat(resolve): laravel + rails frameworks — precise controller@method / controller#action`

---

### Task 20: ASP.NET — `[Http*]` + class `[Route]` prefix, minimal API, DI suffixes

**Files:** Create: `src/frameworks/csharp.rs`. Tests: `tests/fw_aspnet_test.rs`,
`tests/fixtures/dispatch/aspnet/`.

**Flow closed ⇔** `GET /api/articles` (route) → the `[HttpGet]` action method → the
DI'd service it calls. Same shape as Spring: **bare `[HttpGet]` + class `[Route("api/x")]`
prefix** is the dominant multi-action-controller pattern; without the join, those
controllers have zero routes (eShopOnWeb 9 → 33).

**Detection signals.** `name = "aspnet"`, `languages = [CSharp]`. Detect, in order:
1. a `.csproj` containing `Microsoft.AspNetCore` | `Microsoft.NET.Sdk.Web` | `System.Web.Mvc`;
2. `Program.cs` containing `WebApplication` | `CreateHostBuilder` | `UseStartup`;
3. `Startup.cs` exists;
4. else scan sources matching `(Controller|Program|Startup)\.cs$` for the attribute /
   base-class signatures. (Arm 4 is the **feature-folder** case — a repo laying controllers
   out by feature was entirely undetected: 0 → 19 routes.)

**Extract** (over comment-stripped source):
- Class-level `[Route("p")]` → prefix.
- `\[(HttpGet|HttpPost|HttpPut|HttpPatch|HttpDelete)(?:\s*\(\s*"([^"]+)"[^)]*\))?\s*\]` —
  **bare allowed**. Path = prefix joined with the sub-path. Method = the attribute name
  minus `Http`, **uppercased**.
- Handler = within the **next 600 chars**:
  `(?:public|private|protected|internal)\s+[\w<>,\s\[\]?.]+?\s+(\w+)\s*\(`.
- Minimal API: `\.Map(Get|Post|Put|Patch|Delete)\s*\(\s*"([^"]+)"\s*,\s*([^,)]+)` → the
  **tail identifier** of the handler expr.

**Resolve:**
| Ref | Kind | Conf |
|---|---|---|
| `*Controller` | class | 0.85 |
| `*Service` or `I*` | class \| interface | 0.85 |
| `*Repository` | class \| interface | 0.85 |
| `*ViewModel` \| `*Dto` | class | 0.80 |
| PascalCase model | class | 0.70 |

- [ ] TDD **end-to-end first**: fixture `ArticlesController.cs` with `[Route("api/articles")]`
  on the class + a **bare** `[HttpGet]` on `GetAll()` which calls `_articleService.ListAsync()`;
  plus `ArticleService.cs`. Assert `find_path(route("aspnet", "GET", "/api/articles").id,
  ArticleService.ListAsync)` connects. Add a minimal-API fixture
  (`app.MapGet("/health", HealthHandler.Check)`) asserting route → `Check`.
- [ ] Unit tests: bare attribute + class prefix join; attribute with its own path; two
  actions in one controller → 2 routes; the 600-char handler window bound; feature-folder
  detection (arm 4) fires with no `.csproj`.
- [ ] **Deferred (record):** EF Core LINQ/`DbSet` dispatch (metaprogramming frontier).
- [ ] Commit: `feat(resolve): aspnet framework — attribute + minimal-API routes, DI conventions`

---

### Task 21: Synthesizer harness — `SynthPass`, streaming primitives, dedupe, chunked insert

**Files:** Create: `src/synth/mod.rs`, `src/synth/lineindex.rs`. Modify:
`crates/selene-db/src/store.rs` + `store_impl.rs` (**one new primitive**, see below),
`src/lib.rs`. Tests: `tests/synth_harness_test.rs`.

**Why this task exists:** all four whole-graph passes (Tasks 31–34) share the same skeleton,
and getting the skeleton wrong is how the TS build earned three separate OOM/perf incidents
(#610, #1212, #1235). Build it once, correctly, then the passes are small.

**Interfaces:**
```rust
pub trait SynthPass: Send + Sync {
    fn name(&self) -> &'static str;                   // == metadata.synthesizedBy
    /// Languages this pass applies to. Empty = all. Checked against
    /// ctx.languages() BEFORE the pass runs — a Python-only repo never scans for JSX.
    fn languages(&self) -> &'static [Language];
    fn run<S: GraphStore>(&self, store: &S, ctx: &dyn ResolutionContext)
        -> impl Future<Output = Result<Vec<Edge>>> + Send;   // COLLECT, do not insert
}

/// Runs every pass, merges, dedupes, inserts. Returns the count inserted.
pub async fn run_synthesis<S: GraphStore>(store: &S, ctx: &dyn ResolutionContext)
    -> Result<u64>;
```

**Orchestration contract (copy exactly):**
- Passes **collect** edges; the orchestrator merges them and applies a **cross-pass dedupe
  keyed on `(source, target)`** — **first pass wins**. (Not `(source, target, kind)`: the TS
  key is `source>target`. Keep it — a second pass must not double-link an already-bridged
  pair.)
- Pass **order is fixed and declared in one place** (determinism: the dedupe makes order
  observable). v0 order: `callback`, `event-emitter`, `react-render`, `jsx-render`.
  (Phase 8's Go `contains` + `implements` pre-passes must be inserted *first* and *before*
  the others read them — leave the slot documented.)
- Insert in **2000-row chunks** via `store.insert_edges`.
- Every pass is wrapped: a failing pass logs and contributes 0 edges, **never** fails the
  index. Report the total as `stats.by_method["callback-synthesis"]`.
- **Runs on the full-index path only.** Incremental sync does **not** re-run synthesis —
  this is a **known, inherited gap** (callback-edge-synthesis.md "Remaining work #2").
  Record it in `lib.rs`'s deferrals ledger explicitly; do not silently inherit it.
- Every edge: `provenance: Heuristic`, `metadata.synthesizedBy = pass.name()`.

**`selene-db` addition (required — `get_nodes_by_kind() -> Vec<Node>` will OOM):**
```rust
/// Stream nodes of a kind in id order, in pages. O(1) memory (TS #610: materializing
/// all method nodes on a large repo OOM'd). `after` = the last id of the previous page.
fn nodes_by_kind_page(&self, kind: NodeKind, after: Option<&str>, limit: usize)
    -> impl Future<Output = Result<Vec<Node>>> + Send;
```
Add it to the `GraphStore` trait + `SurrealStore` (a `SELECT … WHERE kind = $k AND id > $after
ORDER BY id LIMIT $n` — id order makes paging stable and output deterministic). Provide a
`synth::stream_nodes_by_kind(store, kind)` helper that pages it.

**`src/synth/lineindex.rs`:**
```rust
pub struct LineIndex { /* Vec<usize> of newline byte offsets */ }
impl LineIndex {
    pub fn new(src: &str) -> Self;             // ONE pass per file
    pub fn line_at(&self, byte_offset: usize) -> u32;   // 1-based, binary search
}
```
Every pass and every framework extractor uses this. **Never** `src[..off].split('\n').count()`
per match — that is O(n²) and was TS #1235.

- [ ] TDD: `tests/synth_harness_test.rs` — (a) two stub passes both emitting the same
  `(source, target)` with different kinds → exactly **one** edge survives, from the
  **first** pass in the order; (b) a pass whose `languages()` excludes the repo's languages
  is **never invoked** (assert via a call counter); (c) a panicking pass yields 0 edges and
  `run_synthesis` still returns `Ok`; (d) 4500 edges insert in 3 chunks; (e) determinism —
  run `run_synthesis` twice on the same store, assert the emitted edge vec is
  **byte-identical** (this is the test that catches a `HashMap` sneaking in).
- [ ] TDD `LineIndex`: property test — for 1000 random offsets in a multi-line, multi-byte
  (UTF-8) source, `line_at(o)` equals the naive count. Assert offsets **on** a `\n`.
- [ ] Implement `nodes_by_kind_page` in `selene-db` (+ its own test: paging over 250 nodes
  with limit 100 yields 3 pages, no duplicates, no gaps, stable order).
- [ ] Implement `run_synthesis`. Wire it into Part A's resolver tail (the equivalent of
  `resolveAndPersistBatched`), after base edges are persisted.
- [ ] Commit: `feat(resolve): synthesizer harness — SynthPass, streaming, dedupe, chunked insert`

---

### Task 22: Synthesizer 1/5 — callback / field-observer channels

**Files:** Create: `src/synth/callback.rs`. Tests: `tests/synth_callback_test.rs`,
`tests/fixtures/dispatch/callback/`.

**The hole** (callback-edge-synthesis.md §The hole):
```ts
class Scene {
  private callbacks = new Set<Callback>();
  onUpdate(cb) { this.callbacks.add(cb); }          // REGISTRAR
  triggerUpdate() { for (const cb of this.callbacks) cb(); }  // DISPATCHER
}
this.scene.onUpdate(this.triggerRender);            // REGISTRATION SITE
```
`triggerUpdate → triggerRender` exists at runtime and **not** in the AST: `cb()` is
anonymous. This is why it is a whole-graph pass and not a `resolve()` — there is **no named
ref to resolve**, and the correlation is cross-file (registrar, registration site,
dispatcher are three different places).

**Flow closed ⇔** `mutateElement → … → triggerUpdate → triggerRender → render`. The
synthesized edge is `triggerUpdate → triggerRender`; the flow is only *closed* if a path
exists from the **mutation entry point** all the way to the **render body**. A fixture that
asserts only the one synthesized edge does not prove the flow — assert the path.

**Algorithm (as-built; the divergences below are contract, not bugs):**
1. **Candidates by name**, streaming `method` **and** `function` nodes via
   `stream_nodes_by_kind` (never materialize them all — #610):
   - **Registrar:** name matches `^(on[A-Z]\w*|subscribe|addListener|addEventListener|register|watch|listen|addCallback)$`
     **AND** body contains `this\.(\w+)\.(?:add|push|set)\(` → captures the field `F`.
   - **Dispatcher:** name matches `(emit|trigger|notify|dispatch|fire|publish|flush)` (case-insensitive,
     substring) **AND** body contains `\bof\s+(?:Array\.from\(\s*)?this\.(\w+)` plus some call,
     **or** `this\.(\w+)\.forEach\(`.
2. **Pair registrar ↔ dispatcher by SAME FILE + SAME FIELD `F`.** *(Divergence, deliberate:
   the design said pair by class; the build uses file-as-a-class-proxy. Keep it — it is
   what the fixtures and the id contract were validated against. Multi-class files
   over-pair; accepted.)*
3. **Recover the registered callback:** for each **incoming `calls` edge to the registrar**
   (`store.incoming(registrar.id, [Calls])`), read the caller's **source line at the edge's
   `line`** and regex `{registrarName}\s*\(\s*(?:this\.)?(\w+)` to recover the argument
   name. *(Divergence: regex, not a tree-sitter re-parse. Named args only — arrows/inline
   args are missed here **by design**; see Task 23's note on the anonymous frontier.)*
4. **Resolve the arg** by name to a `method` | `function` node. **Ambiguous ⇒ skip**
   (`candidates.len() != 1 → continue`).
5. **Emit** `dispatcher → callback`, `EdgeKind::Calls`, `provenance: Heuristic`,
   `line = dispatcher.start_line`, metadata:
   ```json
   { "synthesizedBy": "callback", "via": "<registrarName>", "field": "<F>",
     "registeredAt": "<callerFile>:<line>" }
   ```
6. **Cap: `MAX_CALLBACKS_PER_CHANNEL = 40`** per registrar.

**Language gate:** `languages()` — the OO-observer shape is language-agnostic in TS, but the
body regexes are `this.`-based. v0 gate: `[Typescript, Tsx, Javascript, Jsx]`. Record that a
broader gate is a Phase 8 question (Java/C# `this.` works too, but was never validated).

- [ ] TDD **end-to-end first** (`tests/fixtures/dispatch/callback/`): port the excalidraw
  shape — `scene.ts` (`Scene.onUpdate` / `Scene.triggerUpdate`), `app.ts`
  (`constructor` does `this.scene.onUpdate(this.triggerRender)`; `triggerRender` calls
  `renderScene`; `mutateElement` calls `this.scene.triggerUpdate()`). Assert
  `find_path(mutateElement, renderScene)` connects **through** `triggerRender`, and that the
  bridging edge carries `synthesizedBy: "callback"`, `via: "onUpdate"`, `field: "callbacks"`,
  and a `registeredAt` of `app.ts:<the constructor line>`.
- [ ] TDD **precision (the 0-control)**: a fixture with a registrar-named method
  (`onThing`) but **no** dispatcher, and a dispatcher with **no** registrar → **0** edges.
  A repo with none of the shape must synthesize nothing.
- [ ] TDD the cap: 45 registrations on one channel → exactly 40 edges, deterministically the
  same 40 (sort registrations by `(file, line)` before truncating — the TS build's order was
  incidental; **make it explicit** and note the deviation).
- [ ] Implement. Body text via `ctx.read_file` + the node's line range; `LineIndex` for
  offsets. Pre-gate the expensive regexes with cheap `contains()` checks (`"this."`).
- [ ] Commit: `feat(resolve): callback/field-observer dispatch synthesizer`

---

### Task 23: Synthesizer 2/5 — EventEmitter (string-keyed) channels

**Files:** Create: `src/synth/event_emitter.rs`. Tests: `tests/synth_event_test.rs`,
`tests/fixtures/dispatch/event/`.

**The hole:** `emitter.emit('mount')` in one file, `emitter.on('mount', function onmount(){})`
in another. The correlation key is a **string literal**, invisible to the AST.

**Flow closed ⇔** the function containing the `emit('e')` → the **named handler** registered
for `'e'` → the handler's own callees. Canonical: express `use → onmount → …`.

**Algorithm (file-oriented scan, NOT node-oriented):**
1. **Pre-gate** each file with cheap `contains()` before any regex:
   emit side — `.emit(` | `.fire(` | `.dispatchEvent(`; on side — `.on(` | `.once(` |
   `.addListener(`. (TS #1235: an ungated scan cost 20+ minutes on PHP/JS corpora.)
2. `EMIT_RE = \.(?:emit|fire|dispatchEvent)\(\s*['"]([^'"]+)['"]`
   → **dispatcher** = the **tightest enclosing** `method`|`function`|`component` node
   containing the emit line (`enclosing_fn(file, line)`).
3. `ON_RE = \.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:function\s+(\w+)|(?:this\.)?(\w+))`
   → **NAMED HANDLERS ONLY.** `on('e', () => …)` and `on('e', function(){})` match nothing
   and that is **deliberate** — the anonymous-arrow handler is the known frontier
   (callback-edge-synthesis.md "Remaining work #1"). Resolve the handler name to a
   `function` | `method` node.
4. **Correlate by the event-name literal.**
5. **`EVENT_FANOUT_CAP = 6`:** if an event has **> 6 dispatchers OR > 6 handlers**, skip the
   event **entirely**. This is the precision guard that replaces type inference — generic
   names (`error`, `change`, `data`) would otherwise over-link catastrophically.
6. **Emit** dispatcher → handler, `EdgeKind::Calls`, metadata:
   ```json
   { "synthesizedBy": "event-emitter", "event": "<e>",
     "registeredAt": "<file>:<line-of-the-ON-site>" }
   ```
   **No `line` field on the edge** (the map is explicit; do not add one).

**Language gate:** `[Typescript, Tsx, Javascript, Jsx]`.

**Dependency on extraction:** named *inline* handlers (`on('e', function onmount(){})`) must
already be **nodes** — Phase 2's walker extracts named nested functions (TS "Phase 3" of the
callback work). If the fixture below shows `onmount` is not a node, that is a
**selene-extract bug**, not a synthesizer bug: fix it there (named-only; anonymous arrows
must still fall through so their inner calls stay attributed to the enclosing fn).

- [ ] TDD **end-to-end first**: `bus.ts` (an emitter class), `app.ts` with
  `bus.on('mount', function onmount(){ initApp() })` and a `use()` method calling
  `bus.emit('mount')`, plus `initApp` in a third file. Assert `find_path(use, initApp)`
  connects through `onmount` and the bridging edge has `synthesizedBy: "event-emitter"`,
  `event: "mount"`, and a `registeredAt` pointing at the **`on(` site**, not the emit site.
- [ ] TDD the fan-out cap: 7 handlers on `'error'` → **0** edges for `'error'`, while a
  6-handler event in the same fixture still bridges. (Boundary: exactly 6 → edges; 7 → none.)
- [ ] TDD the named-only rule: `bus.on('tick', () => refresh())` → **0** edges. Assert this
  explicitly with a comment naming it a deliberate frontier, so a future contributor doesn't
  "fix" it by linking to the enclosing function (that would be a **wrong** edge —
  silent beats wrong).
- [ ] TDD the 0-control: a repo with `.on(` but no `.emit(` → 0 edges.
- [ ] Commit: `feat(resolve): event-emitter dispatch synthesizer`

---

### Task 24: Synthesizer 3/5 — React re-render (`setState` → `render`)

**Files:** Create: `src/synth/react.rs` (the `react-render` pass). Tests:
`tests/synth_react_render_test.rs`.

> **⛔ SHIPPING GATE — read before starting.** This pass **must not reach `main` alone.**
> Tasks 33 and 34 are ONE mergeable unit. The map records the measurement: shipping
> `react-render` **without** the `jsx-render` hop **measurably RAISED agent reads** — the
> half-bridged flow (`handleClick → render`, then nothing) advertises a hop the agent must
> Read to finish. That is the invariant's worked example. Implement 33, do **not** register
> it in the pass list until 34 is green, and land both in one branch.

**The hole:** `this.setState({...})` triggers React's reconciler, which calls `render()`.
No static call exists.

**Flow closed ⇔ (jointly with Task 25)**
`handleClick → render → <StaticCanvas/> → renderStaticScene`. Task 24 supplies hop 2;
Task 25 supplies hop 3. Neither is a flow on its own.

**Algorithm:**
1. For each `class` node (stream them): children = the targets of its `contains` edges with
   kind `method`.
2. **Require a child named `render`.** No `render` ⇒ skip the class entirely.
3. For **every other sibling method** whose body matches `this\.setState\s*\(`:
   emit `sibling → render`, `EdgeKind::Calls`, `line = sibling.start_line`, metadata:
   ```json
   { "synthesizedBy": "react-render", "via": "setState",
     "registeredAt": "<renderFile>:<render.start_line>" }
   ```
4. **Cap 40 per class.**
5. **Over-approximation is ACCEPTED** (a `setState` in a rarely-taken branch still links).
   The model is *reachability*, not instance precision — do not add guards that trade recall
   for a precision the product doesn't need.

**Language gate:** `[Typescript, Tsx, Javascript, Jsx]`.

- [ ] TDD (unit, this task): a class fixture with `render`, `handleClick` (calls
  `this.setState`), and `helper` (no setState). Assert exactly **one** edge:
  `handleClick → render`, with the metadata above. Assert `helper → render` does **not**
  exist, and that a class **without** a `render` method yields 0 edges.
- [ ] TDD the cap: 45 setState siblings → 40 edges, deterministic (sort siblings by
  `(start_line, name)` before truncating).
- [ ] Implement the pass but **leave it out of the pass registry** — add a
  `// REGISTERED IN TASK 34 — see the shipping gate` comment at the registration site.
  `cargo test` must be green with the pass dormant.
- [ ] Commit: `feat(resolve): react re-render synthesizer (dormant until jsx-child lands)`

---

### Task 25: Synthesizer 4/5 — JSX child (`<Child/>` → component) — **ships with Task 24**

**Files:** Modify: `src/synth/react.rs` (add the `jsx-render` pass; **register both passes**).
Tests: `tests/synth_jsx_test.rs`, `tests/fixtures/dispatch/react-render/`.

**The hole:** `<StaticCanvas .../>` inside `render()` is a **call** to that component's
render. Tree-sitter sees JSX elements, not calls.

**Flow closed ⇔** `handleClick → render → StaticCanvas → renderStaticScene` — the **whole**
chain, using Task 24's hop 2 and this task's hop 3. **This task owns the end-to-end fixture
for both.**

**Algorithm:**
1. Gate: files containing `</` **or** `/>` (cheap `contains` pre-gate).
2. For each `method` | `function` | `component` node whose **body slice** also contains a
   JSX marker: collect tag names with `<([A-Z][A-Za-z0-9_]*)[\s/>]` (capital initial =
   component, per JSX semantics; lowercase = a DOM tag, correctly ignored).
3. Resolve each name to a `component` | `function` | `class` node. **Ambiguous ⇒ skip.**
4. Emit parent → child, `EdgeKind::Calls`, `line = parent.start_line`, metadata:
   ```json
   { "synthesizedBy": "jsx-render", "via": "<TagName>" }
   ```
   **NO `registeredAt`** for this pass (the map is explicit — there is no wiring site; the
   JSX element *is* the call).
5. **Cap `MAX_JSX_CHILDREN = 30` per parent.**

**Language gate:** `[Typescript, Tsx, Javascript, Jsx]`.

- [ ] TDD **end-to-end first — this is the gate for Tasks 33 AND 34**
  (`tests/fixtures/dispatch/react-render/`): `App.tsx` — `class App` with `handleClick`
  (`this.setState`), `render()` returning `<div><StaticCanvas scene={s}/></div>`;
  `StaticCanvas.tsx` — a component whose body calls `renderStaticScene()`;
  `renderer.ts` — `renderStaticScene`. Assert **`find_path(App.handleClick,
  renderStaticScene)` connects** (3 synthesized/static hops). Then assert the two bridging
  edges' metadata (`react-render` with `via: "setState"`; `jsx-render` with
  `via: "StaticCanvas"` and **no** `registeredAt` key).
- [ ] TDD: lowercase tags (`<div>`) produce no edges; an unresolvable tag produces no edge;
  cap at 30 children (35 distinct tags → 30 edges, deterministic — sort tag names).
- [ ] **Register both passes** in the Task-30 pass list, in the order
  `…, react-render, jsx-render`, and delete Task 24's dormancy comment.
- [ ] Re-run the Task 24 unit tests — they must still pass with the pass live.
- [ ] Commit: `feat(resolve): jsx-child synthesizer + activate the react dispatch pair`

---

### Task 26: Synthesizer 5/5 — Django ORM descriptor (resolver-mechanism, `claimsReference`)

**Files:** Modify: `src/frameworks/python.rs` (the Django resolver) — **sequential after
Tasks 23/24**. Tests: `tests/synth_django_orm_test.rs`,
`tests/fixtures/dispatch/django-orm/`.

> **⚠ It is called a "synthesizer" in the roadmap, but it is NOT a synthesizer pass.** It is
> a **framework resolver** branch. This asymmetry is the playbook's central mechanism lesson
> (§2, §3a) and must be preserved, not "cleaned up":
>
> | The ref is… | Mechanism | Provenance of the resulting edge |
> |---|---|---|
> | **named** (`self._iterable_class(self)` — `_iterable_class` is an attribute name) | `claims_reference` + `resolve()` | ordinary resolved edge, **`tree-sitter`**, `resolved_by: framework` |
> | **anonymous** (`cb()`, `<Child/>`, `emit('e')`) | whole-graph synthesizer pass | **`heuristic`** + `synthesizedBy` |
>
> So this task emits **NO heuristic edge and NO `synthesizedBy`**. A reviewer expecting one
> should read this box. Record the asymmetry in `lib.rs`'s ledger.

**The hole:** `QuerySet._fetch_all` calls `self._iterable_class(self)` — a runtime-chosen
iterable class (default `ModelIterable`) whose `__iter__` runs the SQL compiler. Statically,
`_fetch_all`'s only callee was `_prefetch_related_objects`; the query→SQL flow did not exist.

**Flow closed ⇔** `QuerySet._fetch_all → ModelIterable.__iter__ → SQLCompiler.execute_sql`
— a **3-hop** path. Hop 1 is this task's edge; hops 2+ are ordinary static calls **inside**
`__iter__`. The fixture must assert the path to `execute_sql`, not just the one edge.

**Algorithm:**
1. `claims_reference("_iterable_class") == true` — this is the **whole trick**: the ref names
   no declared symbol in the calling file, so Part A's pre-filter would drop it before
   `resolve()` ran. (Same hook Rails/Laravel need — Task 19.)
2. `resolve()` branch for the name `_iterable_class`:
   - find the class node named **`ModelIterable`**;
   - find the `__iter__` **method whose `start_line` falls within that class's line range in
     the same file** (the class-membership test — do not just take any `__iter__`);
   - return it with confidence **0.7**.
3. Ambiguity (no `ModelIterable`, or no in-range `__iter__`) ⇒ `None`. Not a Django repo ⇒
   the ref is simply unresolved. Silent beats wrong.

**Also claimed (from Task 14, restated):** `*.urls`.

- [ ] TDD **end-to-end first** (`tests/fixtures/dispatch/django-orm/`): a minimal
  `query.py` (`class QuerySet` with `_fetch_all` calling `self._iterable_class(self)`),
  `iterables.py` (`class ModelIterable` with `__iter__` calling `compiler.execute_sql()`),
  `compiler.py` (`class SQLCompiler` with `execute_sql`). Assert
  **`find_path(QuerySet._fetch_all, SQLCompiler.execute_sql)` returns a 3-hop path**.
- [ ] TDD the mechanism contract: assert the `_fetch_all → __iter__` edge has
  `provenance == TreeSitter` and **no** `synthesizedBy` in its metadata (it is a resolved
  ref, not a synthesized edge). A test that asserts `Heuristic` here is asserting the wrong
  contract.
- [ ] TDD the pre-filter: a test proving `resolve()` is reached for `_iterable_class` even
  though no symbol of that name is declared anywhere (i.e. `claims_reference` is consulted).
- [ ] TDD the 0-control: a Python repo with an `_iterable_class` attribute but **no**
  `ModelIterable` class → the ref stays unresolved, **no** edge.
- [ ] Commit: `feat(resolve): django ORM descriptor — _iterable_class → ModelIterable.__iter__`

---

### Task 27: Phase 3 gate — end-to-end dispatch-coverage fixture corpus + gate test

**Files:** Create: `tests/dispatch_gate.rs`,
`tests/fixtures/dispatch/expected_flows.toml`. Modify: `src/lib.rs` (the final ledger),
`docs/benchmarks/2026-07-phase3-dispatch-coverage.md`.

**This is the roadmap's Phase 3 gate:** *"dispatch-coverage fixtures resolve end-to-end (no
half-bridged flow)."* Nothing in Phase 3 is done until this is green.

**What it does:** one table-driven test over every fixture built in Tasks 21–35. Each row is
a **flow**, not an edge:

```toml
# tests/fixtures/dispatch/expected_flows.toml
# Entry points are addressed SEMANTICALLY (framework+method+path), never by id string —
# route ids are opaque hashes (Task 11).
[[flow]]
fixture     = "express"
from_route  = { framework = "express", method = "POST", path = "/users/login" }
to          = "hashPassword"          # the terminal the agent would otherwise Read for
max_hops    = 4
via         = ["login"]               # symbols that MUST appear on the path
[[flow]]
fixture     = "react-render"
from_symbol = "App.handleClick"       # non-route entry points use from_symbol
to          = "renderStaticScene"
max_hops    = 4
via         = ["render", "StaticCanvas"]
# … one row per framework (21–29) and per synthesizer (31–35)
```

The test, for each row: build a temp `SurrealStore` → index the fixture dir with
`selene-extract` → run the Part A resolver + `run_synthesis` → resolve the entry point
(`from_route` via the indexed `find_route(framework, method, path)`; `from_symbol` by name)
→ assert `store.find_path(from, to)` returns a path of `≤ max_hops` that **contains every
`via` symbol in order**. A missing `via` symbol means the flow was bridged around a hop
rather than through it — that is a *silently wrong* map and must fail the gate.
An entry point that `find_route` cannot locate is a **gate failure**, not a skip.

- [ ] Build the row table. **Every** framework task (21–29) and **every** synthesizer task
  (31–35) contributes **at least one** row. A framework with no row is a framework whose
  flow was never proven end-to-end — the gate fails on an empty row for it.
- [ ] **Same-line multi-route gate** (the hashed-id consequence from Task 11): the axum
  chained-verb fixture (`get(h).post(h2)`, one line) and the rails `resources :articles`
  fixture (7 actions, one line) must each yield **N distinct route node ids**, and each
  route must be independently reachable via `find_route`. A collision here silently deletes
  routes — assert the count, not just the lookup.
- [ ] **Precision gate (the 0-control corpus).** A `tests/fixtures/dispatch/_control/`
  directory: a plain repo per language with none of the dispatch shapes. Assert
  `run_synthesis` emits **exactly 0 edges** on it. (Playbook §5.2: "0 on every non-Swift
  control" — the closure-collection pass's proof of precision. A synthesizer that fires on
  the control is over-linking and poisons the map.)
- [ ] **No-explosion gate.** For each fixture, record node + edge counts in
  `expected_flows.toml` and assert they are stable (an extraction/resolution change that
  balloons counts means something over-fired). Deltas require a deliberate update + a note.
- [ ] **Determinism gate.** Index each fixture **twice** into two fresh stores; assert the
  full edge set (source, target, kind, metadata) is **identical**, including order.
- [ ] Write `docs/benchmarks/2026-07-phase3-dispatch-coverage.md`: the coverage matrix
  (framework/synthesizer × flow × status), copied from the playbook §6 format, filled in for
  v0 only, with the deferred frontiers listed explicitly (Gin middleware chain, anonymous
  arrow handlers, lazy React data-routers, actix `web::scope` prefix, EF Core, Eloquent/
  ActiveRecord dynamic finders, incremental-sync re-synthesis).
- [ ] Final `src/lib.rs` ledger: public interface, the v0 framework list, the 4 synthesized
  `synthesizedBy` values shipped (`callback`, `event-emitter`, `react-render`, `jsx-render`),
  the metadata key contract, and every deferral above.
- [ ] Commit: `test(resolve): phase-3 dispatch-coverage gate — end-to-end flows, 0-control, determinism`

---

## Open questions for the maintainer

1. **Cargo workspace ownership (Task 18).** Part A's brief also lists "cargo workspace globs"
   under the import resolver. This plan puts the crate map in
   `src/frameworks/cargo.rs` and exposes `cargo_workspace_crate_map(ctx)` for Part A's
   import resolver to call. **Confirm the direction** (framework owns it, import consumes)
   or flip it — either works, but only one may exist.
2. **`selene-db` trait change (Task 21).** The synthesizers need
   `nodes_by_kind_page(kind, after, limit)` on `GraphStore`; today only
   `get_nodes_by_kind() -> Vec<Node>` exists, which OOMs on large repos (the TS #610
   incident). This is a Phase-1 crate change inside Phase 3 — **confirm it is in scope**
   (the PRD §5.4 note anticipated exactly this: "budget those primitives before freezing
   the trait").
## Maintainer decisions — RESOLVED (2026-07-13), already folded into the tasks above

3. ~~**Route-node ids are not `node_id()`-hashed.**~~ **RESOLVED — redesigned.** Route ids
   stay **hashed** like every other node (no new id exception). Route semantics move into
   **first-class indexed fields** (`routeMethod`, `routePath`, `framework`; `file`/`line`
   already exist on `Node`), and every downstream lookup is an **indexed SurrealQL query**
   via `find_route(framework, method, path)` — never id-string parsing. Folded into: the
   Task 11 contract (+ its `selene-core` `Node` fields, `selene-db` `DEFINE FIELD`/`DEFINE
   INDEX` step, and `EXTRACTION_VERSION` bump), the Global Constraints, every fixture
   assertion in Tasks 21–29, and Task 27's `expected_flows.toml`.
   **Consequence on record (new, load-bearing):** the id hash input is
   `(file, kind, name, start_line)`, so routes emitted from the **same line** (axum
   `get(h).post(h2)`; rails `resources` → 7 actions; stacked flask decorators) are now
   separated **only by `name`**. The `{METHOD} {path}` name spelling is therefore no longer
   cosmetic — it is the uniqueness key. Task 11 and Task 27 both carry an explicit
   distinct-id assertion for the same-line case; a framework author who names a route by
   path alone will silently drop routes.
   **Relayed to Part C:** parity must compare **semantic identity**
   (`framework, method, path, file, line`), never raw id spelling — TS ids are literal
   strings, ours are hashes, so a byte-diff of route ids is meaningless.
4. ~~**Next.js file routes deferred?**~~ **RESOLVED — IN scope for Phase 3.** Folded into
   Task 13 (`pages/` + `app/page.*` file routes, `[x]` → `:x`), with its own end-to-end
   fixture. **One deviation on record:** TS matched `filePath.includes('page.')`, which also
   matches `mypage.tsx`; we match the **basename** `^page\.(tsx?|jsx?)$` instead. It is a
   bug fix, so route counts may legitimately differ from the TS build — Part C should not
   treat that delta as a parity failure.
5. ~~**Config languages for Spring.**~~ **RESOLVED — authorized.** `Yaml` + `Properties`
   added to `selene-extract`'s `Language` enum as **file-level-only** languages. Folded into
   Task 16 as an explicit first step, with a sequencing note (it is the **only** Part-B task
   touching `selene-extract/src/language.rs`; Task 11 touches `orchestrator.rs`, so no
   Part-B collision — the controller must still check it against Part A's dispatch).

## Open questions still outstanding

1. **Cargo workspace ownership (Task 18).** Part A's brief also lists "cargo workspace globs"
   under the import resolver. This plan puts the crate map in
   `src/frameworks/cargo.rs` and exposes `cargo_workspace_crate_map(ctx)` for Part A's
   import resolver to call. **Confirm the direction** (framework owns it, import consumes)
   or flip it — either works, but only one may exist.
2. **`selene-db` trait change (Task 21).** The synthesizers need
   `nodes_by_kind_page(kind, after, limit)` on `GraphStore`; today only
   `get_nodes_by_kind() -> Vec<Node>` exists, which OOMs on large repos (the TS #610
   incident). This is a Phase-1 crate change inside Phase 3 — **confirm it is in scope**
   (the PRD §5.4 note anticipated exactly this: "budget those primitives before freezing
   the trait"). *Note: decision 3 above already opens `selene-db` for the route fields +
   indexes, so the crate is being touched in Phase 3 regardless — this is now a smaller ask.*
3. **`claims_reference` is a Part-A dependency.** Tasks 28 (Rails/Laravel) and 35 (Django
   ORM) are **inert without it** — the ref is dropped by the pre-filter before `resolve()`
   is ever called (the exact TS-build gotcha). If Part A did not specify the hook, three
   framework bridges silently resolve to nothing. Confirm at assembly.



---

# Phase 3 — Part C: the resolution parity gate + the facade

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Scope of this part.** Part A (Tasks 1–10) builds the resolver core: the `ResolutionContext`
seam, the `resolve_one` ladder, import resolution, the name matcher, chained calls, function
refs, `create_edges`. Part B (Tasks 20–36) builds the framework resolvers and the dispatch
synthesizers. **Part C builds the pipeline that drives them, and the gates that decide
whether any of it is real**: the batched persist + pass driver (`src/batch.rs` — which *both*
Part A and Part B assign to Part C, and neither writes), the TS↔Rust resolution parity gate,
the dispatch-coverage flow gate, the results doc, and the `selene-resolve` facade. Tasks are
numbered from **40** (the controller renumbers at assembly).

**The phase gate (roadmap, Phase 3):** *"dispatch-coverage fixtures resolve end-to-end (no
half-bridged flow)."* **Task 32** is that gate, literally. Tasks 41–43 are the parity gate that
keeps the rest of the resolver honest while Task 32 keeps the *flows* honest — they measure
different failures and neither substitutes for the other (§ *Why two gates*, below).

---

## Reconciliation with Parts A and B (read: what changed after they landed)

Parts A and B now exist (`phase3-partA-core.md`, `phase3-partB-frameworks.md`). This part is
reconciled against them: the symbol names below are **theirs**, not assumptions.

### Interfaces this part consumes (real spellings)

```rust
// Part A — Task 2/3
pub trait ResolutionContext: Send + Sync { /* root, file_exists, read_file, all_files, … */ }
pub struct StoreContext<S: GraphStore> { /* … */ }          // the GraphStore-backed impl
pub struct ReferenceResolver<C: ResolutionContext> { /* … */ }
impl ReferenceResolver<C> {
    pub fn new(ctx: C) -> Self;
    pub fn resolve_one(&mut self, r: &UnresolvedRef) -> Option<ResolvedRef>;
    pub fn create_edges(&self, resolved: &[ResolvedRef]) -> Vec<Edge>;
    pub fn detected_frameworks(&self) -> Vec<String>;        // ← satisfies requirement (1) below
}
pub struct ResolutionStats { pub total, pub resolved, pub unresolved, pub by_method }
pub enum ResolvedBy { ExactMatch, Import, QualifiedName, Framework, Fuzzy,
                      InstanceMethod, FilePath, FunctionRef }   // + as_str()

// Part B — Tasks 20/30
pub fn all_framework_resolvers() -> &'static [&'static dyn FrameworkResolver];  // .name() each
pub fn detect_frameworks(ctx: &dyn ResolutionContext) -> Vec<&'static dyn FrameworkResolver>;
pub trait SynthPass: Send + Sync { /* … */ }
pub async fn run_synthesis<S: GraphStore>(store: &S, ctx: &dyn ResolutionContext) -> Result<usize>;
```

**Two of my three assumed introspection surfaces already exist**:
`ReferenceResolver::detected_frameworks()` (Part A, Task 3) and `all_framework_resolvers()`
(Part B, Task 20) — good.

### ⚠ The one interface still missing — Parts A/B must add it

**`selene_resolve::synth::registered_synthesizers() -> &'static [&'static str]`** — every
`synthesizedBy` value the `SynthPass` registry can emit (v0: `callback`, `event-emitter`,
`react-render`, `jsx-render`). Part B's Task 30 defines `SynthPass` and `run_synthesis()`
but exposes **no way to enumerate the registered passes by name**.

Without it, Task 32's `every_framework_and_synthesizer_has_a_flow` cannot be written — and
that assertion is the only thing standing between "we shipped a fifth dispatch channel" and
"we shipped a fifth dispatch channel gated by nobody". It is a two-line addition to Part B's
Task 30 (`fn synthesized_by(&self) -> &'static str` on the trait — which each pass already
needs for its metadata stamp — plus a registry map over it). **Requested of the controller;
do not work around it by hard-coding the list in the test — a hard-coded list is a second
source of truth and it will drift on the first Phase 8 channel.**

### Three maintainer decisions folded in

1. **Route node ids are HASHED, not TS's literal strings.** Route nodes carry the Phase 2
   id contract like every other node (`"<kind>:" + sha256(...)[..32]`, no exception), and
   the route's semantics live in **first-class indexed fields**: `method`, `path`, `file`,
   `line`, `framework` — queried via indexed SurrealQL, never parsed out of an id string.
   ⇒ **The parity gate compares route-anchored edges SEMANTICALLY** (Task 31). ⚠ **This
   contradicts Part B's Task 20 as currently written** (its route-node table specifies the
   literal `route:{file}:{line}:{METHOD}:{path}` ids, and its open question #3 asks for
   exactly this decision). **Part B Task 20 must be rewritten** — flagged to the controller.
2. **Next.js file routes are IN Phase 3** (Part B's open question #4 answered: not deferred).
   ⇒ a `fw-nextjs` fixture project (Task 29) and a Next.js flow (Task 32).
3. **The Spring config bridge is IN**: `Yaml`/`Properties` become file-level-only languages
   so `application.yml` → `@Value("${k}")` resolves. ⇒ the Spring flow **must traverse a
   config-key hop** (Task 32) — a cross-*language* hop, and precisely the kind that
   half-bridges silently (the code side resolves, the config side dead-ends, and nothing in
   a same-language gate notices).

### Two collisions with Part B the controller must resolve

- **Part B's Task 36 ("Phase 3 gate — dispatch-coverage fixture corpus + gate test")
  duplicates this part's Tasks 41/44.** Recommendation: **C's Task 32 supersedes B36's gate
  test**, and B36's fixture-building step folds into C's Task 29. Reason: B36 asserts
  `store.find_path(from, to)` returns a path ≤ `max_hops` containing `via` *symbols* — that
  is a **path-existence** check, and a path search is satisfied by *any* route through the
  graph, including one that goes around the very dispatch hop the gate exists to defend. It
  also cannot see a hop closed by the **wrong mechanism** (a fuzzy match that lands right by
  luck). Task 32 asserts **each hop's edge individually, with its `via` mechanism pinned**.
  **B36's three other sub-gates are excellent and are absorbed into Task 32 verbatim**: the
  0-control corpus (synthesis emits exactly 0 edges on a repo with none of the shapes), the
  no-explosion count check, and the determinism check. Nothing is lost by the supersession.
- **`src/batch.rs` — the batched persist + pass driver — is assigned to Part C by both
  A and B, and written by neither.** Part A's file-structure table marks it `[C]`; Part A's
  seam table lists "**C** (batching/passes wiring)" as a `resolver.rs` toucher; Part B's Task
  30 says `run_synthesis` "wires into Part A's resolver tail (the equivalent of
  `resolveAndPersistBatched`)" — which is that file. **So Part C writes it (Task 28).**
  Without it the phase has a resolver, frameworks and synthesizers, and no pipeline to run
  them: `resolve_and_persist_batched` is where the pass ordering, the 5000-row batch loop,
  the keyed delete, the non-progress guard and the synthesis tail all live. If the controller
  intended someone else to own it, move Task 28 — but it cannot be nobody's.

### What the gate is keyed to

**Whatever Part B actually ships is what Task 32 gates.** The flow table is keyed off the
*registries* (`all_framework_resolvers()`, `registered_synthesizers()`), not off any list in
any plan — if Part B ships one framework fewer or one more, the completeness assertion fails
until the flow table and the registry agree. A plan is a hypothesis; the registry is the fact.

---

## Why two gates, and what each one cannot see

Phase 2 taught this the hard way. Its gate compared **counts** and stayed GREEN while:
class inheritance was unwired in *every* language; Ruby calls were truncated to their
receiver (`calls:@db.query` → `calls:@db` — *identical counts*); Python emitted a phantom
edge that a unit test had PINNED as correct; PHP imports lacked the only spelling that
resolves. All four surfaced only when the gate compared **names**, and when the corpus was
extended to actually exercise the constructs.

Resolution's version of each failure is strictly worse, because resolution's output is
*bindings*:

| Failure | What a count gate sees | What catches it |
|---|---|---|
| A ref binds to the **wrong target** (`UserService.save` → some other `save`) | nothing — the edge count is identical | **edge identity** (Task 31): `(source, target, kind, provenance)` |
| A ref binds to the right target **by the wrong strategy** (fuzzy 0.5 won where import 0.9 should have) — a pipeline-order regression that is invisible today and mis-binds tomorrow | nothing | `resolvedBy` + `confidence` in the identity/metadata tuple (Task 31) |
| A framework silently **fails to `detect()`** → no route nodes → no route edges → both sides compare empty sets | nothing (0 == 0) | `detected_frameworks_agree` + the dumper's refuse-to-write (Tasks 42/43) |
| A **flow resolves 3 hops of 4** — the agent gets a map that dead-ends, then reads to finish (the invariant says this is *worse than none*) | nothing: 3 real edges are 3 real edges, and they all match TS if TS is also broken | **per-hop flow assertions** (Task 32) |
| A synthesizer ships with **no fixture** and is gated by nobody | nothing | registry↔flow-table completeness (Task 32) |

So: **Task 31 asks "does Rust bind what TS binds?"** (a *differential* gate — it inherits
TS's correctness, and TS's bugs, which is why it needs `deviations.toml`). **Task 32 asks
"is the flow actually closed?"** (an *absolute* gate — it would fail even if Rust matched a
TS that was itself half-bridged). A parity gate alone would happily certify a faithful port
of a broken flow.

---

## Global constraints (carried from the roadmap + Phases 1–2)

- **Tolerance is 0**, on identity and on metadata. Resolution is deterministic over
  byte-identical inputs. Every difference is a bug (fix it) or a justified deviation (one
  machine-checked entry in `deviations.toml`, citing its TS evidence). A **stale entry
  FAILS the build**.
- Errors collected, never thrown; no `unwrap`/`expect` outside `#[cfg(test)]` (test files
  keep the Phase-2 `#![allow(clippy::unwrap_used, clippy::expect_used)]` header).
- Wire enums via `as_str()`; `resolvedBy` / `synthesizedBy` strings are the wire contract
  (`maps/resolution.md` §Wire, `maps/frameworks-synth.md` §Wire).
- Determinism: sort every dumped collection; the diff is a **multiset** diff in both
  directions (so a duplicate emitted once too often is caught, not just a rename).
- Every task: `cargo fmt && cargo clippy --all-targets && cargo test -p selene-resolve`
  green before its commit. TDD: write the failing assertion first.
- One conventional commit per task.

---

### Task 28: `src/batch.rs` — the batched persist + the pass driver

**Files:** Create `crates/selene-resolve/src/batch.rs`; modify `src/resolver.rs` (the
Part-A-stubbed wiring point), `src/lib.rs` (one `pub use`). Tests:
`crates/selene-resolve/tests/batch_test.rs`.

**Why this is here.** Parts A and B both assign this file to Part C and neither writes it
(see *Reconciliation* above). It is the pipeline: without it, `resolve_one` is a function
nobody calls, `run_synthesis` has no tail to hang off, and the gates of Tasks 43/44 have
nothing to drive. **It must land before them.**

**Interfaces** (`maps/resolution.md` §Pass ordering, §Batching):
```rust
pub const RESOLVE_BATCH: usize = 5000;      // the pending-row batch
pub const PERSIST_CHUNK: usize = 1000;      // the sub-transaction chunk
impl<C: ResolutionContext> ReferenceResolver<C> {
    pub fn resolve_all(&mut self, refs: &[UnresolvedRef]) -> ResolutionResult;
}
pub async fn resolve_and_persist_batched<S: GraphStore>(
    store: &S, root: &Path, on_progress: Option<&dyn Fn(usize, usize)>,
) -> Result<ResolutionStats>;
pub async fn resolve_and_persist<S: GraphStore>(          // the scoped/sync path (Phase 6 uses it)
    store: &S, root: &Path, refs: &[UnresolvedRef],
) -> Result<ResolutionStats>;
```

- [ ] **The pass order is the contract** (`maps/resolution.md` §Pass ordering) — port it as a
  fixed sequence, not a configurable pipeline: `initialize()` (framework detection against
  the *populated* index) → `run_post_extract()` → **`resolve_and_persist_batched()`** →
  `resolve_chained_calls_via_conformance()` (A9) → `resolve_deferred_this_member_refs()`
  (A10). The two conformance passes run **after** edges persist because they need
  `implements`/`extends` edges to already exist (#750/#808) — reordering them silently
  guts them, and nothing but a flow gate would notice.
- [ ] **The batch loop reads at offset 0, always.** Processed rows *leave* the pending set
  (resolved → deleted; unresolvable → `status='failed'`), so paging with an advancing offset
  would skip rows. Loop: `unresolved_refs_batch(0, RESOLVE_BATCH)` → `resolve_one` each →
  persist → repeat until empty.
- [ ] **The non-progress guard — do not omit it.** If the pending count after a batch is
  `>= prevRemaining`, **break**. This exists because a resolver that returns a *mutated*
  `original.reference_name` makes the keyed delete no-op, the row stays pending, and the loop
  re-resolves it forever: in TS this produced a **5M-edge / 1.4 GB runaway**. The guard is the
  only thing that catches it, and the Go bare-name chain fallback (A9) is the exact code path
  that triggered it — which is why A9's fallback must return the **original** ref as
  `.original`. Test both halves: a resolver that mutates the name must trip the guard, not hang.
- [ ] **Persistence per batch in `PERSIST_CHUNK`-sized sub-transactions**: `insert_edges(chunk)`
  → `delete_resolved(keys)` → `mark_failed(keys)`, where the key is
  `(from_node_id, reference_name, reference_kind)`. ⚠ **See Part A's open coordination point
  #1**: `GraphStore` currently keys by a 2-part `(from_node_id, reference_name)`, which can
  drain a `calls` ref and a `function_ref` of the same name from the same node together — a
  silent recall loss the pending-count sweep cannot detect. Part A's Task 1 measures it; **if
  it is real, this task is where the 3-part key lands** (a `selene-db` trait change).
- [ ] **The synthesis tail**: after base edges persist, call Part B's
  `run_synthesis(store, ctx)`, best-effort (a throwing pass degrades to a warning, never a
  failed index), and report the count as `stats.by_method["callback-synthesis"]` — the wire
  key `maps/resolution.md` §Wire names. Synthesis runs on the **batched (full-index) path
  only**; incremental sync does not re-run it (the known TS gap — record it in the facade's
  deferral list, Task 34, rather than silently inheriting it).
- [ ] **The async seam** (Part A's open coordination point #2): `ResolutionContext` is *sync*
  over an *async* `GraphStore`. Part A pins the shape and says **"Part C guarantees the
  wrapper"** — so guarantee it here: the resolver runs inside `spawn_blocking`, warm-caches
  first, and `block_on`s only there. **Running the resolver directly on a tokio worker
  deadlocks.** Put that sentence in the module doc, next to the wrapper.
- [ ] `cooperative-yield` is **dropped, not ported** (a Node event-loop artifact — the
  liveness-watchdog heartbeat, #850/#1091). The batching/streaming discipline it protected
  **is** kept: chunked inserts, streamed node iteration, never materializing unbounded kinds
  (#610/#1212 OOM). Say both in the module doc — the distinction is the whole point.
- [ ] TDD: `batch_test.rs` against `SurrealStore::in_memory()` — (a) a 3-file fixture resolves
  and the edges land in the **store** (not just the return value); (b) > `RESOLVE_BATCH` refs
  drain in multiple batches and every pending row is processed exactly once; (c) the
  non-progress guard trips on a name-mutating stub resolver instead of looping; (d) the pass
  order is observable: a conformance-only edge (a method found on a supertype) exists **only**
  when the conformance pass runs after persist; (e) determinism — two runs, identical edge set.
- [ ] Commit: `feat(resolve): batched persist + the resolution pass driver`

---

### Task 29: The resolution fixture corpus — project trees, not snippets

**Files:** Create `crates/selene-resolve/tests/fixtures/resolve/<project>/…` (project
trees), `crates/selene-resolve/tests/fixtures/resolve/projects.toml` (the manifest).

**Why project trees.** Extraction parity could use single files, because extraction is a
per-file function. **Resolution is cross-file by definition** — its entire output is edges
*between* files, and a framework only fires if its `detect()` finds `package.json` /
`manage.py` / `Cargo.toml` / `artisan` / `Gemfile` in a *project*. A corpus of loose
snippets would produce a baseline of zero cross-file edges and zero detected frameworks,
which both engines would reproduce, and the gate would be green forever having compared
nothing. **The fixture unit is a directory tree with its manifest files.**

**Provenance of the bytes.** Same rule as Phase 2: every fixture body is copied
**byte-for-byte** from the TS contract suites (`__tests__/resolution.test.ts`,
`__tests__/frameworks.test.ts`, `__tests__/frameworks-integration.test.ts`, and the
per-synthesizer suites), materialized to disk so **both engines consume identical bytes**.
Where a suite's inline snippet lacks the manifest file its framework detects on, add the
smallest possible one (a `package.json` with just the dep, an empty `manage.py`) and say so
in `projects.toml`'s `notes`.

- [ ] **Corpus layout.** One directory per project, named `<class>-<subject>`:

  | Class | Projects | What each must exercise |
  |---|---|---|
  | `core-*` | `core-ts`, `core-py`, `core-go`, `core-rust`, `core-java`, `core-php`, `core-ruby`, `core-csharp`, `core-cpp` | the ecosystem's **import resolution** (relative, aliased, package/module, re-export chain) + **one chained call** + **one ambiguous name** (two same-named symbols in different dirs — pins that we decline rather than guess, `maps/resolution.md` §Confidence) + **one negative control**: a chained call on a type that LACKS the method ⇒ **no edge** (the "validated inference" invariant; every TS chain block has this test and it is the one that catches a resolver that guesses) |
  | `fw-*` | `fw-express`, `fw-django`, `fw-flask`, `fw-fastapi`, `fw-spring`, `fw-gin`, `fw-axum`, `fw-aspnet`, `fw-laravel`, `fw-rails`, `fw-react`, **`fw-nextjs`** | the framework's **canonical flow** (route/entry → handler → one more hop into a service/model), with the manifest file its `detect()` keys on. Three carry extra weight: **`fw-axum`** also carries a **cargo workspace** (root `Cargo.toml` `[workspace] members`, two member crates) — the workspace-crate resolve is a 0.95-confidence path that beats the name-matcher and is otherwise untested; **`fw-spring`** also carries `src/main/resources/application.yml` + a `@Value("${…}")` bind (see below); **`fw-nextjs`** is a *file-routed* framework (no route call to regex — the route IS the path `pages/users/[id].tsx`), so it exercises a code path nothing else does |
  | `synth-*` | `synth-callback`, `synth-eventemitter`, `synth-react` (re-render **and** JSX child — see below), `synth-django-orm` | the synthesizer's registrar/dispatcher pair, plus **a caller upstream of the registrar and a callee downstream of the handler**, so the *chain* is longer than the synthesized hop. A fixture that contains only the bridged hop cannot distinguish "the hop works" from "the flow closes" |
  | `_control/` | one plain sub-repo per v0 language | **the precision corpus** (absorbed from Part B's Task 36): ordinary code containing **none** of the dispatch shapes. Synthesis must emit **exactly 0 edges** on it. Every positive assertion in this plan is satisfied by a synthesizer that bridges *everything*; only the control corpus fails it. Playbook §5.2 ("0 on every non-Swift control") is the closure-collection pass's proof of precision, and it is the cheapest precision test that exists |

- [ ] **`fw-spring` must carry the config bridge** (maintainer decision: `Yaml`/`Properties`
  are file-level-only languages in Phase 3). The project holds `application.yml` with a leaf
  key, a `@Value("${app.greeting}")` field, and a handler that reads it — so the flow in
  Task 32 traverses a **cross-language, code↔config hop**. This is the hop most likely to
  half-bridge in silence: the Java side resolves fine on its own, the config side simply
  dead-ends, and *nothing in a same-language gate notices*. Note in `projects.toml` that the
  yaml constant node must carry **no value** (secret redaction, #383) — Task 32 asserts it.
- [ ] **`fw-nextjs` must exercise both route flavors** (`pages/` and `app/page.tsx`), since
  the TS resolver's `app/` check is `filePath.includes('page.')` — looser than the `pages/`
  segment check, and it matches `mypage.tsx` (`maps/frameworks-synth.md` §Rust port notes
  records this as a real TS bug). Include a `mypage.tsx` **negative control**: if we port
  the bug, it emits a phantom route and the gate shows it; if we fix it, TS and Rust diverge
  and it becomes a **deviation with evidence**. Either way it is a decision, not an accident.

  `synth-react` deliberately holds **both** the re-render and the JSX-child channels in one
  project: `maps/frameworks-synth.md` records that shipping react-render *without* the JSX
  hop measurably **raised** agent reads — a half-bridged flow. Keeping them in one fixture
  means the flow table (Task 32) can assert the whole chain
  (`onClick → setState → render → <StaticCanvas/> → renderStaticScene`) and a regression
  that drops either channel breaks it.
- [ ] **Every fixture project must be non-trivially cross-file** (≥ 2 source files, ≥ 1
  edge whose source and target live in *different* files). A single-file project produces
  no cross-file edges and gates nothing — it is the resolution-shaped version of the
  all-zeros baseline. Asserted in Task 31 (`baseline_is_not_vacuous`).
- [ ] **No `.git`, no build outputs, no `node_modules`.** The dumper copies each project to
  a temp dir (Task 30) — a git dir would make the TS scan take its git fast path and the
  temp copy's would be missing/stale. Fixtures are scanned by the FS walker on both sides.
- [ ] **`projects.toml`** — one `[[project]]` per directory. This is the manifest the gate
  reads, and the spine of every anti-vacuity assertion:

  ```toml
  [[project]]
  dir = "fw-spring"
  languages = ["java", "yaml"]         # every language the corpus expects to see indexed
  expect_frameworks = ["spring"]       # detect() MUST return exactly this set — on BOTH engines
  min_cross_file_edges = 4             # a floor, not the count (the count is expected.json's job)
  notes = "pom.xml carries only the spring-boot-starter-web dep — the smallest thing springResolver.detect() keys on (frameworks-synth.md §Spring)."
  ```

  `expect_frameworks` is the sharpest anti-vacuity check in the whole gate. A framework
  whose `detect()` silently returns `false` emits no route nodes, so no route edges, so
  **both** engines dump an empty set and a pure diff is green. This field turns that
  silence into a failure — on the TS side in the dumper (Task 30) and on the Rust side in
  the gate (Task 31).
- [ ] `min_cross_file_edges` is a **floor** (an anti-vacuity tripwire), never the assertion.
  The assertion is the exact edge-identity multiset in `expected.json`. Two numbers that
  can drift apart are one number too many — keep the floor loose (the count of hops the
  project's flow *needs*), and let the baseline carry the truth.
- [ ] TDD: the only test at this task is `projects_manifest_matches_disk` — every
  `[[project]]` has a directory, every directory has a `[[project]]`, every project has ≥ 2
  source files. Commit it red first (the manifest lists projects whose dirs you have not
  written yet), then materialize the trees until it is green. This is the same
  `every_fixture_on_disk_is_gated` hole Phase 2 shipped with, closed *before* it can open.
- [ ] Commit: `test(resolve): shared TS↔Rust resolution fixture corpus (project trees + manifest)`

---

### Task 30: The TS baseline dumper — run the REAL CodeGraph resolver

**Files:** Create `tools/parity/dump-ts-resolution.mjs`; generate
`crates/selene-resolve/tests/fixtures/resolve/expected.json` (committed).

The extraction dumper (`tools/parity/dump-ts-extraction.mjs`) is the template — read it
first. Its three hard-won lessons carry over verbatim: **the loader is `vite-node`**, the
**commit SHA is derived, not asked for**, and the script **refuses to write a broken
baseline**. What changes is the unit of work: extraction dumped one file at a time through
`extractFromSource`; resolution must run the **whole pipeline over a whole project**,
because resolution's inputs are the *populated index* (frameworks re-detect against it,
`runPostExtract` finalizes across files, the conformance passes need `implements`/`extends`
edges to already exist).

- [ ] **Run the real pipeline, in its real order** (`maps/resolution.md` §Pass ordering) —
  not a hand-assembled subset. Per project: `indexAll()` (extraction + `fw.extract`) →
  `resolver.initialize()` → `runPostExtract()` → `resolveAndPersistBatched()` →
  `resolveChainedCallsViaConformance()` → `resolveDeferredThisMemberRefs()` →
  (`synthesizeCallbackEdges` runs inside the batched pass's tail — do not call it twice).
  Prefer driving codegraph's own `CodeGraph` entry point (`src/index.ts`) so the ordering
  is *its* ordering and cannot drift from what TS actually ships. **If you find yourself
  re-implementing the pass order in the dumper, stop** — you are now dumping a baseline
  from a pipeline that exists nowhere, and the gate is measuring the dumper.
- [ ] **Copy each project to a temp dir before indexing.** CodeGraph writes `.codegraph/`
  into the project root; indexing in place would pollute (and risk committing) the corpus.
  Copy → index → dump → delete the temp dir.
- [ ] **VERIFY node ids are path-relative, and refuse if they are not.** Node ids embed
  `filePath` (`"<kind>:" + hex(sha256("{filePath}:{kind}:{name}:{line}"))[..32]`). If
  CodeGraph's indexer stores **absolute** paths, every id in the temp copy is a function of
  the temp dir's name — ids would differ from Rust's, differ between two runs of the
  dumper, and the whole gate would be comparing noise. Assert on the first project that
  every node's `filePath` is relative (no leading `/`, no drive letter); **exit 1** with an
  explicit message if not. (If it turns out absolute paths are what TS stores, the fix is
  to index at a *fixed* path and normalize on dump — but find that out here, loudly, not
  three tasks later via an inexplicable 100% mismatch.)
- [ ] **Dump edge IDENTITY, not counts** — as a **label**, never a raw id. Two independent
  reasons, and the second is the load-bearing one:
  1. Raw node ids are opaque 32-hex digests. A diff over them is unreadable, and an
     unreadable gate is an unmaintained gate.
  2. **Route ids do not correspond across engines at all.** TS spells a route node's id as
     the literal `route:{file}:{line}:{METHOD}:{path}`; **ours is hashed** like every other
     node, with the route's semantics in first-class fields (`method`, `path`, `file`,
     `line`, `framework`) — the maintainer's decision, and the Phase 2 id contract with no
     exceptions. An id-string comparison would therefore fail on **every route-anchored
     edge**, for a reason that has nothing to do with resolution being right or wrong.

  So the label is **semantic**, and route nodes get their own spelling:

  ```jsonc
  {
    // Non-route endpoints: <kind>:<qualifiedName>@<relPath>:<startLine>
    "src":   "route:[spring|GET|/users]@src/UserController.java:14",   // ← route: see below
    "dst":   "method:UserController::list@src/UserController.java:15",
    "kind":  "calls",                     // EdgeKind wire string (post-promotion — see below)
    "prov":  "tree-sitter",               // Provenance wire string
    "by":    "framework",                 // metadata.resolvedBy  ("" for synthesized edges)
    "synth": "",                          // metadata.synthesizedBy ("" for resolved edges)
    "conf":  0.9,                         // metadata.confidence (null for synthesized)
    "meta":  { "refName": "UserController.list", "via": "", "event": "", "field": "", "registeredAt": "" }
  }
  ```

  **The route label is `route:[{framework}|{METHOD}|{path}]@{file}:{line}`** — the five
  first-class fields, in a fixed order, and *nothing else*. On the TS side, parse them back
  out of its literal id (that string is exactly those fields concatenated, which is why the
  translation is lossless); on the Rust side, read them off the node's fields. Verb-less
  routers (django `path()`, react) use `METHOD = ANY`, matching TS's own `ANY` convention.
- [ ] **Do not "fix" a route-id mismatch by loosening the comparison.** If someone later
  hits route diffs and relaxes the tuple to counts, or drops route edges from the diff, the
  gate goes blind **exactly where dispatch bridging lives** — every framework's entry hop is
  a route edge. The semantic label is the fix; the loosened comparison is the disaster it
  prevents. Say so in the dumper's header comment, where the next person will read it.
- [ ] Also dump, per **non-route** edge endpoint, the **raw TS node id** into a side map
  `nodeIds: { "<label>": "<id>" }`. Task 31 asserts Rust computes the *same id for the same
  label* — that keeps the id formula gated even though the diff itself reads labels. Without
  it, an id-formula divergence would be invisible here and would corrupt every downstream
  consumer that key-matches on id prefixes. **Route nodes are excluded from this map by
  construction** (their ids are *designed* to differ), and that exclusion is itself asserted
  in Task 31 — so nobody can quietly add routes back into the id check and have it "pass"
  by deleting the routes.
- [ ] **The `kind` is the POST-promotion kind.** `createEdges` rewrites it:
  `function_ref` → `references`; `extends` → `implements` when the target is an
  interface/protocol; `calls` → `instantiates` when the target is a class/struct
  (`maps/resolution.md` §Edge creation). Dump what is *stored*, and dump `metadata.refKind`
  (present only when a promotion changed the kind) — the promotion rules are exactly the
  kind of contract that a port silently drops, and the count of `calls` edges would not
  move if `instantiates` promotion vanished into `calls`.
- [ ] **Dump what did NOT resolve, too.** Per project, the remaining `unresolved_refs` rows
  (`status='failed'` and any still pending) as sorted `"<kind>:<name>@<file>:<line>"`
  strings, plus `stats.byMethod` from the resolution result. A resolver that binds
  *everything* (to garbage) and one that binds *nothing* both need to be visible, and the
  complement set is the only thing that sees them. `byMethod` additionally pins the
  *strategy mix* — if `import` collapses to 0 and `fuzzy` absorbs its work, every target
  may still be right today and wrong on the next repo.
- [ ] **Dump the detected frameworks** per project (`resolver.getDetectedFrameworks()`) and
  **cross-check against `projects.toml`'s `expect_frameworks`**. A mismatch ⇒ **exit 1**.
  This is the anti-vacuity check with the highest yield: a framework that fails to detect
  produces an empty, self-consistent, perfectly-matching baseline.
- [ ] **REFUSE to write the baseline** (exit 1, printing every offender) if ANY of:
  (a) a project produced **zero cross-file edges**, or fewer than its
  `min_cross_file_edges`; (b) a project's detected-framework set ≠ its `expect_frameworks`;
  (c) any node id is absolute-path-derived; (d) any extraction/resolution error of severity
  `error`; (e) `stats.resolved == 0` for any project; (f) a project declares a `synth-*`
  class but produced **zero `provenance:'heuristic'` edges**. This is the extraction
  dumper's discipline, extended to the ways *resolution* can be vacuously empty. Phase 2's
  sabotage test (comment out `initGrammars()` ⇒ every fixture 0 nodes ⇒ exit 1, nothing
  written) proved the mechanism; **repeat the sabotage here**: comment out
  `resolver.initialize()` so no framework detects, and confirm the dumper exits 1 rather
  than writing an all-frameworks-missing baseline. Record that you ran it, in the results
  doc (Task 33).
- [ ] Record `codegraphCommit` (derived via `git -C <cg> rev-parse HEAD`, env
  `CODEGRAPH_COMMIT` overriding — copy the extraction dumper's `codegraphCommit()`
  verbatim), `projectCount`, and per-project totals. **Never hand-edit `expected.json`.**
- [ ] Document in the file header, as the extraction dumper does: the `vite-node` loader
  requirement (`npx tsx` cannot load `web-tree-sitter` — its Emscripten CJS artifact
  defeats Node's ESM lexer, so `Parser === undefined` and `Parser.init()` throws), and the
  run line:
  ```bash
  cd ../codegraph && npx vite-node <selene>/tools/parity/dump-ts-resolution.mjs \
      <selene>/crates/selene-resolve/tests/fixtures/resolve \
      <selene>/crates/selene-resolve/tests/fixtures/resolve/expected.json
  ```
- [ ] Commit: `test(resolve): TS resolution baseline dumper (real CodeGraph resolver, refuses vacuous baselines)`

---

### Task 31: The resolution parity gate — edge IDENTITY, tolerance 0

**Files:** Create `crates/selene-resolve/tests/resolution_parity_gate.rs`,
`crates/selene-resolve/tests/fixtures/resolve/deviations.toml`. Modify
`crates/selene-resolve/Cargo.toml` (dev-deps: `selene-extract`, `selene-db`, `serde`,
`serde_json`, `toml`, `tokio`, `tempfile`).

`crates/selene-extract/tests/parity_gate.rs` is the template — **read it before writing a
line**. Its nine assertions each close a hole that was *actually open*, and every one of
them has a resolution analogue. Keep its structure: pure differ functions (so the harness
itself is testable), a `deviations.toml` with per-half entry kinds, and a module doc that
names the failure modes.

**Harness shape.** Per project in `projects.toml`: `SurrealStore::in_memory()` →
`selene_extract::Indexer::index_all()` → the **Task 28 driver** (which is the full pass
order: `initialize` → `run_post_extract` → `resolve_and_persist_batched` → the two
conformance passes → the synthesis tail) → read every edge back **out of the store** and
label it. Read the edges from the **store**, not from the resolver's return value: what the
graph *contains* is what an agent will query, and a persist path that drops or duplicates
rows is precisely the bug a return-value assertion cannot see (Phase 1's ordered-commit and
Task 28's keyed-delete invariants both live on that path).

### The identity rule — and why route edges are compared SEMANTICALLY

**Non-route nodes:** the id formula is identical on both engines (Phase 2 proved it byte for
byte), so the label `<kind>:<qualifiedName>@<relPath>:<line>` is a faithful, human-legible
stand-in and the raw ids are cross-checked separately (`edge_endpoint_ids_agree`).

**Route nodes: the ids are *designed* not to match.** TS's route id is the literal string
`route:{file}:{line}:{METHOD}:{path}`; ours is **hashed** like every other node (the Phase 2
contract admits no exceptions), with the route's semantics in first-class indexed fields —
`method`, `path`, `file`, `line`, `framework` — matched by indexed SurrealQL query, never by
parsing an id. So a route-anchored edge is compared on **`(framework, method, path, file,
line)`**, via the label `route:[{framework}|{METHOD}|{path}]@{file}:{line}`.

**Why this must be said out loud in the test's module doc:** an id-string comparison would
fail on *every route edge*, for a reason that has nothing to do with resolution correctness.
The obvious "fix" — loosen the comparison, or exclude route edges from the diff — would
blind the gate **precisely where dispatch bridging lives**: every framework's entry hop is a
route edge, and route→handler is the first hop of every flow in Task 32. A gate that skips
route edges is a gate that cannot see a framework binding its routes to the wrong handlers.
Compare the semantics, not the spelling.

### The five assertions that diff

- [ ] **`ts_rust_edge_identity_parity`** — THE gate. Multiset diff, both directions, over
  the tuple `(src_label, dst_label, kind, prov, resolvedBy, synthesizedBy)`, where route
  endpoints carry the semantic label above. Counts alone cannot see a resolver that binds a
  reference to the **wrong target**; identity can. Pair one-sided entries positionally so a
  re-target reads as one `ts=… rust=…` line (copy `diff_names_one`'s pairing). Justified
  divergences: `[[edge-deviation]]`.
- [ ] **`ts_rust_edge_metadata_parity`** — for edges matched in the identity half, diff
  `confidence`, `refName`, `refKind`, `via`, `event`, `field`, `registeredAt`. Why a
  separate half: an edge can have the right endpoints and still be *wrong in a way the MCP
  layer reads* — `registeredAt` is surfaced verbatim in explore Flow and node trails
  (`maps/frameworks-synth.md` §Wire), `refName` is the edge-resurrection key (#1240), and
  `confidence` is the number the whole `resolveOne` pipeline order is expressed in. A
  confidence that drifts from 0.9 to 0.7 changes nothing today and changes which strategy
  wins tomorrow. Justified divergences: `[[metadata-deviation]]`.
- [ ] **`ts_rust_unresolved_parity`** — diff the leftover unresolved/failed ref set and
  `stats.byMethod`. This is the complement gate: it sees the resolver that binds *nothing*
  (identity diff would be a long list of TS-only edges — fine, that fails too) **and** the
  one that binds *too much* (a fuzzy matcher that no longer declines on the
  `AMBIGUOUS_NAME_CEILING` would drain this set while every individual edge it invented
  looks plausible). `byMethod` pins the strategy mix. Divergences: `[[unresolved-deviation]]`.
- [ ] **`edge_endpoint_ids_agree`** — for every **non-route** label in the baseline's
  `nodeIds` map, the Rust node carrying that label must have the **same node id**. The diff
  reads labels (human-legible); this assertion keeps the *id formula* gated, since every
  downstream consumer key-matches on id prefixes (`file:`, `class:`, …) and an id divergence
  would be invisible in a label diff yet corrupt the whole store. **Route nodes are excluded
  by design** — assert that too (`route_nodes_are_excluded_from_id_agreement`: the `nodeIds`
  map contains **no** `route:` label, and every Rust route node's id **is** a hashed
  `node_id()`, not a literal string). Both halves matter: the first stops someone
  "fixing" a route-id diff by deleting routes from the map; the second stops the Phase 2 id
  contract being quietly re-broken for routes.
- [ ] **`detected_frameworks_agree`** — per project, TS's `getDetectedFrameworks()` (from
  the baseline) == Rust's `detected_framework_names()` == `projects.toml`'s
  `expect_frameworks`. **Three-way**, deliberately: TS==Rust alone would pass when *both*
  fail to detect, and the manifest is the only party that knows what *should* have fired.

### The four structural assertions (ported one-for-one from Phase 2)

- [ ] **`every_project_and_file_on_disk_is_gated`** — the diff iterates the BASELINE, so a
  project (or a *file inside* a project) added but never dumped is compared by nobody while
  the gate says green. Phase 2 shipped with **ten** heritage fixtures in exactly that state.
  Assert set equality in both directions, at **both** granularities: projects, and the
  source files within each project. The file-level half matters more here than it did in
  Phase 2: adding one file to `fw-spring` changes what resolves without adding a project.
- [ ] **`language_detection_agrees`** — per file, per project: TS detects from the PATH,
  Rust from path AND CONTENT. A disagreement means the two engines extracted different
  things and the gate would be calling that parity.
- [ ] **`baseline_is_not_vacuous`** — the anti-vacuity spine, and the assertion this gate
  most needs, because resolution's empty state is so plausible. Assert, from the Rust side:
  every project has **> 0 cross-file edges** and ≥ its `min_cross_file_edges`; every
  `synth-*` project has **> 0 `Heuristic`-provenance edges**; every project's
  `stats.resolved > 0`; the baseline's `codegraphCommit` is not `"unknown"`; the total edge
  count across the corpus is non-trivial. **Also assert the corpus exercises the contract
  surface**: at least one edge of each of `resolvedBy` ∈ {`import`, `qualified-name`,
  `instance-method`, `exact-match`, `framework`, `function-ref`, `file-path`, `fuzzy`}
  exists somewhere in the baseline. A corpus that never produces an `import`-resolved edge
  is not gating the import resolver, no matter how green it is.
- [ ] **`harness_catches_a_synthetic_mismatch`** — perturb known-good inputs and require the
  differ to report exactly the injected fault. Four perturbations, each mapping to a real
  failure class: (1) a **re-target** (same src/kind, different dst) ⇒ one paired diff line
  — this is the wrong-target bug the count gate cannot see; (2) a **strategy swap**
  (`resolvedBy` import→fuzzy, everything else identical) ⇒ one diff; (3) an **over-emission**
  (a Rust edge TS never emits) ⇒ one diff with an empty `ts` side; (4) a **duplicate** (the
  same edge twice) ⇒ one diff, proving the multiset arithmetic. Without this, a differ that
  silently returned `vec![]` would make the gate green forever — which is the exact failure
  the gate exists to prevent.

### `deviations.toml` — machine-checked, stale entries FAIL

- [ ] Three entry kinds mirroring the three diff halves (`[[edge-deviation]]`,
  `[[metadata-deviation]]`, `[[unresolved-deviation]]`), each with a mandatory `reason`
  ≥ 20 chars **citing the TS source** (file:line), and each asserting `ts != rust`.
  `every_deviation_is_justified` enforces both — *"a deviation without a named cause is an
  unexamined bug wearing a deviation's clothes."*
- [ ] **A stale entry — one matching no observed difference — FAILS the gate.** A fixed
  divergence must not leave behind a permanent whitelist that silently re-permits the
  regression. This is not optional and it is not a warning.
- [ ] Expect a `[[grammar-drift]]`-shaped need to *not* arise here (grammars are Phase 2's
  problem), but expect its cousin: places where **TS is wrong and we stay silent**. Phase 2
  found three, all the same shape — TS emits a reference to something that has no definition
  node and can never resolve. Resolution's version is worse, because resolution *acts* on
  such refs: it either binds them to a wrong target or leaves them failed. Every one you find
  gets an entry, with the fixture that exercises it and a focused unit test in Part A/B's
  suite (name the test in the `reason`, as Phase 2's entries do).
- [ ] **Do not use a deviation to paper over a flow that does not close.** If a hop is
  missing, that is Task 32's failure and it is a bug. A `[[edge-deviation]]` saying "we do
  not emit this hop" is how a half-bridged flow gets *ratified*. The only legitimate
  deviations are ones where **TS emits something wrong** and we correctly stay silent (or
  vice-versa, where TS misses something we correctly emit).
- [ ] TDD: write `harness_catches_a_synthetic_mismatch` **first**, against hand-built
  fixtures of the differ's input types — it needs no baseline and no resolver, and it is
  the assertion that proves the other seven mean anything.
- [ ] Commit: `test(resolve): TS↔Rust resolution parity gate — edge identity, metadata, unresolved (tolerance 0)`

---

### Task 32: The dispatch-coverage gate — **THE Phase 3 gate**

**Files:** Create `crates/selene-resolve/tests/dispatch_coverage_gate.rs`,
`crates/selene-resolve/tests/fixtures/resolve/flows.toml`.

> **Supersedes Part B's Task 36.** B36 asserts `store.find_path(from, to)` returns a path
> ≤ `max_hops` containing certain `via` *symbols*. That is a **path-existence** check, and a
> path search is satisfied by *any* route through the graph — including one that goes around
> the very dispatch hop the gate was written to defend. It also cannot see a hop closed by
> the **wrong mechanism** (a fuzzy match that lands on the right symbol by luck is not a
> bridged dispatch; it is a coin flip that will land elsewhere on the next repo). This task
> asserts **each hop's edge individually, with its `via` mechanism pinned**, and **absorbs
> B36's three other sub-gates verbatim** (0-control precision, no-explosion, determinism —
> below). B36's fixture-building step folds into Task 29. Nothing of B36 is lost.

The roadmap's Phase 3 gate, verbatim: *"dispatch-coverage fixtures resolve end-to-end (no
half-bridged flow)."* This task is that sentence, made executable.

**The invariant it defends** (CLAUDE.md, and `design/dynamic-dispatch-coverage-playbook.md`
§7): *"Dynamic-dispatch coverage must be end-to-end. Partial coverage is **worse than none**
— a half-bridged flow reveals a hop the agent then reads to finish."* This is not
hyperbole and it is not theoretical: the playbook records that shipping the React
re-render channel *without* the JSX-child hop **measurably raised** agent reads. Three
working hops out of four is a graph that tells an agent "the flow continues… somewhere",
which sends it to `Read` with *extra* confidence. **A flow that resolves 3 of 4 hops
FAILS this gate.**

**Why the parity gate cannot do this job.** Task 31 is differential: it certifies that Rust
binds what TS binds. If TS's flow were itself broken at a hop, Task 31 would go green on a
faithfully-ported broken flow. And a *count*-shaped check is blind by construction: three
real edges are three real edges. Only an assertion that names **every hop of a canonical
flow and demands each one individually** can see a hole in the middle of a chain.

### `flows.toml` — one canonical flow per framework and per synthesizer

- [ ] Each entry is the framework's **signature control flow** (playbook §4 Step 1: "how
  does X reach/become Y"), written as an **ordered hop list from the entry point to the
  handler body**. Route endpoints use the **semantic route label** of Task 31
  (`route:[{framework}|{METHOD}|{path}]@{file}:{line}`), never a raw id:

  ```toml
  [[flow]]
  name    = "spring: request → controller → service → repository"
  project = "fw-spring"
  # Each hop: the edge that MUST exist. `via` pins HOW it was bridged — a hop that
  # resolves by the wrong mechanism is a different bug, and one that will re-break.
  hops = [
    { from = "route:[spring|GET|/users]@src/UserController.java:14", to = "method:UserController::list@src/UserController.java:15", kind = "calls", via = "framework" },
    { from = "method:UserController::list@src/UserController.java:15", to = "method:UserService::findAll@src/UserService.java:9", kind = "calls", via = "instance-method" },
    { from = "method:UserService::findAll@src/UserService.java:9", to = "method:UserRepository::findAll@src/UserRepository.java:7", kind = "calls", via = "framework" },
  ]

  # The CONFIG bridge — a cross-LANGUAGE hop, and the one most likely to half-bridge in
  # silence (the Java side resolves on its own; the yaml side simply dead-ends, and no
  # same-language assertion notices). Maintainer decision: Yaml/Properties are file-level
  # languages in Phase 3 precisely so this closes.
  [[flow]]
  name    = "spring: @Value bind → application.yml config key"
  project = "fw-spring"
  hops = [
    { from = "class:GreetingService@src/GreetingService.java:8", to = "constant:spring-value:app.greeting@src/GreetingService.java:10", kind = "contains", via = "extract" },
    { from = "constant:spring-value:app.greeting@src/GreetingService.java:10", to = "constant:app.greeting@src/main/resources/application.yml:2", kind = "references", via = "framework" },
  ]
  ```

  **`via` vocabulary** (three forms, all needed): a `resolvedBy` value for resolved edges
  (`framework`, `import`, `instance-method`, …); `synth:<synthesizedBy>` for heuristic ones
  (v0: `synth:callback`, `synth:event-emitter`, `synth:react-render`, `synth:jsx-render`);
  and `extract` for edges emitted at extraction time (`provenance: tree-sitter`, no
  `resolvedBy`) — the `contains` hop above is one. Pinning `via` is what stops a hop from
  being "closed" by accident: a fuzzy match that lands on the right symbol today is not a
  bridged dispatch, it is a coin flip that will land elsewhere on a repo with two `findAll`s.
  ⚠ **Only the four v0 channels may appear as `synth:*`.** `interface-impl`, `go-implements`,
  `cpp-override` and the other ~30 TS passes are **Phase 8** — a flow that needs one of them
  is a flow whose framework is not fully bridged in v0, and the honest response is to say so
  in the results doc's §6, not to invent a channel.
- [ ] **Coverage: one flow per framework, one per synthesizer, no exceptions.** Every
  framework Part B registers (including **Next.js** — maintainer decision, in scope) and
  every `synthesizedBy` channel it can emit gets a `[[flow]]` whose chain *runs through it*.
  The `fw-*` and `synth-*` projects of Task 29 exist to make each of these writable.
- [ ] **Each flow must start at a real entry point and end in a handler body.** A "flow"
  whose hop list is one edge long is not a flow, it is an edge — and gating it proves
  nothing about the property this gate exists for. Minimum 2 hops; the framework flows are
  typically 3–4 (route → handler → service → repo/model). The `synth-react` flow is the
  reference case for why: `onClick → setState-sibling → render → <Child/> → renderChild`
  spans **both** synthesized channels, and dropping either one breaks the chain in the
  middle — exactly the regression that "partial coverage is worse than none" describes.
- [ ] **Open question to settle while writing the Spring flows** (record the answer in the
  results doc): does extraction emit an edge from `GreetingService::message` to the
  `@Value` bind field it reads? If it does, the two Spring flows above **merge into one
  4-hop chain** (route → controller → service → bind → config key), which is strictly
  better — the config key then sits on the request flow, where an agent actually walks to
  it. If it does not, they stay two flows and the *seam between them* is a documented
  coverage limit, not an invisible one.

### The assertions

- [ ] **`every_flow_hop_resolves`** — the gate. For each `[[flow]]`, index+resolve its
  project once, then assert **each hop's edge exists individually**, with the right `kind`
  and the right `via`. Report **every** broken hop, not the first, and format the failure as
  the chain with the break marked — the message is the debugging tool:

  ```
  FLOW BROKEN: spring: request → controller → service-interface → impl   (fw-spring)
    ✓ route:GET /users            → method:UserController::list        [calls via framework]
    ✓ method:UserController::list → method:UserService::findAll        [calls via instance-method]
    ✗ method:UserService::findAll → method:UserServiceImpl::findAll    [calls via synth:interface-impl]  ← MISSING
    ✓ method:UserServiceImpl::…   → method:UserRepository::findAll     [calls via instance-method]
    3 of 4 hops resolve. A half-bridged flow is WORSE than none (CLAUDE.md): the agent
    follows the map to the break and then reads. Close the hop; do not weaken the flow.
  ```

  **Assert hop-by-hop, never "a path exists".** A path search would happily report the flow
  "connected" via some *other* route through the graph while the dispatch hop this gate was
  written to defend is missing — a green gate over the precise hole it exists to find. (It
  is also how you would accidentally certify a fuzzy-matcher's lucky guess as a bridged
  dispatch.)
- [ ] **`flows_form_connected_chains`** — a cheap structural check on the *table*, not the
  graph: each flow's `hops[i].to == hops[i+1].from`. A hop list with a gap in it is a typo
  that would otherwise make the gate assert something weaker than it appears to. Fail on it.
- [ ] **`every_framework_and_synthesizer_has_a_flow`** — the coverage-completeness
  assertion, and the one that keeps this gate honest **as Part B grows**. Set-equality, both
  directions:
  - `{ f.name() for f in all_framework_resolvers() }` == `{frameworks exercised by ≥ 1 flow}`
  - `{ registered_synthesizers() }` == `{synth:* channels exercised by ≥ 1 flow}`

  A framework or synthesizer added without a flow **FAILS** — it does not ship silently
  ungated. This is Phase 2's `every_fixture_on_disk_is_gated` hole, generalized to the
  registry: there, ten fixtures sat gated by nobody behind a green gate; here, an entire
  dispatch channel could. (Which framework exercises which flow is derived from the
  `[[flow]]`'s project + the `via` values — no second list to drift.) ⚠ **`registered_synthesizers()`
  does not exist yet** — it is the one interface this part still needs from Part B (see
  *Reconciliation*). **Do not hard-code the channel list here**: a hard-coded list is a
  second source of truth, it will drift on the first Phase 8 channel, and the assertion's
  entire value is that it is keyed to the registry.
- [ ] **`negative_controls_stay_silent`** — a `[[negative]]` section, and the other half of
  the doctrine. **"Silent beats wrong":** a synthesizer that bridges everything would pass
  every flow assertion above while filling the graph with lies. So pin the non-edges too:
  - a chained call on a type that **lacks** the method ⇒ **no edge** (the `resolveMethodOnType`
    validated-inference invariant — every TS chain block carries this test);
  - an EventEmitter event with **> `EVENT_FANOUT_CAP` (6)** dispatchers or handlers ⇒ the
    whole event is **skipped**, not linked 7 ways;
  - an ambiguous name above the ambiguity ceiling ⇒ **declines**, does not guess;
  - **the Spring config-key constant carries NO value** (`value` field absent/empty) — the
    yaml leaf `app.greeting: hunter2` must produce a key node and **never** the secret
    (redaction, #383). A config bridge that helpfully stored the value would pass every
    positive flow assertion in this file while turning the graph into a credential dump.

  Each `[[negative]]` names `project`, `from`, `to`, and a `reason`. If a `[[negative]]` edge
  ever *appears*, the gate fails — precision regressions are the ones that make a graph worse
  than no graph at all, and they are invisible to every assertion that only counts what is
  present.

### Absorbed from Part B's Task 36 (do not drop these — they are the precision half)

- [ ] **`synthesis_emits_nothing_on_the_control_corpus`** — run the full pipeline over
  `tests/fixtures/resolve/_control/` (Task 29: one plain repo per v0 language, containing
  **none** of the dispatch shapes) and assert `run_synthesis` emits **exactly 0 edges**.
  Playbook §5.2 ("0 on every non-Swift control"). Every positive assertion above is satisfied
  by a synthesizer that fires on everything; **this is the only one that isn't.** A single
  edge here is an over-linking synthesizer, and an over-linking synthesizer poisons the map
  for every query, not just the one it was built for.
- [ ] **`fixture_counts_do_not_explode`** — record each project's node + edge counts in
  `flows.toml` and assert they hold. A resolution or extraction change that *balloons* counts
  means something over-fired; a change that shrinks them means something stopped firing.
  Deltas require a deliberate update **and a note** — never a silent re-baseline.
- [ ] **`resolution_is_deterministic`** — index+resolve each fixture **twice** into two fresh
  stores; assert the full edge set (source, target, kind, metadata) is **identical**. This is
  not a formality: resolution disambiguates same-named symbols by **insertion order**
  (Phase 2's #1015 ordered-commit invariant is upstream of it), so a nondeterministic commit
  order does not produce a *flaky* gate — it produces edges that silently point at a
  different `save()` on every index.
- [ ] **The flow table's targets are the TS baseline's edges.** Every hop you write must
  correspond to an edge in `expected.json` (Task 30) — i.e. TS resolves it too. If a hop is
  in the flow table but **absent from the TS baseline**, you have found either a fixture bug
  or a genuine TS coverage hole. Do not paper over it: surface it (results doc §Known limits,
  and to the maintainer). Add an assertion — `flow_hops_exist_in_ts_baseline` — that checks
  this mechanically, so the two tables cannot drift apart. This is what keeps Task 32 (absolute)
  and Task 31 (differential) mutually reinforcing rather than independently rotting.
- [ ] TDD: write the flow table for **one** framework and its `[[negative]]` first; watch
  `every_flow_hop_resolves` fail on the un-implemented hops (it will — Part B may still be
  landing); fill the table out as the channels land. **The gate goes in red and is driven
  green by Part B** — that is the intended order, and it is why this task can be written
  before Part B is finished.
- [ ] Commit: `test(resolve): dispatch-coverage gate — canonical flows resolve end-to-end, per hop`

---

### Task 33: The parity results doc + the coverage-limits ledger

**Files:** Create `docs/benchmarks/2026-07-phase3-resolution-parity.md`.

Model: `docs/benchmarks/2026-07-phase2-extract-parity.md` — read it. Its §6 ("Known
coverage limits") is the section that matters most, and the reason is worth restating
because it is the single most valuable paragraph either phase produced:

> *"The gate gates what the corpus contains, and nothing else. … Class inheritance sat on
> exactly such a list — and when fixtures were finally written for it, it turned out to be
> **unimplemented in nine languages at once**, invisible behind a green gate. Every
> subsequent corpus extension has found real bugs. The pattern is reliable enough to plan
> around: **write the fixture first.**"*

- [ ] **§1 Headline** — TS vs Rust edge totals (resolved / synthesized / unresolved), the
  deviation count, and the `codegraph` commit SHA the baseline came from (with `src/`
  pristine — say so, as Phase 2 did).
- [ ] **§2 Per-project** — edges by `resolvedBy` and by `synthesizedBy`, so the **strategy
  mix** is visible at a glance. A table that shows `import` at 0 is a table that shows the
  import resolver is untested, whatever the totals say.
- [ ] **§3 What the gate asserts, and the holes it closes** — the assertion table, one row
  per assertion, each naming the hole. Copy Phase 2's framing: *most of these exist because
  a gate was, at some point, green while comparing nothing.* Include the two defenses that
  live outside the Rust test (the dumper's refuse-to-write, the derived commit SHA) and
  **record the sabotage test** from Task 30 (initialize() commented out ⇒ exit 1, nothing
  written) — an untested refusal is not a refusal.
- [ ] **§4 The deviations** — one subsection each: the TS evidence (file:line), the fixture
  that exercises it, the focused test that guards it, and why silent beats wrong. Phase 2's
  §4 is the shape.
- [ ] **§5 Dispatch coverage** — the flow table, rendered: every framework and synthesizer,
  its canonical flow, hop count, and ✅/❌. This is the phase gate's report card and the
  thing a reader will look for first.
- [ ] **§6 Known coverage limits** — *"read as a to-do, not a footnote."* Everything the
  corpus does **not** exercise: wave-2 frameworks (Phase 8), the ~30 synthesizer channels
  beyond the v0 five, incremental-sync re-synthesis (a known TS gap —
  `synthesizeCallbackEdges` runs only on the batched full-index path), anonymous-arrow
  EventEmitter handlers (unlinked in TS too), and any language whose chained-call block has
  no fixture. Name them, so the next corpus extension has a work list rather than a hunch.
- [ ] **If deviations exceed ~2% of any project's edge count, STOP and surface to the
  maintainer** — that is a resolution-parity failure, not a tolerance. (Phase 2 carried the
  same tripwire on node counts.)
- [ ] Commit: `docs(resolve): Phase 3 resolution-parity + dispatch-coverage results`

---

### Task 34: Facade polish — `selene-resolve`'s public surface, deferrals, deviation ledger

**Files:** Modify `crates/selene-resolve/src/lib.rs`; touch `CLAUDE.md` (status line).

`crates/selene-extract/src/lib.rs` is the shape — **copy it**. Three things make it work,
and all three transfer:

- [ ] **The public-interface ledger.** A table with one row per item in
  `maps/resolution.md` §Public interface **and** `maps/frameworks-synth.md` §Public
  interface, each mapping to its Rust equivalent **or to an explicit deferral**. The rule
  Phase 2 enforced: *every* map item appears in this table. An item that is neither ported
  nor deferred is an item nobody decided about.
- [ ] **The deferrals, each with its phase and its reason.** At minimum:
  - **wave-2 frameworks → Phase 8** (SvelteKit, Vue/Nuxt, NestJS, Astro, Play, Drupal,
    Terraform, CICS, Swift/ObjC + React-Native/Expo/Fabric bridges, GoFrame) — the roadmap
    scopes Phase 3 to the v0-language frameworks only.
  - **the ~30 synthesizer channels beyond the v0 four → Phase 8** (`callback`,
    `event-emitter`, `react-render`, `jsx-render` ship; `interface-impl`, `go-implements`,
    `cpp-override`, `closure-collection`, … do not), and say *why these four*: they are the
    ones the playbook validated as closing a canonical flow end-to-end. Adding a channel
    without its completing hops is the "partial coverage is worse than none" failure — so
    the deferral is a *design* decision, not a backlog. (The Django ORM descriptor is the
    fifth v0 dispatch bridge but is a **resolver**, not a synthesizer — `resolvedBy:
    'framework'`, not `heuristic` provenance. Say so here; the roadmap's "all 5
    synthesizers" phrasing is loose and has already misled one plan.)
  - **`cooperative-yield` → dropped, not ported.** It is a Node event-loop artifact (the
    liveness-watchdog heartbeat, #850/#1091). Rust resolution runs off the async runtime;
    there is no single-threaded event loop to starve. The *batching and streaming*
    discipline it protected (chunked inserts, `iterate_nodes_by_kind` streaming, never
    materializing unbounded kinds — #610/#1212 OOM) **is** ported, and that distinction is
    the note worth writing down.
  - **`importMappingCache` (import-resolver.ts:998) → dead in TS, not ported** (declared and
    cleared, never written or read; the real cache is the resolver's LRU).
  - **incremental-sync re-synthesis → Phase 6** (with `selene-sync`), matching the known TS
    gap; note it so it is a decision rather than an omission.
- [ ] **The deviation ledger pointer.** One paragraph naming
  `tests/fixtures/resolve/deviations.toml` as **the single authority** for every intentional
  TS↔Rust divergence — not a commit message, not a code comment. Phase 2's lib.rs says this
  because three deviations *nearly got lost* living only in comments. Same sentence, same
  reason.
- [ ] **The route-node contract** gets its own short section (it is the one place Phase 3
  deliberately *departs* from the TS wire shape): route ids are **hashed** like every other
  node — the Phase 2 contract, no exceptions — and the route's semantics live in first-class
  indexed fields (`method`, `path`, `file`, `line`, `framework`), queried by index, never by
  parsing an id string. Name the consequence: the parity gate compares route edges
  **semantically** (Task 31), and any future consumer must query the fields, not the id.
- [ ] **Re-export the public surface**: `ReferenceResolver`, `resolve_and_persist_batched`
  (Task 28), `ResolutionStats`, `ResolutionContext`, `StoreContext`,
  `UnresolvedRef` / `ResolvedRef` / `ResolvedBy`, `FrameworkResolver` + the registry accessors,
  `run_synthesis` + `registered_synthesizers()`, the error types. Anything a
  downstream crate (Phase 4's `selene-graph`, Phase 5's MCP) needs must be reachable without
  a `pub(crate)` escape hatch.
- [ ] **State the invariants in the crate docs**, in the resolver's own terms — they are the
  contract Part A/B were built to, and the crate docs are where the next reader looks:
  *validated inference* (a type guess is accepted only if the method actually exists on it,
  so mis-inference yields **no edge, never a wrong one**); *path-shaped refs never fall back
  to symbol matching* (#660); *ubiquitous names decline rather than guess* (#999);
  *dispatch bridges end-to-end or not at all*.
- [ ] `cargo doc -p selene-resolve` **warning-free** (no broken intra-doc links).
- [ ] Full workspace green: `cargo fmt --check && cargo clippy --all-targets && cargo test`.
- [ ] Update `CLAUDE.md`'s status line (Phase 3 complete; `selene-resolve` implemented) and
  the crate-list bullet for `selene-resolve` (it currently reads "stub").
- [ ] Commit: `feat(resolve): selene-resolve public facade`

---

## Self-review checklist (after Task 34)

Run this against the finished phase. Each line is a hole that was *actually open* in a prior
phase, or an invariant that the maps say costs correctness when it slips.

**The gate is a gate**
- [ ] The gate compares **edge identity** — `(source, target, kind, provenance, resolvedBy,
  synthesizedBy)` — not counts. A resolver that binds a ref to the **wrong target** fails it.
  (Counts cannot see this. Phase 2's count gate was green while Ruby calls were truncated.)
- [ ] The baseline came from the **real CodeGraph resolver**, driven through its **real pass
  order** (`initialize → runPostExtract → resolveAndPersistBatched → both conformance
  passes`), not a hand-assembled subset — and `expected.json` records the `codegraph` commit
  SHA it came from.
- [ ] The dumper **refuses to write** a baseline that is empty, error-laden, missing a
  framework it was told to expect, or free of heuristic edges in a `synth-*` project — and
  **the refusal was sabotage-tested** (comment out `initialize()`; confirm exit 1, nothing
  written).
- [ ] Every fixture **project and file on disk** is in the baseline, asserted set-equal in
  both directions. A fixture added without regenerating **FAILS**; it is not silently
  ungated. (Phase 2 shipped with ten fixtures in exactly that state.)
- [ ] **Detected frameworks agree three ways** — TS == Rust == `projects.toml`'s
  `expect_frameworks`. (TS==Rust alone passes when *both* fail to detect and the whole
  framework layer is gated by nothing.)
- [ ] **Language detection agrees** per file (TS from path, Rust from path+content) — the gate
  is not comparing two different engines and calling it parity.
- [ ] The harness has a **self-test that plants a synthetic mismatch** — a re-target, a
  strategy swap, an over-emission, a duplicate — and each is reported. A differ that returned
  `vec![]` would otherwise be green forever.
- [ ] `deviations.toml` is **machine-checked**: every entry cites its TS evidence, every entry
  has `ts != rust`, and **a stale entry FAILS the build**.
- [ ] No deviation ratifies a **missing hop**. Deviations record where *TS is wrong*; they are
  not a place to park a flow that does not close.

**The flows close**
- [ ] Every canonical flow resolves **end-to-end, hop by hop** — asserted per-hop, never as
  "a path exists" (which a lucky route through the graph would satisfy while the dispatch hop
  is missing). **3 of 4 hops FAILS.**
- [ ] Every hop pins **`via`** (`resolvedBy` / `synth:<channel>`) — a hop closed by the wrong
  mechanism is a coin flip, not a bridge.
- [ ] **Every framework and every synthesizer in the registry has a flow** (set-equality
  against `REGISTERED_SYNTHESIZERS` + the framework registry). A channel cannot ship ungated.
- [ ] **Negative controls stay silent**: a chained call on a type lacking the method emits **no
  edge**; an over-cap EventEmitter event is skipped entirely; an ubiquitous name declines. A
  synthesizer that bridges everything passes every positive assertion and poisons the graph.
- [ ] `synth-react` gates **both** the re-render and JSX-child channels in one chain — the
  case the playbook records as *raising* reads when shipped half-done.
- [ ] Every flow hop **exists in the TS baseline too** (`flow_hops_exist_in_ts_baseline`), so
  the absolute gate and the differential gate cannot drift apart.

**The contracts did not drift** (`maps/resolution.md`, `maps/frameworks-synth.md`)
- [ ] `resolvedBy` wire strings intact: `exact-match | import | qualified-name | framework |
  fuzzy | instance-method | file-path | function-ref` (+ `callback-synthesis` in `byMethod`).
- [ ] Every `synthesizedBy` value the registry emits is in the map's enumerated set;
  `provenance: Heuristic` on every synthesized edge; `registeredAt: 'file:line'` present where
  the map says it is (and **absent** for `jsx-render`, which has none — the MCP layer reads
  these keys).
- [ ] Edge-kind **promotions** survive: `function_ref` → `references`; `extends` → `implements`
  on an interface target; `calls` → `instantiates` on a class target; `metadata.refKind` set
  only when a promotion changed the kind.
- [ ] Confidence constants did not drift (0.9 short-circuit; 0.95/0.9/0.85/0.8/0.7/0.5 tiers) —
  they are the pipeline order expressed as numbers, and the gate's metadata half compares them.
- [ ] `ResolvedRef.original.reference_name` equals the stored row's name — the keyed-delete
  invariant (a mutated name no-ops the delete and the batch loop's non-progress guard is the
  only thing standing between you and the 5M-edge runaway).
- [ ] Fan-out caps carried: 6 events / 8 fields / 8 emitter keys / 40 callbacks / 30 JSX
  children. Ambiguous ⇒ drop, never guess.

**Hygiene**
- [ ] No `unwrap`/`expect` outside `#[cfg(test)]`; errors collected, never thrown.
- [ ] Deterministic: two runs over one fixture project produce identical edge sets and identical
  ids (assert it — resolution reads insertion order to disambiguate same-named symbols, so a
  nondeterministic commit order silently re-targets edges).
- [ ] `cargo doc -p selene-resolve` warning-free; `cargo fmt --check && cargo clippy
  --all-targets && cargo test` green across the workspace.
- [ ] Results doc records: deviations, the strategy mix per project, the sabotage test, and
  **§6 Known coverage limits as a to-do list** — the section that, in Phase 2, was the one that
  kept finding real bugs.

---

## Open coordination points (surfaced to the maintainer; do not silently resolve)

1. **Two introspection surfaces Parts A/B must expose** (Assumption 3):
   `Resolver::detected_framework_names()` and `REGISTERED_SYNTHESIZERS`. Without them,
   `detected_frameworks_agree` and `every_framework_and_synthesizer_has_a_flow` cannot be
   written — and those are the two assertions that stop a framework or a dispatch channel from
   shipping gated by nobody. They are small; they are also load-bearing.
2. **Does CodeGraph's `indexAll` store node `filePath` relative or absolute?** The whole gate
   rests on node ids being reproducible across engines, and ids embed the path. Task 30 asserts
   it and exits 1 if absolute — but if it *is* absolute, the dumper needs a fixed indexing path
   plus normalization, which is a design change, not a fix. **Find out in Task 30, loudly.**
3. **Resolution baseline runtime.** The dumper runs a full CodeGraph index+resolve per fixture
   project (~24 projects). If it turns out slow enough to be painful, the answer is *fewer,
   richer projects* — never *fewer assertions*.
4. **`EVENT_FANOUT_CAP` and friends are TS's numbers.** The negative controls in Task 32 pin
   them. If Part B tunes any cap, the gate's negative control changes with it and both need to
   be a deliberate decision, recorded in the results doc.
5. **Django ORM is a resolver, not a synthesizer** (`claimsReference('_iterable_class')` →
   `ModelIterable.__iter__`, conf 0.7, `resolvedBy:'framework'`, **not** `heuristic`
   provenance). The roadmap's Phase 3 line calls it one of "all 5 synthesizers". The map is
   right and the roadmap's phrasing is loose; Task 32 gates its flow either way, but the
   `[[flow]]` hop's `via` is `framework`, not `synth:…`. Flagging so the roadmap's wording can
   be corrected rather than the code bent to match it.
