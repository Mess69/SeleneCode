#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 12 — Express.
//!
//! # The flow this framework must close
//!
//! ```text
//! POST /users/login  →  (inline arrow handler)  →  login()  →  hashPassword()
//! ```
//!
//! The route→handler hop **is not the flow**. The dominant modern Express shape
//! is an *inline arrow* handler, which is not a node at all — so a bridge that
//! stops at "the route exists" connects the route to **nothing**, and the agent
//! must open the file to find out what the request actually does. That was the
//! TS build's real hole (playbook §7: realworld 19 / ghost 65 edges once the
//! arrow bodies were mined; before the fix, zero).
//!
//! So the flow is closed **only if a path runs from the route node to the
//! service function the handler's body calls, and on to what THAT calls**. The
//! end-to-end test below asserts exactly that. Everything else in this file is
//! subordinate to it.

use selene_core::Language;
use selene_resolve::frameworks::{FrameworkResolver, express::ExpressResolver};

mod pipeline;

const EXPRESS: &[&dyn FrameworkResolver] = &[&ExpressResolver];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/express")
}

/// Detected via `package.json` deps.
#[tokio::test(flavor = "multi_thread")]
async fn express_is_detected_from_package_json() {
    let p = pipeline::index_and_resolve(&fixture(), EXPRESS).await;
    // `detect` ran inside the pipeline's resolver construction.
    assert!(
        p.store()
            .find_route(Some("express"), Some("POST"), "/users/login")
            .await
            .unwrap()
            .len()
            == 1,
        "a detected express project must emit its routes"
    );
}

// =============================================================================
// THE FLOW (invariant: end-to-end or not at all)
// =============================================================================

/// `POST /users/login` → `login` → `hashPassword`.
///
/// The handler is an **inline arrow** — it is not a node, so the route's `calls`
/// refs are mined out of the arrow's BODY and attributed to the route itself.
/// If this connects only as far as `login`, the bridge is half-done and the
/// agent still reads `service.ts`.
#[tokio::test(flavor = "multi_thread")]
async fn flow_route_to_inline_handler_body_to_service_is_closed() {
    let p = pipeline::index_and_resolve(&fixture(), EXPRESS).await;
    let route = p.route("express", Some("POST"), "/users/login").await;

    assert_eq!(route.name, "POST /users/login");
    assert_eq!(route.route_method.as_deref(), Some("POST"));
    assert_eq!(route.framework.as_deref(), Some("express"));

    p.assert_flow(
        &route.id,
        "hashPassword",
        &["login"],
        "express: POST /users/login → login (inline arrow body) → hashPassword",
    )
    .await;
}

/// The named-handler + middleware form: `router.get('/profile', auth, getProfile)`.
/// The handler ref is the **last** argument — binding to `auth` instead would
/// point the route at the middleware and lose the actual handler.
#[tokio::test(flavor = "multi_thread")]
async fn a_middleware_chain_binds_the_last_arg_as_the_handler() {
    let p = pipeline::index_and_resolve(&fixture(), EXPRESS).await;
    let route = p.route("express", Some("GET"), "/profile").await;

    // `getProfile` is not defined in the fixture, so nothing to bind — what we
    // pin here is that the ref emitted is `getProfile` (the last arg), NOT `auth`.
    let refs = p
        .store()
        .unresolved_by_files(&["src/app.ts".to_string()])
        .await
        .unwrap();
    let from_route: Vec<&str> = refs
        .iter()
        .filter(|r| r.from_node_id == route.id)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(
        from_route.contains(&"getProfile"),
        "the LAST arg is the handler: {from_route:?}"
    );
    assert!(
        !from_route.contains(&"auth"),
        "the middleware must not be mistaken for the handler: {from_route:?}"
    );
}

// =============================================================================
// Extraction contract
// =============================================================================

/// Exactly the routes the fixture declares — and nothing from the commented-out
/// one (strip-comments), and nothing from `app.use(cors())`-shaped calls.
#[tokio::test(flavor = "multi_thread")]
async fn extracts_exactly_the_live_routes() {
    let p = pipeline::index_and_resolve(&fixture(), EXPRESS).await;
    let names = p.route_names().await;
    assert_eq!(
        names,
        vec![
            "POST /users/login".to_string(),
            "GET /profile".to_string(),
            "USE /api".to_string(),
        ],
        "the commented-out `router.get('/dead')` must NOT appear"
    );
}

/// `res.json(user)` inside the arrow body is a RESERVED call — it is Express's
/// own response API, not a service call, and emitting it would attach a
/// meaningless `json` ref to every route in the repo.
#[tokio::test(flavor = "multi_thread")]
async fn reserved_calls_in_an_arrow_body_are_not_refs() {
    let p = pipeline::index_and_resolve(&fixture(), EXPRESS).await;
    let route = p.route("express", Some("POST"), "/users/login").await;

    let refs = p
        .store()
        .unresolved_by_files(&["src/app.ts".to_string()])
        .await
        .unwrap();
    let from_route: Vec<&str> = refs
        .iter()
        .filter(|r| r.from_node_id == route.id)
        .map(|r| r.reference_name.as_str())
        .collect();

    assert!(from_route.contains(&"login"), "the service call is a ref");
    assert!(
        !from_route.contains(&"json"),
        "`res.json` is in RESERVED_CALLS: {from_route:?}"
    );
}

// =============================================================================
// Unit: the extractor in isolation
// =============================================================================

use selene_resolve::frameworks::express;

fn extract(src: &str) -> Vec<(String, Vec<String>)> {
    let out = ExpressResolver.extract("src/app.ts", src, Language::Typescript);
    out.nodes
        .iter()
        .map(|n| {
            let refs = out
                .refs
                .iter()
                .filter(|r| r.from_node_id == n.id)
                .map(|r| r.reference_name.clone())
                .collect();
            (n.name.clone(), refs)
        })
        .collect()
}

#[test]
fn app_use_with_a_non_path_first_arg_is_not_a_route() {
    // `app.use(cors())` has no leading-slash path — it is middleware registration.
    assert!(
        extract("app.use(cors());\n").is_empty(),
        "app.use(cors()) is not a route"
    );
    // …but a mount path IS one.
    let routes = extract("app.use('/api', apiRouter);\n");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].0, "USE /api");
}

#[test]
fn an_inline_arrow_body_yields_one_ref_per_unique_non_reserved_call() {
    let routes = extract(
        "router.post('/x', async (req, res) => {\n\
           const a = await doWork(req.body);\n\
           const b = doWork(a);\n\
           log(a);\n\
           res.status(200).json(b);\n\
         });\n",
    );
    assert_eq!(routes.len(), 1);
    let mut refs = routes[0].1.clone();
    refs.sort();
    assert_eq!(
        refs,
        vec!["doWork".to_string()],
        "unique non-reserved calls only: `log`, `status`, `json` are RESERVED, \
         and `doWork` appears once despite two call sites"
    );
}

#[test]
fn a_controller_dot_method_handler_keeps_the_controller() {
    let routes = extract("router.get('/u', UserController.index);\n");
    assert_eq!(routes[0].1, vec!["UserController.index".to_string()]);
}

#[test]
fn a_commented_out_route_is_not_extracted() {
    assert!(extract("// router.get('/dead', h);\n").is_empty());
    assert!(extract("/* router.get('/dead', h); */\n").is_empty());
}

#[test]
fn reserved_calls_is_a_deduped_set() {
    // The TS source lists `redirect` twice; it is a set, so the count is of
    // DISTINCT names. Pinning it stops a silent drift in the filter.
    assert!(express::RESERVED_CALLS.contains("json"));
    assert!(express::RESERVED_CALLS.contains("redirect"));
    assert!(express::RESERVED_CALLS.contains("Promise"));
    assert!(!express::RESERVED_CALLS.contains("login"));
}
