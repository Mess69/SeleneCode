//! The **second passes** — the ones that can only run after the first pass has
//! persisted its edges.
//!
//! # Why a second pass exists at all
//!
//! A chained call whose method lives on a **supertype**
//! (`Foo.getInstance().inheritedMethod()`) cannot resolve during the first pass:
//! `resolve_method_on_type`'s conformance walk follows `implements`/`extends`
//! edges, and **those edges do not exist yet** — the first pass is what creates
//! them. So a chain reference that finds nothing is *deferred*, and retried here,
//! once the type graph is real (#750).
//!
//! # The lifetime coupling is deliberate — do not "fix" it
//!
//! The deferral queues live **in memory, on the resolver instance**. That is not
//! an oversight: the batched pass **deletes** (or fails) each row as it processes
//! it, so by the time these passes run, the rows are gone from the store. The
//! queue *is* the only remaining record of them. A resolver rebuilt between the
//! passes would silently drop every deferred reference — and the flows they close
//! (inherited methods, default-interface methods, trait defaults, Go embedded
//! structs) would vanish with no error anywhere.
//!
//! # Persistence is the caller's job
//!
//! These passes return the resolved references; Part C's driver turns them into
//! edges (`create_edges`) and inserts them. The resolver is generic over
//! `C: ResolutionContext` and holds no store — keeping it that way is what lets
//! every strategy be tested against an in-memory fake.

use selene_core::Language;

use crate::context::ResolutionContext;
use crate::matcher::chains::{
    is_php_prop_chain, is_scoped_chain_language, match_dotted_call_chain, match_scoped_call_chain,
};
use crate::matcher::method::match_method_call;
use crate::resolver::ReferenceResolver;
use crate::types::ResolvedRef;

impl<C: ResolutionContext> ReferenceResolver<C> {
    /// Drain the deferred chain references and retry them, now that the
    /// `implements`/`extends` edges exist (#750).
    ///
    /// Caches are cleared first — the whole point is to see edges the first pass
    /// created *after* these references were queued. A stale cache here would make
    /// the pass a no-op that looks like it ran.
    ///
    /// Every retry still goes through `resolve_method_on_type`, so the
    /// absent-method guarantee holds: an inherited method that does not actually
    /// exist anywhere on the conformance chain yields **no edge**.
    pub fn resolve_chained_calls_via_conformance(&mut self) -> Vec<ResolvedRef> {
        let deferred = std::mem::take(&mut self.deferred_chain_refs);
        if deferred.is_empty() {
            return Vec::new();
        }

        // The first pass built edges after these refs were deferred — read them.
        self.ctx.clear_caches();

        let mut resolved = Vec::new();
        for r in &deferred {
            let Some(lang) = Language::from_wire(&r.language) else {
                continue;
            };

            // PHP `this->prop.method` resolves through declared-type inference
            // (`match_method_call`), whose `resolve_method_on_type` call now has a
            // conformance walk to make. The `::`-receiver languages split on `::`;
            // the dotted ones on `.`.
            let hit = if is_php_prop_chain(r) {
                match_method_call(r, &self.ctx)
            } else if is_scoped_chain_language(lang) {
                match_scoped_call_chain(r, &self.ctx)
            } else {
                match_dotted_call_chain(r, &self.ctx)
            };

            if let Some(hit) = self.gate_language(hit, r) {
                resolved.push(hit);
            }
        }
        resolved
    }
}
