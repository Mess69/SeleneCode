#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 9 — chained-call resolution via `return_type`, and the conformance pass.
//!
//! `Foo.getInstance().bar()` must resolve to `Foo::bar` — **never to a same-named
//! decoy on an unrelated type**. Before this mechanism, 7 of 9 statically-typed
//! languages did exactly that: they dropped the receiver, name-matched `bar`, and
//! attached a *false* edge. That is why every language block below carries the same
//! two tests: the chain resolves, **and** an absent method yields nothing.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::{
    ReferenceResolver, ResolvedBy, is_deferrable_chain, match_cpp_call_chain,
    match_dotted_call_chain, match_scoped_call_chain,
};

fn chain(name: &str, file: &str, lang: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "function:caller".into(),
        reference_name: name.into(),
        reference_kind: "calls".into(),
        line: Some(10),
        column: Some(0),
        candidates: vec![],
        file_path: file.into(),
        language: lang.as_str().into(),
        status: RefStatus::Pending,
        name_tail: name.rsplit('.').next().unwrap_or(name).into(),
    }
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

/// A factory whose declared return type is captured (Phase 2's `Node.return_type`).
fn factory(id: &str, ty: &str, name: &str, returns: &str, file: &str, lang: Language) -> Node {
    let mut n = method(id, ty, name, file, lang);
    n.return_type = Some(returns.into());
    n
}

fn class(id: &str, name: &str, file: &str, lang: Language) -> Node {
    node(id, NodeKind::Class, name, name, file, lang)
}

// =============================================================================
// Java / Kotlin / C# — the dotted chain
// =============================================================================

/// The canonical case, and the decoy it must not take.
#[test]
fn a_java_dotted_chain_resolves_on_the_factorys_return_type() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:getInstance",
            "Foo",
            "getInstance",
            "Foo", // `static Foo getInstance()`
            "src/Foo.java",
            Language::Java,
        ))
        .with_node(method(
            "method:bar",
            "Foo",
            "bar",
            "src/Foo.java",
            Language::Java,
        ))
        // THE DECOY: a same-named `bar` on an unrelated type. A bare-name matcher
        // takes this one — that is the correctness bug this mechanism exists for.
        .with_node(method(
            "method:decoy",
            "Unrelated",
            "bar",
            "src/Other.java",
            Language::Java,
        ));

    let hit = match_dotted_call_chain(
        &chain("Foo.getInstance().bar", "src/Main.java", Language::Java),
        &ctx,
    )
    .expect("the chain resolves through the factory's return type");
    assert_eq!(hit.target_node_id, "method:bar", "NOT the decoy");
    assert_eq!(hit.confidence, 0.85);
    assert_eq!(hit.resolved_by, ResolvedBy::InstanceMethod);
}

/// THE SAFETY TEST. Every language block has one, and it is the assertion that
/// made this mechanism safe to ship.
#[test]
fn a_java_chain_creates_no_edge_when_the_type_lacks_the_method() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:getInstance",
            "Foo",
            "getInstance",
            "Foo",
            "src/Foo.java",
            Language::Java,
        ))
        // `Foo` exists, but it has no `missing` — while an unrelated type does.
        .with_node(method(
            "method:decoy",
            "Unrelated",
            "missing",
            "src/Other.java",
            Language::Java,
        ));

    assert!(
        match_dotted_call_chain(
            &chain("Foo.getInstance().missing", "src/Main.java", Language::Java),
            &ctx
        )
        .is_none(),
        "the receiver's type is known and does NOT declare the method ⇒ NO EDGE. \
         Never the same-named decoy."
    );
}

#[test]
fn a_kotlin_constructor_receiver_resolves_on_the_class_itself() {
    let ctx = FakeContext::new()
        .with_node(class("class:Foo", "Foo", "src/Foo.kt", Language::Kotlin))
        .with_node(method(
            "method:bar",
            "Foo",
            "bar",
            "src/Foo.kt",
            Language::Kotlin,
        ));

    // Kotlin constructs without `new`, so a bare capitalized `Foo()` IS a construction.
    let hit = match_dotted_call_chain(&chain("Foo().bar", "src/Main.kt", Language::Kotlin), &ctx)
        .expect("`Foo()` constructs a `Foo` in Kotlin");
    assert_eq!(hit.target_node_id, "method:bar");
    assert_eq!(hit.confidence, 0.85);
}

/// Java and C# need `new` — a bare `Foo()` there is a METHOD CALL, not a
/// construction. Treating it as one would invent a receiver type out of nothing.
#[test]
fn java_does_not_construct_via_a_bare_capitalized_call() {
    let ctx = FakeContext::new()
        .with_node(class("class:Foo", "Foo", "src/Foo.java", Language::Java))
        .with_node(method(
            "method:bar",
            "Foo",
            "bar",
            "src/Foo.java",
            Language::Java,
        ));

    assert!(
        match_dotted_call_chain(&chain("Foo().bar", "src/Main.java", Language::Java), &ctx)
            .is_none(),
        "in Java, `Foo()` is a call to a method named `Foo`, not a construction — \
         assuming otherwise would invent a type"
    );
}

#[test]
fn a_csharp_chain_resolves_and_stays_safe() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:Create",
            "Builder",
            "Create",
            "Builder",
            "src/Builder.cs",
            Language::CSharp,
        ))
        .with_node(method(
            "method:Build",
            "Builder",
            "Build",
            "src/Builder.cs",
            Language::CSharp,
        ));

    let hit = match_dotted_call_chain(
        &chain("Builder.Create().Build", "src/P.cs", Language::CSharp),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "method:Build");

    assert!(
        match_dotted_call_chain(
            &chain("Builder.Create().Missing", "src/P.cs", Language::CSharp),
            &ctx
        )
        .is_none()
    );
}

// =============================================================================
// Go — including the variable-inner fallback and its #760 runaway contract
// =============================================================================

#[test]
fn a_go_bare_factory_resolves_on_its_return_type() {
    let mut new_fn = node(
        "function:New",
        NodeKind::Function,
        "New",
        "New",
        "pkg/engine.go",
        Language::Go,
    );
    new_fn.return_type = Some("Engine".into());

    let ctx = FakeContext::new()
        .with_node(new_fn)
        .with_node(method(
            "method:Run",
            "Engine",
            "Run",
            "pkg/engine.go",
            Language::Go,
        ))
        .with_node(method(
            "method:decoy",
            "Other",
            "Run",
            "pkg/other.go",
            Language::Go,
        ));

    let hit = match_dotted_call_chain(&chain("New().Run", "main.go", Language::Go), &ctx)
        .expect("`New()` returns an `Engine`");
    assert_eq!(hit.target_node_id, "method:Run", "NOT the decoy");
    assert_eq!(hit.confidence, 0.85);
}

#[test]
fn a_go_factory_with_a_known_return_type_still_refuses_an_absent_method() {
    let mut new_fn = node(
        "function:New",
        NodeKind::Function,
        "New",
        "New",
        "pkg/engine.go",
        Language::Go,
    );
    new_fn.return_type = Some("Engine".into());

    let ctx = FakeContext::new().with_node(new_fn).with_node(method(
        "method:decoy",
        "Other",
        "Missing",
        "pkg/other.go",
        Language::Go,
    ));

    assert!(
        match_dotted_call_chain(&chain("New().Missing", "main.go", Language::Go), &ctx).is_none(),
        "the return type IS known and lacks the method — the bare-name fallback \
         must NOT fire here, or the absent-method guarantee is gone"
    );
}

/// The variable-inner fallback: `engine()` is a package-level VARIABLE holding a
/// function value (gin's shape), so its type cannot be recovered. Rather than drop
/// the edge the un-re-encoded path would have found, fall back to bare-name
/// resolution.
///
/// ⚠⚠ And this is the #760 RUNAWAY CONTRACT: the returned `original` MUST be the
/// stored row (`engine().Run`), not the synthetic bare `Run` used to find the
/// target. The batch loop drains rows with a delete keyed on `reference_name` — a
/// mutated name matches nothing, the row stays pending, the offset-0 loop re-reads
/// it forever, and the run explodes. It grew gin's graph to 5M edges / 1.4 GB.
#[test]
fn the_go_variable_inner_fallback_returns_the_stored_row_unmutated() {
    let ctx = FakeContext::new()
        // `engine` has NO return type — it is a variable holding a function value.
        .with_node(node(
            "variable:engine",
            NodeKind::Variable,
            "engine",
            "engine",
            "pkg/g.go",
            Language::Go,
        ))
        .with_node(method(
            "method:Run",
            "Engine",
            "Run",
            "pkg/engine.go",
            Language::Go,
        ));

    let row = chain("engine().Run", "main.go", Language::Go);
    let hit =
        match_dotted_call_chain(&row, &ctx).expect("the bare-name fallback finds the unique `Run`");
    assert_eq!(hit.target_node_id, "method:Run");

    assert_eq!(
        hit.original.reference_name, "engine().Run",
        "#760: the ORIGINAL row rides through unmutated. If the synthetic bare \
         `Run` were propagated here, the keyed delete would match nothing, the \
         batch would never drain, and the loop would re-resolve and re-insert \
         forever (5M edges / 1.4 GB on gin)."
    );
    assert_eq!(hit.original.reference_name, row.reference_name);
}

// =============================================================================
// Rust / PHP — the `::`-scoped chain
// =============================================================================

#[test]
fn a_rust_scoped_chain_resolves_self_returns_to_the_factorys_own_type() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:new",
            "Config",
            "new",
            "self", // `-> Self`, normalized by the extractor to the `self` marker
            "src/config.rs",
            Language::Rust,
        ))
        .with_node(method(
            "method:build",
            "Config",
            "build",
            "src/config.rs",
            Language::Rust,
        ))
        .with_node(method(
            "method:decoy",
            "Other",
            "build",
            "src/other.rs",
            Language::Rust,
        ));

    let hit = match_scoped_call_chain(
        &chain("Config::new().build", "src/main.rs", Language::Rust),
        &ctx,
    )
    .expect("`-> Self` means the factory's own class");
    assert_eq!(hit.target_node_id, "method:build", "NOT the decoy");
    assert_eq!(hit.confidence, 0.85);
}

#[test]
fn a_rust_scoped_chain_creates_no_edge_when_the_type_lacks_the_method() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:new",
            "Config",
            "new",
            "self",
            "src/config.rs",
            Language::Rust,
        ))
        .with_node(method(
            "method:decoy",
            "Other",
            "missing",
            "src/other.rs",
            Language::Rust,
        ));

    assert!(
        match_scoped_call_chain(
            &chain("Config::new().missing", "src/main.rs", Language::Rust),
            &ctx
        )
        .is_none()
    );
}

/// PHP `Cls::for($x)->method()` — the per-tenant Laravel client idiom (#608).
#[test]
fn a_php_scoped_chain_resolves_a_static_factory() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:for",
            "Client",
            "for",
            "self", // `: static`
            "src/Client.php",
            Language::Php,
        ))
        .with_node(method(
            "method:send",
            "Client",
            "send",
            "src/Client.php",
            Language::Php,
        ))
        .with_node(method(
            "method:decoy",
            "Other",
            "send",
            "src/Other.php",
            Language::Php,
        ));

    let hit = match_scoped_call_chain(
        &chain("Client::for().send", "src/App.php", Language::Php),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "method:send");
    assert!(
        match_scoped_call_chain(
            &chain("Client::for().missing", "src/App.php", Language::Php),
            &ctx
        )
        .is_none()
    );
}

/// An INSTANCE chain (`list.map().filter()`) is deliberately left bare by the
/// extractor's gate, so it never reaches here — and if one does, the `::` guard
/// refuses it rather than guessing.
#[test]
fn a_scoped_chain_refuses_a_non_scoped_inner() {
    let ctx = FakeContext::new().with_node(method(
        "method:filter",
        "Vec",
        "filter",
        "src/v.rs",
        Language::Rust,
    ));
    assert!(
        match_scoped_call_chain(
            &chain("list.map().filter", "src/main.rs", Language::Rust),
            &ctx
        )
        .is_none(),
        "only a `::` static-factory chain qualifies — an instance chain keeps its \
         existing (bare) resolution untouched"
    );
}

// =============================================================================
// C / C++
// =============================================================================

#[test]
fn a_cpp_singleton_chain_resolves_and_stays_safe() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:instance",
            "Registry",
            "instance",
            "Registry",
            "src/registry.cpp",
            Language::Cpp,
        ))
        .with_node(method(
            "method:lookup",
            "Registry",
            "lookup",
            "src/registry.cpp",
            Language::Cpp,
        ))
        .with_node(method(
            "method:decoy",
            "Other",
            "lookup",
            "src/other.cpp",
            Language::Cpp,
        ));

    let hit = match_cpp_call_chain(
        &chain("Registry::instance().lookup", "src/main.cpp", Language::Cpp),
        &ctx,
    )
    .expect("the singleton chain resolves");
    assert_eq!(hit.target_node_id, "method:lookup", "NOT the decoy");
    assert_eq!(hit.confidence, 0.85);

    assert!(
        match_cpp_call_chain(
            &chain(
                "Registry::instance().missing",
                "src/main.cpp",
                Language::Cpp
            ),
            &ctx
        )
        .is_none(),
        "absent method ⇒ no edge"
    );
}

// =============================================================================
// The deferral + the conformance pass (#750)
// =============================================================================

/// The whole reason the second pass exists — and the lifetime coupling that makes
/// it work.
///
/// During the FIRST pass the `implements`/`extends` edges do not exist yet (that
/// pass is what creates them), so an inherited method cannot resolve. The reference
/// is deferred **on the resolver instance**, and retried once the type graph is
/// real. This test runs ONE resolver across both worlds, because that is the only
/// arrangement in which the deferral means anything: the batched pass deletes the
/// row from the store as it processes it, so the in-memory queue is the only
/// surviving record of it.
#[test]
fn an_inherited_chained_method_resolves_only_in_the_conformance_pass() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:getInstance",
            "Dog",
            "getInstance",
            "Dog",
            "src/Dog.java",
            Language::Java,
        ))
        .with_node(class("class:Dog", "Dog", "src/Dog.java", Language::Java))
        .with_node(class(
            "class:Animal",
            "Animal",
            "src/Animal.java",
            Language::Java,
        ))
        // `speak` lives on the SUPERTYPE.
        .with_node(method(
            "method:speak",
            "Animal",
            "speak",
            "src/Animal.java",
            Language::Java,
        ));
    // NOTE: no supertype edge yet — this is exactly the first pass's view.

    let row = chain("Dog.getInstance().speak", "src/Main.java", Language::Java);
    let mut resolver = ReferenceResolver::new(ctx);

    // --- pass 1 ---------------------------------------------------------------
    assert!(
        resolver.resolve_one(&row).is_none(),
        "the first pass cannot see the inheritance — there is nothing to resolve \
         against, and it must NOT guess at the same-named method on another type"
    );
    assert_eq!(
        resolver.deferred_chain_refs().len(),
        1,
        "…so the reference is DEFERRED (#750)"
    );
    assert_eq!(
        resolver.deferred_chain_refs()[0].reference_name,
        "Dog.getInstance().speak"
    );

    // --- the first pass persists its edges ------------------------------------
    // `class Dog extends Animal` becomes a real edge in the graph. THIS is what the
    // conformance pass was waiting for.
    resolver
        .ctx()
        .add_supertype_edge("class:Dog", "class:Animal");

    // --- pass 2 ---------------------------------------------------------------
    let resolved = resolver.resolve_chained_calls_via_conformance();

    assert_eq!(resolved.len(), 1, "the conformance pass resolves it");
    assert_eq!(resolved[0].target_node_id, "method:speak");
    assert_eq!(resolved[0].confidence, 0.85);
    assert_eq!(
        resolved[0].original.reference_name, "Dog.getInstance().speak",
        "the STORED row rides through the second pass unmutated too (#760)"
    );

    // The queue is DRAINED — a second call must not re-resolve the same rows.
    assert!(resolver.deferred_chain_refs().is_empty());
    assert!(resolver.resolve_chained_calls_via_conformance().is_empty());
}

/// The conformance walk is still VALIDATED: a method that exists nowhere on the
/// chain yields no edge, even in the second pass.
#[test]
fn the_conformance_pass_still_refuses_an_absent_method() {
    let ctx = FakeContext::new()
        .with_node(factory(
            "method:getInstance",
            "Dog",
            "getInstance",
            "Dog",
            "src/Dog.java",
            Language::Java,
        ))
        .with_node(class("class:Dog", "Dog", "src/Dog.java", Language::Java))
        .with_node(class(
            "class:Animal",
            "Animal",
            "src/Animal.java",
            Language::Java,
        ))
        .with_supertype("class:Dog", "class:Animal")
        // A same-named method on an UNRELATED type — the decoy.
        .with_node(method(
            "method:decoy",
            "Bird",
            "fly",
            "src/Bird.java",
            Language::Java,
        ));

    let row = chain("Dog.getInstance().fly", "src/Main.java", Language::Java);
    let mut resolver = ReferenceResolver::new(ctx);
    resolver.resolve_one(&row);

    assert!(
        resolver.resolve_chained_calls_via_conformance().is_empty(),
        "neither `Dog` nor `Animal` declares `fly` — NO EDGE, not the `Bird` decoy"
    );
}

#[test]
fn only_chain_shaped_calls_in_chain_languages_are_deferred() {
    let ctx = FakeContext::new().with_node(method("method:x", "T", "x", "a.java", Language::Java));
    let mut resolver = ReferenceResolver::new(ctx);

    // A TypeScript chain is NOT deferred — the mechanism is deliberately not
    // shipped there (gradual typing ⇒ recall-negative).
    let ts = chain("Foo.create().bar", "src/a.ts", Language::Typescript);
    assert!(!is_deferrable_chain(&ts));
    resolver.resolve_one(&ts);
    assert!(resolver.deferred_chain_refs().is_empty());

    // An ordinary call is not a chain.
    let plain = chain("plain.call", "src/a.java", Language::Java);
    resolver.resolve_one(&plain);
    assert!(resolver.deferred_chain_refs().is_empty());
}
