#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 11 — the framework registry, the route-node contract, and `find_route`.
//!
//! The registry is EMPTY until Tasks 12–20 land, so these tests drive it through
//! `detect_frameworks_among` / `applicable_frameworks`, which take an explicit
//! resolver list. That is the injection seam (an integration test lives in
//! another crate and cannot reach a `#[cfg(test)]` hook inside the lib) — and it
//! is also how every real caller composes, so nothing here is a test-only path.

use std::collections::BTreeSet;
use std::path::Path;

use selene_core::{FileRecord, Language, Node, NodeKind, UnresolvedRef};
use selene_db::SurrealStore;
use selene_resolve::frameworks::{
    FrameworkExtraction, FrameworkResolver, REGISTRY_ORDER, RouteSpec, all_framework_resolvers,
    applicable_frameworks, detect_frameworks_among, find_route, framework_resolver, route_node,
    run_framework_extract_for_files,
};
use selene_resolve::{ResolutionContext, ResolvedRef, StoreContext};

mod common;

// =============================================================================
// Fixtures
// =============================================================================

fn file_record(path: &str, lang: Language) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: "deadbeef".to_string(),
        language: lang.as_str().to_string(),
        size: 10,
        modified_at: 0,
        indexed_at: 0,
        node_count: 0,
        errors: vec![],
    }
}

/// A real `StoreContext` over a real in-memory `SurrealStore`, rooted at `dir`
/// (so `read_file` reads the actual files on disk).
async fn store_ctx(dir: &Path, files: &[(&str, Language)]) -> StoreContext<SurrealStore> {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    for (p, l) in files {
        store.upsert_file(&file_record(p, *l)).await.unwrap();
    }
    StoreContext::new(store, dir.to_path_buf()).await.unwrap()
}

// =============================================================================
// Test resolvers
// =============================================================================

/// A resolver that detects, speaks one language, and emits one route per file.
struct Fake {
    name: &'static str,
    langs: Option<&'static [Language]>,
    detects: bool,
    panics_on_detect: bool,
    emits_route: Option<(&'static str, &'static str)>, // (METHOD, path)
}

impl FrameworkResolver for Fake {
    fn name(&self) -> &'static str {
        self.name
    }
    fn languages(&self) -> Option<&'static [Language]> {
        self.langs
    }
    fn detect(&self, _ctx: &dyn ResolutionContext) -> bool {
        assert!(!self.panics_on_detect, "detect() panics on purpose");
        self.detects
    }
    fn resolve(&self, _r: &UnresolvedRef, _ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        None
    }
    fn extract(&self, path: &str, _content: &str, lang: Language) -> FrameworkExtraction {
        let Some((method, route_path)) = self.emits_route else {
            return FrameworkExtraction::default();
        };
        FrameworkExtraction {
            nodes: vec![route_node(
                &RouteSpec::new(self.name, Some(method), route_path, path, 1),
                lang,
                0,
            )],
            refs: vec![],
        }
    }
}

fn fake(name: &'static str, langs: Option<&'static [Language]>, detects: bool) -> Fake {
    Fake {
        name,
        langs,
        detects,
        panics_on_detect: false,
        emits_route: None,
    }
}

// =============================================================================
// Registry: order, filtering, panic containment
// =============================================================================

/// Registry order IS resolve precedence (first hit ≥ 0.9 short-circuits), so an
/// unstable order is a silent behavior change. A `HashMap` here would flap.
#[test]
fn registry_order_is_stable_across_calls() {
    let first: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    for _ in 0..100 {
        let again: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
        assert_eq!(again, first, "registry order must not vary between calls");
    }
}

/// Whatever is registered must appear in `REGISTRY_ORDER`, in that relative
/// order. This is the guard for Tasks 12–20: adding a resolver out of order (or
/// under a name not in the contract) fails here.
#[test]
fn registry_order_matches_the_contract() {
    let registered: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    for name in &registered {
        assert!(
            REGISTRY_ORDER.contains(name),
            "resolver '{name}' is registered but not declared in REGISTRY_ORDER"
        );
    }
    let expected: Vec<&str> = REGISTRY_ORDER
        .iter()
        .copied()
        .filter(|n| registered.contains(n))
        .collect();
    assert_eq!(
        registered, expected,
        "registry must be in REGISTRY_ORDER (first-match-wins precedence), not alphabetical"
    );
    // Names are unique.
    let unique: BTreeSet<&str> = registered.iter().copied().collect();
    assert_eq!(unique.len(), registered.len(), "duplicate resolver name");

    // And every registered resolver is reachable by name.
    for name in &registered {
        assert!(framework_resolver(name).is_some());
    }
    assert!(framework_resolver("no-such-framework").is_none());
}

/// `languages() == None` means "every language"; a listed language filters.
#[test]
fn applicable_frameworks_filters_by_language() {
    const TS: &[Language] = &[Language::Typescript, Language::Tsx];
    const PY: &[Language] = &[Language::Python];

    let ts = fake("express", Some(TS), true);
    let py = fake("django", Some(PY), true);
    let all = fake("universal", None, true);
    let detected: Vec<&dyn FrameworkResolver> = vec![&ts, &py, &all];

    let for_ts: Vec<&str> = applicable_frameworks(&detected, Language::Typescript)
        .iter()
        .map(|r| r.name())
        .collect();
    assert_eq!(
        for_ts,
        vec!["express", "universal"],
        "a TS file gets the TS framework and the language-agnostic one — in registry order"
    );

    let for_py: Vec<&str> = applicable_frameworks(&detected, Language::Python)
        .iter()
        .map(|r| r.name())
        .collect();
    assert_eq!(for_py, vec!["django", "universal"]);

    let for_go: Vec<&str> = applicable_frameworks(&detected, Language::Go)
        .iter()
        .map(|r| r.name())
        .collect();
    assert_eq!(for_go, vec!["universal"], "no TS/PY framework on a Go file");
}

/// Errors are collected, never thrown: a resolver that panics while detecting is
/// excluded with a warning, and the *other* frameworks still detect. One bad
/// manifest regex must not fail the whole index.
#[test]
fn a_panicking_detect_is_caught_and_excluded() {
    let good = fake("express", None, true);
    let bad = Fake {
        name: "explodes",
        langs: None,
        detects: true,
        panics_on_detect: true,
        emits_route: None,
    };
    let also_good = fake("django", None, true);
    let resolvers: Vec<&dyn FrameworkResolver> = vec![&good, &bad, &also_good];

    let ctx = common::FakeContext::new();

    // Silence the panic backtrace this test intentionally triggers.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (detected, warnings) = detect_frameworks_among(&resolvers, &ctx);
    std::panic::set_hook(prev);

    let names: Vec<&str> = detected.iter().map(|r| r.name()).collect();
    assert_eq!(
        names,
        vec!["express", "django"],
        "the panicking framework is dropped; the others survive and keep their order"
    );
    assert_eq!(warnings.len(), 1, "the failure is reported, not swallowed");
    assert!(
        warnings[0].contains("explodes") && warnings[0].contains("detect()"),
        "the warning names the culprit: {}",
        warnings[0]
    );
}

// =============================================================================
// The route-node contract
// =============================================================================

/// The load-bearing one. axum's `.route("/x", get(h).post(h2))` emits two routes
/// from ONE line: same file, same kind, same line. The hash input is
/// `(file, kind, name, line)` — so they are distinguished ONLY by `name`, which
/// is why the name carries the verb. A framework author who names a route by its
/// path alone silently collapses N routes into one; this is the test that says so.
#[test]
fn route_node_ids_are_distinct_for_two_routes_on_one_line() {
    let get = route_node(
        &RouteSpec::new("rust", Some("GET"), "/x", "src/main.rs", 12),
        Language::Rust,
        0,
    );
    let post = route_node(
        &RouteSpec::new("rust", Some("POST"), "/x", "src/main.rs", 12),
        Language::Rust,
        0,
    );

    assert_ne!(
        get.id, post.id,
        "GET /x and POST /x on one line MUST be two nodes — if this fails, the name \
         spelling stopped carrying the verb and every chained-verb route is being \
         silently overwritten"
    );
    assert_eq!(get.name, "GET /x");
    assert_eq!(post.name, "POST /x");
    assert_eq!(get.start_line, 12);
    assert_eq!(get.route_method.as_deref(), Some("GET"));
    assert_eq!(get.route_path.as_deref(), Some("/x"));
    assert_eq!(get.framework.as_deref(), Some("rust"));

    // The id is the ORDINARY hashed node id — it is not the TS literal string,
    // and it does not contain the method or path anywhere in it.
    assert_eq!(
        get.id,
        selene_core::node_id("src/main.rs", NodeKind::Route, "GET /x", 12),
        "route ids are the plain hashed node id — no special case"
    );
    assert!(
        !get.id.contains("/x") && !get.id.contains("GET"),
        "the id must be opaque: semantics live in the indexed fields, not the id string"
    );
}

/// The name/qualified-name spellings are agent-visible wire strings (explore
/// prints them), so they are pinned per router shape.
#[test]
fn route_name_and_qualified_name_spellings() {
    // Verb router.
    let n = route_node(
        &RouteSpec::new("express", Some("POST"), "/users/login", "src/app.ts", 4),
        Language::Typescript,
        0,
    );
    assert_eq!(n.name, "POST /users/login");
    assert_eq!(n.qualified_name, "src/app.ts::POST:/users/login");

    // Path-only router (django path(), React Router): no method, bare path.
    let n = route_node(
        &RouteSpec::new("react", None, "/article/:slug", "src/App.tsx", 9),
        Language::Tsx,
        0,
    );
    assert_eq!(n.name, "/article/:slug");
    assert_eq!(n.qualified_name, "src/App.tsx::route:/article/:slug");
    assert_eq!(n.route_method, None, "a path-only router has no verb");

    // Verb-less registration.
    let n = route_node(
        &RouteSpec::new("go", Some("ANY"), "/health", "main.go", 2),
        Language::Go,
        0,
    );
    assert_eq!(n.name, "ANY /health");

    // DRF viewset rides the verb shape.
    let n = route_node(
        &RouteSpec::new("django", Some("VIEWSET"), "/articles", "urls.py", 7),
        Language::Python,
        0,
    );
    assert_eq!(n.name, "VIEWSET /articles");

    // Laravel's `resource:` spelling is the one that needs an override.
    let mut spec = RouteSpec::new("laravel", None, "articles", "routes/api.php", 3);
    spec.name_override = Some("resource:articles");
    spec.qualified_name_override = Some("routes/api.php::RESOURCE:articles");
    let n = route_node(&spec, Language::Php, 0);
    assert_eq!(n.name, "resource:articles");
    assert_eq!(n.qualified_name, "routes/api.php::RESOURCE:articles");
    assert_eq!(
        n.route_path.as_deref(),
        Some("articles"),
        "an overridden name does not change the queryable fields"
    );
}

/// Every ordinary (non-route) node leaves the three route fields absent from its
/// JSON entirely — NOT `null`. This is what keeps Phase 2's snapshots and parity
/// baseline byte-identical after the field addition.
#[test]
fn non_route_nodes_serialize_without_the_route_fields() {
    let n: Node = common::node(
        "function:f",
        NodeKind::Function,
        "f",
        "f",
        "src/a.ts",
        Language::Typescript,
    );
    let json = serde_json::to_value(&n).unwrap();
    let obj = json.as_object().unwrap();
    for key in ["routeMethod", "routePath", "framework"] {
        assert!(
            !obj.contains_key(key),
            "'{key}' must be ABSENT (not null) on an ordinary node — a null here \
             would move every Phase 2 snapshot"
        );
    }
}

// =============================================================================
// find_route — the indexed lookup (the only supported way)
// =============================================================================

/// Two routes on the same path, different verbs. The verb filter must select
/// exactly one — this is the query that replaces TS's id-string key-matching.
#[tokio::test(flavor = "multi_thread")]
async fn find_route_filters_by_verb_framework_and_path() {
    let store = SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();

    let get = route_node(
        &RouteSpec::new("rust", Some("GET"), "/x", "src/main.rs", 12),
        Language::Rust,
        0,
    );
    let post = route_node(
        &RouteSpec::new("rust", Some("POST"), "/x", "src/main.rs", 12),
        Language::Rust,
        0,
    );
    let other = route_node(
        &RouteSpec::new("express", Some("GET"), "/x", "src/app.ts", 3),
        Language::Typescript,
        0,
    );
    store
        .insert_nodes(&[get.clone(), post.clone(), other.clone()])
        .await
        .unwrap();

    // Path only → all three.
    let all = find_route(&store, None, None, "/x").await.unwrap();
    assert_eq!(all.len(), 3);

    // Path + verb → the two GETs.
    let gets = find_route(&store, None, Some("GET"), "/x").await.unwrap();
    assert_eq!(gets.len(), 2);
    assert!(
        gets.iter()
            .all(|n| n.route_method.as_deref() == Some("GET"))
    );

    // Path + verb + framework → exactly one.
    let one = find_route(&store, Some("rust"), Some("GET"), "/x")
        .await
        .unwrap();
    assert_eq!(one.len(), 1, "framework+verb+path selects a single route");
    assert_eq!(one[0].id, get.id);
    assert_eq!(one[0].name, "GET /x");

    // A path nobody registered → empty, not an error.
    assert!(
        find_route(&store, None, None, "/nope")
            .await
            .unwrap()
            .is_empty(),
        "an unknown route is an empty result, never an error"
    );
}

// =============================================================================
// The emission pass
// =============================================================================

/// `run_framework_extract_for_files` writes the emitted route nodes to the store
/// and they come back through the indexed lookup — the full emission → query
/// round trip, with a framework that is language-gated OUT contributing nothing.
#[tokio::test(flavor = "multi_thread")]
async fn framework_extract_emits_routes_and_respects_the_language_gate() {
    const TS: &[Language] = &[Language::Typescript];
    const PY: &[Language] = &[Language::Python];

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.ts"), "router.post('/login', h)\n").unwrap();
    let ctx = store_ctx(dir.path(), &[("app.ts", Language::Typescript)]).await;
    let store = ctx.store();

    let ts_fw: &'static Fake = Box::leak(Box::new(Fake {
        name: "express",
        langs: Some(TS),
        detects: true,
        panics_on_detect: false,
        emits_route: Some(("POST", "/login")),
    }));
    let py_fw: &'static Fake = Box::leak(Box::new(Fake {
        name: "django",
        langs: Some(PY),
        detects: true,
        panics_on_detect: false,
        emits_route: Some(("GET", "/should-not-appear")),
    }));
    let detected: Vec<&'static dyn FrameworkResolver> = vec![ts_fw, py_fw];

    let stats = run_framework_extract_for_files(store, &ctx, &detected, &["app.ts".to_string()])
        .await
        .unwrap();

    assert_eq!(
        stats.nodes, 1,
        "only the TS framework applies to a .ts file"
    );
    assert!(stats.warnings.is_empty());

    let found = find_route(store, Some("express"), Some("POST"), "/login")
        .await
        .unwrap();
    assert_eq!(
        found.len(),
        1,
        "the emitted route is queryable by semantics"
    );
    assert_eq!(found[0].name, "POST /login");
    assert_eq!(
        found[0].language,
        Language::Typescript,
        "the route node carries the emitting file's language (threaded through \
         `route_node` — the enum killed the empty-string sentinel + stamp pass)"
    );

    assert!(
        find_route(store, None, None, "/should-not-appear")
            .await
            .unwrap()
            .is_empty(),
        "the Python framework must not run on a TypeScript file"
    );
}

/// Determinism: the same project extracted twice yields byte-identical node sets,
/// in the same order.
#[tokio::test(flavor = "multi_thread")]
async fn framework_extract_is_deterministic() {
    const TS: &[Language] = &[Language::Typescript];
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.ts"), "x\n").unwrap();

    let fw: &'static Fake = Box::leak(Box::new(Fake {
        name: "express",
        langs: Some(TS),
        detects: true,
        panics_on_detect: false,
        emits_route: Some(("GET", "/a")),
    }));
    let detected: Vec<&'static dyn FrameworkResolver> = vec![fw];

    let mut runs = Vec::new();
    for _ in 0..2 {
        let ctx = store_ctx(dir.path(), &[("app.ts", Language::Typescript)]).await;
        run_framework_extract_for_files(ctx.store(), &ctx, &detected, &["app.ts".to_string()])
            .await
            .unwrap();
        let ids: Vec<String> = ctx
            .store()
            .get_nodes_by_kind(NodeKind::Route)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        runs.push(ids);
    }
    assert_eq!(runs[0], runs[1], "same input ⇒ same route ids");
}

/// A framework that blows up on one file contributes a warning and ZERO nodes —
/// and the index still succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_extract_warns_and_never_fails_the_index() {
    struct Exploder;
    impl FrameworkResolver for Exploder {
        fn name(&self) -> &'static str {
            "exploder"
        }
        fn languages(&self) -> Option<&'static [Language]> {
            None
        }
        fn detect(&self, _: &dyn ResolutionContext) -> bool {
            true
        }
        fn resolve(&self, _: &UnresolvedRef, _: &dyn ResolutionContext) -> Option<ResolvedRef> {
            None
        }
        fn extract(&self, _: &str, _: &str, _: Language) -> FrameworkExtraction {
            panic!("bad regex")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.ts"), "x\n").unwrap();
    let ctx = store_ctx(dir.path(), &[("app.ts", Language::Typescript)]).await;

    let fw: &'static Exploder = Box::leak(Box::new(Exploder));
    let detected: Vec<&'static dyn FrameworkResolver> = vec![fw];

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let stats =
        run_framework_extract_for_files(ctx.store(), &ctx, &detected, &["app.ts".to_string()])
            .await
            .expect("a panicking framework must NOT fail the index");
    std::panic::set_hook(prev);

    assert_eq!(stats.nodes, 0);
    assert_eq!(stats.warnings.len(), 1);
    assert!(stats.warnings[0].contains("exploder"));
}
