//! [`ReferenceResolver`] — the `resolve_one` ladder, the language gates, and
//! [`ReferenceResolver::create_edges`].
//!
//! # ⚠ This file is the crate's #1 shared seam
//!
//! The ladder below is laid down **whole**, in `maps/resolution.md`
//! §`resolveOne` pipeline's exact order, with every not-yet-implemented step
//! present as a named stub. Later tasks fill **exactly one step each** and
//! **never re-order**: the order *is* the contract (§Rust port notes — "port as
//! a fixed pipeline, not a rules engine"). Tasks 6, 9 and 10, then Part B and
//! Part C, each edit this file; run them **strictly sequentially**.
//!
//! # Why the order is behavior
//!
//! Every step can produce a candidate, and the ladder ends by taking the
//! **highest-confidence** one (**first-wins on ties** — a `reduce` that keeps
//! the earlier candidate on equality). Two steps also **return immediately** at
//! confidence ≥ **0.9** rather than competing. Move a step and you change which
//! symbol a reference binds to, silently, in a way no type checks.

use std::collections::HashMap;

use selene_core::{Edge, EdgeKind, Language, NodeKind, Provenance, UnresolvedRef};
use serde_json::{Map, Value, json};

use crate::builtins::{capitalize_ascii, is_built_in_or_external};
use crate::context::ResolutionContext;
use crate::families::{crosses_known_family, same_language_family};
use crate::frameworks::{FrameworkResolver, detect_frameworks};
use crate::imports::{resolve_jvm_import, resolve_via_import};
use crate::matcher::chains::is_deferrable_chain;
use crate::matcher::fnref::{match_function_ref, resolve_this_member_fn_ref};
use crate::matcher::match_reference;
use crate::types::ResolvedRef;

/// The reference resolver: one instance per index/sync pass.
///
/// The deferral queues are **in-memory and instance-scoped by design**: the
/// batched pass deletes (or fails) a row before the edges its conformance walk
/// needs exist, so the second passes can only run against the same resolver
/// instance that queued them (`maps/resolution.md` §Rust port notes). Preserve
/// that lifetime coupling.
/// Whether [`ReferenceResolver::classify`] wants a reference deferred to a later conformance pass —
/// returned instead of pushed so the ladder stays a pure function and a batch can run in parallel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Defer {
    None,
    Chain,
    ThisMember,
}

pub struct ReferenceResolver<C: ResolutionContext> {
    pub(crate) ctx: C,
    /// Chain-shaped `calls` refs the first pass could not resolve — drained by
    /// the conformance pass once `implements`/`extends` edges exist (Task 9).
    pub(crate) deferred_chain_refs: Vec<UnresolvedRef>,
    /// `this.<member>` function refs whose member is inherited — drained by the
    /// supertype pass (Task 10).
    pub(crate) deferred_this_member_refs: Vec<UnresolvedRef>,
    /// The frameworks detected in this project, in **registry order** — which is
    /// resolve precedence (ladder step 7, first hit ≥ 0.9 wins outright).
    ///
    /// Detected **once**, at construction, never per reference: `detect` reads
    /// manifests and probes paths, so doing it per reference would be quadratic.
    pub(crate) frameworks: Vec<&'static dyn FrameworkResolver>,
}

impl<C: ResolutionContext> ReferenceResolver<C> {
    /// A resolver over `ctx`, with the project's frameworks detected.
    ///
    /// Detection runs here, once — the equivalent of the TS build's
    /// `createResolver` → `initialize()`. A resolver whose `detect` panics is
    /// caught and excluded (errors collected, never thrown).
    pub fn new(ctx: C) -> Self {
        let frameworks = detect_frameworks(&ctx);
        Self {
            ctx,
            deferred_chain_refs: Vec::new(),
            deferred_this_member_refs: Vec::new(),
            frameworks,
        }
    }

    /// A resolver over `ctx` with an **explicit** framework list.
    ///
    /// Two callers. It is the seam a framework's own tests inject through —
    /// `detect` keys on manifests (`pom.xml`, `requirements.txt`), and a unit test
    /// should be able to exercise `resolve`/`extract` without staging a whole
    /// project tree. It is also how Part C's driver hands over the frameworks it
    /// has already detected, instead of detecting them twice.
    pub fn with_frameworks(ctx: C, frameworks: Vec<&'static dyn FrameworkResolver>) -> Self {
        Self {
            ctx,
            deferred_chain_refs: Vec::new(),
            deferred_this_member_refs: Vec::new(),
            frameworks,
        }
    }

    /// The project view this resolver reads through.
    pub fn ctx(&self) -> &C {
        &self.ctx
    }

    /// The names of the frameworks detected in this project, in registry order.
    ///
    /// **This spelling is the contract** — Part C's `detected_frameworks_agree`
    /// gate calls exactly this, three ways (TS baseline == Rust == the fixture
    /// manifest).
    pub fn detected_frameworks(&self) -> Vec<String> {
        self.frameworks
            .iter()
            .map(|f| f.name().to_string())
            .collect()
    }

    /// Refs queued for the chained-call conformance pass (Task 9 drains them).
    pub fn deferred_chain_refs(&self) -> &[UnresolvedRef] {
        &self.deferred_chain_refs
    }

    /// Refs queued for the inherited-`this.<member>` pass (Task 10 drains them).
    pub fn deferred_this_member_refs(&self) -> &[UnresolvedRef] {
        &self.deferred_this_member_refs
    }

    // =========================================================================
    // The ladder
    // =========================================================================

    /// Resolve one reference. `None` is an ordinary, successful miss.
    ///
    /// The twelve steps below are `maps/resolution.md` §`resolveOne` pipeline,
    /// in order. **Do not re-order them.**
    /// Resolve one reference, recording any deferral on `self`. Thin wrapper over [`Self::classify`].
    pub fn resolve_one(&mut self, r: &UnresolvedRef) -> Option<ResolvedRef> {
        let (hit, defer) = self.classify(r);
        match defer {
            Defer::Chain => self.deferred_chain_refs.push(r.clone()),
            Defer::ThisMember => self.deferred_this_member_refs.push(r.clone()),
            Defer::None => {}
        }
        hit
    }

    /// The ladder as a PURE function of `(reference, ctx)` — no `&mut self`. Deferrals are RETURNED,
    /// not pushed, so a whole batch's references can resolve in parallel (`resolve_all` uses rayon)
    /// and the caller records the deferrals in reference order. **Order is behavior**; parallelism
    /// must leave the result identical, which the tolerance-0 parity gate proves.
    pub(crate) fn classify(&self, r: &UnresolvedRef) -> (Option<ResolvedRef>, Defer) {
        let mut defer = Defer::None;
        // --- step 1: built-in / external filter ------------------------------
        if is_built_in_or_external(r, &self.ctx) {
            return (None, defer);
        }

        // --- step 2: CFML component-path inheritance (#1152) -----------------
        // Wave 2 (Phase 8): `cfml`/`cfscript` have no extractor, so no ref can
        // carry those languages yet. When they land, this step goes HERE — ahead
        // of the pre-filter, because a dotted component path
        // (`coldbox.system.web.Controller`) names no symbol and the pre-filter
        // would drop it. It has NO fallthrough on a miss.

        // --- step 3: the fast pre-filter -------------------------------------
        // Skip the ref entirely unless *something* could possibly match it. This
        // runs on every reference in the repo, so it is a hash lookup, never a
        // query.
        //
        // The import escape is not an optimization detail: a re-export rename
        // chain (`import { login } from './barrel'`, where the barrel does
        // `export { signIn as login }`) deliberately names a symbol that is
        // declared NOWHERE — only the renamed upstream symbol exists. Without the
        // escape, every renamed re-export silently loses its edge.
        //
        // The `claims_reference` arm is the same kind of escape, for frameworks: a
        // rails route (`articles#index`), a laravel route (`UserController@index`),
        // django's `_iterable_class` and spring's `app:prefix` name NO declared
        // symbol anywhere. Without this arm those references are dropped HERE,
        // before `resolve()` is ever called, and the bridge is silently inert —
        // the TS build shipped that bug twice.
        let existence_name = r.reference_name.as_str();
        if !has_any_possible_match(existence_name, &self.ctx)
            && !matches_any_import(r, &self.ctx)
            && !self
                .frameworks
                .iter()
                .any(|f| f.claims_reference(existence_name))
        {
            return (None, defer);
        }
        // Wave 2, in this step: ArkTS leading-dot attribute refs (`.titleStyle`)
        // are existence-checked with the dot stripped; Nix path imports bypass
        // the check entirely (they name a FILE, not a symbol).

        // Steps 7, 8 and 10 push here; steps 9 and 12 reduce over it.
        let mut candidates: Vec<ResolvedRef> = Vec::new();

        // --- step 4: `function_ref` — a dedicated, strictly-gated path --------
        // A function-as-value reference NEVER reaches the frameworks loop, the name
        // matcher, or fuzzy: a wrong callback edge claims a registration that does
        // not exist, which is worse than admitting we do not know.
        if r.reference_kind == "function_ref" {
            // `this.<member>` values resolve ONLY against the enclosing class's own
            // members — never a same-named symbol elsewhere.
            if r.reference_name.starts_with("this.") {
                let (hit, should_defer) = resolve_this_member_fn_ref(r, &self.ctx);
                if should_defer {
                    // The member may be INHERITED, and the implements/extends edges
                    // do not exist yet (#808) — retry in the supertype pass.
                    defer = Defer::ThisMember;
                }
                return (self.gate_language(hit, r), defer);
            }

            // An imported callback resolves through its import — the most precise
            // cross-file signal there is. Accepted ONLY if the target really is a
            // function or a method.
            if let Some(via) = self.gate_language(resolve_via_import(r, &self.ctx), r)
                && self
                    .ctx
                    .node_by_id(&via.target_node_id)
                    .is_some_and(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
            {
                return (Some(via), defer);
            }

            return (self.gate_language(match_function_ref(r, &self.ctx), r), defer);
        }

        // --- step 5: JVM FQN imports -----------------------------------------
        // Returns DIRECTLY, ahead of the frameworks and the name matcher: a
        // `com.example.Bar` import is unambiguous through the qualified-name
        // index even when several `Bar`s exist in different packages (#314).
        if let Some(hit) = resolve_jvm_import(r, &self.ctx) {
            return (Some(hit), defer);
        }

        // --- step 6: Razor `@using` ------------------------------------------
        // Wave 2 (Phase 8): `razor` has no extractor yet.

        // --- step 7: the frameworks loop -------------------------------------
        // Registry order IS precedence: the first framework to answer with
        // confidence ≥ 0.9 wins outright (spring's `@Value` → config key at 0.9,
        // laravel's `Controller@method` at 0.9; rails' `c#a` at 0.85 competes
        // instead). Anything weaker becomes a candidate and competes on
        // max-confidence with imports and the name matcher — which is why the
        // per-framework confidence constants are load-bearing and must not be
        // rounded.
        //
        // The gate here is deliberately weaker than `gate_language`: a framework
        // exists to build cross-language bridges (a yaml config key → a Java
        // field), so only `references`/`imports` results crossing two KNOWN
        // families are dropped. A `calls` bridge and a config↔code edge both
        // survive.
        for framework in &self.frameworks {
            if let Some(hit) = self.gate_framework_language(framework.resolve(r, &self.ctx), r) {
                if hit.confidence >= 0.9 {
                    return (Some(hit), defer);
                }
                candidates.push(hit);
            }
        }

        // --- step 8: import-based resolution ---------------------------------
        // ≥ 0.9 returns immediately; anything weaker competes as a candidate.
        if let Some(hit) = self.gate_language(resolve_via_import(r, &self.ctx), r) {
            if hit.confidence >= 0.9 {
                return (Some(hit), defer);
            }
            candidates.push(hit);
        }

        // --- step 9: path-only refs — NEVER fall through to name matching -----
        // A PHP `include 'inc/db.php'` resolves to a FILE through import
        // resolution or not at all. Falling through to the symbol matcher would
        // mis-connect it to an unrelated `db.php` elsewhere in the tree — and a
        // wrong edge is worse than none (#660).
        if self.is_path_only_ref(r) {
            return (best_candidate(candidates), defer);
        }

        // --- step 10: name matching -------------------------------------------
        // (Wave 2: the Nix same-file post-filter attaches here — a Nix callee
        // binds lexically or through explicit import wiring, never by name.)
        if let Some(hit) = self.gate_language(match_reference(r, &self.ctx), r) {
            candidates.push(hit);
        }

        // --- step 11: defer for the conformance passes ------------------------
        if candidates.is_empty() {
            // A chained call whose method may live on a SUPERTYPE cannot resolve
            // yet: the conformance walk follows `implements`/`extends` edges, and
            // this pass is what creates them. Queue it for the second pass (#750).
            if is_deferrable_chain(r) {
                defer = Defer::Chain;
            }
            return (None, defer);
        }

        // --- step 12: the highest-confidence candidate, first-wins on ties ----
        (best_candidate(candidates), defer)
    }

    // =========================================================================
    // Step 9's guard
    // =========================================================================

    /// A reference that names a **path**, not a symbol.
    ///
    /// Step 9's guard. These never fall through to name matching (#660).
    fn is_path_only_ref(&self, r: &UnresolvedRef) -> bool {
        // PHP include/require: the extractor emits the static string path as an
        // `imports` ref.
        is_php_include_path_ref(r)
        // Wave 2 (Phase 8), same branch, same no-fallthrough rule: COBOL
        // copybooks (`is_cobol_copybook_ref`), Nix path imports
        // (`is_nix_path_import_ref`), and every `terraform` ref (its framework
        // resolver IS the whole rulebook — `var.X` can never legally bind
        // outside its module directory).
    }

    // =========================================================================
    // The language gates
    // =========================================================================

    /// Drop a result whose target sits in the wrong language family.
    ///
    /// - `references` / `function_ref`: the target **must** be in the ref's own
    ///   family (a TS type reference cannot name a Kotlin class).
    /// - `imports`: dropped only when the two sit in **different known**
    ///   families — an unfamilied language (python, ruby, go…) never "crosses",
    ///   which is why its imports survive this gate.
    /// - Everything else (`calls`, `extends`, …) passes: cross-language `calls`
    ///   bridges are real (React Native JS → native).
    pub(crate) fn gate_language(
        &self,
        result: Option<ResolvedRef>,
        r: &UnresolvedRef,
    ) -> Option<ResolvedRef> {
        let result = result?;
        let (Some(target_lang), Some(ref_lang)) = (
            self.target_language(&result.target_node_id),
            Language::from_wire(&r.language),
        ) else {
            // A language we cannot type is a language we cannot gate. Pass it
            // through — the TS does exactly this (`if (!tgt || !ref.language)`).
            return Some(result);
        };

        match r.reference_kind.as_str() {
            "references" | "function_ref" if !same_language_family(target_lang, ref_lang) => None,
            "imports" if crosses_known_family(target_lang, ref_lang) => None,
            _ => Some(result),
        }
    }

    /// The framework-strategy gate — **only** `references`/`imports`, and only
    /// when both languages sit in *different known* families.
    ///
    /// Deliberately weaker than [`Self::gate_language`]: framework resolvers
    /// exist to build cross-language bridges (a Drupal `routing.yml` → a PHP
    /// controller; React Native JS → native `calls`). Those bridges are either
    /// `calls` edges or config↔code edges whose config side (`yaml`,
    /// `properties`) is in no known family — so gating only the
    /// both-known-and-crossing case lets every legitimate bridge through, while
    /// still killing the coincidental collisions (a TS `<TestRunner>` component
    /// ref matching a Kotlin `class TestRunner`).
    fn gate_framework_language(
        &self,
        result: Option<ResolvedRef>,
        r: &UnresolvedRef,
    ) -> Option<ResolvedRef> {
        let result = result?;
        if r.reference_kind != "references" && r.reference_kind != "imports" {
            return Some(result);
        }
        let (Some(target_lang), Some(ref_lang)) = (
            self.target_language(&result.target_node_id),
            Language::from_wire(&r.language),
        ) else {
            return Some(result);
        };
        if crosses_known_family(target_lang, ref_lang) {
            return None;
        }
        Some(result)
    }

    fn target_language(&self, node_id: &str) -> Option<Language> {
        self.ctx
            .node_by_id(node_id)
            .and_then(|n| Language::from_wire(&n.language))
    }

    // =========================================================================
    // Edge creation
    // =========================================================================

    /// Turn resolved references into edges — `maps/resolution.md` §Edge creation.
    ///
    /// # The three kind promotions (and nothing else)
    ///
    /// - `function_ref` → **`references`**. The internal capture kind never
    ///   persists as an edge kind; `metadata.fnRef` is what marks it.
    /// - `extends` → **`implements`**, when the target is an interface/protocol
    ///   and the source is *not* (an interface extending an interface stays
    ///   `extends`).
    /// - `calls` → **`instantiates`**, when the target is a class/struct.
    ///   Python and Ruby express instantiation as `Foo()`, which extraction
    ///   cannot tell from a call without symbol info — but resolution can.
    ///
    /// # The metadata is a wire contract
    ///
    /// `refName` is the **original** reference text, and it is what lets a
    /// re-index *resurrect* this edge as exactly the reference that produced it
    /// (`#1240`). Reconstructing the name from the target node instead would
    /// strip receiver context (`h.greet` → `greet`) and risk a wrong rebind, so
    /// an edge without `refName` is deliberately never resurrected. `refKind` is
    /// written **only when a promotion changed the kind**.
    pub fn create_edges(&self, resolved: &[ResolvedRef]) -> Vec<Edge> {
        // One batched sweep for every endpoint kind we need — never a query per
        // edge (a 100k-reference pass would otherwise issue 200k point lookups).
        let mut kinds: HashMap<&str, NodeKind> = HashMap::new();
        for r in resolved {
            for id in [r.target_node_id.as_str(), r.original.from_node_id.as_str()] {
                if !kinds.contains_key(id)
                    && let Some(node) = self.ctx.node_by_id(id)
                {
                    kinds.insert(id, node.kind);
                }
            }
        }

        resolved
            .iter()
            .map(|r| {
                let original_kind = r.original.reference_kind.as_str();
                let target_kind = kinds.get(r.target_node_id.as_str()).copied();
                let source_kind = kinds.get(r.original.from_node_id.as_str()).copied();

                let kind = promote_kind(original_kind, target_kind, source_kind);

                let mut metadata = Map::new();
                metadata.insert("confidence".into(), json!(r.confidence));
                metadata.insert("resolvedBy".into(), json!(r.resolved_by.as_str()));
                metadata.insert("refName".into(), json!(r.original.reference_name));
                if kind.as_str() != original_kind {
                    metadata.insert("refKind".into(), json!(original_kind));
                }
                if original_kind == "function_ref" {
                    metadata.insert("fnRef".into(), json!(true));
                }

                Edge {
                    source: r.original.from_node_id.clone(),
                    target: r.target_node_id.clone(),
                    kind,
                    metadata: Some(Value::Object(metadata)),
                    line: r.original.line,
                    column: r.original.column,
                    provenance: Some(Provenance::TreeSitter),
                }
            })
            .collect()
    }
}

// =============================================================================
// Free functions (shared with `builtins`, and with the strategies to come)
// =============================================================================

/// Could **anything** in the graph match this reference name?
///
/// The pre-filter's hash probe (`maps/resolution.md` §`resolveOne` step 3),
/// run once per reference in the repo. It is deliberately generous — a false
/// positive costs one wasted strategy pass, while a false negative silently
/// deletes an edge — so it tries every shape a qualified name can take:
///
/// - the direct name;
/// - around a `.`: the receiver, the member, the **capitalized** receiver
///   (instance-method resolution), and the **last**-dot tail (a JVM FQN
///   `com.example.foo.Bar` has exactly one useful segment: `Bar`);
/// - around `::`: the receiver, the member, and the **last**-`::` tail (a Rust
///   path `database::profiles::find` names a symbol only in its last segment —
///   without this the pre-filter drops the ref before the Rust path resolver
///   ever sees it);
/// - around a single `:` (Lua `lg:log`) and around `$` (R `lg$log`): member,
///   receiver, capitalized receiver;
/// - after the last `/`: the filename (path-like refs).
pub fn has_any_possible_match<C: ResolutionContext>(name: &str, ctx: &C) -> bool {
    let known = ctx.known_names();

    if known.contains(name) {
        return true;
    }

    // Dotted: `obj.method`, `com.example.Bar`.
    if let Some(dot) = name.find('.')
        && dot > 0
    {
        let receiver = &name[..dot];
        let member = &name[dot + 1..];
        if known.contains(receiver) || known.contains(member) {
            return true;
        }
        if known.contains(&capitalize_ascii(receiver)) {
            return true;
        }
        if let Some(last_dot) = name.rfind('.')
            && last_dot > dot
        {
            let tail = &name[last_dot + 1..];
            if !tail.is_empty() && known.contains(tail) {
                return true;
            }
        }
    }

    // Scoped: `Class::method`, `database::profiles::find`.
    if let Some(colon) = name.find("::")
        && colon > 0
    {
        let receiver = &name[..colon];
        let member = &name[colon + 2..];
        if known.contains(receiver) || known.contains(member) {
            return true;
        }
        if let Some(last_colon) = name.rfind("::")
            && last_colon > colon
        {
            let tail = &name[last_colon + 2..];
            if !tail.is_empty() && known.contains(tail) {
                return true;
            }
        }
    }

    // Lua/Luau `lg:log` (skipped when the name is really `::`-scoped), R `lg$log`.
    for sep in [':', '$'] {
        if sep == ':' && name.contains("::") {
            continue;
        }
        if let Some(idx) = name.find(sep)
            && idx > 0
        {
            let receiver = &name[..idx];
            let member = &name[idx + 1..];
            if known.contains(member) || known.contains(receiver) {
                return true;
            }
            if known.contains(&capitalize_ascii(receiver)) {
                return true;
            }
        }
    }

    // Path-like: `snippets/drawer-menu.liquid`.
    if let Some(slash) = name.rfind('/')
        && slash > 0
    {
        let file_name = &name[slash + 1..];
        if known.contains(file_name) {
            return true;
        }
    }

    false
}

/// Does the ref's name match an import declared in its own file?
///
/// The pre-filter's escape hatch (`resolve_one` step 3). It matches the import's
/// `local_name` exactly, or the ref being a member access on it (`utils.parse`
/// against `import * as utils`).
///
/// This is not an optimization detail. A re-export **rename** chain
/// (`import { login } from './barrel'`, where the barrel does
/// `export { signIn as login }`) deliberately names a symbol that is declared
/// NOWHERE — only the renamed upstream symbol exists. Without this escape, the
/// pre-filter drops the reference and every renamed re-export silently loses its
/// edge.
pub fn matches_any_import<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> bool {
    let mappings = ctx.import_mappings(&r.file_path);
    mappings.iter().any(|m| {
        r.reference_name == m.local_name
            || r.reference_name.starts_with(&format!("{}.", m.local_name))
    })
}

/// A PHP `include`/`require` path reference (#660): a php `imports` ref whose
/// name looks like a path (contains `/` or `.`).
///
/// Path-shaped refs resolve to a **file** or to nothing at all — they never fall
/// through to symbol matching.
pub fn is_php_include_path_ref(r: &UnresolvedRef) -> bool {
    r.language == Language::Php.as_str()
        && r.reference_kind == "imports"
        && (r.reference_name.contains('/') || r.reference_name.contains('.'))
}

/// The highest-confidence candidate, **first-wins on ties**.
///
/// The tie rule is a contract, not an accident: TS reduces with `curr > best`,
/// which keeps the EARLIER candidate on equality, and candidate order is ladder
/// order (frameworks before imports before name matching). A `>=` here would
/// silently re-rank every equal-confidence pair in the codebase.
fn best_candidate(candidates: Vec<ResolvedRef>) -> Option<ResolvedRef> {
    candidates.into_iter().reduce(|best, curr| {
        if curr.confidence > best.confidence {
            curr
        } else {
            best
        }
    })
}

/// The three kind promotions of §Edge creation. Split out so it is testable
/// without a context.
fn promote_kind(
    original_kind: &str,
    target_kind: Option<NodeKind>,
    source_kind: Option<NodeKind>,
) -> EdgeKind {
    // `function_ref` is internal-only: it persists as a `references` edge,
    // marked by `metadata.fnRef`.
    if original_kind == "function_ref" {
        return EdgeKind::References;
    }

    let Ok(kind) = original_kind.parse::<EdgeKind>() else {
        // An unknown reference kind cannot be promoted, and cannot be an edge
        // kind either — but extraction only ever emits EdgeKind wire strings
        // plus `function_ref`, so this is unreachable in practice. Falling back
        // to `references` (the weakest, most generic relation) keeps a
        // malformed row from taking down the pass.
        return EdgeKind::References;
    };

    match kind {
        // A class extending an interface is IMPLEMENTING it. An interface
        // extending an interface is not.
        EdgeKind::Extends
            if matches!(target_kind, Some(NodeKind::Interface | NodeKind::Protocol))
                && !matches!(source_kind, Some(NodeKind::Interface | NodeKind::Protocol)) =>
        {
            EdgeKind::Implements
        }
        // `Foo()` in Python/Ruby is a call at extraction time and an
        // instantiation once `Foo` is known to be a class.
        EdgeKind::Calls if matches!(target_kind, Some(NodeKind::Class | NodeKind::Struct)) => {
            EdgeKind::Instantiates
        }
        other => other,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::types::ResolvedBy;

    #[test]
    fn promotions_are_exactly_the_three() {
        use NodeKind::*;
        // function_ref → references, always.
        assert_eq!(
            promote_kind("function_ref", Some(Function), Some(Function)),
            EdgeKind::References
        );
        // extends → implements ONLY when target is an interface and source is not.
        assert_eq!(
            promote_kind("extends", Some(Interface), Some(Class)),
            EdgeKind::Implements
        );
        assert_eq!(
            promote_kind("extends", Some(Protocol), Some(Struct)),
            EdgeKind::Implements
        );
        assert_eq!(
            promote_kind("extends", Some(Interface), Some(Interface)),
            EdgeKind::Extends,
            "an interface extending an interface really does extend it"
        );
        assert_eq!(
            promote_kind("extends", Some(Class), Some(Class)),
            EdgeKind::Extends
        );
        // calls → instantiates ONLY for class/struct targets.
        assert_eq!(
            promote_kind("calls", Some(Class), Some(Function)),
            EdgeKind::Instantiates
        );
        assert_eq!(
            promote_kind("calls", Some(Struct), Some(Function)),
            EdgeKind::Instantiates
        );
        assert_eq!(
            promote_kind("calls", Some(Function), Some(Function)),
            EdgeKind::Calls
        );
        // Everything else passes through untouched.
        assert_eq!(
            promote_kind("references", Some(Class), Some(Function)),
            EdgeKind::References
        );
        assert_eq!(
            promote_kind("imports", Some(File), Some(File)),
            EdgeKind::Imports
        );
        // An unknown target kind cannot promote.
        assert_eq!(promote_kind("calls", None, None), EdgeKind::Calls);
    }

    #[test]
    fn best_candidate_keeps_the_earlier_on_a_tie() {
        use selene_core::RefStatus;
        let row = UnresolvedRef {
            from_node_id: "function:a".into(),
            reference_name: "x".into(),
            reference_kind: "calls".into(),
            line: None,
            column: None,
            candidates: vec![],
            file_path: "a.ts".into(),
            language: "typescript".into(),
            status: RefStatus::Pending,
            name_tail: "x".into(),
        };
        let cand = |id: &str, conf: f64| ResolvedRef {
            original: row.clone(),
            target_node_id: id.into(),
            confidence: conf,
            resolved_by: ResolvedBy::ExactMatch,
        };

        // Ties keep the FIRST — candidate order is ladder order.
        let picked = best_candidate(vec![cand("first", 0.7), cand("second", 0.7)]).unwrap();
        assert_eq!(picked.target_node_id, "first");

        // A strictly higher confidence still wins, wherever it sits.
        let picked = best_candidate(vec![
            cand("low", 0.4),
            cand("high", 0.9),
            cand("also_high", 0.9),
        ])
        .unwrap();
        assert_eq!(picked.target_node_id, "high");

        assert!(best_candidate(vec![]).is_none());
    }
}
