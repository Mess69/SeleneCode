#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 14 — Django (urlconf + DRF router).
//!
//! # The flow this framework must close
//!
//! ```text
//! path('articles/<slug>/', ArticleDetail.as_view())
//!     →  ArticleDetail  →  .get()  →  get_article()
//! ```
//!
//! Route→view alone is not the flow: the agent's question is what the view
//! *does*, which lives in the view's body. Both view shapes are asserted —
//! the class-based view (which dispatches into a method, so the chain runs
//! through containment) and the function-based view (which does not, and is
//! therefore asserted with the STRICT edge-kind set).
//!
//! Django's *other* flow — QuerySet → SQL compiler via `_iterable_class` — is a
//! separate chain and belongs to Task 26.

use selene_core::Language;
use selene_resolve::frameworks::{FrameworkResolver, python::DjangoResolver};

mod pipeline;

const DJANGO: &[&dyn FrameworkResolver] = &[&DjangoResolver];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/django")
}

// =============================================================================
// THE FLOWS
// =============================================================================

/// Class-based view: `path('articles/<slug>/', ArticleDetail.as_view())` →
/// `ArticleDetail` → its `get` method → `get_article`.
///
/// The class→method hop is containment — that IS how a CBV dispatches, and it is
/// the move an agent makes reading the flow. See `CBV_FLOW_KINDS`.
#[tokio::test(flavor = "multi_thread")]
async fn flow_urlconf_to_class_based_view_body_is_closed() {
    let p = pipeline::index_and_resolve(&fixture(), DJANGO).await;
    let route = p.route("django", None, "articles/<slug>/").await;

    assert_eq!(
        route.name, "articles/<slug>/",
        "the route name is the RAW url string"
    );
    assert_eq!(
        route.route_method, None,
        "django path() is a path-only router"
    );

    p.assert_flow_kinds(
        &route.id,
        "get_article",
        &["ArticleDetail"],
        pipeline::CBV_FLOW_KINDS,
        "django: path(...) → ArticleDetail → .get() → get_article",
    )
    .await;
}

/// Function-based view: `path('legacy/', views.article_detail)` →
/// `article_detail` → `get_article`. No class, so this one is asserted with the
/// **strict** edge kinds — no containment hop is needed or allowed.
#[tokio::test(flavor = "multi_thread")]
async fn flow_urlconf_to_function_based_view_body_is_closed_strictly() {
    let p = pipeline::index_and_resolve(&fixture(), DJANGO).await;
    let route = p.route("django", None, "legacy/").await;

    p.assert_flow(
        &route.id,
        "get_article",
        &["article_detail"],
        "django: path('legacy/', views.article_detail) → article_detail → get_article",
    )
    .await;
}

/// DRF: `router.register(r'articles', ArticleViewSet)` → the ViewSet → its body.
#[tokio::test(flavor = "multi_thread")]
async fn flow_drf_router_to_viewset_body_is_closed() {
    let p = pipeline::index_and_resolve(&fixture(), DJANGO).await;
    let route = p.route("django", Some("VIEWSET"), "/articles").await;

    assert_eq!(route.name, "VIEWSET /articles");

    p.assert_flow_kinds(
        &route.id,
        "get_article",
        &["ArticleViewSet"],
        pipeline::CBV_FLOW_KINDS,
        "django DRF: router.register('articles', ArticleViewSet) → ViewSet → get_article",
    )
    .await;
}

// =============================================================================
// Extraction contract
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn extracts_exactly_the_live_routes() {
    let p = pipeline::index_and_resolve(&fixture(), DJANGO).await;
    let mut names = p.route_names().await;
    names.sort();
    // Sorted ASCII: '^' (0x5E) precedes 'a' (0x61).
    assert_eq!(
        names,
        vec![
            "VIEWSET /articles".to_string(),
            r"^old/(?P<pk>\d+)/$".to_string(),
            "api/".to_string(),
            "articles/<slug>/".to_string(),
            "legacy/".to_string(),
        ],
        "the commented-out `path('dead/', …)` must NOT appear"
    );
}

/// `include('api.urls')` names NO declared symbol. Without `claims_reference`
/// the resolver's pre-filter drops it before `resolve()` runs, and the bridge is
/// silently inert. Assert the ref survives to be a real reference.
#[tokio::test(flavor = "multi_thread")]
async fn include_emits_an_imports_ref_that_the_pre_filter_does_not_eat() {
    let p = pipeline::index_and_resolve(&fixture(), DJANGO).await;
    let route = p.route("django", None, "api/").await;

    let refs = p
        .store()
        .unresolved_by_files(&["blog/urls.py".to_string()])
        .await
        .unwrap();
    let mine: Vec<_> = refs.iter().filter(|r| r.from_node_id == route.id).collect();

    assert_eq!(mine.len(), 1, "include() emits exactly one ref");
    assert_eq!(mine[0].reference_name, "api.urls");
    assert_eq!(
        mine[0].reference_kind, "imports",
        "an include is an imports ref, not a view reference"
    );
    assert!(
        DjangoResolver.claims_reference("api.urls"),
        "…and the framework CLAIMS it, or the pre-filter would drop it before \
         resolve() ever ran — the hook exists for exactly this"
    );
}

// =============================================================================
// Unit
// =============================================================================

fn extract(src: &str) -> Vec<(String, Vec<(String, String)>)> {
    let out = DjangoResolver.extract("blog/urls.py", src, Language::Python);
    out.nodes
        .iter()
        .map(|n| {
            let refs = out
                .refs
                .iter()
                .filter(|r| r.from_node_id == n.id)
                .map(|r| (r.reference_name.clone(), r.reference_kind.clone()))
                .collect();
            (n.name.clone(), refs)
        })
        .collect()
}

#[test]
fn as_view_is_stripped_and_dotted_handlers_take_the_last_segment() {
    let r = extract("path('a/', ArticleDetail.as_view()),\npath('b/', views.detail),\n");
    assert_eq!(r[0].1[0].0, "ArticleDetail");
    assert_eq!(r[1].1[0].0, "detail");
}

#[test]
fn re_path_and_url_forms_are_routes() {
    let r = extract("re_path(r'^x$', v),\nurl(r'^y$', v),\n");
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].0, "^x$");
}

#[test]
fn admin_register_is_not_a_viewset_route() {
    // `admin.register(Article, ArticleAdmin)` has a CLASS first arg — the string
    // first arg is what makes `router.register` a route. Nothing here matches,
    // and even a string-keyed register whose 2nd arg is not a View is skipped.
    assert!(extract("admin.register(Article, ArticleAdmin)\n").is_empty());
    assert!(
        extract("thing.register('key', SomeHelper)\n").is_empty(),
        "the 2nd arg must be a *View/*ViewSet"
    );
    let r = extract("router.register(r'articles', ArticleViewSet)\n");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, "VIEWSET /articles");
}

#[test]
fn a_non_urls_python_file_emits_no_routes() {
    let out = DjangoResolver.extract(
        "blog/models.py",
        "class Article:\n    pass\n",
        Language::Python,
    );
    assert!(out.nodes.is_empty());
}
