//! Chained-call resolution (#645/#608/#750) — a call whose **receiver is itself a
//! call**.
//!
//! ```text
//! Foo.getInstance().bar();   // `bar` must resolve to Foo::bar, never to a decoy
//! ```
//!
//! Before this mechanism existed, every statically-typed language **dropped the
//! receiver** and name-matched the bare method (`bar`) — so in 7 of 9 languages it
//! silently attached to a same-named method **on an unrelated type**. That is a
//! correctness bug, not a coverage gap: the graph said something false.
//!
//! # The three parts (two of them already shipped)
//!
//! 1. **Phase 2** captured the factory's declared return type (`Node.return_type`,
//!    with `-> Self` normalized to the marker string `self`).
//! 2. **Phase 2** re-encoded the chained receiver as the marker `inner().method` —
//!    a string that never appears in an ordinary reference.
//! 3. **This module** resolves it: infer the receiver's type from what the inner
//!    call returns, then resolve the outer method **on that type** through
//!    [`resolve_method_on_type`], which **validates** that the method exists there.
//!
//! So a wrong inference produces **no edge, never a wrong one**. Every language
//! block in the TS suite carries the same *"creates NO edge when the type lacks the
//! method"* test, and that guarantee is what made this safe to ship.
//!
//! # TypeScript is deliberately NOT here
//!
//! It was fully implemented in the TS build (5 synthetic tests passing) and
//! **consciously not shipped**. TS leans on type *inference*, so a factory like
//! NestJS's `Test.createTestingModule(m)` carries no `: TestingModuleBuilder`
//! annotation — the type cannot be recovered, the re-encoded chain cannot resolve,
//! and it **drops the bare-name edge the existing resolver already found**.
//! Real-repo A/B: **+0 added on typeorm AND nest, −164 on nest**. It is
//! precision-positive and recall-negative, which is the wrong trade. Do not
//! "finish" it here; the only path is reading *inferred* return types, which is a
//! much larger change.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::matcher::method::{imported_fqn_of, resolve_method_on_type};
use crate::matcher::names::{match_by_exact_name, match_fuzzy};
use crate::matcher::receiver::{lookup_callee_return_type, resolve_cpp_call_result_type};
use crate::types::{ResolvedBy, ResolvedRef};

/// The extractor's chained-receiver marker: `inner().method`.
///
/// ⚠ The greedy `(.+)` binds to the **LAST** `().` — so `A().b().c` splits into
/// inner `A().b` and method `c`. That is JS's behavior, and the spike (F5a)
/// confirmed the Rust `regex` crate agrees, so the pattern ports verbatim. It also
/// means a chain re-encodes **one hop**: deeper hops keep the bare name.
static CHAIN_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, covered by the tests below
    Regex::new(r"^(.+)\(\)\.(\w+)$").unwrap()
});

/// PHP `$this->prop->method()`, encoded `this->prop.method`. It has no `()`, so
/// [`CHAIN_SHAPE`] misses it — it needs its own deferral predicate.
static PHP_PROP_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, covered by the tests below
    Regex::new(r"^this->\w+\.\w+$").unwrap()
});

/// Languages whose failed chain references are **deferred** to the conformance
/// pass (the method may live on a supertype, whose `implements`/`extends` edges do
/// not exist yet during the first pass).
///
/// v0: java, kotlin, csharp, rust, go. The rest (swift, scala, dart, objc, pascal)
/// are wave-2 languages — their entries are harmless (no such reference can exist)
/// and keep the set honest against the source.
pub const CHAIN_LANGUAGES: [Language; 10] = [
    Language::Java,
    Language::Kotlin,
    Language::CSharp,
    Language::Rust,
    Language::Go,
    Language::Swift,
    Language::Scala,
    Language::Dart,
    Language::Objc,
    Language::Pascal,
];

/// Languages whose chain receiver is `::`-scoped (PHP, Rust).
const SCOPED_CHAIN_LANGUAGES: [Language; 2] = [Language::Php, Language::Rust];

/// Languages where an unprefixed capitalized call `Foo(args)` **constructs** the
/// class, so a `Foo().method` receiver's type is `Foo` itself.
///
/// Java and C# need `new`, so a bare `Foo()` there is a *method call*, not a
/// construction — including them would invent a type out of nothing.
const CONSTRUCTS_VIA_BARE_CALL: [Language; 5] = [
    Language::Kotlin,
    Language::Swift,
    Language::Scala,
    Language::Dart,
    Language::Pascal,
];

/// Split `inner().method` into its two halves.
pub fn split_chain(name: &str) -> Option<(String, String)> {
    let caps = CHAIN_SHAPE.captures(name)?;
    Some((caps[1].to_string(), caps[2].to_string()))
}

/// Is this a reference the conformance pass should retry (`resolve_one` step 11)?
///
/// A `calls` reference in a [`CHAIN_LANGUAGES`] language matching [`CHAIN_SHAPE`],
/// or a PHP `this->prop.method`.
pub fn is_deferrable_chain(r: &UnresolvedRef) -> bool {
    if r.reference_kind != "calls" {
        return false;
    }
    let Some(lang) = Language::from_wire(&r.language) else {
        return false;
    };
    if CHAIN_LANGUAGES.contains(&lang) && CHAIN_SHAPE.is_match(&r.reference_name) {
        return true;
    }
    lang == Language::Php && PHP_PROP_SHAPE.is_match(&r.reference_name)
}

/// Is this the PHP property shape (which resolves through `match_method_call`,
/// not through a chain resolver)?
pub fn is_php_prop_chain(r: &UnresolvedRef) -> bool {
    r.language == Language::Php.as_str() && PHP_PROP_SHAPE.is_match(&r.reference_name)
}

/// Does this language's chain receiver use `::`?
pub fn is_scoped_chain_language(lang: Language) -> bool {
    SCOPED_CHAIN_LANGUAGES.contains(&lang)
}

// =============================================================================
// The three resolvers — all end in resolve_method_on_type. All → 0.85.
// =============================================================================

/// C/C++: `Foo::instance().bar` — the receiver is a `field_expression` call.
pub fn match_cpp_call_chain<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let (inner, method) = split_chain(&r.reference_name)?;
    let cls = resolve_cpp_call_result_type(&inner, r, ctx, 0)?;
    resolve_method_on_type(
        &cls,
        &method,
        r,
        ctx,
        0.85,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
}

/// PHP `Cls::for($x)->method()` (#608 — the per-tenant Laravel client idiom) and
/// Rust `Foo::new().bar()`, both encoded `Cls::factory().method`.
///
/// The receiver's type is what `Cls::factory` **returns**: the extractor's `self`
/// marker (PHP `: self`/`: static`, Rust `-> Self`) means the factory's own class;
/// anything else is a concrete type.
pub fn match_scoped_call_chain<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let (inner, method) = split_chain(&r.reference_name)?;
    // ONLY a static-factory (`Cls::method`) chain — an instance chain is left bare
    // so its existing resolution is untouched.
    if !inner.contains("::") {
        return None;
    }
    let factory_class = &inner[..inner.rfind("::")?];
    let ret = lookup_callee_return_type(&inner, r, ctx)?;

    // `self` is the extractor's marker for self/static/$this → the factory's class.
    let resolved_class = if ret == "self" {
        factory_class.to_string()
    } else {
        ret
    };
    resolve_method_on_type(
        &resolved_class,
        &method,
        r,
        ctx,
        0.85,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
}

/// The dot-notation languages: `Foo.getInstance().bar()`, encoded
/// `Foo.getInstance().bar`.
///
/// The receiver's type is `Foo.getInstance`'s declared return type. Two special
/// receivers:
///
/// - **A bare Go factory** (`New().Method()`): the return type first, and — on a
///   miss — a bare-name fallback (see the #760 note on that branch).
/// - **A bare capitalized constructor** (`Foo().method`): only in the languages
///   where an unprefixed capitalized call actually constructs
///   ([`CONSTRUCTS_VIA_BARE_CALL`]).
pub fn match_dotted_call_chain<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let lang = Language::from_wire(&r.language)?;
    let (inner, method) = split_chain(&r.reference_name)?;

    let Some(last_dot) = inner.rfind('.').filter(|i| *i > 0) else {
        // --- a BARE inner: `New().Method()` / `Foo().method()` ----------------
        if lang == Language::Go {
            if let Some(ret) = lookup_callee_return_type(&inner, r, ctx) {
                let fqn = imported_fqn_of(&ret, r, ctx);
                return resolve_method_on_type(
                    &ret,
                    &method,
                    r,
                    ctx,
                    0.85,
                    ResolvedBy::InstanceMethod,
                    fqn.as_deref(),
                    0,
                );
            }

            // `inner` is not a function with a captured return type — typically a
            // package-level VARIABLE holding a function value (gin's `engine()`),
            // whose type cannot be recovered. Fall back to bare-name resolution so
            // we do not DROP an edge the un-re-encoded path would have found.
            //
            // (When `inner` IS a real factory but the method does not exist on its
            // return type, the branch above already returned `None` — the
            // absent-method guarantee is preserved.)
            //
            // ⚠⚠ #760, THE RUNAWAY CONTRACT ⚠⚠
            // Resolve the TARGET through a synthetic bare-name reference, but return
            // the match tied to the **ORIGINAL** row (`inner().method`). The batch
            // loop reads pending rows from offset 0 every pass and drains them with a
            // delete keyed on `reference_name`. Propagating the synthetic ref's bare
            // `method` as `.original` would make that delete match NOTHING: the row
            // stays pending, the batch never empties, and the loop re-resolves and
            // re-inserts forever. That is not hypothetical — it grew gin's graph to
            // **5M edges / 1.4 GB** before it was caught.
            let mut synthetic = r.clone();
            synthetic.reference_name = method.clone();
            let bare =
                match_by_exact_name(&synthetic, ctx).or_else(|| match_fuzzy(&synthetic, ctx))?;
            return Some(ResolvedRef {
                original: r.clone(), // ← the STORED ROW, not the synthetic one
                ..bare
            });
        }

        // A bare capitalized inner is a class construction in the languages where an
        // unprefixed capitalized call constructs. A lowercase bare inner is a
        // top-level `factory().method()` whose type we cannot recover — bail.
        if !CONSTRUCTS_VIA_BARE_CALL.contains(&lang)
            || !inner.chars().next().is_some_and(|c| c.is_uppercase())
        {
            return None;
        }
        let fqn = imported_fqn_of(&inner, r, ctx);
        return resolve_method_on_type(
            &inner,
            &method,
            r,
            ctx,
            0.85,
            ResolvedBy::InstanceMethod,
            fqn.as_deref(),
            0,
        );
    };

    // --- `Receiver.factory(args).method()` ------------------------------------
    let factory_class = inner[..last_dot].rsplit('.').next()?; // the simple class name
    let factory_method = &inner[last_dot + 1..];
    if factory_class.is_empty() || factory_method.is_empty() {
        return None;
    }

    let ret = lookup_callee_return_type(&format!("{factory_class}::{factory_method}"), r, ctx)?;
    // (Wave 2, both at 0.8 and both in THIS branch's `None` arm: an ObjC
    // class-message factory — `[X alloc]`/`[X new]` returns an instance of `X` by
    // `instancetype` convention — and a Pascal `TFoo`/`IFoo` constructor, which has
    // no `: TBar` annotation but returns its own class. Neither fires when a
    // concrete return type WAS captured and simply lacks the method: that is the
    // absent-method guarantee, and it must not be traded away for coverage.)

    let fqn = imported_fqn_of(&ret, r, ctx);
    resolve_method_on_type(
        &ret,
        &method,
        r,
        ctx,
        0.85,
        ResolvedBy::InstanceMethod,
        fqn.as_deref(),
        0,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use selene_core::RefStatus;

    fn re(name: &str, lang: Language) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: "function:caller".into(),
            reference_name: name.into(),
            reference_kind: "calls".into(),
            line: Some(1),
            column: Some(0),
            candidates: vec![],
            file_path: "a.rs".into(),
            language: lang.as_str().into(),
            status: RefStatus::Pending,
            name_tail: name.into(),
        }
    }

    /// Spike F5a: the greedy `(.+)` binds the LAST `().` — JS semantics, and the
    /// Rust `regex` crate agrees. A chain re-encodes exactly ONE hop.
    #[test]
    fn the_chain_shape_binds_the_last_paren_dot() {
        assert_eq!(
            split_chain("Foo.getInstance().bar").unwrap(),
            ("Foo.getInstance".into(), "bar".into())
        );
        assert_eq!(
            split_chain("A().b().c").unwrap(),
            ("A().b".into(), "c".into()),
            "the greedy capture takes the LAST `().`"
        );
        assert_eq!(
            split_chain("Foo::new().bar").unwrap(),
            ("Foo::new".into(), "bar".into())
        );
        assert!(
            split_chain("foo.bar").is_none(),
            "the marker never appears in an ordinary ref"
        );
    }

    #[test]
    fn deferral_is_scoped_to_chain_languages_and_shapes() {
        assert!(is_deferrable_chain(&re("Foo.create().bar", Language::Java)));
        assert!(is_deferrable_chain(&re("Foo::new().bar", Language::Rust)));
        assert!(is_deferrable_chain(&re("New().Method", Language::Go)));
        assert!(is_deferrable_chain(&re("this->repo.save", Language::Php)));

        assert!(
            !is_deferrable_chain(&re("Foo.create().bar", Language::Typescript)),
            "TypeScript is deliberately NOT a chain language — gradual typing makes \
             the mechanism recall-negative there (−164 on nest)"
        );
        assert!(!is_deferrable_chain(&re("plain.call", Language::Java)));

        let mut not_a_call = re("Foo.create().bar", Language::Java);
        not_a_call.reference_kind = "references".into();
        assert!(!is_deferrable_chain(&not_a_call), "only `calls` refs defer");
    }
}
