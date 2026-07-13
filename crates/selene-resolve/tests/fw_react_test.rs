#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 13 — React Router (v5 / v6 / data-router) + Next.js file routes.
//!
//! # The flow this framework must close
//!
//! ```text
//! <Route path="/article/:slug" element={<Article/>}/>
//!     →  Article  →  useArticle()  →  fetchArticle()
//! ```
//!
//! Route→component alone is NOT the flow: it stops exactly where the agent's
//! question begins ("what does this page actually fetch?"), so the agent opens
//! `Article.tsx` anyway. The chain has to reach the data call.

use selene_core::Language;
use selene_resolve::frameworks::{FrameworkResolver, react::ReactResolver};

mod pipeline;

const REACT: &[&dyn FrameworkResolver] = &[&ReactResolver];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/react")
}

// =============================================================================
// THE FLOWS (end-to-end or not at all)
// =============================================================================

/// v6 `element={<Article/>}` → Article → useArticle → fetchArticle.
#[tokio::test(flavor = "multi_thread")]
async fn flow_v6_route_to_component_to_hook_to_api_is_closed() {
    let p = pipeline::index_and_resolve(&fixture(), REACT).await;
    let route = p.route("react", None, "/article/:slug").await;

    assert_eq!(
        route.name, "/article/:slug",
        "a path-only router's name is the path"
    );
    assert_eq!(route.route_method, None, "no HTTP verb on a React route");

    p.assert_flow(
        &route.id,
        "fetchArticle",
        &["Article", "useArticle"],
        "react v6: <Route element={<Article/>}> → Article → useArticle → fetchArticle",
    )
    .await;
}

/// v5 `component={Article}` reaches the same terminal.
#[tokio::test(flavor = "multi_thread")]
async fn flow_v5_component_prop_is_closed() {
    let p = pipeline::index_and_resolve(&fixture(), REACT).await;
    let route = p.route("react", None, "/v5/article").await;
    p.assert_flow(
        &route.id,
        "fetchArticle",
        &["Article", "useArticle"],
        "react v5: <Route component={Article}> → … → fetchArticle",
    )
    .await;
}

/// The object data-router reaches the same terminal.
#[tokio::test(flavor = "multi_thread")]
async fn flow_data_router_is_closed() {
    let p = pipeline::index_and_resolve(&fixture(), REACT).await;
    let route = p.route("react", None, "/data/article").await;
    p.assert_flow(
        &route.id,
        "fetchArticle",
        &["Article", "useArticle"],
        "react data-router: createBrowserRouter([{path, element}]) → … → fetchArticle",
    )
    .await;
}

/// Next.js `pages/articles/[slug].tsx` → `/articles/:slug` → the default export
/// → useArticle → fetchArticle.
#[tokio::test(flavor = "multi_thread")]
async fn flow_nextjs_pages_route_is_closed() {
    let p = pipeline::index_and_resolve(&fixture(), REACT).await;
    let route = p.route("react", None, "/articles/:slug").await;

    assert_eq!(route.start_line, 1, "a file route is anchored at line 1");
    p.assert_flow(
        &route.id,
        "fetchArticle",
        &["ArticlePage", "useArticle"],
        "next.js pages: [slug].tsx → ArticlePage → useArticle → fetchArticle",
    )
    .await;
}

// =============================================================================
// Extraction contract
// =============================================================================

/// Exactly the routes the fixture declares — and NOT `mypage.tsx` (the TS bug
/// we deliberately do not port), NOT `_app.tsx`, NOT the pathless `<Route>`.
#[tokio::test(flavor = "multi_thread")]
async fn extracts_exactly_the_expected_routes() {
    let p = pipeline::index_and_resolve(&fixture(), REACT).await;
    let mut names = p.route_names().await;
    names.sort();

    assert_eq!(
        names,
        vec![
            "/article/:slug".to_string(),  // v6
            "/articles".to_string(),       // next app/articles/page.tsx
            "/articles/:slug".to_string(), // next pages/articles/[slug].tsx
            "/data/article".to_string(),   // data router
            "/v5/article".to_string(),     // v5
        ],
        "no route from `_app.tsx`, from `mypage.tsx`, or from the pathless <Route>"
    );
}

/// The bug-fix guard, stated on its own because it is a deliberate divergence
/// from the TS build: `app/articles/mypage.tsx` must NOT be a route. TS tested
/// `filePath.includes('page.')`, which matches it.
#[tokio::test(flavor = "multi_thread")]
async fn mypage_tsx_under_app_is_not_a_route() {
    let p = pipeline::index_and_resolve(&fixture(), REACT).await;
    let files: Vec<String> = p.routes().await.into_iter().map(|n| n.file_path).collect();
    assert!(
        !files.iter().any(|f| f.contains("mypage")),
        "TS matched `page.` as a substring and would emit a route here — \
         we match the BASENAME `^page\\.(tsx?|jsx?)$`. Routes: {files:?}"
    );
}

// =============================================================================
// Unit
// =============================================================================

fn extract(file: &str, src: &str) -> Vec<String> {
    ReactResolver
        .extract(file, src, Language::Tsx)
        .nodes
        .into_iter()
        .map(|n| n.name)
        .collect()
}

#[test]
fn a_route_without_a_path_emits_nothing() {
    assert!(extract("src/A.tsx", "<Route element={<Foo/>} />").is_empty());
}

#[test]
fn a_component_beyond_the_400_char_window_does_not_pair() {
    let filler = " ".repeat(420);
    let src = format!("<Route path=\"/x\"{filler}element={{<Foo/>}} />");
    assert!(
        extract("src/A.tsx", &src).is_empty(),
        "past the window the pairing is a guess, and a wrong route is worse than none"
    );
}

#[test]
fn object_paths_only_become_routes_inside_a_data_router() {
    // No `create*Router` in the file: `path:` is just an object key.
    let cfg = "const opts = { path: '/not-a-route', element: <Foo/> };";
    assert!(extract("src/config.tsx", cfg).is_empty());

    let router = "createBrowserRouter([{ path: '/yes', element: <Foo/> }]);";
    assert_eq!(extract("src/router.tsx", router), vec!["/yes".to_string()]);
}

#[test]
fn next_config_and_underscore_files_are_not_routes() {
    assert!(extract("src/pages/_app.tsx", "export default function A(){}").is_empty());
    assert!(extract("next.config.js", "export default function A(){}").is_empty());
}

#[test]
fn an_app_route_requires_the_page_basename() {
    assert_eq!(
        extract("src/app/blog/page.tsx", "export default function Blog(){}"),
        vec!["/blog".to_string()]
    );
    assert!(
        extract(
            "src/app/blog/mypage.tsx",
            "export default function Nope(){}"
        )
        .is_empty(),
        "the TS `includes('page.')` bug — not ported"
    );
}
