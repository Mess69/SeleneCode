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

use std::collections::HashSet;

use selene_core::{Node, NodeKind, UnresolvedRef};
use std::sync::Arc;

use crate::context::ResolutionContext;
use crate::families::same_language_family;
use crate::matcher::chains::{
    is_php_prop_chain, is_scoped_chain_language, match_dotted_call_chain, match_scoped_call_chain,
};
use crate::matcher::method::match_method_call;
use crate::resolver::ReferenceResolver;
use crate::types::{ResolvedBy, ResolvedRef};

/// How far up the supertype graph the `this.<member>` walk climbs (#808).
const THIS_MEMBER_MAX_DEPTH: usize = 5;

/// The kinds that can own a supertype.
const SUPERTYPE_BEARING: [NodeKind; 6] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Enum,
];

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
            let lang = r.language;

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

impl<C: ResolutionContext> ReferenceResolver<C> {
    /// Drain the deferred `this.<member>` function-refs and resolve them against
    /// the enclosing class's **supertypes** (#808).
    ///
    /// `this.handleSubmit` registered in a subclass resolves to
    /// `FormBase::handleSubmit` — but only once the `implements`/`extends` edges
    /// exist, which is why these were deferred.
    ///
    /// # NODE-anchored, never name-keyed — this is the whole design
    ///
    /// The walk starts from the class node **in the reference's own file**, follows
    /// `implements`/`extends` **edges** to supertype **nodes**, and looks members up
    /// through `contains` edges. No name-based unions anywhere.
    ///
    /// That is not a stylistic choice. A name-keyed `get_supertypes("Engine")`
    /// unions the parents of *every* `Engine` in the repo — and rails has a dozen —
    /// which produced a **cross-class wrong edge**. Switching to the node walk
    /// eliminated it (rails +440 → +385, every sampled edge genuine). Depth is
    /// capped at 5.
    pub fn resolve_deferred_this_member_refs(&mut self) -> Vec<ResolvedRef> {
        let deferred = std::mem::take(&mut self.deferred_this_member_refs);
        if deferred.is_empty() {
            return Vec::new();
        }

        // The first pass built the type graph after these refs were queued.
        self.ctx.clear_caches();

        let mut resolved = Vec::new();
        for r in &deferred {
            if let Some(hit) = self.resolve_one_this_member(r) {
                resolved.push(hit);
            }
        }
        resolved
    }

    fn resolve_one_this_member(&self, r: &UnresolvedRef) -> Option<ResolvedRef> {
        let ref_lang = r.language;
        let member = r
            .reference_name
            .strip_prefix("this.")
            .filter(|m| !m.is_empty())?;
        let from = self.ctx.node_by_id(&r.from_node_id)?;

        // The enclosing class's simple NAME (a class-body-level hook attributes to
        // the class node itself; an ordinary member carries it as its qualified-name
        // prefix).
        let class_name = if SUPERTYPE_BEARING.contains(&from.kind) || from.kind == NodeKind::Module
        {
            from.name.clone()
        } else {
            let sep = from.qualified_name.rfind("::").filter(|s| *s > 0)?;
            let prefix = &from.qualified_name[..sep];
            match prefix.rfind("::") {
                Some(i) => prefix[i + 2..].to_string(),
                None => prefix.to_string(),
            }
        };

        // Anchor on the class node in the REFERENCE'S OWN FILE — never a same-named
        // class elsewhere.
        let mut frontier: Vec<Arc<Node>> = self
            .ctx
            .nodes_by_name(&class_name)
            .iter()
            .filter(|n| SUPERTYPE_BEARING.contains(&n.kind) && n.file_path == r.file_path)
            .cloned()
            .collect();

        if frontier.is_empty() {
            // The class may be declared in another file (a partial / reopened class).
            // Fall back to same-family nodes of that name — still node-anchored from
            // here on.
            frontier = self
                .ctx
                .nodes_by_name(&class_name)
                .iter()
                .filter(|n| {
                    SUPERTYPE_BEARING.contains(&n.kind)
                        && same_language_family(n.language, ref_lang)
                })
                .cloned()
                .collect();
        }

        let mut seen: HashSet<String> = frontier.iter().map(|n| n.id.clone()).collect();

        for _ in 0..THIS_MEMBER_MAX_DEPTH {
            if frontier.is_empty() {
                break;
            }
            let mut next: Vec<Arc<Node>> = Vec::new();

            for type_node in &frontier {
                for supertype in self.ctx.supertypes(&type_node.id) {
                    if !seen.insert(supertype.id.clone())
                        || !SUPERTYPE_BEARING.contains(&supertype.kind)
                    {
                        continue;
                    }

                    // The member lookup is anchored on the supertype's `contains`
                    // edges — its OWN members, not a name search.
                    let found = self.ctx.members_of(&supertype.id).into_iter().find(|m| {
                        m.name == member
                            && matches!(m.kind, NodeKind::Function | NodeKind::Method)
                            && same_language_family(m.language, ref_lang)
                    });
                    if let Some(target) = found {
                        return Some(ResolvedRef {
                            original: r.clone(), // the STORED row (#760)
                            target_node_id: target.id.clone(),
                            confidence: 0.85,
                            resolved_by: ResolvedBy::FunctionRef,
                        });
                    }
                    next.push(supertype);
                }
            }
            frontier = next;
        }

        // Inherited from nothing we can see ⇒ NO EDGE. `this.` will not settle for a
        // same-named member on an unrelated class.
        None
    }
}
