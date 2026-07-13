#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 15 — Flask + FastAPI.
//!
//! # The invariant these tests exist to enforce
//!
//! **Dispatch coverage is end-to-end or not at all** (PRD §8.2). A route that
//! resolves to its handler but whose handler's body call does not resolve is
//! *worse than no route at all*: the agent follows the map, lands one hop short,
//! and goes back to reading files. So every flow test below asserts **every hop**:
//!
//! ```text
//! route(POST /articles) → def create()  →  create_article()
//!        hop 1 (framework)                    hop 2 (Part A's name matcher)
//! ```
//!
//! A 3-of-4-hop resolution is a FAILURE, not partial credit.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::frameworks::python::{FastApi, Flask};
use selene_resolve::frameworks::{FrameworkResolver, all_framework_resolvers};
use selene_resolve::{ReferenceResolver, ResolvedBy};

fn py_fn(id: &str, name: &str, file: &str) -> Node {
    node(id, NodeKind::Function, name, name, file, Language::Python)
}

/// A `calls` reference from the handler's body — the SECOND hop of the flow.
fn body_call(from: &str, callee: &str, file: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.into(),
        reference_name: callee.into(),
        reference_kind: "calls".into(),
        line: Some(5),
        column: Some(4),
        candidates: vec![],
        file_path: file.into(),
        language: Language::Python.as_str().into(),
        status: RefStatus::Pending,
        name_tail: callee.into(),
    }
}

// =============================================================================
// Flask — the whole flow
// =============================================================================

/// **Flow closed ⇔** `POST /articles` → `def create()` → `create_article()`.
///
/// The fixture carries a **stacked** decorator, which is the norm and the thing
/// that breaks naive next-line pairing.
#[test]
fn flask_flow_route_to_handler_to_service() {
    let views = "\
from flask import Blueprint
from .services import create_article

articles_bp = Blueprint('articles', __name__)

@articles_bp.route('/articles', methods=['POST'])
@login_required
def create():
    return create_article(request.json)
";

    let ctx = FakeContext::new()
        .with_file("app/views.py", views)
        .with_file(
            "app/services.py",
            "def create_article(data):\n    return data\n",
        )
        // The nodes Phase 2's extractor would have produced.
        .with_node(py_fn("function:create", "create", "app/views.py"))
        .with_node(py_fn(
            "function:create_article",
            "create_article",
            "app/services.py",
        ));

    // --- hop 1: the route node, and the ref it emits --------------------------
    let out = Flask.extract("app/views.py", views, Language::Python);
    assert_eq!(out.nodes.len(), 1, "one route");

    let route = &out.nodes[0];
    assert_eq!(route.kind, NodeKind::Route);
    assert_eq!(
        route.name, "POST /articles",
        "the verb is IN the name (id uniqueness)"
    );
    assert_eq!(route.route_method.as_deref(), Some("POST"));
    assert_eq!(route.route_path.as_deref(), Some("/articles"));
    assert_eq!(route.framework.as_deref(), Some("flask"));
    assert_eq!(route.start_line, 6);

    assert_eq!(out.refs.len(), 1);
    assert_eq!(
        out.refs[0].reference_name, "create",
        "the handler is the next `def` AFTER the decorator — the stacked \
         @login_required is skipped. Binding the route to `login_required` would \
         leave the flow one hop short of the handler, which is worse than no route."
    );
    assert_eq!(out.refs[0].from_node_id, route.id);

    // --- hop 1 resolves: route → handler --------------------------------------
    let ctx = ctx.with_node(route.clone());
    let mut resolver = ReferenceResolver::new(ctx);
    let hop1 = resolver
        .resolve_one(&out.refs[0])
        .expect("hop 1: the route binds to its handler");
    assert_eq!(hop1.target_node_id, "function:create");

    // --- hop 2 resolves: handler body → service -------------------------------
    let hop2 = resolver
        .resolve_one(&body_call(
            "function:create",
            "create_article",
            "app/views.py",
        ))
        .expect("hop 2: the handler's body call binds to the service");
    assert_eq!(hop2.target_node_id, "function:create_article");

    // THE FLOW IS CLOSED: route → handler → service, every hop asserted.
}

#[test]
fn flask_defaults_to_get_and_reads_a_tuple_method_list() {
    let src = "\
@app.route('/health')
def health():
    pass

@app.route('/legacy', methods=('GET',))
def legacy():
    pass
";
    let out = Flask.extract("app.py", src, Language::Python);
    assert_eq!(out.nodes.len(), 2);
    assert_eq!(
        out.nodes[0].route_method.as_deref(),
        Some("GET"),
        "no methods= ⇒ GET"
    );
    assert_eq!(
        out.nodes[1].route_method.as_deref(),
        Some("GET"),
        "a TUPLE `methods=('GET',)` was previously mislabeled — the first quoted \
         token is the verb, whatever brackets surround it"
    );
}

/// Two stacked `@route` decorators on ONE handler → **two** route nodes, one
/// handler. They collide on `(file, kind, line)`… no, on `(file, kind)` — and are
/// separated only by their `name`, which is why the verb lives in the name.
#[test]
fn two_stacked_route_decorators_yield_two_routes_and_one_handler() {
    let src = "\
@app.route('/x', methods=['GET'])
@app.route('/x', methods=['POST'])
def handle():
    pass
";
    let out = Flask.extract("app.py", src, Language::Python);
    assert_eq!(out.nodes.len(), 2, "two routes");
    assert_ne!(out.nodes[0].id, out.nodes[1].id, "…with DISTINCT ids");
    assert_eq!(out.nodes[0].route_method.as_deref(), Some("GET"));
    assert_eq!(out.nodes[1].route_method.as_deref(), Some("POST"));

    assert_eq!(out.refs.len(), 2);
    assert!(
        out.refs.iter().all(|r| r.reference_name == "handle"),
        "both routes point at the SAME handler — the second decorator does not \
         make the first one's `def` disappear"
    );
}

/// Flask-RESTful: one route per path, all `ANY`, all pointing at the Resource
/// class (the verbs live in the class's own methods).
#[test]
fn flask_restful_add_resource_emits_one_any_route_per_path() {
    let src = "api.add_resource(ArticleList, '/articles', '/articles/')\n";
    let out = Flask.extract("app.py", src, Language::Python);

    assert_eq!(out.nodes.len(), 2, "two paths ⇒ two routes");
    assert!(
        out.nodes
            .iter()
            .all(|n| n.route_method.as_deref() == Some("ANY")),
        "the verb lives in the Resource class's methods, not in the registration"
    );
    assert_eq!(out.nodes[0].route_path.as_deref(), Some("/articles"));
    assert_eq!(out.nodes[1].route_path.as_deref(), Some("/articles/"));
    assert_eq!(out.refs.len(), 2);
    assert!(out.refs.iter().all(|r| r.reference_name == "ArticleList"));
}

#[test]
fn flask_detects_from_a_manifest_or_from_an_app_factory_entrypoint() {
    let by_manifest = FakeContext::new().with_file("requirements.txt", "Flask==3.0\n");
    assert!(Flask.detect(&by_manifest));

    // No manifest at all — the app-factory entrypoint is the only signal. Without
    // this arm a real project reported ZERO routes.
    let by_factory = FakeContext::new().with_file(
        "src/app/__init__.py",
        "from flask import Flask\n\ndef create_app():\n    return Flask(__name__)\n",
    );
    assert!(Flask.detect(&by_factory));

    let neither = FakeContext::new().with_file("requirements.txt", "django\n");
    assert!(!Flask.detect(&neither));
}

// =============================================================================
// FastAPI — the whole flow
// =============================================================================

/// **Flow closed ⇔** `GET /` (a router-root route) → `async def get()` →
/// `list_articles()`.
///
/// The handler is named `get` **on purpose**: it is a Python builtin method name,
/// and Part A's built-in filter drops a bare `get` reference *unless something in
/// the codebase declares it*. The route→handler reference must survive that
/// filter, or every handler named after a builtin silently loses its route.
#[test]
fn fastapi_flow_router_root_route_to_a_builtin_named_handler_to_service() {
    let routes = "\
from fastapi import APIRouter
from app.services import list_articles

router = APIRouter()

@router.get(\"\")
async def get():
    return list_articles()
";

    let ctx = FakeContext::new()
        .with_file("app/routers/articles.py", routes)
        .with_file("app/services.py", "def list_articles():\n    return []\n")
        .with_node(py_fn("function:get", "get", "app/routers/articles.py"))
        .with_node(py_fn(
            "function:list_articles",
            "list_articles",
            "app/services.py",
        ));

    // --- hop 1 ---------------------------------------------------------------
    let out = FastApi.extract("app/routers/articles.py", routes, Language::Python);
    assert_eq!(out.nodes.len(), 1);

    let route = &out.nodes[0];
    assert_eq!(
        route.route_path.as_deref(),
        Some("/"),
        "`@router.get(\"\")` is a ROUTER-ROOT route and it is real — treating an \
         empty path as absent was a recall bug"
    );
    assert_eq!(route.route_method.as_deref(), Some("GET"));
    assert_eq!(route.framework.as_deref(), Some("fastapi"));
    assert_eq!(out.refs[0].reference_name, "get");

    let ctx = ctx.with_node(route.clone());
    let mut resolver = ReferenceResolver::new(ctx);

    let hop1 = resolver.resolve_one(&out.refs[0]).expect(
        "hop 1: a handler named `get` — a Python BUILTIN method name — must still \
         bind. Part A's builtin filter only drops a bare builtin-method name when \
         NOTHING in the codebase declares it; this repo declares `def get()`, so \
         the reference survives. Without that guard every handler named after a \
         builtin loses its route.",
    );
    assert_eq!(hop1.target_node_id, "function:get");

    // --- hop 2 ---------------------------------------------------------------
    let hop2 = resolver
        .resolve_one(&body_call(
            "function:get",
            "list_articles",
            "app/routers/articles.py",
        ))
        .expect("hop 2: the handler's body call binds to the service");
    assert_eq!(hop2.target_node_id, "function:list_articles");
}

#[test]
fn fastapi_reads_every_verb_and_a_multiline_decorator() {
    let src = "\
@router.post(\"/articles\", response_model=ArticleOut)
async def create():
    pass

@app.delete(
    \"/articles/{slug}\",
)
def remove():
    pass
";
    let out = FastApi.extract("api.py", src, Language::Python);
    assert_eq!(out.nodes.len(), 2);
    assert_eq!(out.nodes[0].route_method.as_deref(), Some("POST"));
    assert_eq!(out.nodes[0].route_path.as_deref(), Some("/articles"));
    assert_eq!(
        out.nodes[1].route_method.as_deref(),
        Some("DELETE"),
        "a decorator split across lines still registers"
    );
    assert_eq!(out.refs[1].reference_name, "remove");
}

#[test]
fn fastapi_resolves_router_and_dependency_conventions() {
    let ctx = FakeContext::new()
        .with_node(node(
            "variable:router",
            NodeKind::Variable,
            "articles_router",
            "articles_router",
            "app/routers/articles.py",
            Language::Python,
        ))
        .with_node(py_fn("function:dep", "get_db", "app/dependencies/db.py"));

    let router_ref = UnresolvedRef {
        from_node_id: "file:app/main.py".into(),
        reference_name: "articles_router".into(),
        reference_kind: "references".into(),
        line: Some(1),
        column: Some(0),
        candidates: vec![],
        file_path: "app/main.py".into(),
        language: Language::Python.as_str().into(),
        status: RefStatus::Pending,
        name_tail: "articles_router".into(),
    };
    let hit = FastApi
        .resolve(&router_ref, &ctx)
        .expect("a `*_router` variable");
    assert_eq!(hit.target_node_id, "variable:router");
    assert_eq!(hit.confidence, 0.8);
    assert_eq!(hit.resolved_by, ResolvedBy::Framework);

    let dep_ref = UnresolvedRef {
        reference_name: "get_db".into(),
        ..router_ref.clone()
    };
    let hit = FastApi
        .resolve(&dep_ref, &ctx)
        .expect("a `get_*` dependency");
    assert_eq!(hit.target_node_id, "function:dep");
    assert_eq!(hit.confidence, 0.75);

    // A name matching neither convention is NOT this framework's business.
    let other = UnresolvedRef {
        reference_name: "something_else".into(),
        ..router_ref
    };
    assert!(
        FastApi.resolve(&other, &ctx).is_none(),
        "None = not mine, never a guess"
    );
}

#[test]
fn fastapi_detects_from_a_manifest_or_an_entrypoint() {
    let by_manifest = FakeContext::new().with_file("pyproject.toml", "fastapi = \"^0.110\"\n");
    assert!(FastApi.detect(&by_manifest));

    let by_entry =
        FakeContext::new().with_file("main.py", "from fastapi import FastAPI\napp = FastAPI()\n");
    assert!(FastApi.detect(&by_entry));

    assert!(!FastApi.detect(&FakeContext::new().with_file("main.py", "print('hi')\n")));
}

// =============================================================================
// The registry
// =============================================================================

#[test]
fn flask_and_fastapi_are_registered_in_order() {
    let names: Vec<&str> = all_framework_resolvers().iter().map(|f| f.name()).collect();
    let flask = names.iter().position(|n| *n == "flask");
    let fastapi = names.iter().position(|n| *n == "fastapi");
    assert!(flask.is_some() && fastapi.is_some(), "both registered");
    assert!(
        flask < fastapi,
        "registry order IS resolve precedence, and REGISTRY_ORDER declares flask \
         before fastapi"
    );
}

/// A commented-out route emits **nothing**.
///
/// This shipped as a KNOWN LIMIT in Task 15 (the Python extractors read raw
/// source) and is now closed: both run over Task 11's shared
/// `strip_comments_for_regex`, like every other extractor. A phantom route is not
/// a harmless artifact — it is a node an agent can be sent to, and it will read
/// the file to find out why it is empty.
#[test]
fn a_commented_out_decorator_is_not_a_route() {
    let src = "# @app.route('/ghost')\n# def ghost():\n#     pass\n\n@app.route('/real')\ndef real():\n    pass\n";
    let out = Flask.extract("app.py", src, Language::Python);
    assert_eq!(out.nodes.len(), 1, "the ghost is not a route");
    assert_eq!(out.nodes[0].name, "GET /real");
    assert_eq!(
        out.nodes[0].start_line, 5,
        "the stripper is byte-offset preserving, so blanking the comment above does \
         not shift the line this route's id is hashed from"
    );

    let src = "# @router.get('/ghost')\nasync def ghost():\n    pass\n";
    assert!(
        FastApi
            .extract("api.py", src, Language::Python)
            .nodes
            .is_empty()
    );
}
