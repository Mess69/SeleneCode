//! `selene-resolve` — cross-file resolution: the `ReferenceResolver`, import
//! and name matching, the framework registry, and the dynamic-dispatch
//! synthesizers.
//!
//! Target design: `docs/specs/2026-07-11-rust-graph-db-migration-design.md`
//! (PRD §2, §3); build plan: `docs/plans/2026-07-13-phase3-selene-resolve.md`;
//! the TS parity source is mapped in
//! `docs/reference/from-codegraph/maps/resolution.md`.
//!
//! # Where this crate sits
//!
//! `selene-extract` (Phase 2) emits **zero cross-file edges**: every reference
//! that reaches beyond its file leaves as a `selene_core::UnresolvedRef`. This
//! crate binds them — and it is the only thing that does.
//!
//! ```text
//! selene-extract → UnresolvedRef rows (pending) ─┐
//!                                                ├→ ReferenceResolver → Edge
//! selene-db (GraphStore: nodes, edges, refs)  ───┘     (this crate)
//! ```
//!
//! # Generic over the store, always
//!
//! Every entry point is generic over `S: GraphStore`. This crate never names
//! `SurrealStore` (it appears in `[dev-dependencies]`, for integration tests,
//! and nowhere else): the store is a seam the resolver's tests mock, not a
//! backend it is coupled to.
//!
//! # Invariants (they are why the resolver is shaped the way it is)
//!
//! - **Validated inference — no edge beats a wrong edge.** Every type guess
//!   ends in `resolve_method_on_type`: the method must actually exist on the
//!   inferred type (or a supertype it conforms to), else no edge at all.
//!   Path-shaped references never fall back to symbol matching (`#660`), and
//!   ubiquitous names decline rather than guess (`#999`).
//! - **Ordering is behavior.** The `resolve_one` ladder, the `match_reference`
//!   strategy order, the ≥ 0.9 return-immediately threshold, first-wins ties —
//!   all of it is observable in the edge output. It is a fixed pipeline, not a
//!   rules engine.
//! - **`ResolvedRef.original` is the stored row, unmutated.** The keyed delete
//!   is what drains the batch loop; a mutated reference name no-ops it and the
//!   run explodes (`#760`).
//! - **Determinism.** Same input ⇒ same edges, same order. `BTreeMap`/`BTreeSet`
//!   wherever iteration order can reach the output.
//! - **Errors are collected, never thrown.** `Err` is a store malfunction; a
//!   reference that finds nothing is an ordinary, successful miss.
//!
//! # Deliberately not ported
//!
//! - **`cooperative-yield.ts`** (`maybeYield`, a 250 ms budget). It exists to
//!   keep Node's event loop responsive for a liveness watchdog we do not have.
//!   Resolution runs off the async runtime (under `spawn_blocking`, like
//!   Phase 2's extraction), so there is nothing to yield to. Not mapped to
//!   `tokio::task::yield_now` — that would be cargo-culting the symptom.
//! - **`import-resolver.ts`'s module-level `importMappingCache`** — declared,
//!   cleared, never written. The real cache is the resolver's LRU.
//!
//! # The framework registry (Part B, Task 11)
//!
//! [`frameworks`] is the seam every framework resolver plugs into. Three things
//! in it are contracts, not implementation details:
//!
//! - **Route ids are opaque; route semantics are indexed fields.** A route node's
//!   id is the ordinary hashed `node_id` — it does **not** encode the method or
//!   path (CodeGraph TS's did, and downstream code key-matched on the string).
//!   The semantics live in `Node::route_method` / `route_path` / `framework`, and
//!   the only supported lookup is the indexed [`find_route`] /
//!   `GraphStore::find_route`. **Never parse or string-build a route id.**
//! - **The route `name` spelling (`"{METHOD} {path}"`) is load-bearing.** The id
//!   hash is over `(file, kind, name, line)`, and several frameworks emit
//!   multiple routes from ONE line (axum chained verbs, rails `resources`), so
//!   `name` is the only thing keeping their ids distinct.
//! - **Emission runs HERE, not in `selene-extract` (decision D2).** The pipeline
//!   is extract → resolve; a framework hook inside the extractor would invert the
//!   layering (and cycle the dependency). [`run_framework_extract`] is a pass over
//!   the already-indexed files, and it runs **before** resolution — a reference
//!   cannot bind to a route node that does not exist yet.
//!
//! `EXTRACTION_VERSION` was bumped **1 → 2** for the route fields. That is
//! Phase 3's **only** bump: no other task in the phase changes output shape.
//!
//! # Deferred to Phase 8 (explicitly not v0)
//!
//! Frameworks: NestJS, SvelteKit, Vue/Nuxt, Vapor, Astro, Play, GoFrame, Drupal,
//! Terraform, CICS, Swift↔ObjC, React Native, Expo, Fabric. The v0 eleven are in
//! [`REGISTRY_ORDER`].
//!
//! Synthesizers: the ~31 passes beyond the five v0 channels (callback/observer,
//! EventEmitter, React re-render, JSX child, Django ORM) — closure-collection,
//! gin-middleware-chain, mybatis-java-xml, rn-event-channel, the rest.
//!
//! # Build status (Phase 3)
//!
//! Landed: the spike (Task 1, `tests/spike_seam.rs`), the skeleton (Task 2),
//! the [`ReferenceResolver`] ladder (Task 3 — built-in filters, the fast
//! pre-filter, the language gates, `create_edges`), imports (4–6), and the
//! framework registry + route contract + strip-comments (Task 11). Still stubs,
//! in ladder order: the name matcher (7–8), chains (9), function refs (10), the
//! eleven framework resolvers (12–20), the synthesizers and the batch driver
//! (Part C).

mod builtins;
mod cache;
mod context;
mod error;
mod families;
pub mod frameworks;
mod imports;
mod resolver;
mod strip_comments;
mod types;

pub use builtins::is_built_in_or_external;
pub use cache::{CACHE_SIZE_ENV, DEFAULT_CACHE_LIMIT, SyncLru, cache_limit, content_cache_limit};
pub use context::{ResolutionContext, StoreContext};
pub use error::{ResolveError, Result};
pub use families::{crosses_known_family, is_known_language_family, same_language_family};
pub use frameworks::{
    FrameworkExtractStats, FrameworkExtraction, FrameworkResolver, REGISTRY_ORDER, RouteSpec,
    all_framework_resolvers, applicable_frameworks, detect_frameworks, detect_frameworks_among,
    find_route, framework_resolver, route_node, route_node_in, run_framework_extract,
    run_framework_extract_for_files, run_post_extract,
};
pub use imports::aliases::{apply_aliases, load_project_aliases};
pub use imports::cpp_includes::load_cpp_include_dirs;
pub use imports::go_module::load_go_module;
pub use imports::mappings::{extract_import_mappings, extract_re_exports};
pub use imports::workspace::{load_workspace_packages, resolve_workspace_import};
pub use imports::{
    REEXPORT_MAX_DEPTH, is_external_import, resolve_import_path, resolve_jvm_import,
    resolve_via_import,
};
pub use resolver::{
    ReferenceResolver, has_any_possible_match, is_php_include_path_ref, matches_any_import,
};
pub use strip_comments::strip_comments_for_regex;
pub use types::{
    AliasMap, AliasPattern, GoModule, ImportMapping, ReExport, ResolutionResult, ResolutionStats,
    ResolvedBy, ResolvedRef, WorkspacePackages,
};
