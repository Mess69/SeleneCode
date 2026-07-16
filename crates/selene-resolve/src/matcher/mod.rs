//! [`match_reference`] — the name-matcher's strategy ladder.
//!
//! # ⚠ This file is the crate's #2 shared seam
//!
//! The strategy ladder below is laid down **whole**, in `maps/resolution.md`
//! §`matchReference` strategy order, with every not-yet-implemented step present
//! as a named stub. Tasks 8, 9 and 10 fill **exactly one step each** and **never
//! re-order**. Run them strictly sequentially.
//!
//! # First hit wins — this is not a scoring blend
//!
//! Unlike `resolve_one` (which accumulates candidates and takes the
//! highest-confidence one), `match_reference` **returns the first strategy that
//! produces anything at all**. The order therefore *is* the precedence: a
//! qualified-name match at 0.85 beats an exact-name match that would have scored
//! 0.9, because qualified names carry more information. Re-ordering these steps
//! silently re-points references across the whole graph.

pub mod chains;
pub mod fnref;
pub mod method;
pub mod names;
pub mod receiver;
pub mod scoring;

use selene_core::{Language, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::matcher::chains::{
    match_cpp_call_chain, match_dotted_call_chain, match_scoped_call_chain,
};
use crate::matcher::fnref::match_function_ref;
use crate::matcher::method::match_method_call;
use crate::matcher::names::{
    match_by_exact_name, match_by_file_path, match_by_qualified_name, match_fuzzy,
};
use crate::types::ResolvedRef;

/// Resolve a reference by NAME, through the strategy ladder.
///
/// Step 10 of the `resolve_one` ladder — the last strategy before a reference is
/// given up on (or deferred to a conformance pass).
pub fn match_reference<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    // --- `function_ref` short-circuits ---------------------------------------
    // A function-as-value reference resolves ONLY through its dedicated matcher —
    // never through the qualified/exact/fuzzy fallthrough below. A wrong callback
    // edge is worse than none: it claims a registration that does not exist.
    if r.reference_kind == "function_ref" {
        return match_function_ref(r, ctx);
    }

    // Wave 2 (Phase 8), both in this position and both NO-FALLTHROUGH:
    //   * ArkTS chained UI attributes (a leading-dot name) resolve only to
    //     `@Extend`/`@Styles`/`@AnimatableExtend`/`@Builder`-decorated helpers —
    //     falling through to bare-name matching manufactured 36k wrong edges on
    //     a samples monorepo.
    //   * Erlang `-behaviour(m)` refs (and everything an `.app`/`.app.src` file
    //     emits) target a MODULE only — `-behaviour(supervisor)` otherwise
    //     resolved to an unrelated `-define(supervisor, …)` macro.

    // --- (0) a path-like name → a file node ----------------------------------
    if let Some(hit) = match_by_file_path(r, ctx) {
        return Some(hit);
    }

    // --- (1) a qualified name -------------------------------------------------
    if let Some(hit) = match_by_qualified_name(r, ctx) {
        return Some(hit);
    }

    // The three chain resolvers (#645/#608/#750). Each infers the receiver's type
    // from what the INNER call returns, then validates the outer method on it — so
    // a wrong inference yields no edge, never a wrong one. They sit ABOVE the
    // method matcher because a chained receiver is more information than a bare
    // one, and below the qualified name because that is more still.
    let lang = r.language;

    // --- (1b) C/C++: `Foo::instance().bar` -----------------------------------
    if matches!(lang, Language::C | Language::Cpp)
        && let Some(hit) = match_cpp_call_chain(r, ctx)
    {
        return Some(hit);
    }

    // --- (1c) `::`-scoped: PHP `Cls::for($x)->m`, Rust `Foo::new().m` ---------
    if matches!(lang, Language::Php | Language::Rust)
        && let Some(hit) = match_scoped_call_chain(r, ctx)
    {
        return Some(hit);
    }

    // --- (1d) dotted: `Foo.create().bar` -------------------------------------
    // v0: java/kotlin/csharp/go. (swift/scala/dart/objc/pascal are wave 2 — their
    // rows live in the chain tables and cost nothing until their extractors land.)
    if matches!(
        lang,
        Language::Java | Language::Kotlin | Language::CSharp | Language::Go
    ) && let Some(hit) = match_dotted_call_chain(r, ctx)
    {
        return Some(hit);
    }

    // --- (2) a method call on an inferred receiver type ----------------------
    // The receiver's type is inferred from its local declaration and then
    // VALIDATED (the method must exist on it) — so a mis-inference yields no
    // edge, never a wrong one.
    if let Some(hit) = match_method_call(r, ctx) {
        return Some(hit);
    }

    // --- (3) an exact name ----------------------------------------------------
    if let Some(hit) = match_by_exact_name(r, ctx) {
        return Some(hit);
    }

    // --- (4) fuzzy: the last resort, unique-or-nothing ------------------------
    match_fuzzy(r, ctx)
}
