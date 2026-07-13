#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 19 — Rails.
//!
//! # The flow
//!
//! ```text
//! resources :articles  →  GET /articles  →  ArticlesController#index  →  Article.recent
//! ```
//!
//! Two things must hold at once, and both are easy to lose:
//!
//! 1. **One line, N routes, N distinct ids.** `resources :articles, only: [:index,
//!    :create]` expands to two routes *on the same line of the same file* — they
//!    collide on everything a node id hashes except `name`.
//! 2. **The reference carries the controller.** `articles#index`, never `index`.
//!    The fixture ships a second controller that also defines `index`, and a
//!    bare-name bind lands on it — silently, at high confidence.

use selene_resolve::frameworks::ruby::{RESTFUL_ROUTES, Rails};
use selene_resolve::frameworks::{FrameworkResolver, all_framework_resolvers};

mod pipeline;

const RAILS: &[&dyn FrameworkResolver] = &[&Rails];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/rails")
}

// =============================================================================
// THE FLOW
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn flow_restful_route_to_its_action_to_the_model() {
    let p = pipeline::index_and_resolve(&fixture(), RAILS).await;

    let index = p.route("rails", Some("GET"), "/articles").await;
    p.assert_flow(
        &index.id,
        "recent",
        &["index"],
        "GET /articles → ArticlesController#index → Article.recent",
    )
    .await;

    let create = p.route("rails", Some("POST"), "/articles").await;
    p.assert_flow(
        &create.id,
        "build_from",
        &["create"],
        "POST /articles → ArticlesController#create → Article.build_from",
    )
    .await;
}

/// `only: [:index, :create]` ⇒ exactly two routes — **on the same line**, with
/// **distinct ids**.
#[tokio::test(flavor = "multi_thread")]
async fn one_line_two_routes_two_distinct_ids() {
    let p = pipeline::index_and_resolve(&fixture(), RAILS).await;
    let routes = p.routes().await;

    assert_eq!(
        routes.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["GET /articles", "POST /articles"],
        "`only:` selects; the other five RESTful actions were not declared"
    );
    assert_eq!(
        routes[0].start_line, routes[1].start_line,
        "both expanded from ONE source line"
    );
    assert_ne!(
        routes[0].id, routes[1].id,
        "…and are separated only by `name`. Name them by path alone and \
         `resources :articles` collapses seven routes into three."
    );
}

/// The precision regression: `CommentsController` also defines `index`.
#[tokio::test(flavor = "multi_thread")]
async fn the_route_binds_to_its_own_controllers_action() {
    let p = pipeline::index_and_resolve(&fixture(), RAILS).await;
    let index = p.route("rails", Some("GET"), "/articles").await;

    let targets = p.targets_of(&index.id).await;
    assert_eq!(targets.len(), 1, "one route, one action");
    assert_eq!(targets[0].name, "index");
    assert_eq!(
        targets[0].file_path, "app/controllers/articles_controller.rb",
        "CommentsController declares `index` too. `articles#index` is a PRECISE \
         claim — it names one target — and a resolver that degrades it to a \
         bare-name match produces a false edge the agent will trust."
    );
}

// =============================================================================
// Units
// =============================================================================

/// All seven, in order — the table is a contract.
#[test]
fn the_full_restful_expansion_is_seven_routes_with_seven_distinct_ids() {
    use selene_core::Language;

    let out = Rails.extract("config/routes.rb", "resources :articles\n", Language::Ruby);
    assert_eq!(out.nodes.len(), RESTFUL_ROUTES.len());
    assert_eq!(out.nodes.len(), 7);

    let ids: std::collections::HashSet<&str> = out.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids.len(),
        7,
        "seven routes off ONE line ⇒ seven distinct ids, or routes silently vanish"
    );

    let mut names: Vec<&str> = out.nodes.iter().map(|n| n.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "DELETE /articles/:id",
            "GET /articles",
            "GET /articles/:id",
            "GET /articles/:id/edit",
            "GET /articles/new",
            "PATCH /articles/:id",
            "POST /articles",
        ],
        "index/create share a path and differ by VERB; show/edit share a verb and \
         differ by PATH — the name must carry both"
    );

    assert!(
        out.refs
            .iter()
            .all(|r| r.reference_name.starts_with("articles#")),
        "every action reference carries its controller"
    );
}

#[test]
fn explicit_routes_read_both_the_to_and_the_fat_arrow_form() {
    use selene_core::Language;

    let src = "get '/health', to: 'system#health'\nget '/legacy' => 'system#legacy'\n";
    let out = Rails.extract("config/routes.rb", src, Language::Ruby);
    let refs: Vec<&str> = out.refs.iter().map(|r| r.reference_name.as_str()).collect();
    assert_eq!(refs, vec!["system#health", "system#legacy"]);
}

#[test]
fn rails_is_registered_after_laravel() {
    let names: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    let rails = names.iter().position(|n| *n == "rails");
    assert!(rails.is_some());
    assert!(
        names.iter().position(|n| *n == "laravel") < rails,
        "REGISTRY_ORDER declares laravel before rails"
    );
}
