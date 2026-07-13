#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 3 — the `resolve_one` ladder: the built-in filter matrix, the fast
//! pre-filter, and `create_edges`.
//!
//! The two shadowing rules are what these tests exist for. A built-in filter is
//! easy to write and easy to get *subtly* wrong in the direction that deletes
//! real edges: filter `get` and a Flask `def get()` handler loses its route;
//! filter `malloc` and a C project with a custom allocator loses every call to
//! it. Both cases are pinned below.

mod common;

use common::{FakeContext, node, ts_fn};
use selene_core::{EdgeKind, Language, NodeKind, Provenance, RefStatus, UnresolvedRef};
use selene_resolve::{ReferenceResolver, ResolvedBy, ResolvedRef, is_built_in_or_external};

/// An `UnresolvedRef` with the given name/kind/language.
fn r(name: &str, kind: &str, lang: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "function:caller".into(),
        reference_name: name.into(),
        reference_kind: kind.into(),
        line: Some(7),
        column: Some(3),
        candidates: vec![],
        file_path: "src/caller.ts".into(),
        language: lang.as_str().into(),
        status: RefStatus::Pending,
        name_tail: name.rsplit(['.', ':']).next().unwrap_or(name).into(),
    }
}

fn calls(name: &str, lang: Language) -> UnresolvedRef {
    r(name, "calls", lang)
}

// =============================================================================
// Step 1 — the built-in / external filter
// =============================================================================

#[test]
fn js_builtins_and_library_calls_are_filtered() {
    let ctx = FakeContext::new().with_node(ts_fn("function:mine", "myHelper", "src/a.ts"));

    for name in [
        "console",
        "console.log",
        "Math.floor",
        "JSON.parse",
        "Promise",
        "setTimeout",
        "useEffect", // a React hook from React itself
    ] {
        assert!(
            is_built_in_or_external(&calls(name, Language::Typescript), &ctx),
            "{name} must be filtered as a JS/TS built-in"
        );
    }
    assert!(
        !is_built_in_or_external(&calls("myHelper", Language::Typescript), &ctx),
        "a user symbol is never filtered"
    );
}

/// Python's shadowing rule, both halves. This is the test that stops the filter
/// from deleting a Flask view's route→handler edge.
#[test]
fn python_builtins_yield_to_user_symbols() {
    // A repo that declares a class `MyDict` and a view `def get()`.
    let ctx = FakeContext::new()
        .with_node(node(
            "class:MyDict",
            NodeKind::Class,
            "MyDict",
            "MyDict",
            "src/models.py",
            Language::Python,
        ))
        .with_node(node(
            "function:get",
            NodeKind::Function,
            "get",
            "get",
            "src/views.py",
            Language::Python,
        ));

    // Bare built-ins: always filtered.
    assert!(is_built_in_or_external(
        &calls("print", Language::Python),
        &ctx
    ));
    assert!(is_built_in_or_external(
        &calls("isinstance", Language::Python),
        &ctx
    ));

    // A call on a built-in TYPE receiver: filtered.
    assert!(is_built_in_or_external(
        &calls("dict.get", Language::Python),
        &ctx
    ));
    assert!(is_built_in_or_external(
        &calls("list.append", Language::Python),
        &ctx
    ));

    // A built-in METHOD on an unknown receiver: filtered (`items` is a local list).
    assert!(is_built_in_or_external(
        &calls("items.append", Language::Python),
        &ctx
    ));

    // ...but the CAPITALIZED receiver names a real class ⇒ the user's class wins.
    assert!(
        !is_built_in_or_external(&calls("myDict.get", Language::Python), &ctx),
        "`myDict.get` capitalizes to `MyDict`, which the repo declares — the \
         user class shadows the builtin type"
    );

    // A BARE builtin-method name is filtered ONLY when nothing declares it.
    assert!(
        !is_built_in_or_external(&calls("get", Language::Python), &ctx),
        "the repo declares `def get()` (a Flask view) — filtering it would \
         silently delete its route→handler edge"
    );
    assert!(
        is_built_in_or_external(&calls("extend", Language::Python), &ctx),
        "`extend` is declared nowhere ⇒ it really is the builtin"
    );
}

#[test]
fn go_stdlib_packages_and_builtins_are_filtered() {
    let ctx = FakeContext::new().with_node(node(
        "function:Serve",
        NodeKind::Function,
        "Serve",
        "Serve",
        "src/server.go",
        Language::Go,
    ));

    assert!(is_built_in_or_external(
        &calls("fmt.Println", Language::Go),
        &ctx
    ));
    assert!(is_built_in_or_external(
        &calls("http.ListenAndServe", Language::Go),
        &ctx
    ));
    assert!(is_built_in_or_external(&calls("make", Language::Go), &ctx));
    assert!(is_built_in_or_external(
        &calls("append", Language::Go),
        &ctx
    ));
    assert!(!is_built_in_or_external(
        &calls("Serve", Language::Go),
        &ctx
    ));
    assert!(
        !is_built_in_or_external(&calls("myPkg.Serve", Language::Go), &ctx),
        "a non-stdlib package receiver is not filtered"
    );
}

/// C/C++'s shadowing rule: `std::` always goes, but a stdlib NAME the user
/// redefines stays. C projects routinely wrap `printf`/`malloc`/`read`.
#[test]
fn c_stdlib_names_yield_to_user_definitions_but_std_never_does() {
    // A repo with its OWN `printf` (a logging wrapper) and no other stdlib names.
    let ctx = FakeContext::new().with_node(node(
        "function:printf",
        NodeKind::Function,
        "printf",
        "printf",
        "src/log.c",
        Language::C,
    ));

    assert!(
        !is_built_in_or_external(&calls("printf", Language::C), &ctx),
        "the repo defines its own `printf` — filtering it would make the graph \
         WRONG, not cleaner (user shadowing wins)"
    );
    assert!(
        is_built_in_or_external(&calls("malloc", Language::C), &ctx),
        "`malloc` is declared nowhere in this repo ⇒ it is the stdlib's"
    );
    assert!(
        is_built_in_or_external(&calls("std::sort", Language::Cpp), &ctx),
        "the `std::` prefix is filtered UNCONDITIONALLY — tree-sitter never \
         emits it as a user-defined qualified name"
    );
    assert!(
        is_built_in_or_external(&calls("make_unique", Language::Cpp), &ctx),
        "a C++ builtin nothing shadows"
    );
}

#[test]
fn an_untypeable_language_is_never_filtered() {
    let ctx = FakeContext::new();
    let mut odd = calls("print", Language::Python);
    odd.language = "klingon".into();
    assert!(
        !is_built_in_or_external(&odd, &ctx),
        "a language we cannot type is a language we cannot filter for — pass it \
         through rather than guessing (a wrong filter is a lost edge)"
    );
}

// =============================================================================
// Step 3 — the fast pre-filter
// =============================================================================

/// A name nothing could match short-circuits **before any strategy runs**. The
/// `FakeContext` read counter is the instrument: a graph read means a strategy
/// ran.
#[test]
fn the_pre_filter_short_circuits_before_any_strategy_reads_the_graph() {
    let ctx = FakeContext::new().with_node(ts_fn("function:known", "knownFn", "src/a.ts"));
    let mut resolver = ReferenceResolver::new(ctx);

    assert!(
        resolver
            .resolve_one(&calls("totallyUnknown", Language::Typescript))
            .is_none()
    );
    assert_eq!(
        resolver.ctx().read_count(),
        0,
        "the pre-filter is a hash lookup against known_names — a ref that can \
         match nothing must never touch the graph"
    );
}

/// The probe's qualified-name shapes: a dotted receiver, a dotted member, a
/// CAPITALIZED receiver, a JVM FQN's last segment, a Rust path's last segment,
/// and a path-like name's filename.
#[test]
fn the_pre_filter_probes_every_qualified_shape() {
    use selene_resolve::has_any_possible_match;

    let ctx = FakeContext::new()
        .with_node(ts_fn("function:handler", "handler", "src/a.ts"))
        .with_node(node(
            "class:Repo",
            NodeKind::Class,
            "Repo",
            "Repo",
            "src/repo.ts",
            Language::Typescript,
        ));

    assert!(has_any_possible_match("handler", &ctx), "direct");
    assert!(has_any_possible_match("Repo.save", &ctx), "dotted receiver");
    assert!(
        has_any_possible_match("thing.handler", &ctx),
        "dotted member"
    );
    assert!(
        has_any_possible_match("repo.save", &ctx),
        "CAPITALIZED receiver — `repo` → `Repo` (instance-method resolution)"
    );
    assert!(
        has_any_possible_match("com.example.deep.Repo", &ctx),
        "a JVM FQN's only useful segment is the LAST one"
    );
    assert!(
        has_any_possible_match("database::profiles::handler", &ctx),
        "a Rust path's only useful segment is the LAST one — without this the \
         pre-filter drops the ref before the Rust path resolver ever sees it"
    );
    assert!(
        has_any_possible_match("Repo::save", &ctx),
        "scoped receiver"
    );
    assert!(has_any_possible_match("lg:handler", &ctx), "Lua `:` member");
    assert!(has_any_possible_match("lg$handler", &ctx), "R `$` member");
    assert!(
        has_any_possible_match("snippets/handler", &ctx),
        "path-like: the filename after the last `/`"
    );

    assert!(!has_any_possible_match("nothingLikeThis", &ctx));
    assert!(!has_any_possible_match("no.match.here", &ctx));
}

/// The import escape: a renamed re-export names a symbol that is declared
/// NOWHERE (`import { login }` where the barrel does `export { signIn as login }`).
/// Without this arm, every renamed re-export silently loses its edge.
#[test]
fn the_pre_filter_lets_an_imported_name_through_even_with_no_declaration() {
    use selene_resolve::ImportMapping;

    let ctx = FakeContext::new()
        // The repo declares `signIn` — but NOT `login`.
        .with_node(ts_fn("function:signIn", "signIn", "src/auth.ts"))
        .with_import_mapping(
            "src/caller.ts",
            ImportMapping {
                local_name: "login".into(),
                exported_name: "login".into(),
                source: "./barrel".into(),
                is_default: false,
                is_namespace: false,
                resolved_path: None,
            },
        );

    assert!(
        !selene_resolve::has_any_possible_match("login", &ctx),
        "precondition: `login` is declared nowhere"
    );

    // ...yet the ref survives step 3, because the file IMPORTS that name.
    let login = calls("login", Language::Typescript);
    assert!(
        selene_resolve::matches_any_import(&login, &ctx),
        "the import escape is what carries a renamed re-export past the \
         pre-filter — without it, `login` is dropped at step 3 and the edge to \
         `signIn` is lost forever"
    );

    // A member access on a namespace import escapes too (`utils.parse`).
    let mut member = calls("login.now", Language::Typescript);
    member.file_path = "src/caller.ts".into();
    assert!(selene_resolve::matches_any_import(&member, &ctx));

    // A name the file does not import does NOT escape.
    let other = calls("logout", Language::Typescript);
    assert!(!selene_resolve::matches_any_import(&other, &ctx));

    // End to end: the ladder runs it (Task 6 gives it a strategy; today it is a
    // clean miss, not a panic).
    let mut resolver = ReferenceResolver::new(ctx);
    assert!(resolver.resolve_one(&login).is_none());
}

// =============================================================================
// `create_edges`
// =============================================================================

fn resolved(original: UnresolvedRef, target: &str, by: ResolvedBy, conf: f64) -> ResolvedRef {
    ResolvedRef {
        original,
        target_node_id: target.into(),
        confidence: conf,
        resolved_by: by,
    }
}

#[test]
fn create_edges_applies_the_three_promotions_and_nothing_else() {
    let ctx = FakeContext::new()
        .with_node(ts_fn("function:caller", "caller", "src/caller.ts"))
        .with_node(node(
            "class:Widget",
            NodeKind::Class,
            "Widget",
            "Widget",
            "src/widget.ts",
            Language::Typescript,
        ))
        .with_node(node(
            "interface:Drawable",
            NodeKind::Interface,
            "Drawable",
            "Drawable",
            "src/drawable.ts",
            Language::Typescript,
        ))
        .with_node(ts_fn("function:target", "target", "src/t.ts"));
    let resolver = ReferenceResolver::new(ctx);

    // (a) calls → instantiates, because the target is a class (`Widget()` in
    //     Python/Ruby is a call at extraction time, an instantiation once
    //     `Widget` is known to be a class).
    let mut call_ref = calls("Widget", Language::Typescript);
    call_ref.from_node_id = "function:caller".into();

    // (b) extends → implements, because the target is an interface and the
    //     source is a class.
    let mut ext_ref = r("Drawable", "extends", Language::Typescript);
    ext_ref.from_node_id = "class:Widget".into();

    // (c) function_ref → references (+ fnRef: true).
    let mut fn_ref = r("target", "function_ref", Language::Typescript);
    fn_ref.from_node_id = "function:caller".into();

    // (d) an ordinary call — no promotion.
    let mut plain = calls("target", Language::Typescript);
    plain.from_node_id = "function:caller".into();

    let edges = resolver.create_edges(&[
        resolved(call_ref, "class:Widget", ResolvedBy::ExactMatch, 0.9),
        resolved(ext_ref, "interface:Drawable", ResolvedBy::ExactMatch, 0.95),
        resolved(fn_ref, "function:target", ResolvedBy::FunctionRef, 0.8),
        resolved(plain, "function:target", ResolvedBy::Import, 0.9),
    ]);

    assert_eq!(edges.len(), 4, "one edge per resolved ref, in input order");
    assert_eq!(edges[0].kind, EdgeKind::Instantiates);
    assert_eq!(edges[1].kind, EdgeKind::Implements);
    assert_eq!(edges[2].kind, EdgeKind::References);
    assert_eq!(edges[3].kind, EdgeKind::Calls);

    for e in &edges {
        assert_eq!(e.provenance, Some(Provenance::TreeSitter));
    }

    // refKind is written ONLY when a promotion changed the kind.
    let md = |i: usize| edges[i].metadata.clone().unwrap();
    assert_eq!(md(0)["refKind"], "calls");
    assert_eq!(md(1)["refKind"], "extends");
    assert_eq!(md(2)["refKind"], "function_ref");
    assert!(
        md(3).get("refKind").is_none(),
        "an unpromoted edge carries NO refKind"
    );

    // fnRef marks function-as-value edges, and only those.
    assert_eq!(md(2)["fnRef"], true);
    for i in [0, 1, 3] {
        assert!(md(i).get("fnRef").is_none());
    }

    // refName is the ORIGINAL reference text — the resurrection key (#1240).
    assert_eq!(md(0)["refName"], "Widget");
    assert_eq!(md(3)["refName"], "target");
    assert_eq!(md(3)["resolvedBy"], "import");
    assert_eq!(md(3)["confidence"], 0.9);

    // Positions ride through from the reference.
    assert_eq!(edges[0].line, Some(7));
    assert_eq!(edges[0].column, Some(3));
}

/// An interface extending an interface really does *extend* it — the promotion
/// requires the SOURCE not to be an interface/protocol.
#[test]
fn an_interface_extending_an_interface_stays_extends() {
    let ctx = FakeContext::new()
        .with_node(node(
            "interface:A",
            NodeKind::Interface,
            "A",
            "A",
            "src/a.ts",
            Language::Typescript,
        ))
        .with_node(node(
            "interface:B",
            NodeKind::Interface,
            "B",
            "B",
            "src/b.ts",
            Language::Typescript,
        ));
    let resolver = ReferenceResolver::new(ctx);

    let mut ext = r("B", "extends", Language::Typescript);
    ext.from_node_id = "interface:A".into();
    let edges = resolver.create_edges(&[resolved(ext, "interface:B", ResolvedBy::ExactMatch, 0.9)]);

    assert_eq!(edges[0].kind, EdgeKind::Extends);
    assert!(edges[0].metadata.clone().unwrap().get("refKind").is_none());
}

/// `create_edges` must never mutate the reference name it carries — the keyed
/// delete (`GraphStore::delete_resolved`) matches on it, and a mutated name
/// no-ops the delete, re-reads the same rows forever, and explodes the run
/// (#760).
#[test]
fn refname_is_the_stored_row_verbatim() {
    let ctx = FakeContext::new()
        .with_node(ts_fn("function:caller", "caller", "src/caller.ts"))
        .with_node(node(
            "class:Foo",
            NodeKind::Class,
            "Foo",
            "Foo",
            "src/foo.ts",
            Language::Typescript,
        ));
    let resolver = ReferenceResolver::new(ctx);

    // A chained-factory marker: the shape most at risk of being "helpfully"
    // rewritten on the way through (Task 9's Go fallback).
    let mut chained = calls("Foo.create().bar", Language::Typescript);
    chained.from_node_id = "function:caller".into();
    let stored_name = chained.reference_name.clone();

    let edges = resolver.create_edges(&[resolved(
        chained,
        "class:Foo",
        ResolvedBy::QualifiedName,
        0.85,
    )]);

    assert_eq!(
        edges[0].metadata.clone().unwrap()["refName"],
        serde_json::json!(stored_name),
        "refName is the STORED row's name, marker and all — never a normalized \
         or reconstructed one (#760 / #1240)"
    );
}

#[test]
fn create_edges_on_an_empty_input_is_empty() {
    let resolver = ReferenceResolver::new(FakeContext::new());
    assert!(resolver.create_edges(&[]).is_empty());
}
