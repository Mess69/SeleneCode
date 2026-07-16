//! Function-as-value resolution (#756) — the **resolution half** of the mechanism
//! Phase 2 captured.
//!
//! A function used as a *value* — `register(handler)`, `o->cb = handler`,
//! `{ .cb = handler }`, `signal(SIGINT, handler)` — produced **no edge at all** in
//! any of the 19 languages before this. So `callers(my_recv_cb)` on a C callback
//! showed nothing but direct calls: every registered callback looked dead, and the
//! registration sites — the agent's actual next question, *"where is this wired
//! up?"* — were invisible.
//!
//! Phase 2 shipped the capture (`FN_REF_SPECS` + the same-file/imported-binding
//! gate, emitting `reference_kind: "function_ref"`). This module binds those
//! candidates.
//!
//! # Unique or drop — there is no fuzzy fallback, ever
//!
//! `function_ref` references short-circuit the whole matcher: exact name,
//! function/method kinds only, same language family, same-file first, and
//! **cross-file only when the name is UNIQUE**. Ambiguity yields *no edge*.
//!
//! That is not conservatism for its own sake. A wrong callback edge **claims a
//! registration that does not exist** — it tells the agent "this handler is wired
//! up here" when it is not, which is worse than admitting we do not know. The
//! dispatch side (`o->cb(x)` → the concrete registered function) needs data-flow
//! through struct fields and stays uncovered for the same reason: partial coverage
//! is worse than none.

use selene_core::{Language, Node, NodeKind, UnresolvedRef};
use std::sync::Arc;

use crate::context::ResolutionContext;
use crate::families::same_language_family;
use crate::types::{ResolvedBy, ResolvedRef};

/// Languages where a **bare** identifier can only ever be a `function`, never a
/// method.
///
/// In JS/TS/Python a method is reachable only through a receiver (`this.m` /
/// `self.m` / `Cls.m`), so a bare identifier naming a method is a coincidence —
/// and allowing method targets soaked up **locals passed as arguments**
/// (`new Set(selectedPointsIndices)`; docopt.py's `name`/`match` parameters). C++
/// likewise: a member value needs `&Cls::method`. PHP string callables name global
/// functions (the `[$obj, 'm']` array form carries its own shape).
///
/// The others keep method targets, because there a method value is real: C# method
/// groups, Java/Kotlin method references, Swift/Dart implicit self.
const BARE_FN_ONLY: [Language; 8] = [
    Language::Typescript,
    Language::Tsx,
    Language::Javascript,
    Language::Jsx,
    Language::Arkts,
    Language::Cpp,
    Language::Python,
    Language::Php,
];

/// The kinds that can own a `this.<member>` scope.
const SUPERTYPE_BEARING: [NodeKind; 6] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Enum,
];

fn bind(r: &UnresolvedRef, target: &str, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        // ⚠ #756 rule 10 / #760: ALWAYS the stored row. `match_function_ref` is one
        // of the two places that could plausibly return a synthetic reference, and
        // a mutated `reference_name` makes the keyed delete a no-op — the batch
        // never drains and the run explodes.
        original: r.clone(),
        target_node_id: target.to_string(),
        confidence,
        resolved_by: ResolvedBy::FunctionRef,
    }
}

/// The earliest-declared node (a stable pick among same-name overloads).
fn earliest(nodes: &[Arc<Node>]) -> Option<&Arc<Node>> {
    nodes.iter().min_by_key(|n| n.start_line)
}

/// Resolve a function-as-value reference by name.
///
/// | case | confidence |
/// |---|---|
/// | a `::`-qualified member pointer (`&Cls::method`), unique | **0.9** |
/// | same file, unique | **0.95** |
/// | same file, overloads | **0.9** |
/// | cross-file, **unique** | **0.8** |
/// | anything ambiguous | **no edge** |
pub fn match_function_ref<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    // `this.<member>` is resolved ONLY by the class-scoped path — never here.
    if r.reference_name.starts_with("this.") {
        return None;
    }
    let ref_lang = r.language;

    // --- a qualified member pointer: `&Widget::on_click` ----------------------
    // Exempt from BARE_FN_ONLY — `&Cls::m` is an EXPLICIT member reference, and its
    // own syntax is what makes it self-selecting. Still unique-or-drop, and still
    // scope-anchored: a `Decoy::handle` can never match a `KtHandlers::handle` ref.
    if r.reference_name.contains("::") {
        let member = &r.reference_name[r.reference_name.rfind("::")? + 2..];
        // Hoisted: this suffix is per-REFERENCE, and building it inside the
        // filter was one format! alloc per candidate.
        let scoped_suffix = format!("::{}", r.reference_name);
        let scoped: Vec<Arc<Node>> = ctx
            .nodes_by_name(member)
            .into_iter()
            .filter(|n| {
                matches!(n.kind, NodeKind::Function | NodeKind::Method)
                    && same_language_family(n.language, ref_lang)
                    && n.id != r.from_node_id
                    && (n.qualified_name == r.reference_name
                        || n.qualified_name.ends_with(&scoped_suffix))
            })
            .collect();
        if scoped.is_empty() {
            return None;
        }

        let same_file: Vec<Arc<Node>> = scoped
            .iter()
            .filter(|n| n.file_path == r.file_path)
            .cloned()
            .collect();
        // Cross-file AND ambiguous ⇒ drop.
        if same_file.is_empty() && scoped.len() > 1 {
            return None;
        }
        let pool = if same_file.is_empty() {
            &scoped
        } else {
            &same_file
        };
        return Some(bind(r, &earliest(pool)?.id, 0.9));
    }

    // --- a bare name ----------------------------------------------------------
    let bare_fn_only = BARE_FN_ONLY.contains(&ref_lang);

    let candidates: Vec<Arc<Node>> = ctx
        .nodes_by_name(&r.reference_name)
        .into_iter()
        .filter(|n| {
            (n.kind == NodeKind::Function || (!bare_fn_only && n.kind == NodeKind::Method))
                && same_language_family(n.language, ref_lang)
                // A function registering ITSELF is not a dependency edge.
                && n.id != r.from_node_id
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // (Wave 2: Swift's implicit-self scoping and its overload-family refusal attach
    // here — a bare identifier can name a METHOD only of the ENCLOSING type, and
    // several same-named methods in one file is an API overload family that a bare
    // identifier almost never means.)

    // The same-file definition wins — the extraction gate guarantees most survivors
    // have one, and it is the dominant C pattern (a static callback registered in a
    // same-file ops table).
    let same_file: Vec<Arc<Node>> = candidates
        .iter()
        .filter(|n| n.file_path == r.file_path)
        .cloned()
        .collect();
    if !same_file.is_empty() {
        // Same-name overloads in one file are the same conceptual symbol; the
        // earliest one is a deterministic pick.
        let confidence = if same_file.len() == 1 { 0.95 } else { 0.9 };
        return Some(bind(r, &earliest(&same_file)?.id, confidence));
    }

    // Cross-file: ONLY an unambiguous match resolves. Two same-named handlers in
    // two files is exactly the case where a guess would invent a registration.
    if candidates.len() == 1 {
        return Some(bind(r, &candidates[0].id, 0.8));
    }
    None
}

/// Resolve a `this.<member>` function-ref against the **enclosing class's own
/// members** — nothing else (**0.95**).
///
/// `addEventListener(…, this.onResize)` must hit the enclosing class's method, and
/// `this.fonts` (a *property*, post-#808 field classification) must hit nothing at
/// all. There is **no fallback of any kind**: a same-named method on another class
/// is not what `this.` means.
///
/// A miss is **deferred**, not dropped: the member may be **inherited**, and the
/// `implements`/`extends` edges do not exist during the first pass. Returns
/// `(result, deferred)` — `deferred` is `true` when the caller should queue it.
pub fn resolve_this_member_fn_ref<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> (Option<ResolvedRef>, bool) {
    let Some(member) = r
        .reference_name
        .strip_prefix("this.")
        .filter(|m| !m.is_empty())
    else {
        return (None, false);
    };
    let Some(from) = ctx.node_by_id(&r.from_node_id) else {
        return (None, false);
    };

    // A hook declared at CLASS-BODY level (Ruby's `before_action :authenticate`)
    // attributes to the class node itself — its qualified name IS the scope. For an
    // ordinary member, strip the member segment off.
    let class_prefix = if SUPERTYPE_BEARING.contains(&from.kind) || from.kind == NodeKind::Module {
        from.qualified_name.clone()
    } else {
        match from.qualified_name.rfind("::") {
            Some(sep) if sep > 0 => from.qualified_name[..sep].to_string(),
            // Not inside a class scope at all — `this.` means nothing here.
            _ => return (None, false),
        }
    };

    let candidates: Vec<Arc<Node>> = ctx
        .nodes_by_qualified_name(&format!("{class_prefix}::{member}"))
        .into_iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method)
                && n.file_path == r.file_path
                && n.id != r.from_node_id
        })
        .collect();

    match earliest(&candidates) {
        Some(target) => (Some(bind(r, &target.id, 0.95)), false),
        // Not on the class itself — possibly INHERITED. Defer to the supertype pass
        // rather than give up (#808).
        None => (None, true),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_bare_fn_only_set_is_the_documented_one() {
        // Where a bare identifier CANNOT be a method value.
        for l in [
            Language::Typescript,
            Language::Javascript,
            Language::Python,
            Language::Cpp,
            Language::Php,
        ] {
            assert!(
                BARE_FN_ONLY.contains(&l),
                "{l:?} restricts bare refs to functions"
            );
        }
        // Where a method value is real (method groups, method references).
        for l in [
            Language::CSharp,
            Language::Java,
            Language::Kotlin,
            Language::Go,
        ] {
            assert!(!BARE_FN_ONLY.contains(&l), "{l:?} keeps method targets");
        }
    }
}
