#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 20 — ASP.NET.
//!
//! # The flow
//!
//! ```text
//! GET /api/articles  →  ArticlesController.GetAll()  →  _articleService.ListAsync()
//! ```
//!
//! The route only exists if the **bare `[HttpGet]`** is joined to the class's
//! `[Route("api/articles")]` prefix. That is the dominant multi-action-controller
//! shape, and without the join those controllers have zero routes (eShopOnWeb: 9 →
//! 33). The flow only *closes* if the action's DI'd service call resolves too — a
//! route that reaches `GetAll` and stops is a hop short of the answer.

use selene_core::Language;
use selene_resolve::frameworks::csharp::AspNet;
use selene_resolve::frameworks::{FrameworkResolver, all_framework_resolvers};

mod common;
mod pipeline;

use common::FakeContext;

const ASPNET: &[&dyn FrameworkResolver] = &[&AspNet];

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch/aspnet")
}

// =============================================================================
// THE FLOW
// =============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn flow_bare_attribute_route_to_action_to_service() {
    let p = pipeline::index_and_resolve(&fixture(), ASPNET).await;

    let route = p.route("aspnet", Some("GET"), "/api/articles").await;
    p.assert_flow(
        &route.id,
        "ListAsync",
        &["GetAll"],
        "GET /api/articles → ArticlesController.GetAll() → ArticleService.ListAsync()",
    )
    .await;
}

/// The minimal API is a different registration entirely, and it must reach its
/// handler too.
#[tokio::test(flavor = "multi_thread")]
async fn flow_minimal_api_route_to_its_handler() {
    let p = pipeline::index_and_resolve(&fixture(), ASPNET).await;

    let route = p.route("aspnet", Some("GET"), "/health").await;
    let targets = p.targets_of(&route.id).await;
    assert_eq!(targets.len(), 1, "one route, one handler");
    assert_eq!(
        targets[0].name, "Check",
        "`app.MapGet(\"/health\", HealthHandler.Check)` binds to the TAIL identifier"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fixture_has_exactly_the_two_routes() {
    let p = pipeline::index_and_resolve(&fixture(), ASPNET).await;
    // Sorted by (file, line, name) — `Controllers/…` precedes `Program.cs`.
    assert_eq!(
        p.route_names().await,
        vec!["GET /api/articles", "GET /health"],
        "the class [Route] prefix is NOT itself a route — emitting it would send an \
         agent to a controller declaration instead of an action"
    );
}

// =============================================================================
// Units
// =============================================================================

/// Two actions in one controller → two routes, each on its own action.
#[test]
fn two_actions_in_one_controller_yield_two_routes() {
    let src = "\
[Route(\"api/articles\")]
public class ArticlesController : ControllerBase
{
    [HttpGet]
    public IActionResult GetAll() { return Ok(); }

    [HttpPost]
    public IActionResult Create() { return Ok(); }
}
";
    let out = AspNet.extract("ArticlesController.cs", src, Language::CSharp);
    let rows: Vec<(&str, &str)> = out
        .nodes
        .iter()
        .map(|n| {
            let action = out
                .refs
                .iter()
                .find(|r| r.from_node_id == n.id)
                .map(|r| r.reference_name.as_str())
                .unwrap_or("<none>");
            (n.name.as_str(), action)
        })
        .collect();

    assert_eq!(
        rows,
        vec![
            ("GET /api/articles", "GetAll"),
            ("POST /api/articles", "Create"),
        ],
        "same path, different verbs — and each attribute finds its OWN action"
    );
}

/// The 600-char window is a bound, not a suggestion: an attribute whose action is
/// further away than that binds to nothing rather than to something wrong.
#[test]
fn the_handler_window_is_bounded() {
    let filler = " ".repeat(700);
    let src = format!("[HttpGet]{filler}public IActionResult Far() {{ return Ok(); }}");
    let out = AspNet.extract("C.cs", &src, Language::CSharp);

    assert_eq!(out.nodes.len(), 1, "the route is still real");
    assert!(
        out.refs.is_empty(),
        "…but the action is beyond the 600-char window. Binding to whatever turns \
         up eventually is how an attribute adopts the NEXT method in the file."
    );
}

/// Detection arm 4 — the feature-folder layout, with **no `.csproj` and no
/// `Program.cs`**. A repo laid out this way was entirely undetected: 0 → 19 routes.
#[test]
fn detection_arm_4_finds_a_feature_folder_project() {
    let by_feature = FakeContext::new().with_file(
        "src/Features/Articles/ArticlesController.cs",
        "[ApiController]\npublic class ArticlesController : ControllerBase { }\n",
    );
    assert!(
        AspNet.detect(&by_feature),
        "no manifest, no conventional entry point — just a controller where the \
         feature lives. Detection that only works on the tutorial layout is not \
         detection."
    );

    let program = FakeContext::new().with_file(
        "Program.cs",
        "var app = WebApplication.CreateBuilder(args);\n",
    );
    assert!(AspNet.detect(&program), "arm 2");

    let not_aspnet = FakeContext::new().with_file("Utils.cs", "public static class Utils { }\n");
    assert!(!AspNet.detect(&not_aspnet));
}

#[test]
fn di_conventions_cover_the_interface_spelling() {
    use selene_core::{NodeKind, RefStatus, UnresolvedRef};

    let ctx = FakeContext::new()
        .with_node(common::node(
            "iface:IArticleService",
            NodeKind::Interface,
            "IArticleService",
            "IArticleService",
            "src/Interfaces/IArticleService.cs",
            Language::CSharp,
        ))
        .with_node(common::node(
            "class:ArticleService",
            NodeKind::Class,
            "ArticleService",
            "ArticleService",
            "src/Services/ArticleService.cs",
            Language::CSharp,
        ));

    let r = |name: &str| UnresolvedRef {
        from_node_id: "ctor".into(),
        reference_name: name.into(),
        reference_kind: "references".into(),
        line: Some(1),
        column: Some(0),
        candidates: vec![],
        file_path: "Controllers/ArticlesController.cs".into(),
        language: Language::CSharp,
        status: RefStatus::Pending,
        name_tail: name.into(),
    };

    let iface = AspNet
        .resolve(&r("IArticleService"), &ctx)
        .expect("I+Pascal is a C# interface");
    assert_eq!(iface.target_node_id, "iface:IArticleService");
    assert_eq!(iface.confidence, 0.85);

    let concrete = AspNet
        .resolve(&r("ArticleService"), &ctx)
        .expect("*Service");
    assert_eq!(concrete.target_node_id, "class:ArticleService");
}

#[test]
fn aspnet_is_registered_last() {
    let names: Vec<&str> = all_framework_resolvers().iter().map(|r| r.name()).collect();
    assert_eq!(
        names.last().copied(),
        Some("aspnet"),
        "REGISTRY_ORDER ends with aspnet — and registry order IS resolve precedence"
    );
    assert_eq!(
        names,
        vec![
            "express", "react", "django", "flask", "fastapi", "spring", "go", "rust", "laravel",
            "rails", "aspnet"
        ],
        "all eleven v0 frameworks, in REGISTRY_ORDER. Phase 3's framework layer is \
         complete."
    );
}
