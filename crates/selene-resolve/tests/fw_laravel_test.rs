#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 19 — Laravel.
//!
//! # The flow, and the precision regression it guards
//!
//! ```text
//! GET /articles  →  ArticleController::index  →  $this->articleService->listArticles()
//! ```
//!
//! The fixture holds a **second controller that also defines `index()`**. That is
//! not decoration: emit the handler reference as the bare name `index` and the
//! matcher binds to whichever one it reaches first — in the TS build *every*
//! Laravel route resolved to a single controller's `index`, and the route map
//! looked complete while being systematically wrong.
//!
//! So the flow test asserts the resolved target's **file**, not just that a path
//! exists. A wrong bind at 0.9 is a false edge the agent will trust, which is
//! strictly worse than no edge at all.

use selene_core::{Language, RefStatus, UnresolvedRef};
use selene_resolve::frameworks::laravel::{FACADE_MAPPINGS, Laravel};
use selene_resolve::frameworks::{FrameworkResolver, all_framework_resolvers};

mod common;
mod pipeline;

use common::FakeContext;

const LARAVEL: &[&dyn FrameworkResolver] = &[&Laravel];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/laravel")
}

// =============================================================================
// THE FLOW
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn flow_route_to_the_right_controller_action_to_the_service() {
    let p = pipeline::index_and_resolve(&fixture(), LARAVEL).await;

    let get = p.route("laravel", Some("GET"), "/articles").await;
    p.assert_flow(
        &get.id,
        "listArticles",
        &["index"],
        "GET /articles → ArticleController::index → ArticleService::listArticles",
    )
    .await;

    // The legacy `'ArticleController@store'` string form reaches the same place.
    let post = p.route("laravel", Some("POST"), "/articles").await;
    p.assert_flow(
        &post.id,
        "createArticle",
        &["store"],
        "POST /articles → ArticleController::store → ArticleService::createArticle",
    )
    .await;
}

/// **The precision regression.** Two controllers define `index()`; the route must
/// land on *`ArticleController`'s*.
#[tokio::test(flavor = "multi_thread")]
async fn the_route_binds_to_its_own_controllers_action_not_another_ones() {
    let p = pipeline::index_and_resolve(&fixture(), LARAVEL).await;
    let get = p.route("laravel", Some("GET"), "/articles").await;

    let targets = p.targets_of(&get.id).await;
    assert_eq!(targets.len(), 1, "one route, one handler");
    let target = &targets[0];

    assert_eq!(target.name, "index");
    assert_eq!(
        target.file_path, "app/Http/Controllers/ArticleController.php",
        "CommentController also declares index(). A bare `index` reference binds to \
         whichever the matcher finds first — that is how every route in a repo ends \
         up pointing at one controller, and why the reference carries the class."
    );
}

// =============================================================================
// Units
// =============================================================================

/// Without `claims_reference`, `ArticleController@index` — which names no declared
/// symbol anywhere — is dropped by the ladder's pre-filter and this framework is
/// never asked. The TS build shipped that bug.
#[tokio::test(flavor = "multi_thread")]
async fn claims_reference_is_actually_consulted() {
    assert!(
        Laravel.claims_reference("ArticleController@index"),
        "the claim itself"
    );

    // And it is wired: the pipeline resolved the route refs, whose names exist
    // nowhere in the source as symbols.
    let p = pipeline::index_and_resolve(&fixture(), LARAVEL).await;
    let get = p.route("laravel", Some("GET"), "/articles").await;
    assert!(
        !p.targets_of(&get.id).await.is_empty(),
        "the route bound to nothing — `claims_reference` is not wired into \
         resolve_one's pre-filter, and every Laravel route is inert"
    );
}

/// A facade is the framework, not this project. It must be **refused**, not
/// name-matched onto a same-named local class.
#[test]
fn a_facade_reference_resolves_to_nothing() {
    fn facade_ref(name: &str) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: "x".into(),
            reference_name: name.into(),
            reference_kind: "calls".into(),
            line: Some(1),
            column: Some(0),
            candidates: vec![],
            file_path: "routes/api.php".into(),
            language: Language::Php.as_str().into(),
            status: RefStatus::Pending,
            name_tail: name.into(),
        }
    }

    let ctx = FakeContext::new();
    for (facade, _) in FACADE_MAPPINGS {
        assert!(
            Laravel
                .resolve(&facade_ref(&format!("{facade}::query")), &ctx)
                .is_none(),
            "{facade} is Laravel's, not the project's — falling through would bind \
             it to any local class of the same name"
        );
    }
}

#[test]
fn laravel_is_registered_after_rust() {
    let names: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    let laravel = names.iter().position(|n| *n == "laravel");
    assert!(laravel.is_some());
    assert!(
        names.iter().position(|n| *n == "rust") < laravel,
        "REGISTRY_ORDER declares rust before laravel"
    );
}
