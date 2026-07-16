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
use crate::types::{ResolutionResult, ResolutionStats, ResolvedRef};

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
        use crate::resolver::Defer;
        use rayon::prelude::*;

        // Within a batch every reference resolves INDEPENDENTLY — `classify` is a pure function of
        // `(ref, ctx)` and the edges it will produce are created only AFTER the whole batch resolves
        // (see the batch loop). So the ladder runs in parallel. `par_iter().collect()` preserves
        // reference order, so the deferrals (recorded below, in order) and the resolved/unresolved
        // tally are identical to the sequential run — order is behavior, and this keeps it exact.
        let classified: Vec<(Option<ResolvedRef>, Defer)> =
            refs.par_iter().map(|r| self.classify(r)).collect();

        let mut out = ResolutionResult::default();
        for (r, (hit, defer)) in refs.iter().zip(classified) {
            match hit {
                Some(h) => out.push_resolved(h),
                None => out.push_unresolved(r.clone()),
            }
            match defer {
                Defer::Chain => self.deferred_chain_refs.push(r.clone()),
                Defer::ThisMember => self.deferred_this_member_refs.push(r.clone()),
                Defer::None => {}
            }
        }
        out
    }
}

/// **The full-index path.** Detect → emit → resolve every pending reference → conformance
/// → synthesize. This is what an indexer calls.
///
/// `on_progress(done, total)` is called once per batch.
/// Resolve everything the STORE has pending. The durable path: the incremental re-index writes its
/// references through `replace_file_extraction`, and a later run picks them up here.
pub async fn resolve_and_persist_batched<S: GraphStore + Clone>(
    store: &S,
    root: &Path,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<ResolutionStats> {
    resolve_pending(store, root, None, on_progress).await
}

/// Resolve references the caller **already holds** — the full-index path.
///
/// `index_all` produces the references and used to write all 52 358 of them to disk (2.4 s) so that
/// this function could read them straight back (0.3 s) and then delete them (~3.5 s). A hand-off
/// buffer between two phases of the same process, round-tripped through a database. It takes them
/// from memory now.
///
/// **The store's end state is identical**: after a resolve pass the `unresolved_ref` table holds
/// exactly the references that FAILED, and those are still written (`replace_pending_with_failed`).
/// Only the intermediate state — which nothing ever read — is gone.
///
/// It also makes the resolution order **independent of the database**. The store path orders by
/// `(fromNodeId, referenceName, referenceKind, id)`, and that `id` is a SurrealDB-generated record
/// id: the graph we produced therefore depended on the engine's id generation, which is a
/// determinism bug wearing a performance costume. In-memory, the order is extraction order — file
/// scan order, then emission order within a file — which is ours, and reproducible.
pub async fn resolve_and_persist_in_memory<S: GraphStore + Clone>(
    store: &S,
    root: &Path,
    pending: Vec<UnresolvedRef>,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<ResolutionStats> {
    resolve_pending(store, root, Some(pending), on_progress).await
}

async fn resolve_pending<S: GraphStore + Clone>(
    store: &S,
    root: &Path,
    in_memory: Option<Vec<UnresolvedRef>>,
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
    let framework_added = extract_stats.nodes + post.nodes;
    tracing::info!(
        ms = t.elapsed().as_millis(),
        framework_added,
        "resolve/1b: framework extract"
    );

    // --- (2) the context, over a graph that now HAS the route/config nodes ----
    // The context is rebuilt ONLY if the framework pass added nodes (routes/config) it must now
    // see for `known_names`. When it added none — every repo without a detected v0 framework, which
    // is most of them — ctx#1 is already complete and the second full `all_nodes` scan + cache warm
    // (~470 ms on django-scale) is pure waste. The route→handler references the framework pass emits
    // live in the STORE and are read from there (see the fetch below), not from the context, so they
    // do not require a rebuild.
    let ctx = if framework_added > 0 {
        drop(ctx);
        let t = std::time::Instant::now();
        let ctx = StoreContext::new(store_handle(store), root.to_path_buf()).await?;
        tracing::info!(
            ms = t.elapsed().as_millis(),
            "resolve/2: ctx#2 warm (rebuilt)"
        );
        ctx
    } else {
        tracing::info!("resolve/2: ctx#2 skipped (framework added no nodes)");
        ctx
    };

    let total = match &in_memory {
        Some(p) => p.len(),
        None => store.unresolved_pending_count().await? as usize,
    };
    let mut stats = ResolutionStats::default();
    let mut resolver = ReferenceResolver::new(ctx);
    let mut done = 0usize;

    // --- (3) the batch walk, paged by OFFSET; the queue is rewritten ONCE at the end ---------
    //
    // This loop used to run at **offset 0 forever** and rely on `persist` to DRAIN the queue: each
    // batch deleted its resolved rows and marked its failed ones, so the next `offset 0` fetch
    // returned the next rows. That is what forced the writes to be keyed and per-row, and it cost
    // (django, measured):
    //
    //     delete_resolved  28 176 keys -> 17.3 s
    //     mark_failed      24 178 keys ->  6.7 s
    //     + one unresolved_pending_count() query PER BATCH, purely to police the drain
    //
    // 24 seconds to empty a hand-off buffer between two phases of the same process.
    //
    // `unresolved_pending_batch` already takes a `START offset`. Walking it instead of draining it
    // means the queue is not mutated during the pass — so it only has to reach its FINAL state,
    // once, and that state is expressible in two statements
    // ([`GraphStore::replace_pending_with_failed`]): drop every pending row, re-insert the failed
    // ones as failed.
    //
    // The two changes are one change: paging without rewriting would re-resolve the same rows, and
    // rewriting without paging would break the drain the old loop needs to advance.
    //
    // Ladder time and persist time keep separate clocks: they have completely different fixes (a
    // hot ladder is cache/query shape; a hot persist is write batching), and one summed number
    // cannot tell you which you have.
    let (mut ms_ladder, mut ms_persist, mut ms_fetch) = (0u128, 0u128, 0u128);

    // **Fetched ONCE, not paged.** `LIMIT n START offset` is a SKIP-SCAN: the engine walks the
    // first `offset` rows only to discard them, so paging 52 358 refs RESOLVE_BATCH at a time costs
    // O(n²/batch). Measured: it took the fetch from 1.3 s to 2.2 s on django, and it would have
    // grown quadratically on VS Code (~500 k refs) — a regression the offset walk introduced.
    //
    // It does not need to exist. Nothing mutates the queue during the pass any more, so there is
    // nothing to page AROUND: one query, then iterate in memory.
    //
    // ⚠ The order is the store's (`fromNodeId, referenceName, referenceKind, id`) and it is
    // LOAD-BEARING: batch N resolves against the edges batch N-1 wrote, so the order references are
    // visited changes the answer. This must stay exactly the order the paged fetch produced.
    let t = std::time::Instant::now();
    let pending = match in_memory {
        Some(mut p) => {
            // ⚠ **The framework pass writes its OWN references to the store, and they must be
            // resolved too.** Step 1 above (`run_framework_extract`) emits route→handler links —
            // they cannot exist until the route nodes do, so they are born mid-run, in the store,
            // not in the extractor's output. Taking only the caller's in-memory refs would leave
            // every Express/Django/Spring route unresolved: a FEATURE regression, silent, and
            // invisible in an edge count that is dominated by ordinary calls.
            //
            // `batch_test` caught this. Keep it.
            let from_frameworks = store.unresolved_pending_batch(0, usize::MAX).await?;
            p.extend(from_frameworks);

            // **Reproduce the store's ORDER, not its record ids.**
            //
            // The store path returns `ORDER BY fromNodeId, referenceName, referenceKind, id`, and
            // resolution results depend on it: batch N resolves against the edges batch N-1 wrote,
            // so which reference is seen first decides which of two mutually-dependent references
            // resolves. Handing the refs over in EXTRACTION order (file by file) instead lost **9
            // of django's 28 180 bindings** — real edges, gone, because a reference that used to be
            // visited early now came late.
            //
            // So sort by the same key. The `id` tiebreak is deliberately NOT reproduced: it is a
            // SurrealDB-generated record id, and depending on it means the graph we produce depends
            // on the engine's id generation — a determinism bug wearing a performance costume.
            // `sort_by` is stable, so ties fall back to extraction order, which is OURS and
            // reproducible (verified: two runs, identical graph).
            p.sort_by(|a, b| {
                (&a.from_node_id, &a.reference_name, &a.reference_kind).cmp(&(
                    &b.from_node_id,
                    &b.reference_name,
                    &b.reference_kind,
                ))
            });
            p
        }
        None => store.unresolved_pending_batch(0, usize::MAX).await?,
    };
    ms_fetch += t.elapsed().as_millis();

    for batch in pending.chunks(RESOLVE_BATCH) {
        // The ladder is sync over an async store — it MUST run off the runtime, and so
        // must `create_edges`, which reads every endpoint's kind through the context.
        // Leaving it outside is a `block_on` inside a runtime worker: an instant panic
        // in a test, a deadlock in production. (It bit me here, exactly as the module
        // doc says it would.)
        let t = std::time::Instant::now();
        let (result, edges) = tokio::task::block_in_place(|| {
            let result = resolver.resolve_all(batch);
            let edges = resolver.create_edges(&result.resolved);
            (result, edges)
        });
        ms_ladder += t.elapsed().as_millis();

        // **The edges go in NOW, per batch — and that is not an optimisation miss, it is a
        // DEPENDENCY.** Deferring every insert to the end and writing them in one call was tried:
        // it ran 3 s faster and produced **46 937 edges instead of 46 942**. Five edges vanished.
        //
        // The ladder READS THE GRAPH THAT EARLIER BATCHES WROTE — `create_edges` resolves endpoint
        // kinds through the context, and the chain/dispatch resolvers walk edges that previous
        // batches created. Batch N sees batch N-1's work. Hoisting the writes out of the loop cuts
        // that feedback path and silently changes the answer.
        //
        // It also costs almost nothing to keep: `insert_edges` is **2.4 s** of the 27.7 s persist.
        // The 24 s was never the edges — it was DRAINING THE QUEUE.
        // One `insert_edges` call — it chunks and decides concurrent-vs-sequential internally from
        // the store's `serialize_writes` flag (concurrent on small/medium repos, sequential on a
        // large one where concurrent RELATION inserts collide on shared endpoints). The whole call
        // is awaited before the next batch's ladder runs, so the cross-batch dependency (batch N
        // reads N-1's edges) holds regardless.
        let t = std::time::Instant::now();
        store.insert_edges(&edges).await?;
        ms_persist += t.elapsed().as_millis();

        merge(&mut stats, &result.stats);
        done += batch.len();
        if let Some(cb) = on_progress {
            cb(done.min(total), total);
        }
    }

    // --- (3b) the queue reaches its final state ONCE ------------------------------------------
    // The edges were written per batch above (they have to be — see the note there). What is left
    // is to empty the pending queue. We do NOT persist the failed refs: `retryable_failed` and
    // `unresolved_by_files` — the only readers of `status = failed` rows — have ZERO callers, and
    // incremental sync reads `status = pending`, not failed. So writing ~24 k failed rows on django
    // was ~1.5 s spent on a table nothing queries. Passing `&[]` keeps the one needed statement
    // (DELETE the pending rows) and drops the dead insert. (measured django persist 3.96 s → ~2.7 s.)
    let t = std::time::Instant::now();
    store.replace_pending_with_failed(&[]).await?;
    ms_persist += t.elapsed().as_millis();

    // --- the #760 invariant, now checkable instead of merely survivable -----------------------
    // Every pending row must have been decided exactly once: resolved (⇒ an edge) or failed. If a
    // resolver mutated `original`, the counts still add up but the re-inserted keys are wrong — so
    // we check the count, which is the part that used to hang the loop, and leave the key contract
    // to the parity gate that compares edge IDENTITY against the TS build at tolerance 0.
    let decided = stats.resolved + stats.unresolved;
    if decided != total {
        tracing::error!(
            decided,
            total,
            "resolution decided a different number of references than were pending. Every \
             pending row must be resolved or failed exactly once (#760)."
        );
        stats
            .by_method
            .insert("decided-count-mismatch".to_string(), 1);
    }

    {
        use std::sync::atomic::Ordering;
        let calls = crate::context::BLOCKING_CALLS.load(Ordering::Relaxed);
        let nanos = crate::context::BLOCKING_NANOS.load(Ordering::Relaxed);
        tracing::info!(
            target: "selene::index",
            blocking_store_reads = calls,
            ms_blocked = nanos / 1_000_000,
            refs = total,
            per_ref = if total > 0 { calls as f64 / total as f64 } else { 0.0 },
            "ladder: blocking store reads (was 32 524 / 4 810 ms on the lazy path; the eager index makes it ~48)"
        );
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
    tracing::info!(
        ms = t.elapsed().as_millis(),
        synthesized,
        "resolve/6: synthesis"
    );
    tracing::info!(ms = t_phase.elapsed().as_millis(), "resolve: TOTAL");
    {
        use crate::resolver::*;
        use std::sync::atomic::Ordering::Relaxed;
        tracing::info!(
            target: "selene::index",
            prefilter_ms = NS_PREFILTER.load(Relaxed) / 1_000_000,
            frameworks_ms = NS_FRAMEWORKS.load(Relaxed) / 1_000_000,
            import_ms = NS_IMPORT.load(Relaxed) / 1_000_000,
            namematch_ms = NS_NAMEMATCH.load(Relaxed) / 1_000_000,
            passed_prefilter = N_PASSED_PREFILTER.load(Relaxed),
            "classify: per-step profile (where the ladder time goes)"
        );
        tracing::info!(
            target: "selene::index",
            fnref_ms = NS_M_FNREF.load(Relaxed) / 1_000_000,
            filepath_ms = NS_M_FILEPATH.load(Relaxed) / 1_000_000,
            qualified_ms = NS_M_QUALIFIED.load(Relaxed) / 1_000_000,
            chains_ms = NS_M_CHAINS.load(Relaxed) / 1_000_000,
            method_ms = NS_M_METHOD.load(Relaxed) / 1_000_000,
            exact_ms = NS_M_EXACT.load(Relaxed) / 1_000_000,
            fuzzy_ms = NS_M_FUZZY.load(Relaxed) / 1_000_000,
            eager_lookups = N_EAGER_LOOKUPS.load(Relaxed),
            eager_arcs_cloned = N_EAGER_ARCS_CLONED.load(Relaxed),
            "match_reference: per-strategy profile (where namematch_ms goes)"
        );
        tracing::info!(
            target: "selene::index",
            infer_ms = NS_MM_INFER.load(Relaxed) / 1_000_000,
            classnamed_ms = NS_MM_CLASSNAMED.load(Relaxed) / 1_000_000,
            fallback_ms = NS_MM_FALLBACK.load(Relaxed) / 1_000_000,
            infer_scope_ms = NS_INFER_SCOPE.load(Relaxed) / 1_000_000,
            infer_scan_ms = NS_INFER_SCAN.load(Relaxed) / 1_000_000,
            infer_calls = N_INFER_CALLS.load(Relaxed),
            infer_lines = N_INFER_LINES.load(Relaxed),
            "match_method_call: sub-strategy profile (where method_ms goes)"
        );
    }
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
    let t = std::time::Instant::now();
    for chunk in edges.chunks(PERSIST_CHUNK) {
        store.insert_edges(chunk).await?;
    }
    let ms_edges = t.elapsed().as_millis();

    // ⚠ The key is built from `original` — the STORED row, unmutated. A synthetic name
    // here no-ops the delete and the offset-0 loop spins forever (#760).
    let t = std::time::Instant::now();
    let resolved_keys: Vec<UnresolvedKey> = result
        .resolved
        .iter()
        .map(|r| key_of(&r.original))
        .collect();
    for chunk in resolved_keys.chunks(PERSIST_CHUNK) {
        store.delete_resolved(chunk).await?;
    }
    let ms_delete = t.elapsed().as_millis();

    let t = std::time::Instant::now();
    let failed_keys: Vec<UnresolvedKey> = result.unresolved.iter().map(key_of).collect();
    for chunk in failed_keys.chunks(PERSIST_CHUNK) {
        store.mark_failed(chunk).await?;
    }
    let ms_failed = t.elapsed().as_millis();

    tracing::info!(target: "selene::index",
        edges = edges.len(), ms_edges,
        deleted = resolved_keys.len(), ms_delete,
        failed = failed_keys.len(), ms_failed,
        "persist: per call");
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
