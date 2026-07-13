//! **The resolution pass driver** (Task 27) — the thing that actually runs this crate.
//!
//! Everything else here is a library. This is the pipeline: the one production path
//! that indexes references into edges. Without it `resolve_one` is a function nobody
//! calls, `run_synthesis` has no tail to hang off, and both gates drive a pipeline
//! composed in a test — which is exactly how this crate shipped **four** seams whose
//! unit tests passed while nothing invoked them.
//!
//! # The pass order IS the contract. It is a fixed sequence, not a pipeline you can configure.
//!
//! ```text
//! 1. run_framework_extract   route/config nodes — BEFORE the context is built
//! 2. StoreContext::new       warms known_names — must therefore come AFTER (1)
//! 3. the ladder              resolve_one over every pending row, batched
//!    ├─ persist edges, delete resolved rows, mark failures
//! 4. conformance passes      chained calls (#750), inherited `this.X` (#808)
//! 5. clear_caches()          the caches predate every edge steps 3-4 just wrote
//! 6. run_synthesis           LAST — every channel correlates nodes with those edges
//! ```
//!
//! Each ordering constraint is load-bearing, and each one fails **silently** if broken:
//!
//! - **(1) before (2).** A `@Value("${app.timeout}")` / `ArticleController@index`
//!   reference is named after a node the *framework pass emits*. `known_names` is warmed
//!   once, in `StoreContext::new`; build the context first and `resolve_one`'s step-3
//!   pre-filter drops every framework reference before any resolver is asked. Five
//!   frameworks bind nothing, and every unit test still passes.
//! - **(4) after (3).** The conformance passes walk `implements`/`extends` edges — the
//!   edges step 3 just persisted. Run them earlier and they walk an empty graph: a pass
//!   that looks like it ran and resolved nothing.
//! - **(5) before (6).** The context's caches predate those edges. A stale cache makes
//!   every synthesizer a silent no-op.
//! - **(6) last.** Every channel correlates nodes with resolved edges (a registrar's
//!   callback argument is a symbol the ladder bound; `<Child/>` is a component the
//!   import resolver reached).
//!
//! # The batch loop reads at offset 0. Always.
//!
//! Processed rows *leave* the pending set — resolved rows are deleted, unresolvable ones
//! are marked `failed` — so an advancing offset would step over rows that shuffled down
//! into the window it already passed. Offset 0, until the set is empty.
//!
//! # ⚠ The non-progress guard, and the 1.4 GB it exists to prevent
//!
//! The keyed delete matches on `(from_node_id, reference_name, reference_kind)` — the
//! **stored row**. A resolver that returns a *mutated* `original.reference_name` (say, a
//! synthetic bare name it invented while chaining) makes that delete match nothing. The
//! row stays pending. The offset-0 loop reads it again, resolves it again, writes the
//! edge again… In the TS build this produced a **5M-edge, 1.4 GB** database on `gin`.
//!
//! So: if a batch does not *reduce* the pending count, the loop **breaks** and reports
//! it. The guard is the backstop, not the fix — the fix is `ResolvedRef.original` being
//! the stored row, unmutated (#760), which is why the Go bare-name chain fallback
//! resolves through a synthetic ref but returns the original one. Both halves are
//! tested: `a_name_mutating_resolver_trips_the_guard_instead_of_looping_forever`.
//!
//! # The async seam: the resolver is SYNC over an ASYNC store
//!
//! [`ResolutionContext`] is sync; `GraphStore` is async; the context bridges them with
//! `Handle::block_on`. **Calling the resolver directly from a tokio worker deadlocks.**
//! So the ladder runs inside `spawn_blocking`/`block_in_place`, with the caches warmed
//! first. That sentence is the whole contract, and it lives here, next to the wrapper
//! that honors it.
//!
//! # `cooperative-yield` is dropped, not ported — but its discipline is kept
//!
//! The TS `maybeYield` (a 250 ms budget) exists to keep Node's event loop responsive for
//! a liveness watchdog we do not have; resolution here runs off the runtime entirely, so
//! there is nothing to yield to. Porting it would be cargo-culting the symptom. What it
//! *protected* is kept in full: chunked inserts ([`PERSIST_CHUNK`]), a bounded pending
//! batch ([`RESOLVE_BATCH`]), and never materializing an unbounded node kind (#610/#1212).

use std::path::Path;

use selene_core::{Edge, UnresolvedRef};
use selene_db::{GraphStore, UnresolvedKey};

use crate::Result;
use crate::context::{ResolutionContext, StoreContext};
use crate::frameworks::{detect_frameworks, run_framework_extract, run_post_extract};
use crate::resolver::ReferenceResolver;
use crate::synth::run_synthesis;
use crate::types::{ResolutionResult, ResolutionStats};

/// How many pending rows one batch reads.
pub const RESOLVE_BATCH: usize = 5000;

/// The sub-transaction chunk for every write. A batch of 5000 becomes five inserts, not
/// one transaction the store has to hold whole.
pub const PERSIST_CHUNK: usize = 1000;

impl<C: ResolutionContext> ReferenceResolver<C> {
    /// Run the ladder over `refs`, tallying by strategy.
    ///
    /// Sync — it must be called from a blocking context (see the module docs on the
    /// async seam).
    pub fn resolve_all(&mut self, refs: &[UnresolvedRef]) -> ResolutionResult {
        let mut out = ResolutionResult::default();
        for r in refs {
            match self.resolve_one(r) {
                Some(hit) => out.push_resolved(hit),
                None => out.push_unresolved(r.clone()),
            }
        }
        out
    }
}

/// **The full-index path.** Detect → emit → resolve every pending reference → conformance
/// → synthesize. This is what an indexer calls.
///
/// `on_progress(done, total)` is called once per batch.
pub async fn resolve_and_persist_batched<S: GraphStore + Clone>(
    store: &S,
    root: &Path,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<ResolutionStats> {
    // Phase timings. A 12k-file repo spends minutes in here; without per-phase spans a
    // regression is a single opaque number and every diagnosis starts by re-deriving them.
    let t_phase = std::time::Instant::now();

    // --- (1) framework emission, BEFORE the context is built ------------------
    // The context warms `known_names` once; these nodes must already exist or every
    // reference named after one of them is pre-filtered away. See the module docs.
    let ctx = StoreContext::new(store_handle(store), root.to_path_buf()).await?;
    tracing::info!(ms = t_phase.elapsed().as_millis(), "resolve/1a: ctx#1 warm");
    let t = std::time::Instant::now();
    let detected = detect_frameworks(&ctx);
    let extract_stats = run_framework_extract(store, &ctx, &detected).await?;
    let post = run_post_extract(store, &ctx, &detected).await?;
    drop(ctx);
    tracing::info!(ms = t.elapsed().as_millis(), "resolve/1b: framework extract");

    // --- (2) the context, over a graph that now HAS the route/config nodes ----
    let t = std::time::Instant::now();
    let ctx = StoreContext::new(store_handle(store), root.to_path_buf()).await?;
    tracing::info!(ms = t.elapsed().as_millis(), "resolve/2: ctx#2 warm");

    let total = store.unresolved_pending_count().await? as usize;
    let mut stats = ResolutionStats::default();
    let mut resolver = ReferenceResolver::new(ctx);
    let mut remaining_before = usize::MAX;
    let mut done = 0usize;

    // --- (3) the batch loop, at offset 0 --------------------------------------
    // Ladder time and persist time get separate clocks on purpose: they have completely
    // different fixes (a hot ladder is cache/query shape; a hot persist is write batching),
    // and one summed number cannot tell you which you have.
    let (mut ms_ladder, mut ms_persist, mut ms_fetch) = (0u128, 0u128, 0u128);
    loop {
        let t = std::time::Instant::now();
        let batch = store.unresolved_pending_batch(0, RESOLVE_BATCH).await?;
        ms_fetch += t.elapsed().as_millis();
        if batch.is_empty() {
            break;
        }

        // The ladder is sync over an async store — it MUST run off the runtime, and so
        // must `create_edges`, which reads every endpoint's kind through the context.
        // Leaving it outside is a `block_on` inside a runtime worker: an instant panic
        // in a test, a deadlock in production. (It bit me here, exactly as the module
        // doc says it would.)
        let t = std::time::Instant::now();
        let (result, edges) = tokio::task::block_in_place(|| {
            let result = resolver.resolve_all(&batch);
            let edges = resolver.create_edges(&result.resolved);
            (result, edges)
        });
        ms_ladder += t.elapsed().as_millis();

        let t = std::time::Instant::now();
        persist(store, &edges, &result).await?;
        ms_persist += t.elapsed().as_millis();

        merge(&mut stats, &result.stats);
        done += batch.len();
        if let Some(cb) = on_progress {
            cb(done.min(total), total);
        }

        // --- the non-progress guard (see the module docs, and the 1.4 GB) -----
        let remaining = store.unresolved_pending_count().await? as usize;
        if remaining >= remaining_before {
            tracing::error!(
                remaining,
                remaining_before,
                "resolution made NO progress across a batch — breaking. A resolver \
                 returned a MUTATED `original.reference_name`, so the keyed delete \
                 matched nothing, the rows stayed pending, and this loop would re-resolve \
                 them forever (5M edges / 1.4 GB in the TS build). `ResolvedRef.original` \
                 must be the STORED row (#760)."
            );
            stats
                .by_method
                .insert("non-progress-guard-tripped".to_string(), 1);
            break;
        }
        remaining_before = remaining;
    }

    tracing::info!(
        ms_fetch,
        ms_ladder,
        ms_persist,
        refs = total,
        "resolve/3: batch loop"
    );

    // --- (4) the conformance passes — AFTER the edges they walk exist ---------
    let t = std::time::Instant::now();
    let (conformance, conformance_edges) = tokio::task::block_in_place(|| {
        let mut hits = resolver.resolve_chained_calls_via_conformance();
        hits.extend(resolver.resolve_deferred_this_member_refs());
        let edges = resolver.create_edges(&hits);
        (hits, edges)
    });
    if !conformance.is_empty() {
        for chunk in conformance_edges.chunks(PERSIST_CHUNK) {
            store.insert_edges(chunk).await?;
        }
        // These refs were already drained from the pending set by the batch loop (they
        // resolved to nothing THEN and were marked failed); the edge is the whole output.
        stats.resolved += conformance.len();
        *stats
            .by_method
            .entry("conformance".to_string())
            .or_insert(0) += conformance.len();
    }

    tracing::info!(ms = t.elapsed().as_millis(), "resolve/4: conformance");

    // --- (5) drop the stale caches, (6) synthesize LAST ------------------------
    // NB: synthesis runs on caches we just dropped — by design (a stale cache makes every
    // pass a silent no-op). So it re-reads the graph cold, and that cost is real. Timed.
    let t = std::time::Instant::now();
    resolver.ctx().clear_caches();
    let synthesized = run_synthesis(store, resolver.ctx())
        .await
        .unwrap_or_else(|e| {
            // Best-effort: a throwing pass degrades to a warning, never a failed index.
            tracing::warn!(error = %e, "synthesis failed — the base graph stands");
            0
        });
    tracing::info!(ms = t.elapsed().as_millis(), synthesized, "resolve/6: synthesis");
    tracing::info!(ms = t_phase.elapsed().as_millis(), "resolve: TOTAL");
    if synthesized > 0 {
        stats
            .by_method
            .insert("callback-synthesis".to_string(), synthesized as usize);
    }

    // --- health: a store outage must not look like "nothing resolved" ---------
    stats.store_read_errors = resolver.ctx().store_read_errors();
    stats.framework_nodes = extract_stats.nodes + post.nodes;
    stats.warnings = extract_stats.warnings;

    Ok(stats)
}

/// **The scoped path** — resolve exactly `refs` (the incremental-sync path, Phase 6).
///
/// No framework emission (the caller re-emitted for the changed files), no synthesis:
/// the channels are whole-graph correlations, so a per-file pass cannot be correct. That
/// gap is inherited from the TS build and recorded in `lib.rs`.
pub async fn resolve_and_persist<S: GraphStore + Clone>(
    store: &S,
    root: &Path,
    refs: &[UnresolvedRef],
) -> Result<ResolutionStats> {
    let ctx = StoreContext::new(store_handle(store), root.to_path_buf()).await?;
    let mut resolver = ReferenceResolver::new(ctx);

    let (result, edges) = tokio::task::block_in_place(|| {
        let result = resolver.resolve_all(refs);
        let edges = resolver.create_edges(&result.resolved);
        (result, edges)
    });
    persist(store, &edges, &result).await?;

    let mut stats = result.stats;
    stats.store_read_errors = resolver.ctx().store_read_errors();
    Ok(stats)
}

// =============================================================================
// Persistence
// =============================================================================

/// Insert the edges, then drain the rows: **delete** what resolved, **mark failed** what
/// did not. Both keyed on the 3-part `(from_node_id, reference_name, reference_kind)` —
/// the spike's F1 finding: a 2-part key drains a `calls` ref and a `function_ref` of the
/// same name from the same node **together**, silently losing recall that no pending-count
/// sweep can detect.
async fn persist<S: GraphStore>(
    store: &S,
    edges: &[Edge],
    result: &ResolutionResult,
) -> Result<()> {
    for chunk in edges.chunks(PERSIST_CHUNK) {
        store.insert_edges(chunk).await?;
    }

    // ⚠ The key is built from `original` — the STORED row, unmutated. A synthetic name
    // here no-ops the delete and the offset-0 loop spins forever (#760).
    let resolved_keys: Vec<UnresolvedKey> = result
        .resolved
        .iter()
        .map(|r| key_of(&r.original))
        .collect();
    for chunk in resolved_keys.chunks(PERSIST_CHUNK) {
        store.delete_resolved(chunk).await?;
    }

    let failed_keys: Vec<UnresolvedKey> = result.unresolved.iter().map(key_of).collect();
    for chunk in failed_keys.chunks(PERSIST_CHUNK) {
        store.mark_failed(chunk).await?;
    }
    Ok(())
}

fn key_of(r: &UnresolvedRef) -> UnresolvedKey {
    (
        r.from_node_id.clone(),
        r.reference_name.clone(),
        r.reference_kind.clone(),
    )
}

fn merge(into: &mut ResolutionStats, from: &ResolutionStats) {
    into.total += from.total;
    into.resolved += from.resolved;
    into.unresolved += from.unresolved;
    for (k, v) in &from.by_method {
        *into.by_method.entry(k.clone()).or_insert(0) += v;
    }
}

/// `StoreContext` takes the store **by value** (its sync strategy layer `block_on`s it),
/// while the driver keeps writing through its own handle. A `GraphStore` clone is a
/// refcount bump onto the same database — not a second connection — so both handles see
/// exactly one graph. That is why the driver requires `S: Clone`.
fn store_handle<S: GraphStore + Clone>(store: &S) -> S {
    store.clone()
}
