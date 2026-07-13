#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 7 — the name matcher: file-path / qualified-name / exact-name / fuzzy.
//!
//! These are the named regressions from `__tests__/resolution.test.ts`. Every one
//! of them is a case where the matcher, left to its own devices, binds a
//! reference to the **wrong** target — and a wrong edge is worse than a missing
//! one, because the agent follows it.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::{
    ReferenceResolver, ResolvedBy, match_by_exact_name, match_by_file_path,
    match_by_qualified_name, match_fuzzy, match_reference,
};

fn r(name: &str, kind: &str, file: &str, lang: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "function:caller".into(),
        reference_name: name.into(),
        reference_kind: kind.into(),
        line: Some(12),
        column: Some(0),
        candidates: vec![],
        file_path: file.into(),
        language: lang.as_str().into(),
        status: RefStatus::Pending,
        name_tail: name.rsplit(['.', ':']).next().unwrap_or(name).into(),
    }
}

fn ts_node(id: &str, kind: NodeKind, name: &str, qn: &str, file: &str) -> Node {
    node(id, kind, name, qn, file, Language::Typescript)
}

fn file_node(path: &str, lang: Language) -> Node {
    let base = path.rsplit('/').next().unwrap_or(path);
    node(
        &format!("file:{path}"),
        NodeKind::File,
        base,
        path,
        path,
        lang,
    )
}

// =============================================================================
// Exact name
// =============================================================================

#[test]
fn a_unique_exact_match_resolves_at_0_9_and_cross_language_at_0_5() {
    let ctx = FakeContext::new().with_node(ts_node(
        "function:save",
        NodeKind::Function,
        "save",
        "save",
        "src/db.ts",
    ));

    let hit = match_by_exact_name(&r("save", "calls", "src/a.ts", Language::Typescript), &ctx)
        .expect("a unique name resolves");
    assert_eq!(hit.target_node_id, "function:save");
    assert_eq!(hit.confidence, 0.9);
    assert_eq!(hit.resolved_by, ResolvedBy::ExactMatch);

    // A cross-language `calls` bridge is real, but it is a WEAKER signal.
    let hit = match_by_exact_name(&r("save", "calls", "src/a.go", Language::Go), &ctx).unwrap();
    assert_eq!(
        hit.confidence, 0.5,
        "a cross-language single candidate is penalized, not rejected — the \
         branch is live for `calls`, which is ungated"
    );
}

/// Cross-module confidence lowering: a distant match is a weaker claim than a
/// near one, and the confidence has to say so — it is what `resolve_one` compares
/// against 0.9 when deciding whether to keep looking.
#[test]
fn a_distant_module_match_scores_lower_than_a_near_one() {
    let near = FakeContext::new()
        .with_node(ts_node(
            "a",
            NodeKind::Function,
            "save",
            "save",
            "src/db/a.ts",
        ))
        .with_node(ts_node(
            "b",
            NodeKind::Function,
            "save",
            "save",
            "src/db/b.ts",
        ));
    let hit = match_by_exact_name(
        &r("save", "calls", "src/db/caller.ts", Language::Typescript),
        &near,
    )
    .unwrap();
    assert_eq!(hit.confidence, 0.7, "proximity ≥ 30 (two shared segments)");

    let far = FakeContext::new()
        .with_node(ts_node(
            "a",
            NodeKind::Function,
            "save",
            "save",
            "vendor/x/a.ts",
        ))
        .with_node(ts_node(
            "b",
            NodeKind::Function,
            "save",
            "save",
            "other/y/b.ts",
        ));
    let hit = match_by_exact_name(
        &r("save", "calls", "src/db/caller.ts", Language::Typescript),
        &far,
    )
    .unwrap();
    assert_eq!(hit.confidence, 0.4, "nothing shared ⇒ a weak claim");
}

/// #1079 — a same-file definition is the strongest signal for which of several
/// same-named symbols a call means. Without it, resolution collapses onto
/// whichever was indexed first and a call in `b/svc` targets `a/svc`.
#[test]
fn a_same_file_definition_wins_over_a_same_named_one_elsewhere() {
    let ctx = FakeContext::new()
        .with_node(ts_node(
            "a",
            NodeKind::Function,
            "handle",
            "handle",
            "src/a/svc.ts",
        ))
        .with_node(ts_node(
            "b",
            NodeKind::Function,
            "handle",
            "handle",
            "src/b/svc.ts",
        ));

    let hit = match_by_exact_name(
        &r("handle", "calls", "src/b/svc.ts", Language::Typescript),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "b");
}

/// #915 — an `import` node is an import STATEMENT, not a definition. A reference
/// resolving to a sibling file's import is a meaningless edge (and re-declaring
/// `react` in every file made this O(K²)).
#[test]
fn an_import_kind_node_is_never_an_exact_name_target() {
    let ctx = FakeContext::new()
        .with_node(ts_node(
            "import:react",
            NodeKind::Import,
            "react",
            "react",
            "src/a.ts",
        ))
        .with_node(ts_node(
            "import:react2",
            NodeKind::Import,
            "react",
            "react",
            "src/b.ts",
        ));

    assert!(
        match_by_exact_name(
            &r("react", "references", "src/c.ts", Language::Typescript),
            &ctx
        )
        .is_none(),
        "import nodes are excluded — import→definition is resolve_via_import's \
         job, never name-matching's"
    );
}

/// #999, the ubiquitous-name ceiling, all three halves.
#[test]
fn the_ambiguous_name_ceiling_declines_rather_than_guesses() {
    // Just BELOW the ceiling: unchanged behavior.
    let mut below = FakeContext::new();
    for i in 0..500 {
        below = below.with_node(ts_node(
            &format!("f{i}"),
            NodeKind::Function,
            "get",
            "get",
            &format!("src/m{i}/a.ts"),
        ));
    }
    assert!(
        match_by_exact_name(
            &r("get", "calls", "src/m1/caller.ts", Language::Typescript),
            &below
        )
        .is_some(),
        "500 candidates is AT the ceiling, not above it — still resolves"
    );

    // ABOVE the ceiling: declines.
    let above = below.with_node(ts_node(
        "f500",
        NodeKind::Function,
        "get",
        "get",
        "src/m500/a.ts",
    ));
    assert!(
        match_by_exact_name(
            &r("get", "calls", "src/m1/caller.ts", Language::Typescript),
            &above
        )
        .is_none(),
        "above the ceiling, picking one of K by directory proximity is a guess — \
         DECLINE (the precise strategies already ran; fuzzy still follows, and it \
         only resolves a unique candidate)"
    );
}

/// The ceiling is compared against the GATED, import-filtered candidate count —
/// not the store's raw node count. A package re-declared as an `import` node in a
/// thousand files must not push its ONE real definition over the ceiling.
#[test]
fn import_nodes_do_not_push_a_real_definition_over_the_ceiling() {
    let mut ctx = FakeContext::new().with_node(ts_node(
        "function:real",
        NodeKind::Function,
        "logging",
        "logging",
        "src/logging.ts",
    ));
    // 600 import statements naming `logging` — one per file that imports it.
    for i in 0..600 {
        ctx = ctx.with_node(ts_node(
            &format!("import:{i}"),
            NodeKind::Import,
            "logging",
            "logging",
            &format!("src/m{i}/a.ts"),
        ));
    }

    assert_eq!(
        ctx.count_nodes_named_for_test("logging"),
        601,
        "the RAW node count is above the ceiling…"
    );
    let hit = match_by_exact_name(
        &r("logging", "calls", "src/caller.ts", Language::Typescript),
        &ctx,
    )
    .expect("…but the import nodes are excluded first, leaving ONE real candidate");
    assert_eq!(
        hit.target_node_id, "function:real",
        "gating the ceiling on the raw count would have declined here and \
         silently deleted this edge"
    );
}

// =============================================================================
// Qualified name
// =============================================================================

#[test]
fn a_single_exact_qualified_name_resolves_at_0_95() {
    let ctx = FakeContext::new().with_node(ts_node(
        "method:save",
        NodeKind::Method,
        "save",
        "Repo::save",
        "src/repo.ts",
    ));

    let hit = match_by_qualified_name(
        &r("Repo::save", "calls", "src/a.ts", Language::Typescript),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "method:save");
    assert_eq!(hit.confidence, 0.95);
    assert_eq!(hit.resolved_by, ResolvedBy::QualifiedName);
}

/// #1079, the qualified-name half: two files declare `Logger::log` (an ODR clash,
/// or two translation units). The call site's own file wins.
#[test]
fn an_ambiguous_qualified_name_prefers_the_call_sites_own_file() {
    let ctx = FakeContext::new()
        .with_node(ts_node(
            "a",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/a.ts",
        ))
        .with_node(ts_node(
            "b",
            NodeKind::Method,
            "log",
            "Logger::log",
            "src/b.ts",
        ));

    let hit = match_by_qualified_name(
        &r("Logger::log", "calls", "src/b.ts", Language::Typescript),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "b");
    assert_eq!(hit.confidence, 0.95);
}

#[test]
fn a_suffix_qualified_match_resolves_at_0_85() {
    let ctx = FakeContext::new().with_node(ts_node(
        "method:save",
        NodeKind::Method,
        "save",
        "app::models::Repo::save",
        "src/repo.ts",
    ));

    let hit = match_by_qualified_name(
        &r("Repo::save", "calls", "src/a.ts", Language::Typescript),
        &ctx,
    )
    .expect("a qualified name that is a SUFFIX of the node's own resolves");
    assert_eq!(hit.target_node_id, "method:save");
    assert_eq!(hit.confidence, 0.85);
}

/// #1180 — `service.process()` (a `calls` ref) shares an exact qualified name
/// with the yaml config key `service.process`. Resolving the call to the config
/// key is a wrong edge AND it hides the real callee.
#[test]
fn a_calls_ref_never_resolves_to_a_yaml_or_properties_constant() {
    let ctx = FakeContext::new()
        .with_node(node(
            "constant:key",
            NodeKind::Constant,
            "process",
            "service.process",
            "application.yml",
            Language::Yaml,
        ))
        .with_node(node(
            "constant:key2",
            NodeKind::Constant,
            "process",
            "service.process",
            "app.properties",
            Language::Properties,
        ));

    assert!(
        match_by_qualified_name(
            &r("service.process", "calls", "src/a.java", Language::Java),
            &ctx
        )
        .is_none(),
        "a CALL must never bind to a config key — it falls through to method \
         resolution instead"
    );

    // A `references` ref (a `@Value("${service.process}")` bind) is exactly what
    // SHOULD reach the config key — the framework resolvers build that bridge.
    let hit = match_by_qualified_name(
        &r(
            "service.process",
            "references",
            "src/a.java",
            Language::Java,
        ),
        &ctx,
    );
    assert!(
        hit.is_some(),
        "the exclusion is scoped to `calls` — a config bridge is a `references` edge"
    );
}

// =============================================================================
// File path
// =============================================================================

#[test]
fn a_path_like_reference_resolves_to_a_file_node() {
    let ctx = FakeContext::new()
        .with_node(file_node("src/snippets/menu.liquid", Language::Liquid))
        .with_node(file_node("other/menu.liquid", Language::Liquid));

    // An exact path.
    let hit = match_by_file_path(
        &r(
            "src/snippets/menu.liquid",
            "references",
            "src/index.liquid",
            Language::Liquid,
        ),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "file:src/snippets/menu.liquid");
    assert_eq!(hit.confidence, 0.95);
    assert_eq!(hit.resolved_by, ResolvedBy::FilePath);

    // A suffix.
    let hit = match_by_file_path(
        &r(
            "snippets/menu.liquid",
            "references",
            "src/index.liquid",
            Language::Liquid,
        ),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "file:src/snippets/menu.liquid");
    assert_eq!(hit.confidence, 0.85);
}

/// A bare symbol name is NOT a file. Without the extension guard, `handler` would
/// bind to some `handler.ts` file node instead of the function.
#[test]
fn a_bare_symbol_name_is_not_treated_as_a_file() {
    let ctx = FakeContext::new().with_node(file_node("src/handler.ts", Language::Typescript));
    assert!(
        match_by_file_path(
            &r("handler", "calls", "src/a.ts", Language::Typescript),
            &ctx
        )
        .is_none(),
        "no slash and no extension ⇒ it is a symbol, and the symbol strategies own it"
    );
}

/// The three tiers, in order. A bare `Foo.h` IS a suffix of `deep/nested/Foo.h`,
/// so it takes the 0.85 tier; the 0.7 tier is only reached when the basename
/// matches but the path does NOT end with the reference — a genuinely weak claim.
#[test]
fn the_file_path_tiers_are_exact_then_suffix_then_lone_basename() {
    // Suffix: `Foo.h` really is the tail of `deep/nested/Foo.h`.
    let ctx = FakeContext::new().with_node(file_node("deep/nested/Foo.h", Language::C));
    let hit = match_by_file_path(&r("Foo.h", "imports", "src/main.c", Language::C), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "file:deep/nested/Foo.h");
    assert_eq!(
        hit.confidence, 0.85,
        "a suffix match, not a lone-basename one"
    );

    // Lone basename: the reference names a DIFFERENT directory, so the path is
    // not a suffix of it — the basename is all we have.
    let hit = match_by_file_path(
        &r("wrong/dir/Foo.h", "imports", "src/main.c", Language::C),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "file:deep/nested/Foo.h");
    assert_eq!(
        hit.confidence, 0.7,
        "the basename matched but the path did not — a weak claim, and it says so"
    );
}

// =============================================================================
// Fuzzy
// =============================================================================

#[test]
fn fuzzy_resolves_a_unique_case_insensitive_match_and_refuses_an_ambiguous_one() {
    let unique = FakeContext::new().with_node(ts_node(
        "function:save",
        NodeKind::Function,
        "SaveUser",
        "SaveUser",
        "src/db.ts",
    ));
    let hit = match_fuzzy(
        &r("saveuser", "calls", "src/a.ts", Language::Typescript),
        &unique,
    )
    .expect("a unique lowercase match resolves");
    assert_eq!(hit.confidence, 0.5);
    assert_eq!(hit.resolved_by, ResolvedBy::Fuzzy);

    // Two candidates ⇒ NO edge. Guessing between them is exactly the wrong-edge
    // failure this crate is shaped to avoid.
    let ambiguous = unique.with_node(ts_node(
        "function:save2",
        NodeKind::Function,
        "saveUser",
        "saveUser",
        "src/other.ts",
    ));
    assert!(
        match_fuzzy(
            &r("saveuser", "calls", "src/a.ts", Language::Typescript),
            &ambiguous
        )
        .is_none(),
        "unique or nothing"
    );
}

#[test]
fn fuzzy_only_considers_callable_kinds() {
    let ctx = FakeContext::new().with_node(ts_node(
        "variable:x",
        NodeKind::Variable,
        "Config",
        "Config",
        "src/c.ts",
    ));
    assert!(
        match_fuzzy(
            &r("config", "calls", "src/a.ts", Language::Typescript),
            &ctx
        )
        .is_none(),
        "a variable is not a fuzzy-matchable target — only function/method/class"
    );
}

// =============================================================================
// The dispatcher
// =============================================================================

/// The ladder returns the FIRST strategy that produces anything — so a qualified
/// name at 0.85 beats an exact name that would have scored 0.9. The order IS the
/// precedence, because a qualified name carries more information.
#[test]
fn the_dispatcher_prefers_a_qualified_match_over_an_exact_one() {
    let ctx = FakeContext::new()
        // The qualified (suffix) candidate — would score 0.85.
        .with_node(ts_node(
            "method:right",
            NodeKind::Method,
            "save",
            "app::Repo::save",
            "src/repo.ts",
        ))
        // A decoy whose NAME is literally `Repo::save` — `match_by_exact_name`
        // would take it at 0.9, a HIGHER confidence than the suffix match above.
        .with_node(ts_node(
            "function:decoy",
            NodeKind::Function,
            "Repo::save",
            "decoys::Repo::save",
            "src/decoy.ts",
        ));

    let hit = match_reference(
        &r("Repo::save", "calls", "src/a.ts", Language::Typescript),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        hit.target_node_id, "method:right",
        "the qualified-name strategy runs FIRST and its hit is RETURNED — even \
         though the exact-name decoy would have scored higher (0.9 > 0.85). The \
         ladder is precedence, not a confidence comparison: a qualified name \
         carries more information than a bare one."
    );
    assert_eq!(hit.confidence, 0.85);
}

/// A `function_ref` short-circuits the whole strategy ladder: it resolves through
/// `match_function_ref` (Task 10) or not at all. It must never reach the exact-name
/// or fuzzy strategies, where a wrong callback edge would claim a registration that
/// does not exist.
#[test]
fn a_function_ref_short_circuits_to_its_own_matcher() {
    let ctx = FakeContext::new().with_node(ts_node(
        "function:handler",
        NodeKind::Function,
        "handler",
        "handler",
        "src/h.ts",
    ));

    // It resolves — but through the FUNCTION-REF matcher, with its own rules and its
    // own `resolved_by`, not through the name strategies.
    let hit = match_reference(
        &r("handler", "function_ref", "src/a.ts", Language::Typescript),
        &ctx,
    )
    .expect("a unique cross-file handler resolves");
    assert_eq!(hit.target_node_id, "function:handler");
    assert_eq!(
        hit.resolved_by,
        ResolvedBy::FunctionRef,
        "NOT ExactMatch — the function-ref matcher owns this reference kind"
    );
    assert_eq!(hit.confidence, 0.8, "cross-file unique-or-drop, not 0.9");

    // And it never falls through to fuzzy: a case-different name resolves to NOTHING,
    // where an ordinary `calls` ref would have fuzzy-matched it at 0.5.
    assert!(
        match_reference(
            &r("HANDLER", "function_ref", "src/a.ts", Language::Typescript),
            &ctx
        )
        .is_none(),
        "no fuzzy fallback, ever — a case difference is not a registration"
    );
    assert!(
        match_reference(
            &r("HANDLER", "calls", "src/a.ts", Language::Typescript),
            &ctx
        )
        .is_some(),
        "…while an ordinary call DOES reach fuzzy — which is exactly the difference"
    );
}

/// Ladder step 10, end to end: the resolver reaches the name matcher and the
/// stored row rides through unmutated (#760).
#[test]
fn the_resolver_reaches_the_name_matcher_at_step_10() {
    let ctx = FakeContext::new().with_node(ts_node(
        "function:save",
        NodeKind::Function,
        "save",
        "save",
        "src/db.ts",
    ));
    let mut resolver = ReferenceResolver::new(ctx);

    let row = r("save", "calls", "src/a.ts", Language::Typescript);
    let hit = resolver.resolve_one(&row).expect("step 10 binds it");
    assert_eq!(hit.target_node_id, "function:save");
    assert_eq!(hit.resolved_by, ResolvedBy::ExactMatch);
    assert_eq!(
        hit.original.reference_name, row.reference_name,
        "the STORED row rides through unmutated — the keyed delete matches on it"
    );
}

/// The language gate at step 10: a `references` ref must not bind across two
/// known families, even when the name matches exactly.
#[test]
fn the_step_10_gate_drops_a_cross_family_references_match() {
    let ctx = FakeContext::new().with_node(node(
        "class:TestRunner",
        NodeKind::Class,
        "TestRunner",
        "TestRunner",
        "src/Runner.kt",
        Language::Kotlin,
    ));
    let mut resolver = ReferenceResolver::new(ctx);

    assert!(
        resolver
            .resolve_one(&r("TestRunner", "references", "src/app.tsx", Language::Tsx))
            .is_none(),
        "a TS `<TestRunner>` type reference must not bind to a Kotlin class — \
         web↔jvm is a crossing of two KNOWN families"
    );

    // …but a `calls` bridge across languages is legitimate and survives.
    let ctx = FakeContext::new().with_node(node(
        "function:native",
        NodeKind::Function,
        "nativeCall",
        "nativeCall",
        "src/Native.kt",
        Language::Kotlin,
    ));
    let mut resolver = ReferenceResolver::new(ctx);
    assert!(
        resolver
            .resolve_one(&r("nativeCall", "calls", "src/app.tsx", Language::Tsx))
            .is_some(),
        "a cross-language `calls` bridge (React Native JS → native) is real and \
         must survive the gate"
    );
}
