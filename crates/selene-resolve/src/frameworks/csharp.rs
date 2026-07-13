//! **ASP.NET** (Task 20) — attribute routes, minimal API, DI suffixes.
//!
//! # The bare attribute + the class prefix — same shape as Spring, same stakes
//!
//! ```csharp
//! [Route("api/articles")]          // ← a PREFIX, on the class
//! public class ArticlesController : ControllerBase
//! {
//!     [HttpGet]                    // ← BARE. Still a route: GET /api/articles
//!     public async Task<IActionResult> GetAll() { … }
//! }
//! ```
//!
//! Miss either half and the dominant shape — a multi-action controller — has **no
//! routes at all**. eShopOnWeb went from 9 routes to 33 when the join landed.
//!
//! # Detection has four arms, and the fourth is the one that matters
//!
//! A repo laid out by *feature* (controllers scattered next to the code they
//! serve, no `.csproj` at the root of the scan) was **entirely undetected**: 0
//! routes → 19 once arm 4 — scanning `(Controller|Program|Startup).cs` files for
//! the attribute/base-class signatures — was added. Detection that only works on
//! the conventional layout is detection that works on tutorials.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, NodeKind, RefStatus, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::frameworks::{
    FrameworkExtraction, FrameworkResolver, RouteSpec, by_convention, char_boundary_at_or_below,
    line_of, route_node_in,
};
use crate::strip_comments::strip_comments_for_regex;
use crate::types::ResolvedRef;

const NO_CLOCK: i64 = 0;
const ASPNET: &str = "aspnet";

/// How far past an attribute the action may be. Ported verbatim.
const HANDLER_WINDOW: usize = 600;

/// Detection arm 4 reads source files; bound the scan (it runs once per index, but
/// "once" must still not walk a monorepo).
const MAX_DETECT_SCAN: usize = 200;

macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by tests
            Regex::new($pat).unwrap()
        })
    };
}

/// Class-level `[Route("api/articles")]` — the prefix.
static CLASS_ROUTE: LazyLock<Regex> = re!(r#"\[\s*Route\s*\(\s*"([^"]*)"\s*\)\s*\]"#);

/// What follows a **class-level** attribute: more attributes, then modifiers, then
/// `class`. Anchored — only ever tried right after a `[Route(...)]`, never scanned.
static CLASS_TAIL: LazyLock<Regex> = re!(
    r"^\s*(?:\[[^\]]*\]\s*)*(?:(?:public|internal|private|protected|sealed|abstract|partial|static)\s+)*class\b"
);

/// `[HttpGet]` / `[HttpPost("{id}")]` — **bare allowed**.
static HTTP_ATTR: LazyLock<Regex> = re!(
    r#"\[\s*(HttpGet|HttpPost|HttpPut|HttpPatch|HttpDelete)(?:\s*\(\s*"([^"]+)"[^)]*\))?\s*\]"#
);

/// The action a `[Http*]` decorates.
static ACTION_SIG: LazyLock<Regex> =
    re!(r"(?:public|private|protected|internal)\s+[\w<>,\s\[\]?.]+?\s+(\w+)\s*\(");

/// Minimal API: `app.MapGet("/health", HealthHandler.Check)`.
static MINIMAL_API: LazyLock<Regex> =
    re!(r#"\.Map(Get|Post|Put|Patch|Delete)\s*\(\s*"([^"]+)"\s*,\s*([^,)]+)"#);

/// The detection signatures arm 4 looks for.
static ASPNET_SIGNATURE: LazyLock<Regex> = re!(
    r"\[\s*(?:ApiController|Route|HttpGet|HttpPost)\b|:\s*(?:ControllerBase|Controller)\b|\bWebApplication\b|\bCreateHostBuilder\b|\bUseStartup\b"
);

/// The files arm 4 is allowed to open.
static DETECT_FILES: LazyLock<Regex> = re!(r"(?:^|/)\w*(?:Controller|Program|Startup)\.cs$");

/// ASP.NET.
pub struct AspNet;

impl FrameworkResolver for AspNet {
    fn name(&self) -> &'static str {
        ASPNET
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[Language::CSharp])
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        // 1. A web `.csproj`.
        //
        //    ⚠ `.csproj` has no grammar, so it is **not an indexed file** in v0 —
        //    `all_files()` (which answers from the index) will not list one, and
        //    this arm is dormant. It is kept because it is the parity contract and
        //    because it lights up for free the day project files are tracked. Arms
        //    2–4 are what actually detect ASP.NET today, and arm 4 covers the case
        //    arm 1 was reaching for (a project whose manifest we cannot see).
        if let Some(text) = ctx
            .all_files()
            .iter()
            .find(|f| f.ends_with(".csproj"))
            .and_then(|f| ctx.read_file(f))
            && is_web_csproj(&text)
        {
            return true;
        }

        // 2. `Program.cs` with a web host.
        if let Some(src) = ctx.read_file("Program.cs")
            && (src.contains("WebApplication")
                || src.contains("CreateHostBuilder")
                || src.contains("UseStartup"))
        {
            return true;
        }

        // 3. `Startup.cs` at all.
        if ctx.read_file("Startup.cs").is_some() {
            return true;
        }

        // 4. THE FEATURE-FOLDER ARM. No manifest, no conventional entry point —
        //    just controllers, wherever they live. Without this a whole layout
        //    style is invisible (0 → 19 routes on one real repo).
        ctx.all_files()
            .iter()
            .filter(|f| DETECT_FILES.is_match(f))
            .take(MAX_DETECT_SCAN)
            .any(|f| {
                ctx.read_file(f)
                    .is_some_and(|src| ASPNET_SIGNATURE.is_match(&src))
            })
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        if language != Language::CSharp {
            return out;
        }
        let src = strip_comments_for_regex(content, Language::CSharp);

        // --- the class prefix --------------------------------------------------
        // A `[Route(...)]` is a PREFIX only when a class declaration follows it; on
        // an action it is the route itself. What follows decides, not the spelling.
        let mut prefix = String::new();
        let mut class_level: Vec<usize> = Vec::new();
        for caps in CLASS_ROUTE.captures_iter(&src) {
            let (Some(whole), Some(p)) = (caps.get(0), caps.get(1)) else {
                continue;
            };
            if !CLASS_TAIL.is_match(&src[whole.end()..]) {
                continue;
            }
            class_level.push(whole.start());
            if prefix.is_empty() {
                prefix = p.as_str().to_string();
            }
        }

        // --- `[HttpGet]` / `[HttpPost("{id}")]` --------------------------------
        for caps in HTTP_ATTR.captures_iter(&src) {
            let (Some(whole), Some(attr)) = (caps.get(0), caps.get(1)) else {
                continue;
            };
            let sub = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let method = attr.as_str().trim_start_matches("Http").to_uppercase();
            let route_path = join_path(&prefix, sub);
            let line = line_of(&src, whole.start());

            let node = route_node_in(
                &RouteSpec::new(ASPNET, Some(&method), &route_path, path, line),
                Language::CSharp.as_str(),
                NO_CLOCK,
            );
            if let Some((action, action_line)) = next_action(&src, whole.end()) {
                out.refs.push(cs_ref(&node.id, &action, path, action_line));
            }
            out.nodes.push(node);
        }

        // --- minimal API --------------------------------------------------------
        for caps in MINIMAL_API.captures_iter(&src) {
            let (Some(whole), Some(verb), Some(route_path), Some(handler)) =
                (caps.get(0), caps.get(1), caps.get(2), caps.get(3))
            else {
                continue;
            };
            let line = line_of(&src, whole.start());
            let node = route_node_in(
                &RouteSpec::new(
                    ASPNET,
                    Some(&verb.as_str().to_uppercase()),
                    route_path.as_str(),
                    path,
                    line,
                ),
                Language::CSharp.as_str(),
                NO_CLOCK,
            );
            // `HealthHandler.Check` → `Check`. A lambda names nothing and gets no
            // reference — the route is real, the reference would be a guess.
            if let Some(name) = tail_identifier(handler.as_str()) {
                out.refs.push(cs_ref(&node.id, &name, path, line));
            }
            out.nodes.push(node);
        }

        out.nodes
            .sort_by(|a, b| (a.start_line, &a.name, &a.id).cmp(&(b.start_line, &b.name, &b.id)));
        out.refs.sort_by(|a, b| {
            (a.line, &a.reference_name, &a.from_node_id).cmp(&(
                b.line,
                &b.reference_name,
                &b.from_node_id,
            ))
        });
        out
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if Language::from_wire(&r.language) != Some(Language::CSharp) {
            return None;
        }
        let name = r.reference_name.as_str();

        if name.ends_with("Controller") {
            return by_convention(r, ctx, &[NodeKind::Class], &["/Controllers/"], 0.85);
        }
        if name.ends_with("Service") || is_interface_name(name) {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Class, NodeKind::Interface],
                &["/Services/", "/Interfaces/"],
                0.85,
            );
        }
        if name.ends_with("Repository") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Class, NodeKind::Interface],
                &["/Repositories/", "/Data/"],
                0.85,
            );
        }
        if name.ends_with("ViewModel") || name.ends_with("Dto") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Class],
                &["/ViewModels/", "/Models/", "/Dtos/"],
                0.80,
            );
        }
        if is_pascal_case(name) {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Class],
                &["/Models/", "/Entities/", "/Domain/"],
                0.70,
            );
        }
        None
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn is_web_csproj(text: &str) -> bool {
    text.contains("Microsoft.AspNetCore")
        || text.contains("Microsoft.NET.Sdk.Web")
        || text.contains("System.Web.Mvc")
}

/// `api/articles` + `{id}` → `/api/articles/{id}`; both empty → `/`.
fn join_path(prefix: &str, sub: &str) -> String {
    let parts: Vec<&str> = prefix
        .split('/')
        .chain(sub.split('/'))
        .filter(|s| !s.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// The action a `[Http*]` attribute decorates: the first method signature within
/// [`HANDLER_WINDOW`] bytes. Any further attributes stacked between are skipped —
/// `[Authorize]` is not an action.
fn next_action(src: &str, offset: usize) -> Option<(String, u32)> {
    let end = char_boundary_at_or_below(src, offset.saturating_add(HANDLER_WINDOW));
    let start = char_boundary_at_or_below(src, offset.min(end));
    let m = ACTION_SIG.captures(&src[start..end])?.get(1)?;
    Some((m.as_str().to_string(), line_of(src, start + m.start())))
}

/// `HealthHandler.Check` → `Check`. A lambda (`() => …`) → `None`.
fn tail_identifier(expr: &str) -> Option<String> {
    let e = expr.trim();
    if e.starts_with('(') || e.contains("=>") {
        return None;
    }
    let tail = e.rsplit('.').next()?.trim();
    let ident: String = tail
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
}

fn cs_ref(from: &str, name: &str, file: &str, line: u32) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: "references".to_string(),
        line: Some(line),
        column: Some(0),
        candidates: vec![],
        file_path: file.to_string(),
        language: Language::CSharp.as_str().to_string(),
        status: RefStatus::Pending,
        name_tail: name.to_string(),
    }
}

/// `IArticleService` — C#'s interface convention: `I` + PascalCase.
fn is_interface_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next() == Some('I') && chars.next().is_some_and(|c| c.is_ascii_uppercase())
}

fn is_pascal_case(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().any(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_attribute_joins_the_class_prefix() {
        let src = "\
[Route(\"api/articles\")]
[ApiController]
public class ArticlesController : ControllerBase
{
    [HttpGet]
    public async Task<IActionResult> GetAll() { return Ok(); }

    [HttpGet(\"{id}\")]
    [Authorize]
    public async Task<IActionResult> GetById(int id) { return Ok(); }
}
";
        let out = AspNet.extract("ArticlesController.cs", src, Language::CSharp);
        let names: Vec<&str> = out.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["GET /api/articles", "GET /api/articles/{id}"],
            "the class [Route] is a PREFIX, not a route — and a BARE [HttpGet] is \
             still a route. Without both halves a multi-action controller has no \
             routes at all (eShopOnWeb: 9 → 33)."
        );

        let actions: Vec<&str> = out.refs.iter().map(|r| r.reference_name.as_str()).collect();
        assert_eq!(
            actions,
            vec!["GetAll", "GetById"],
            "the stacked [Authorize] is not an action"
        );
    }

    #[test]
    fn a_minimal_api_lambda_gets_a_route_and_no_reference() {
        assert_eq!(
            tail_identifier("HealthHandler.Check").as_deref(),
            Some("Check")
        );
        assert_eq!(tail_identifier("() => Results.Ok()"), None);
        assert_eq!(tail_identifier("(int id) => Results.Ok(id)"), None);
    }

    #[test]
    fn the_interface_convention_is_i_plus_pascal() {
        assert!(is_interface_name("IArticleService"));
        assert!(
            !is_interface_name("Invoice"),
            "not every I-word is an interface"
        );
        assert!(!is_interface_name("i18n"));
    }
}
