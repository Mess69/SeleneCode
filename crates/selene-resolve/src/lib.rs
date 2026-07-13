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
//! # Build status (Phase 3)
//!
//! Task 1 (the spike, `tests/spike_seam.rs`) and Task 2 (this skeleton) are
//! landed. The strategies themselves — the `resolve_one` ladder, imports, the
//! name matcher, chains, function refs, the framework registry, the
//! synthesizers, and the batch driver — are Tasks 3–33.

mod cache;
mod context;
mod error;
mod families;
mod types;

pub use cache::{CACHE_SIZE_ENV, DEFAULT_CACHE_LIMIT, SyncLru, cache_limit, content_cache_limit};
pub use context::{ResolutionContext, StoreContext};
pub use error::{ResolveError, Result};
pub use families::{crosses_known_family, is_known_language_family, same_language_family};
pub use types::{
    AliasMap, AliasPattern, GoModule, ImportMapping, ReExport, ResolutionResult, ResolutionStats,
    ResolvedBy, ResolvedRef, WorkspacePackages,
};
