#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 17 — Gin / gorilla-mux / chi.
//!
//! # The invariant these tests exist to enforce
//!
//! **Dispatch coverage is end-to-end or not at all** (PRD §8.2):
//!
//! ```text
//! route(POST /articles) → handlers.CreateArticle → service.Create()
//!      hop 1 (framework)          hop 2 (Part A's name matcher)
//! ```
//!
//! Two ways this flow half-bridges, and a test for each:
//!
//! - the route is registered on a **group variable** (`v1.POST`), and a fixed
//!   receiver list finds no routes at all;
//! - the handler sits **behind middleware** (`r.GET("/x", auth.Required(), h)`),
//!   and a first-`)` capture binds the route to `Required` — an edge that *looks*
//!   like an answer and sends the agent one hop short of the handler.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::frameworks::go::Go;
use selene_resolve::frameworks::{FrameworkResolver, all_framework_resolvers};
use selene_resolve::{GoModule, ImportMapping, ReferenceResolver};

const ROUTER: &str = "router.go";
const HANDLERS: &str = "internal/handlers/article.go";
const SERVICE: &str = "internal/service/article.go";

const ROUTER_GO: &str = r#"package router

func Setup(r *gin.Engine) {
	v1 := r.Group("/api/v1")
	v1.POST("/articles", handlers.CreateArticle)
	v1.GET("/articles/:id", middleware.Auth(), handlers.GetArticle)
	// v1.DELETE("/articles/:id", handlers.DeleteArticle)
	r.GET("/ping", func(c *gin.Context) {
		c.JSON(200, gin.H{"message": "pong"})
	})
}
"#;

fn go_fn(id: &str, name: &str, file: &str) -> Node {
    node(id, NodeKind::Function, name, name, file, Language::Go)
}

/// An **exported** Go func — the capital letter is the export, and
/// `resolve_go_cross_package` (#388) requires it: an unexported `create` is
/// unreachable from another package, and binding to it would be a wrong edge.
fn exported_go_fn(id: &str, name: &str, file: &str) -> Node {
    Node {
        is_exported: Some(true),
        ..go_fn(id, name, file)
    }
}

/// `import "example.com/blog/internal/service"` — a Go import binds the whole
/// package under its last path segment.
fn go_import(pkg: &str, source: &str) -> ImportMapping {
    ImportMapping {
        local_name: pkg.into(),
        exported_name: pkg.into(),
        source: source.into(),
        is_default: false,
        is_namespace: true,
        resolved_path: None,
    }
}

fn go_call(from: &str, callee: &str, file: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.into(),
        reference_name: callee.into(),
        reference_kind: "calls".into(),
        line: Some(7),
        column: Some(1),
        candidates: vec![],
        file_path: file.into(),
        language: Language::Go.as_str().into(),
        status: RefStatus::Pending,
        name_tail: callee.rsplit('.').next().unwrap_or(callee).into(),
    }
}

// =============================================================================
// The flow
// =============================================================================

/// **Flow closed ⇔** `POST /articles` → `handlers.CreateArticle` → `service.Create()`.
#[test]
fn go_flow_group_route_to_handler_to_service() {
    let out = Go.extract(ROUTER, ROUTER_GO, Language::Go);

    // --- hop 1: the routes ----------------------------------------------------
    let names: Vec<&str> = out.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["POST /articles", "GET /articles/:id", "GET /ping"],
        "the receiver is ANY identifier — `v1` is a GROUP variable, and routes \
         registered on groups are the norm. A fixed receiver list (`r`/`router`) \
         finds none of these; the TS build missed every group-routed app until \
         this was generalized (gin-vue-admin: 4 routes → 259)."
    );
    assert!(
        !names.iter().any(|n| n.contains("DELETE")),
        "a COMMENTED-OUT route must not become a node"
    );

    let create = &out.nodes[0];
    assert_eq!(create.route_method.as_deref(), Some("POST"));
    assert_eq!(
        create.route_path.as_deref(),
        Some("/articles"),
        "the group prefix is NOT prepended — TS parity, carried deliberately (the \
         prefix lives at the group's declaration, and resolving it needs dataflow \
         this pass does not have). The route's file+line still land the agent on \
         the exact registration site."
    );
    assert_eq!(create.framework.as_deref(), Some("go"));
    assert_eq!(create.start_line, 5);

    // The anonymous `/ping` handler is a route with NO reference: a function
    // literal has no name, and its tail identifier would be `Context`.
    let handlers: Vec<&str> = out.refs.iter().map(|r| r.reference_name.as_str()).collect();
    assert_eq!(
        handlers,
        vec!["CreateArticle", "GetArticle"],
        "`GetArticle` sits BEHIND middleware — binding to `Auth` instead would \
         leave the flow one hop short of the handler, which is worse than no route"
    );

    // --- hop 1 resolves: route → handler --------------------------------------
    // A real Go project: a module path, and a handler file that IMPORTS the
    // service package. `service.Create` is a package-qualified call, not a method
    // on a receiver — it resolves through the import (#388), and it cannot resolve
    // any other way.
    let ctx = FakeContext::new()
        .with_file("go.mod", "module example.com/blog\n")
        .with_file(ROUTER, ROUTER_GO)
        .with_go_module(GoModule {
            module_path: "example.com/blog".into(),
            root_dir: String::new(),
        })
        .with_import_mapping(
            HANDLERS,
            go_import("service", "example.com/blog/internal/service"),
        )
        .with_node(exported_go_fn(
            "fn:CreateArticle",
            "CreateArticle",
            HANDLERS,
        ))
        .with_node(exported_go_fn("fn:GetArticle", "GetArticle", HANDLERS))
        .with_node(exported_go_fn("fn:Create", "Create", SERVICE))
        .with_node(create.clone())
        .with_node(out.nodes[1].clone());

    let mut resolver = ReferenceResolver::new(ctx);
    let hop1 = resolver
        .resolve_one(&out.refs[0])
        .expect("hop 1: the route binds to its handler func");
    assert_eq!(hop1.target_node_id, "fn:CreateArticle");

    // …and the middleware-shadowed route reaches its real handler too.
    let hop1b = resolver
        .resolve_one(&out.refs[1])
        .expect("hop 1: the route behind middleware binds to the HANDLER");
    assert_eq!(hop1b.target_node_id, "fn:GetArticle");

    // --- hop 2 resolves: handler body → the service ---------------------------
    let hop2 = resolver
        .resolve_one(&go_call("fn:CreateArticle", "service.Create", HANDLERS))
        .expect("hop 2: the handler's body call binds to the service func");
    assert_eq!(hop2.target_node_id, "fn:Create");
    assert_eq!(
        hop2.confidence, 0.9,
        "0.9 = resolved through the IMPORT — the receiver `service` names a package \
         DIRECTORY, and the immediate-parent-dir check is what stops it landing on \
         a same-named func in a sibling package. A weaker score here would mean a \
         name fallback covered for it."
    );

    // THE FLOW IS CLOSED: route → handler → service, every hop asserted.
}

// =============================================================================
// Units
// =============================================================================

#[test]
fn any_receiver_and_handle_func_and_a_namespaced_handler() {
    let src = r#"package main

func main() {
	PublicGroup.GET("/health", api.Health)
	s.HandleFunc("/legacy", legacyHandler).Methods("GET")
	mux.Handle("/static", fileServer)
}
"#;
    let out = Go.extract("main.go", src, Language::Go);
    let rows: Vec<(&str, &str, &str)> = out
        .nodes
        .iter()
        .zip(out.refs.iter())
        .map(|(n, r)| {
            (
                n.route_method.as_deref().unwrap_or(""),
                n.route_path.as_deref().unwrap_or(""),
                r.reference_name.as_str(),
            )
        })
        .collect();

    assert_eq!(
        rows,
        vec![
            ("GET", "/health", "Health"),
            ("ANY", "/legacy", "legacyHandler"),
            ("ANY", "/static", "fileServer"),
        ],
        "`PublicGroup` is a receiver like any other; `Handle`/`HandleFunc` carry no \
         verb ⇒ ANY (mux's trailing `.Methods(\"GET\")` is a label we do not read); \
         and the ref is the TAIL identifier of the handler expression"
    );
}

#[test]
fn a_route_inside_a_block_comment_or_a_string_is_not_a_route() {
    let src = r#"package main

/*
	r.GET("/dead", handlers.Dead)
*/
func main() {
	r.GET("/live", handlers.Live)
}
"#;
    let out = Go.extract("main.go", src, Language::Go);
    assert_eq!(out.nodes.len(), 1);
    assert_eq!(out.nodes[0].name, "GET /live");
    assert_eq!(
        out.nodes[0].start_line, 7,
        "the stripper preserves byte offsets, so the line an id is hashed from \
         does not shift when a comment above it is blanked"
    );
}

#[test]
fn go_conventions_prefer_the_conventional_directory_by_infix() {
    // `/handlers/` must match as an INFIX, not a prefix — the real path is
    // `internal/handlers/article.go`, which no prefix rule would catch.
    let decoy = "internal/legacy/article.go";
    let ctx = FakeContext::new()
        .with_node(go_fn("fn:decoy", "ArticleHandler", decoy))
        .with_node(go_fn("fn:real", "ArticleHandler", HANDLERS));

    let r = UnresolvedRef {
        from_node_id: "route:x".into(),
        reference_name: "ArticleHandler".into(),
        reference_kind: "references".into(),
        line: Some(1),
        column: Some(0),
        candidates: vec![],
        file_path: ROUTER.into(),
        language: Language::Go.as_str().into(),
        status: RefStatus::Pending,
        name_tail: "ArticleHandler".into(),
    };

    let hit = Go
        .resolve(&r, &ctx)
        .expect("a *Handler name is a go handler");
    assert_eq!(hit.target_node_id, "fn:real");
    assert_eq!(hit.confidence, 0.8);
}

#[test]
fn go_detects_from_a_go_mod_or_any_go_file_and_sits_in_registry_order() {
    assert!(Go.detect(&FakeContext::new().with_file("go.mod", "module x\n")));
    assert!(Go.detect(&FakeContext::new().with_file("main.go", "package main\n")));
    assert!(!Go.detect(&FakeContext::new().with_file("main.py", "print(1)\n")));

    // Position, not the whole list — see the note in fw_spring_test.
    let names: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    let go = names.iter().position(|n| *n == "go");
    assert!(go.is_some(), "go is registered");
    assert!(
        names.iter().position(|n| *n == "spring") < go,
        "registry order IS resolve precedence, and REGISTRY_ORDER declares spring \
         before go"
    );
}
