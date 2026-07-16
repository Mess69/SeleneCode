#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 18 — Axum / Actix.
//!
//! # The flow this framework must close
//!
//! ```text
//! GET  /articles  →  list_articles()   →  service::list()
//! POST /articles  →  create_article()  →  service::create()
//! ```
//!
//! **Both of them, from one source line.** `.route("/articles",
//! get(list).post(create))` registers two routes, and the TS build emitted only
//! the first — so `POST /articles` did not exist on the map at all
//! (realworld-axum: 12 routes where 19 existed). An agent asking "where is an
//! article created?" got nothing back and opened the file.
//!
//! A test that asserted only the GET flow would pass on that broken behavior.
//! This is *the* canonical case for the invariant, so both flows are asserted, and
//! so is the id distinctness that makes two same-line routes possible at all.

use selene_core::Language;
use selene_resolve::frameworks::rust_fw::RustResolver;
use selene_resolve::frameworks::{FrameworkResolver, all_framework_resolvers};

mod pipeline;

const RUST: &[&dyn FrameworkResolver] = &[&RustResolver];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/axum")
}

// =============================================================================
// THE FLOW — end-to-end or not at all
// =============================================================================

/// Both chained verbs, both handlers, both service calls. Every hop.
#[tokio::test(flavor = "multi_thread")]
async fn flow_both_chained_verbs_reach_their_own_service_fn() {
    let p = pipeline::index_and_resolve(&fixture(), RUST).await;

    let get = p.route("rust", Some("GET"), "/articles").await;
    let post = p.route("rust", Some("POST"), "/articles").await;

    assert_ne!(
        get.id, post.id,
        "two routes off ONE source line — same file, same line, so only `name` \
         separates their hashed ids. Collapse the name and the two routes become \
         one, which is how a POST silently disappears."
    );
    assert_eq!(get.start_line, post.start_line, "…the same line, indeed");

    p.assert_flow(
        &get.id,
        "list",
        &["list_articles"],
        "GET /articles → list_articles() → service::list()",
    )
    .await;

    // The one the TS build lost. If the chained-verb arm regresses, `p.route`
    // above already panics — but this asserts the whole chain, not the node.
    p.assert_flow(
        &post.id,
        "create",
        &["create_article"],
        "POST /articles → create_article() → service::create() — the SECOND verb, \
         which the TS build never emitted",
    )
    .await;
}

/// Exactly two routes, and nothing invented.
#[tokio::test(flavor = "multi_thread")]
async fn the_fixture_has_exactly_the_two_routes() {
    let p = pipeline::index_and_resolve(&fixture(), RUST).await;
    let names = p.route_names().await;
    assert_eq!(names, vec!["GET /articles", "POST /articles"]);
}

/// A workspace crate reference outranks a local coincidence.
///
/// `blog_core` is a sibling crate in `crates/blog-core/`, a directory the
/// reference never names. It resolves through the Cargo crate map to that crate's
/// `lib.rs` — the file node — at **0.95**, which is deliberately above the name
/// matcher's 0.7, because `use blog_core::…` means *that crate* and must not lose
/// to a same-named local symbol.
#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_crate_reference_resolves_to_its_lib_rs() {
    use selene_core::{RefStatus, UnresolvedRef, file_node_id};
    use selene_resolve::{ReferenceResolver, ResolvedBy, StoreContext};

    let dir = fixture();
    let store = selene_db::SurrealStore::in_memory().await.unwrap();
    store.apply_schema().await.unwrap();
    let indexer = selene_extract::Indexer::new(dir.clone(), store);
    indexer.index_all(None).await;
    let ctx = StoreContext::new(indexer.into_store(), dir).await.unwrap();

    let r = UnresolvedRef {
        from_node_id: "fn:app".into(),
        reference_name: "blog_core".into(), // the UNDERSCORE spelling — as `use` writes it
        reference_kind: "imports".into(),
        line: Some(1),
        column: Some(0),
        candidates: vec![],
        file_path: "src/main.rs".into(),
        language: Language::Rust,
        status: RefStatus::Pending,
        name_tail: "blog_core".into(),
    };

    let hit = tokio::task::block_in_place(|| {
        let resolver = ReferenceResolver::with_frameworks(ctx, vec![&RustResolver]);
        RustResolver.resolve(&r, resolver.ctx())
    })
    .expect("the crate map must find `blog-core` under BOTH spellings");

    assert_eq!(
        hit.target_node_id,
        file_node_id("crates/blog-core/src/lib.rs"),
        "the manifest says `blog-core`, every `use` says `blog_core` — the map \
         carries both, or every workspace import silently misses"
    );
    assert_eq!(
        hit.confidence, 0.95,
        "above the name matcher's 0.7 self-file score, on purpose"
    );
    assert_eq!(hit.resolved_by, ResolvedBy::Framework);
}

// =============================================================================
// Units
// =============================================================================

#[test]
fn actix_builder_chain_reads_its_verbs_and_bare_to_is_any() {
    let src = r#"
fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource("/articles")
            .route(web::get().to(list_articles))
            .route(web::post().to(create_article)),
    );
    cfg.service(web::resource("/legacy").to(legacy_handler));
}
"#;
    let out = RustResolver.extract("routes.rs", src, Language::Rust);
    // Pair a route with its handler by **id**, never by position: nodes and refs
    // are each sorted for determinism, and on a shared line the two orders differ.
    // (Zipping them mis-paired `GET` with `create_article` — a test that would have
    // "proved" a wrong edge.)
    let rows: Vec<(&str, &str)> = out
        .nodes
        .iter()
        .map(|n| {
            let handler = out
                .refs
                .iter()
                .find(|r| r.from_node_id == n.id)
                .map(|r| r.reference_name.as_str())
                .unwrap_or("<none>");
            (n.name.as_str(), handler)
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("GET /articles", "list_articles"),
            ("POST /articles", "create_article"),
            ("ANY /legacy", "legacy_handler"),
        ],
        "a bare `.to(h)` carries no verb ⇒ ANY; and the SECOND resource must not \
         swallow the first one's chain (the window is bounded at the next \
         `web::resource`)"
    );
}

#[test]
fn a_closure_handler_gets_a_route_and_no_reference() {
    let src = r#".route("/ping", get(|| async { "pong" }))"#;
    let out = RustResolver.extract("main.rs", src, Language::Rust);
    assert_eq!(out.nodes.len(), 1, "the route is real");
    assert!(
        out.refs.is_empty(),
        "a closure has no name to bind to. Inventing one (`async`, `pong`) would be \
         a wrong edge dressed as an answer — silent beats wrong."
    );
}

#[test]
fn rust_is_registered_after_go() {
    let names: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    let rust = names.iter().position(|n| *n == "rust");
    assert!(rust.is_some(), "rust is registered");
    assert!(
        names.iter().position(|n| *n == "go") < rust,
        "registry order IS resolve precedence; REGISTRY_ORDER declares go before rust"
    );
}
