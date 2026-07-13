//! `selene-resolve` — cross-file resolution: the `ReferenceResolver` ladder,
//! import and name matching, the framework registry, and the dynamic-dispatch
//! synthesizers.
//!
//! Target design: `docs/specs/2026-07-11-rust-graph-db-migration-design.md`
//! (PRD §2, §3); build plan: `docs/plans/2026-07-13-phase3-selene-resolve.md`;
//! the TS parity source is mapped in
//! `docs/reference/from-codegraph/maps/resolution.md` and
//! `docs/reference/from-codegraph/maps/frameworks-synth.md`.
//!
//! # Where this crate sits
//!
//! `selene-extract` (Phase 2) emits **zero cross-file edges**: every reference
//! that reaches beyond its own file leaves as a `selene_core::UnresolvedRef`.
//! This crate binds them — and it is the only thing that does.
//!
//! ```text
//! selene-extract → UnresolvedRef rows (pending) ─┐
//!                                                ├→ ReferenceResolver → Edge
//! selene-db (GraphStore: nodes, edges, refs)  ───┘     (this crate)
//! ```
//!
//! # Driving it: the pipeline a Phase-4 consumer runs
//!
//! There is no single `resolve_everything()` entry point — the **order is the
//! contract**, and it is written here rather than hidden inside one function that
//! a caller would be tempted to reorder. `selene-graph` (Phase 4) and the two
//! gates all drive exactly this:
//!
//! ```text
//! 1. selene-extract indexes the project            → nodes, same-file edges, refs
//! 2. detect_frameworks(&ctx)                       → once per index, never per file
//! 3. run_framework_extract(store, &ctx, &detected) → route/config nodes + their refs
//! 4. REBUILD the StoreContext                      ← see below; skipping this is silent
//! 5. ReferenceResolver::resolve_one over the pending refs
//!    then resolve_chained_calls_via_conformance() and
//!         resolve_deferred_this_member_refs()      → the two second passes
//! 6. create_edges(&resolved) → store.insert_edges  → PERSIST
//! 7. run_synthesis(store, &ctx)                    → the heuristic dispatch bridges
//! ```
//!
//! Two orderings in that list are load-bearing, and both fail **silently** when
//! they are wrong — which is why they are stated, not merely implied:
//!
//! - **Step 4, the context rebuild.** [`StoreContext`] warms its caches
//!   (`known_names` above all) at construction. Step 3 writes *new nodes*. A
//!   context built before them does not know they exist, so the ladder's
//!   step-3 pre-filter drops every framework reference as "that name matches no
//!   symbol" — and every unit test stays green, because `FakeContext` has no
//!   cache to be stale.
//! - **Step 7, synthesis last.** The callback channel finds its registration sites
//!   by reading the `calls` edges **into** the registrar, and those edges do not
//!   exist until step 6 has written them. A driver that ran synthesis earlier would
//!   see an empty graph and synthesize nothing — silently, with the coverage gate
//!   still green.
//!
//! # Invariants (they are why the resolver is shaped the way it is)
//!
//! - **Validated inference — no edge beats a wrong edge.** Every type guess ends
//!   in `resolve_method_on_type`: the method must actually exist on the inferred
//!   type (or a supertype it conforms to), else **no edge at all**. A wrong edge is
//!   a wrong answer the agent will trust; a missing one merely leaves the map
//!   incomplete.
//! - **Path-shaped references never fall back to symbol matching** (`#660`). A PHP
//!   `include 'inc/db.php'` binds to a FILE or to nothing — never to some unrelated
//!   `db.php` symbol elsewhere in the tree.
//! - **Ubiquitous names decline rather than guess** (`#999`). Past
//!   `AMBIGUOUS_NAME_CEILING` candidates, the matcher returns nothing.
//! - **Dispatch bridges are end-to-end or not at all** (PRD §8.2). A half-bridged
//!   flow is *worse* than none: it advertises a hop the agent then has to Read to
//!   finish. This is not a slogan — it is why `react-render` shipped **dormant**
//!   until `jsx-render` landed (a re-render that cannot find its children stops at
//!   `render`, which answers nothing).
//! - **Ordering is behavior.** The `resolve_one` ladder, the strategy order, the
//!   ≥ 0.9 return-immediately threshold, first-wins ties, the synthesizer pass
//!   order (the cross-pass dedupe is first-wins) — all of it is observable in the
//!   edge output. It is a fixed pipeline, not a rules engine.
//! - **`ResolvedRef.original` is the stored row, unmutated.** The keyed delete is
//!   what drains the batch loop; a mutated reference name no-ops it (`#760`).
//! - **Determinism.** Same input ⇒ same edges, same order. `BTreeMap`/`BTreeSet`
//!   wherever iteration order can reach the output.
//! - **Errors are collected, never thrown.** `Err` is a store malfunction; a
//!   reference that finds nothing is an ordinary, successful miss.
//!
//! # The lesson this crate paid for: an inert seam looks exactly like a working one
//!
//! Ladder step 8 (`resolve_via_import`) was **inert in production for three
//! commits**. `ResolutionContext::import_mappings()` and `re_exports()` were stubs
//! that returned empty vectors — and an empty list does not fail. It silently
//! no-ops: every import in the project resolved to nothing, and the resolver
//! reported a clean, successful run.
//!
//! Every strategy test stayed green, because `FakeContext` **injects** the mappings
//! instead of loading them. The tests were testing the strategy; nothing was testing
//! the seam beneath it.
//!
//! > **A seam that returns "nothing found" is indistinguishable from a seam that
//! > works and found nothing.** Only a gate that drives the *real* context against
//! > the *real* store can tell them apart.
//!
//! That is the reason both gates below exist in the shape they do, and the reason
//! `tests/store_context_test.rs` and `store_resolution_e2e_test.rs` load from disk
//! rather than from a fixture struct. If you add a `ResolutionContext` method,
//! assume it is a stub until a test proves otherwise **through `StoreContext`**.
//!
//! # The two gates — they are the contract, not the tests
//!
//! They measure different failures, and neither substitutes for the other.
//!
//! **`tests/resolution_parity_gate.rs` — edge IDENTITY vs the TS engine, tolerance 0.**
//! It dumps the real CodeGraph pipeline over a shared fixture corpus and compares
//! *semantic* edge identity — `(source, target, kind, provenance, synthesizedBy)` —
//! not counts. A count gate cannot see a resolver that binds the right *number* of
//! edges to the **wrong targets**, which is exactly what a port under count-pressure
//! produces. It cannot see whether a *flow* is usable; it only sees whether we agree
//! with TS edge for edge.
//!
//! **`tests/dispatch_coverage_gate.rs` — whole FLOWS, not edges.**
//! It asserts a path runs from each entry point (a route, a click, an emit) to the
//! terminal an agent would otherwise have opened a file to find, and that every
//! required hop lies *on* that path. A 3-of-4-hop bridge **fails**. `Contains` is
//! excluded from the default traversal (`FLOW_KINDS`), so no flow can go green by
//! walking file→class→method structure while the dispatch bridge it exists to prove
//! is missing — with containment allowed, a path can descend from a file node into
//! any symbol in that file, which is a false-green machine. The one sanctioned
//! exception is `CBV_FLOW_KINDS` (class-based dispatch: a Django CBV route references
//! the *class*, and the hop to its handler method genuinely **is** containment — an
//! agent tracing that flow makes the same move). Function-based views are asserted
//! with the strict kinds, so both shapes stay proven. This gate cannot see a wrong
//! edge that happens to keep the path connected — the parity gate can.
//!
//! Both halves are **completeness-keyed**, which is what stops a quiet opt-out: a
//! framework in [`REGISTRY_ORDER`] with no flow row fails `every_registered_framework_is_gated`,
//! and a channel in [`synth::registered_synthesizers`] with no flow row fails
//! `every_registered_synthesizer_is_gated`. The synthesizers additionally carry a
//! **precision** half — `synthesis_emits_nothing_on_the_controls` — because every
//! positive assertion here is satisfied by a synthesizer that bridges *everything*,
//! and only a control (ordinary code containing none of the dispatch shapes) catches
//! one that guesses. A channel that guesses is worse than one that misses: a wrong
//! dispatch edge is a confident lie about how the program runs.
//!
//! Route edges are compared **semantically** (`framework, method, path, file, line`),
//! never by id spelling: TS ids are literal strings and ours are hashes, so an
//! id diff would report 100% divergence and tell you nothing (see below).
//!
//! # The route-node contract — where Phase 3 departs from the TS wire shape
//!
//! CodeGraph TS encoded a route's semantics **into its id string**
//! (`route:{file}:{line}:{METHOD}:{path}`) and key-matched on that string
//! downstream. We do not. A route node's id is the **ordinary hashed
//! `selene_core::node_id`** — Phase 2's contract, no exceptions — and the semantics
//! live in first-class **indexed fields**: `route_method`, `route_path`, `framework`
//! (`file`/`line` are already `file_path`/`start_line`; they are not duplicated).
//!
//! Every lookup is therefore an indexed query — [`find_route`] /
//! `GraphStore::find_route` — and **never** id-string parsing. Any future consumer
//! must query the fields.
//!
//! One consequence is sharp enough to state: the id hash is over
//! `(file, kind, name, start_line)` and does **not** include the route fields.
//! Frameworks that emit several routes from one source line (axum
//! `.route("/x", get(h).post(h2))`, rails `resources` → 7 actions, stacked flask
//! decorators) are separated **only by `name`** — so the `"{METHOD} {path}"` name
//! spelling is the uniqueness key, not decoration. Name a route by its path alone
//! and it silently overwrites its siblings.
//!
//! # Deviation ledger — `tests/fixtures/dispatch/deviations.toml` is the authority
//!
//! Every intentional TS↔Rust divergence lives **there**, one entry each, with the
//! exact edge and a cited reason. It is machine-checked from both sides: an entry
//! matching no observed difference **fails the gate as stale**, so a fixed divergence
//! cannot leave a whitelist behind that silently re-permits a regression.
//!
//! **Do not record deviations anywhere else** — not in a commit message, not in a
//! code comment. A second list would drift out of sync with the one the gate
//! enforces, and the drifted copy is the one the next reader believes.
//!
//! As of the Phase 3 gate it holds **four** entries, in two families — and both are
//! cases where *we* are the more correct side:
//!
//! 1. **Next.js file routes (×2, `pages/` and `app/`).** We emit a route → page-component
//!    edge; TS emits the route node and **nothing from it**. By this phase's own
//!    invariant, TS's route is the half-bridge — it advertises an entry point that
//!    reaches nothing, so an agent following it lands on a route node and must Read
//!    the file anyway. Ours closes the hop.
//! 2. **Python module-source imports (×2, flask and fastapi).** For
//!    `from services import create_article` both engines bind the *symbol*; we
//!    additionally resolve the module source to the module's **file**, giving a
//!    file→file dependency edge. That is a true edge — `app.py` genuinely depends on
//!    `services.py`, and file→file edges are what impact analysis walks — so it is a
//!    superset, not a wrong answer.
//!
//! The ledger's header also records two things that are deliberately **not** entries,
//! and reading it before adding one is worth the minute: a divergence that turned out
//! to be **our** bug (the default-import binding — TS was consistent; we were not),
//! and an "absence" that was only an unrun pass. Neither deserved a whitelist.
//!
//! # Deferred — each with its phase and its reason
//!
//! - **Wave-2 frameworks → Phase 8.** SvelteKit, Vue/Nuxt, Vapor, NestJS, Astro,
//!   Play, Drupal, Terraform, CICS, GoFrame, and the Swift↔ObjC / React-Native /
//!   Expo / Fabric bridges. The roadmap scopes Phase 3 to the **v0 languages'**
//!   frameworks; the eleven that ship are in [`REGISTRY_ORDER`].
//! - **The ~30 synthesizer channels beyond the v0 four → Phase 8** — and this is a
//!   *design* decision, not a backlog. The four that ship (`callback`,
//!   `event-emitter`, `react-render`, `jsx-render`) are the ones the playbook
//!   validated as closing a **canonical flow end-to-end**. Adding a channel without
//!   the hops that complete its flow is precisely the "partial coverage is worse than
//!   none" failure — `interface-impl`, `go-implements`, `cpp-override`,
//!   `closure-collection` and the rest each need their completing hops measured
//!   before they are worth shipping.
//! - **The Django ORM descriptor is the fifth v0 dispatch bridge — but it is a
//!   RESOLVER, not a synthesizer.** The roadmap's "all 5 synthesizers" phrasing is
//!   loose and has already misled one plan. `_iterable_class` is an *attribute name*,
//!   so it is a **named** reference: it goes through `claims_reference` + `resolve()`
//!   and yields an ordinary `tree-sitter` edge with `resolved_by: Framework` —
//!   **no `heuristic` provenance, no `synthesizedBy`**. Anonymous dispatch (`cb()`,
//!   `emit('e')`, `<Child/>`) is what gets a whole-graph pass. Conflating the two
//!   mechanisms is the mistake the playbook exists to prevent.
//! - **Incremental-sync re-synthesis → Phase 6** (with `selene-sync`), matching the
//!   known TS gap. [`synth::run_synthesis`] runs on the **full-index path only**, so a
//!   callback registered (or removed) since the last full index is not reflected until
//!   the next one. The channels are whole-graph by nature — the correlation is
//!   cross-file — so a per-file pass cannot simply be substituted; it is a decision,
//!   not an omission.
//! - **A batched persist driver (`resolve_and_persist_batched`) → lands separately, on
//!   this same phase branch.** As of *this* commit it does not exist, and that is
//!   stated plainly rather than papered over. TS's
//!   `resolveAndPersistBatched(onProgress, batchSize = 5000)` streams pending refs in
//!   batches and drains each with the keyed delete; consumers today drive the seven
//!   steps above directly (as both gates do). The pieces it composes — `resolve_one`,
//!   the two second passes, `create_edges`, `delete_resolved`, `run_synthesis` — are
//!   all public, so it is a convenience wrapper, not a capability gap.
//!
//!   It is also the **fourth** candidate for the inert-seam failure above, and it is
//!   worth naming as such *before* it lands: a batch driver that silently drains zero
//!   refs per batch — a keyed delete that no-ops on a mutated `original`, a loop whose
//!   pending query returns empty — looks exactly like one that ran and found nothing.
//!   Whoever lands it owes it a test that drives the **real** store, not a `FakeContext`.
//! - **`cooperative-yield.ts` → dropped, not ported.** It is a Node event-loop
//!   artifact: `maybeYield` exists so a liveness-watchdog heartbeat can fire mid-pass
//!   on a single-threaded event loop (`#850`/`#1091`). Rust resolution runs off the
//!   async runtime, so there is nothing to starve and nothing to yield to; mapping it
//!   to `tokio::task::yield_now` would be cargo-culting the symptom. **The discipline
//!   it protected IS ported** — chunked edge inserts, streaming
//!   `nodes_by_kind_page` rather than materializing an unbounded kind (`#610`/`#1212`
//!   OOM), cheap `contains` pre-gates before expensive regexes (`#1235`) — and that
//!   distinction is the whole note: the *yielding* was a workaround, the *batching*
//!   was the point.
//! - **`import-resolver.ts`'s module-level `importMappingCache` → dead in TS, not
//!   ported.** Declared and cleared, never written or read. The real cache is the
//!   resolver's LRU.
//!
//! # Public-interface ledger
//!
//! Every item in `maps/resolution.md` §Public interface and
//! `maps/frameworks-synth.md` §Public interface is either ported or explicitly
//! deferred. An item that is neither is an item nobody decided about.
//!
//! | TS | Rust |
//! |---|---|
//! | `ReferenceResolver` + `createResolver` | [`ReferenceResolver`] (`new` / `with_frameworks`) |
//! | `initialize()` (detectFrameworks + clearCaches) | folded into [`ReferenceResolver::new`] — frameworks are detected once, at construction |
//! | `resolveOne` | [`ReferenceResolver::resolve_one`] |
//! | `createEdges` | [`ReferenceResolver::create_edges`] |
//! | `resolveChainedCallsViaConformance` / `resolveDeferredThisMemberRefs` | [`ReferenceResolver::resolve_chained_calls_via_conformance`] / [`ReferenceResolver::resolve_deferred_this_member_refs`] |
//! | `getDetectedFrameworks` | [`ReferenceResolver::detected_frameworks`] |
//! | `runPostExtract` | [`run_post_extract`] |
//! | `warmCaches` / `clearCaches` / `warmCachesYielding` | **not ported** — [`StoreContext`] warms at construction and is rebuilt (not mutated) when the graph changes; the yielding variant dies with `cooperative-yield` |
//! | `resolveAll` / `resolveAndPersist` / `resolveAndPersistListYielding` | **not ported** — thin loops over `resolve_one`; the caller owns the loop (see the pipeline above) |
//! | `resolveAndPersistBatched` | **deferred** — see the deferrals above |
//! | `UnresolvedRef` / `ResolvedRef` / `ResolutionResult` / `ResolutionStats` | `selene_core::UnresolvedRef` / [`ResolvedRef`] / [`ResolutionResult`] / [`ResolutionStats`] |
//! | `ResolvedRef.resolvedBy` union | [`ResolvedBy`] |
//! | `ResolutionContext` (fat interface, `?` optional methods) | [`ResolutionContext`] — required trait methods; [`StoreContext`] is the production impl |
//! | `ImportMapping` / `ReExport` | [`ImportMapping`] / [`ReExport`] |
//! | `matchReference` + the strategy fns | [`match_reference`], [`match_by_exact_name`], [`match_by_qualified_name`], [`match_by_file_path`], [`match_fuzzy`], [`match_method_call`], [`match_function_ref`] |
//! | `matchCppCallChain` / `matchScopedCallChain` / `matchDottedCallChain` | [`match_cpp_call_chain`] / [`match_scoped_call_chain`] / [`match_dotted_call_chain`] |
//! | `resolveMethodOnType` | [`resolve_method_on_type`] |
//! | `preferCallSiteFile` | [`prefer_call_site_file`] |
//! | `sameLanguageFamily` / `isKnownLanguageFamily` / `crossesKnownFamily` | [`same_language_family`] / [`is_known_language_family`] / [`crosses_known_family`] |
//! | `resolveImportPath` / `resolveViaImport` / `resolveJvmImport` | [`resolve_import_path`] / [`resolve_via_import`] / [`resolve_jvm_import`] |
//! | `FrameworkResolver` (name/languages/detect/resolve/claimsReference/extract/postExtract) | [`FrameworkResolver`] — same seven, `claims_reference` / `extract` / `post_extract` defaulted |
//! | `getAllFrameworkResolvers` / `getFrameworkResolver` / `detectFrameworks` / `getApplicableFrameworks` | [`all_framework_resolvers`] / [`framework_resolver`] / [`detect_frameworks`] / [`applicable_frameworks`] |
//! | `registerFrameworkResolver` (replace-by-name, mutable registry) | **not ported** — the registry is a static ordered table ([`REGISTRY_ORDER`]); tests inject via [`detect_frameworks_among`] instead of mutating global state |
//! | `synthesizeCallbackEdges` | [`synth::run_synthesis`] (+ [`synth::registered_synthesizers`], derived from the one declared pass order) |
//! | `getCargoWorkspaceCrateMap` | `frameworks::cargo` |
//! | `FACADE_MAPPINGS` (laravel) | `frameworks::laravel` |
//! | `stripCommentsForRegex` | [`strip_comments_for_regex`] |
//! | the ~30 non-v0 synthesizer channels | **Phase 8** — see the deferrals |
//! | `swift-objc-bridge` / `c-fnptr-synthesizer` / `goframe-synthesizer` | **Phase 8** (wave-2 languages) |
//!
//! # Build status
//!
//! Accurate as of this commit — and kept that way on purpose. The section this
//! replaced still called the frameworks and the synthesizers "stubs" long after both
//! had shipped, which is the same failure mode as an inert seam: a statement that
//! costs nothing to leave wrong, and that the next reader believes.
//!
//! **Shipped and gated:** the `resolve_one` ladder, import resolution, the name
//! matcher, chained calls, function refs, the two second passes, **eleven** framework
//! resolvers ([`REGISTRY_ORDER`]), **four** dynamic-dispatch synthesizer channels
//! ([`synth::SYNTH_PASS_ORDER`]) plus the Django ORM descriptor (a *resolver*, see the
//! deferrals), and both gates — parity GREEN at tolerance 0, coverage GREEN with every
//! framework and every channel carrying a closed end-to-end flow.
//!
//! **Not here yet:** the batched persist driver (`resolve_and_persist_batched`), which
//! lands separately on this phase branch — see the deferrals. Nothing else in the
//! ledger above is outstanding.
//!
//! `selene-graph` (Phase 4) consumes this crate: it drives the seven-step pipeline
//! above and walks the edges the ladder and the synthesizers produced.

mod builtins;
mod cache;
mod context;
mod error;
mod families;
pub mod frameworks;
mod imports;
mod matcher;
mod passes;
mod resolver;
mod strip_comments;
pub mod synth;
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
pub use matcher::chains::{
    CHAIN_LANGUAGES, is_deferrable_chain, match_cpp_call_chain, match_dotted_call_chain,
    match_scoped_call_chain,
};
pub use matcher::fnref::{match_function_ref, resolve_this_member_fn_ref};
pub use matcher::match_reference;
pub use matcher::method::{match_method_call, resolve_method_on_type};
pub use matcher::names::{
    match_by_exact_name, match_by_file_path, match_by_qualified_name, match_fuzzy,
};
pub use matcher::receiver::{
    infer_cpp_receiver_type, infer_java_field_receiver_type, infer_local_receiver_type,
};
pub use matcher::scoring::{
    AMBIGUOUS_NAME_CEILING_ENV, DEFAULT_AMBIGUOUS_NAME_CEILING, ambiguous_name_ceiling,
    find_best_match, path_proximity, pick_closest_file_node, prefer_call_site_file,
};
pub use resolver::{
    ReferenceResolver, has_any_possible_match, is_php_include_path_ref, matches_any_import,
};
pub use strip_comments::strip_comments_for_regex;
pub use synth::{
    INSERT_CHUNK, LineIndex, SYNTH_PASS_ORDER, SynthPassDef, SynthRunFn, registered_synthesizers,
    run_synthesis, run_synthesis_with, stream_nodes_by_kind, synth_passes,
};
pub use types::{
    AliasMap, AliasPattern, GoModule, ImportMapping, ReExport, ResolutionResult, ResolutionStats,
    ResolvedBy, ResolvedRef, WorkspacePackages,
};
