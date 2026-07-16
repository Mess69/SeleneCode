#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 10 — function-ref resolution: unique-or-drop, class-scoped `this.X`,
//! and the inherited-member supertype pass.
//!
//! # Why every rule here refuses rather than guesses
//!
//! A wrong callback edge **claims a registration that does not exist**. It tells
//! the agent "this handler is wired up here" when it is not — which is strictly
//! worse than saying nothing, because the agent believes it. Every precision rule
//! below (`design/function-ref-capture.md` §Precision rules 3, 5, 9, 10) was bought
//! by a real-repo false positive.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::{
    ReferenceResolver, ResolvedBy, match_function_ref, resolve_this_member_fn_ref,
};

fn fn_ref(name: &str, from: &str, file: &str, lang: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.into(),
        reference_name: name.into(),
        reference_kind: "function_ref".into(),
        line: Some(10),
        column: Some(0),
        candidates: vec![],
        file_path: file.into(),
        language: lang,
        status: RefStatus::Pending,
        name_tail: name.rsplit(['.', ':']).next().unwrap_or(name).into(),
    }
}

fn func(id: &str, name: &str, file: &str, lang: Language) -> Node {
    node(id, NodeKind::Function, name, name, file, lang)
}

fn method(id: &str, ty: &str, name: &str, file: &str, lang: Language) -> Node {
    node(
        id,
        NodeKind::Method,
        name,
        &format!("{ty}::{name}"),
        file,
        lang,
    )
}

fn class(id: &str, name: &str, file: &str, lang: Language) -> Node {
    node(id, NodeKind::Class, name, name, file, lang)
}

// =============================================================================
// Unique-or-drop (rule 9)
// =============================================================================

#[test]
fn a_same_file_handler_resolves_at_0_95() {
    let ctx = FakeContext::new()
        .with_node(func("function:caller", "register", "src/a.c", Language::C))
        .with_node(func("function:handler", "on_recv", "src/a.c", Language::C));

    let hit = match_function_ref(
        &fn_ref("on_recv", "function:caller", "src/a.c", Language::C),
        &ctx,
    )
    .expect("the same-file callback resolves");
    assert_eq!(hit.target_node_id, "function:handler");
    assert_eq!(hit.confidence, 0.95);
    assert_eq!(hit.resolved_by, ResolvedBy::FunctionRef);
}

#[test]
fn a_unique_cross_file_handler_resolves_at_0_8() {
    let ctx = FakeContext::new()
        .with_node(func("function:caller", "register", "src/a.c", Language::C))
        .with_node(func(
            "function:handler",
            "on_recv",
            "src/handlers.c",
            Language::C,
        ));

    let hit = match_function_ref(
        &fn_ref("on_recv", "function:caller", "src/a.c", Language::C),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "function:handler");
    assert_eq!(hit.confidence, 0.8, "cross-file is a weaker claim");
}

/// THE rule. Two same-named handlers in two files and no way to choose: **no
/// edge**. Guessing here invents a registration.
#[test]
fn an_ambiguous_cross_file_handler_yields_no_edge() {
    let ctx = FakeContext::new()
        .with_node(func("function:caller", "register", "src/a.c", Language::C))
        .with_node(func("function:h1", "on_recv", "src/net.c", Language::C))
        .with_node(func("function:h2", "on_recv", "src/disk.c", Language::C));

    assert!(
        match_function_ref(
            &fn_ref("on_recv", "function:caller", "src/a.c", Language::C),
            &ctx
        )
        .is_none(),
        "ambiguity yields NO EDGE — never fuzzy, never a guess. A wrong callback \
         edge claims a wiring that does not exist."
    );
}

/// Same-file overloads are the same conceptual symbol: the earliest wins, at the
/// lower 0.9.
#[test]
fn same_file_overloads_pick_the_earliest_at_0_9() {
    let mut first = func("function:first", "handler", "src/a.cs", Language::CSharp);
    first.start_line = 10;
    let mut second = func("function:second", "handler", "src/a.cs", Language::CSharp);
    second.start_line = 40;

    let ctx = FakeContext::new()
        .with_node(func(
            "function:caller",
            "wire",
            "src/a.cs",
            Language::CSharp,
        ))
        .with_node(second)
        .with_node(first);

    let hit = match_function_ref(
        &fn_ref("handler", "function:caller", "src/a.cs", Language::CSharp),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "function:first", "earliest start_line");
    assert_eq!(hit.confidence, 0.9);
}

/// A function registering ITSELF is not a dependency edge.
#[test]
fn a_self_registration_is_excluded() {
    let ctx = FakeContext::new().with_node(func("function:me", "tick", "src/a.c", Language::C));
    assert!(
        match_function_ref(&fn_ref("tick", "function:me", "src/a.c", Language::C), &ctx).is_none(),
        "no self-loops"
    );
}

// =============================================================================
// Rule 3 — bare identifiers resolve to FUNCTIONS only in TS/JS/Python/C++/PHP
// =============================================================================

/// In TS a method is reachable only through a receiver, so a bare identifier
/// naming a method is a coincidence. Allowing method targets soaked up **locals
/// passed as arguments** on real repos.
#[test]
fn a_bare_ts_ref_never_resolves_to_a_method() {
    let ctx = FakeContext::new()
        .with_node(func(
            "function:caller",
            "wire",
            "src/a.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:onResize",
            "Widget",
            "onResize",
            "src/a.ts",
            Language::Typescript,
        ));

    assert!(
        match_function_ref(
            &fn_ref(
                "onResize",
                "function:caller",
                "src/a.ts",
                Language::Typescript
            ),
            &ctx
        )
        .is_none(),
        "a bare TS identifier can only be a FUNCTION — methods need a receiver \
         (`this.onResize`), and matching them soaked up locals passed as arguments"
    );
}

/// …but C# method groups ARE real method values, so it keeps method targets.
#[test]
fn a_bare_csharp_ref_may_resolve_to_a_method() {
    let ctx = FakeContext::new()
        .with_node(func(
            "function:caller",
            "Wire",
            "src/a.cs",
            Language::CSharp,
        ))
        .with_node(method(
            "method:OnTick",
            "Widget",
            "OnTick",
            "src/a.cs",
            Language::CSharp,
        ));

    let hit = match_function_ref(
        &fn_ref("OnTick", "function:caller", "src/a.cs", Language::CSharp),
        &ctx,
    )
    .expect("a C# method group is a real method value");
    assert_eq!(hit.target_node_id, "method:OnTick");
}

// =============================================================================
// The `::`-qualified member pointer (&Cls::method)
// =============================================================================

/// `&Widget::on_click` resolves **scoped to that class** — a `Decoy::handle` can
/// never match a `KtHandlers::handle` reference. That scope-anchoring is what lets
/// this shape skip the name gate at capture time.
#[test]
fn a_qualified_member_pointer_resolves_scoped_to_its_class() {
    let ctx = FakeContext::new()
        .with_node(func(
            "function:caller",
            "wire",
            "src/main.cpp",
            Language::Cpp,
        ))
        .with_node(method(
            "method:right",
            "Widget",
            "on_click",
            "src/widget.cpp",
            Language::Cpp,
        ))
        // A same-named method on ANOTHER class — the decoy.
        .with_node(method(
            "method:decoy",
            "Decoy",
            "on_click",
            "src/decoy.cpp",
            Language::Cpp,
        ));

    let hit = match_function_ref(
        &fn_ref(
            "Widget::on_click",
            "function:caller",
            "src/main.cpp",
            Language::Cpp,
        ),
        &ctx,
    )
    .expect("the qualified pointer resolves");
    assert_eq!(hit.target_node_id, "method:right", "NOT the decoy");
    assert_eq!(hit.confidence, 0.9);
}

#[test]
fn an_ambiguous_cross_file_qualified_pointer_is_dropped() {
    let ctx = FakeContext::new()
        .with_node(func(
            "function:caller",
            "wire",
            "src/main.cpp",
            Language::Cpp,
        ))
        .with_node(method(
            "method:a",
            "Widget",
            "on_click",
            "src/a.cpp",
            Language::Cpp,
        ))
        .with_node(method(
            "method:b",
            "Widget",
            "on_click",
            "src/b.cpp",
            Language::Cpp,
        ));

    assert!(
        match_function_ref(
            &fn_ref(
                "Widget::on_click",
                "function:caller",
                "src/main.cpp",
                Language::Cpp
            ),
            &ctx
        )
        .is_none(),
        "two `Widget::on_click` in two files, and nothing to choose between them"
    );
}

// =============================================================================
// `this.X` — class-scoped, with NO fallback of any kind
// =============================================================================

/// `addEventListener(…, this.onResize)` hits the enclosing class's method.
#[test]
fn a_this_member_ref_resolves_against_its_own_class() {
    let ctx = FakeContext::new()
        .with_node(method(
            "method:mount",
            "Canvas",
            "mount",
            "src/canvas.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:onResize",
            "Canvas",
            "onResize",
            "src/canvas.ts",
            Language::Typescript,
        ))
        // A same-named method on another class, in another file — never a target.
        .with_node(method(
            "method:decoy",
            "Other",
            "onResize",
            "src/other.ts",
            Language::Typescript,
        ));

    let (hit, deferred) = resolve_this_member_fn_ref(
        &fn_ref(
            "this.onResize",
            "method:mount",
            "src/canvas.ts",
            Language::Typescript,
        ),
        &ctx,
    );
    let hit = hit.expect("the enclosing class's own member resolves");
    assert_eq!(hit.target_node_id, "method:onResize", "NOT the decoy");
    assert_eq!(hit.confidence, 0.95);
    assert!(!deferred);
}

/// `this.fonts` is a PROPERTY (post-#808 field classification), not a function
/// value. It resolves to nothing — and crucially it does not fall back to a
/// same-named function elsewhere.
#[test]
fn a_this_property_yields_no_edge_and_no_wrong_fallback() {
    let ctx = FakeContext::new()
        .with_node(method(
            "method:mount",
            "Canvas",
            "mount",
            "src/canvas.ts",
            Language::Typescript,
        ))
        .with_node(node(
            "property:fonts",
            NodeKind::Property,
            "fonts",
            "Canvas::fonts",
            "src/canvas.ts",
            Language::Typescript,
        ))
        // A tempting same-named FUNCTION elsewhere.
        .with_node(func(
            "function:fonts",
            "fonts",
            "src/util.ts",
            Language::Typescript,
        ));

    let (hit, deferred) = resolve_this_member_fn_ref(
        &fn_ref(
            "this.fonts",
            "method:mount",
            "src/canvas.ts",
            Language::Typescript,
        ),
        &ctx,
    );
    assert!(hit.is_none(), "a property is not a function value");
    assert!(
        deferred,
        "it defers (the member COULD be inherited) — but the supertype pass will \
         also find nothing, and `this.` never settles for a same-named function \
         in another file"
    );
}

/// The inherited case (#808), end to end: the member is on a SUPERTYPE, so the
/// first pass defers and the supertype pass resolves it — once the edges exist.
#[test]
fn an_inherited_this_member_resolves_in_the_supertype_pass() {
    let ctx = FakeContext::new()
        .with_node(class(
            "class:MyForm",
            "MyForm",
            "src/form.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:wire",
            "MyForm",
            "wire",
            "src/form.ts",
            Language::Typescript,
        ))
        .with_node(class(
            "class:FormBase",
            "FormBase",
            "src/base.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:handleSubmit",
            "FormBase",
            "handleSubmit",
            "src/base.ts",
            Language::Typescript,
        ))
        // FormBase CONTAINS handleSubmit (the `contains` edge the walk uses).
        .with_member("class:FormBase", "method:handleSubmit");

    let row = fn_ref(
        "this.handleSubmit",
        "method:wire",
        "src/form.ts",
        Language::Typescript,
    );
    let mut resolver = ReferenceResolver::new(ctx);

    // Pass 1: the class's own members do not include it, and the extends edge does
    // not exist yet.
    assert!(resolver.resolve_one(&row).is_none());
    assert_eq!(
        resolver.deferred_this_member_refs().len(),
        1,
        "deferred, not dropped (#808)"
    );

    // The first pass persists `class MyForm extends FormBase`.
    resolver
        .ctx()
        .add_supertype_edge("class:MyForm", "class:FormBase");

    // Pass 2: the node-anchored BFS walks the edge and finds the inherited member.
    let resolved = resolver.resolve_deferred_this_member_refs();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].target_node_id, "method:handleSubmit");
    assert_eq!(resolved[0].confidence, 0.85);
    assert_eq!(resolved[0].resolved_by, ResolvedBy::FunctionRef);
    assert_eq!(
        resolved[0].original.reference_name, "this.handleSubmit",
        "the STORED row rides through unmutated (#760)"
    );

    // Drained.
    assert!(resolver.deferred_this_member_refs().is_empty());
    assert!(resolver.resolve_deferred_this_member_refs().is_empty());
}

/// The walk is **NODE-anchored, not name-keyed** — and this is the rails bug.
///
/// A name-keyed `get_supertypes("Engine")` unions the parents of EVERY class named
/// `Engine` in the repo (rails has a dozen), which produced a cross-class wrong
/// edge. Here: two unrelated `Engine` classes, and only ours extends a base that
/// declares the member. The other one's parent must never be walked.
#[test]
fn the_supertype_walk_is_node_anchored_and_never_unions_same_named_classes() {
    let ctx = FakeContext::new()
        // OUR Engine — extends nothing.
        .with_node(class(
            "class:ours",
            "Engine",
            "src/app/engine.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:wire",
            "Engine",
            "wire",
            "src/app/engine.ts",
            Language::Typescript,
        ))
        // A DIFFERENT `Engine`, in another file, which DOES extend a base carrying
        // a same-named member. A name-keyed union would pull this in.
        .with_node(class(
            "class:theirs",
            "Engine",
            "src/vendor/engine.ts",
            Language::Typescript,
        ))
        .with_node(class(
            "class:VendorBase",
            "VendorBase",
            "src/vendor/base.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:vendorHandler",
            "VendorBase",
            "handler",
            "src/vendor/base.ts",
            Language::Typescript,
        ))
        .with_member("class:VendorBase", "method:vendorHandler")
        .with_supertype("class:theirs", "class:VendorBase");

    let row = fn_ref(
        "this.handler",
        "method:wire",
        "src/app/engine.ts",
        Language::Typescript,
    );
    let mut resolver = ReferenceResolver::new(ctx);
    resolver.resolve_one(&row);

    assert!(
        resolver.resolve_deferred_this_member_refs().is_empty(),
        "OUR `Engine` extends nothing, so `this.handler` resolves to NOTHING. A \
         name-keyed supertype union would have walked the OTHER `Engine`'s parent \
         and produced a cross-class wrong edge — which is exactly what happened on \
         rails before the walk was made node-anchored."
    );
}

// =============================================================================
// The ladder wiring (step 4)
// =============================================================================

/// A `function_ref` NEVER reaches the name matcher or fuzzy — a wrong callback
/// edge is worse than none.
#[test]
fn a_function_ref_never_falls_through_to_fuzzy() {
    let ctx = FakeContext::new()
        .with_node(func(
            "function:caller",
            "wire",
            "src/a.ts",
            Language::Typescript,
        ))
        // Only a CASE-DIFFERENT name exists — fuzzy would happily take it.
        .with_node(func(
            "function:handler",
            "OnClick",
            "src/h.ts",
            Language::Typescript,
        ));

    let mut resolver = ReferenceResolver::new(ctx);
    assert!(
        resolver
            .resolve_one(&fn_ref(
                "onclick",
                "function:caller",
                "src/a.ts",
                Language::Typescript
            ))
            .is_none(),
        "a function_ref resolves by EXACT name or not at all — fuzzy would invent \
         a registration out of a case difference"
    );
}

/// Step 4 end to end, through the resolver.
#[test]
fn the_ladder_resolves_a_function_ref_at_step_4() {
    let ctx = FakeContext::new()
        .with_node(func("function:caller", "register", "src/a.c", Language::C))
        .with_node(func("function:handler", "on_recv", "src/a.c", Language::C));

    let mut resolver = ReferenceResolver::new(ctx);
    let row = fn_ref("on_recv", "function:caller", "src/a.c", Language::C);
    let hit = resolver.resolve_one(&row).expect("step 4 binds it");

    assert_eq!(hit.target_node_id, "function:handler");
    assert_eq!(hit.resolved_by, ResolvedBy::FunctionRef);
    assert_eq!(hit.original.reference_name, row.reference_name);
}
