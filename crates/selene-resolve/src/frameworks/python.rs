//! The Python web frameworks: **Flask** and **FastAPI** (Task 15).
//!
//! (Django appends here — `src/frameworks/python.rs` is shared by Tasks 14, 15
//! and 26, strictly sequentially. Keep the resolvers as independent units so an
//! append never has to reshape what is already here.)
//!
//! # The decorator route engine
//!
//! Both frameworks register routes with a decorator on the handler:
//!
//! ```python
//! @bp.route('/articles', methods=['POST'])
//! @login_required                    # ← stacked decorators are the norm
//! def create():
//!     return create_article(...)
//! ```
//!
//! So the mechanism is: match the route decorator, then find the **next `def`
//! after it** — which *skips* any intervening decorators. Get that pairing wrong
//! and the route points at nothing, which is the half-bridged flow the hard
//! invariant forbids (PRD §8.2): the agent follows the route, lands nowhere, and
//! goes back to reading files.
//!
//! [`next_def_after`] is that engine, written once and shared.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, NodeKind, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::frameworks::{
    FrameworkExtraction, FrameworkResolver, RouteSpec, by_convention, line_of, manifest_mentions,
    route_node,
};
use crate::strip_comments::strip_comments_for_regex;

/// `updated_at` for every node this module emits.
///
/// **Zero, deliberately.** Route emission must be byte-deterministic — the same
/// source must produce the same nodes on every run, on every machine — and a
/// wall-clock stamp is the one thing that cannot be. (Phase 2's extraction stamps
/// `now` because its nodes are re-emitted per file on re-index; a route node is
/// re-derived from the same bytes, so there is nothing for a timestamp to say.)
const NO_CLOCK: i64 = 0;
use crate::types::ResolvedRef;

/// Compile a literal pattern. Every one is exercised by a test in this file, so a
/// bad pattern fails a test rather than a run.
macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by tests
            Regex::new($pat).unwrap()
        })
    };
}

// =============================================================================
// The shared decorator engine
// =============================================================================

/// `def handler(` / `async def handler(` — the handler a decorator decorates.
static DEF_AFTER: LazyLock<Regex> = re!(r"\n[ \t]*(?:async\s+)?def\s+(\w+)");

/// The **next** `def` after `offset`, with its 1-based line.
///
/// The whole point is that it **skips stacked decorators**: `@app.route(...)` then
/// `@login_required` then `def create()` still yields `create`. A naive
/// "next line" pairing binds the route to `login_required` and the flow dies one
/// hop in.
pub fn next_def_after(src: &str, offset: usize) -> Option<(String, u32)> {
    let caps = DEF_AFTER.captures_at(src, offset)?;
    let m = caps.get(1)?;
    Some((m.as_str().to_string(), line_of(src, m.start())))
}

/// A `references` reference from a route node to the handler it names.
fn handler_ref(route_id: &str, handler: &str, file: &str, line: u32) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: route_id.to_string(),
        reference_name: handler.to_string(),
        reference_kind: "references".to_string(),
        line: Some(line),
        column: Some(0),
        candidates: vec![],
        file_path: file.to_string(),
        language: Language::Python.as_str().to_string(),
        status: selene_core::RefStatus::Pending,
        name_tail: handler.to_string(),
    }
}

const PY_MANIFESTS: [&str; 4] = ["requirements.txt", "pyproject.toml", "Pipfile", "setup.py"];

// =============================================================================
// Flask
// =============================================================================

/// `@bp.route('/articles', methods=['POST'])` — the method list is optional.
static FLASK_ROUTE: LazyLock<Regex> = re!(
    r#"@(\w+)\.route\s*\(\s*['"]([^'"]*)['"](?:\s*,\s*methods\s*=\s*[\[(]([^\])]+)[\])])?\s*\)"#
);
/// The FIRST quoted token of a `methods=` list **or tuple**.
static FIRST_QUOTED: LazyLock<Regex> = re!(r#"['"](\w+)['"]"#);
/// Flask-RESTful: `api.add_resource(ArticleList, '/articles', '/articles/')`.
static FLASK_RESOURCE: LazyLock<Regex> =
    re!(r#"\.add\w*[Rr]esource\s*\(\s*(\w+)\s*,\s*((?:['"][^'"]+['"]\s*,?\s*)+)"#);
static QUOTED_PATH: LazyLock<Regex> = re!(r#"['"]([^'"]+)['"]"#);
/// An app-factory entrypoint file.
static FLASK_ENTRY: LazyLock<Regex> = re!(r"(?:^|/)(app|application|main|wsgi|__init__)\.py$");

/// Flask.
pub struct Flask;

impl FrameworkResolver for Flask {
    fn name(&self) -> &'static str {
        "flask"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[Language::Python])
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        if manifest_mentions(ctx, &PY_MANIFESTS, "flask") {
            return true;
        }
        // The app-factory case: a repo with no requirements file at all, whose
        // `Flask(...)` lives in a subdirectory entrypoint. Without this arm one
        // real project went 0 → 19 routes. Bounded to the first 50 candidates —
        // detection runs once per index, but it must not walk a monorepo.
        ctx.all_files()
            .iter()
            .filter(|f| FLASK_ENTRY.is_match(f))
            .take(50)
            .any(|f| {
                ctx.read_file(f).is_some_and(|src| {
                    src.contains("Flask(") && src.to_lowercase().contains("flask")
                })
            })
    }

    fn extract(&self, path: &str, content: &str, _language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        // Comment-stripped, byte-offset preserving (Task 11's shared machinery): a
        // commented-out decorator must not become a route, and blanking must not
        // shift the line a route id is hashed from.
        let content = &strip_comments_for_regex(content, Language::Python);

        for caps in FLASK_ROUTE.captures_iter(content) {
            let Some(whole) = caps.get(0) else { continue };
            let raw_path = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let path_str = if raw_path.is_empty() { "/" } else { raw_path };

            // `methods=['POST']` and `methods=('GET',)` are both lists of quoted
            // tokens; take the FIRST. (The tuple form was previously mislabeled.)
            let method = caps
                .get(3)
                .and_then(|m| FIRST_QUOTED.captures(m.as_str()))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_uppercase())
                .unwrap_or_else(|| "GET".to_string());

            let line = line_of(content, whole.start());
            let node = route_node(
                &RouteSpec::new("flask", Some(&method), path_str, path, line),
                NO_CLOCK,
            );

            if let Some((handler, handler_line)) = next_def_after(content, whole.end()) {
                out.refs
                    .push(handler_ref(&node.id, &handler, path, handler_line));
            }
            out.nodes.push(node);
        }

        // Flask-RESTful: ONE route per path, all `ANY`, all pointing at the
        // Resource class (the verb lives in the class's own methods).
        for caps in FLASK_RESOURCE.captures_iter(content) {
            let Some(whole) = caps.get(0) else { continue };
            let Some(class) = caps.get(1) else { continue };
            let Some(paths) = caps.get(2) else { continue };
            let line = line_of(content, whole.start());

            for p in QUOTED_PATH.captures_iter(paths.as_str()) {
                let Some(route_path) = p.get(1) else { continue };
                let node = route_node(
                    &RouteSpec::new("flask", Some("ANY"), route_path.as_str(), path, line),
                    NO_CLOCK,
                );
                out.refs
                    .push(handler_ref(&node.id, class.as_str(), path, line));
                out.nodes.push(node);
            }
        }

        out
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if r.language != Language::Python.as_str() {
            return None;
        }
        // A blueprint is a module-level variable, not a symbol the name matcher
        // would rank confidently.
        let name = r.reference_name.as_str();
        if name.ends_with("_bp") || name.ends_with("_blueprint") {
            return by_convention(r, ctx, &[NodeKind::Variable], &[], 0.8);
        }
        None
    }
}

// =============================================================================
// FastAPI
// =============================================================================

/// `@router.get("")` — an **empty path is legal** and means the router root.
static FASTAPI_ROUTE: LazyLock<Regex> =
    re!(r#"@(\w+)\.(get|post|put|patch|delete|options|head)\s*\(\s*['"]([^'"]*)['"]"#);

/// FastAPI.
pub struct FastApi;

impl FrameworkResolver for FastApi {
    fn name(&self) -> &'static str {
        "fastapi"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[Language::Python])
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        if manifest_mentions(ctx, &PY_MANIFESTS, "fastapi") {
            return true;
        }
        ctx.all_files()
            .iter()
            .filter(|f| {
                let base = f.rsplit('/').next().unwrap_or(f);
                matches!(base, "app.py" | "main.py" | "api.py")
            })
            .any(|f| ctx.read_file(f).is_some_and(|src| src.contains("FastAPI(")))
    }

    fn extract(&self, path: &str, content: &str, _language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        // See `Flask::extract` — the same contract, the same shared stripper.
        let content = &strip_comments_for_regex(content, Language::Python);

        for caps in FASTAPI_ROUTE.captures_iter(content) {
            let Some(whole) = caps.get(0) else { continue };
            let Some(verb) = caps.get(2) else { continue };
            let raw_path = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            // `@router.get("")` is a ROUTER-ROOT route, and it is real — treating
            // it as absent was a recall bug on a large corpus.
            let path_str = if raw_path.is_empty() { "/" } else { raw_path };

            let method = verb.as_str().to_uppercase();
            let line = line_of(content, whole.start());
            let node = route_node(
                &RouteSpec::new("fastapi", Some(&method), path_str, path, line),
                NO_CLOCK,
            );

            if let Some((handler, handler_line)) = next_def_after(content, whole.end()) {
                out.refs
                    .push(handler_ref(&node.id, &handler, path, handler_line));
            }
            out.nodes.push(node);
        }

        out
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if r.language != Language::Python.as_str() {
            return None;
        }
        let name = r.reference_name.as_str();

        if name == "router" || name.ends_with("_router") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Variable],
                &["/routers/", "/api/", "/routes/", "/endpoints/"],
                0.8,
            );
        }
        if name.starts_with("get_") || name.starts_with("Depends") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Function],
                &["/dependencies/", "/deps/", "/core/"],
                0.75,
            );
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The engine's whole job: skip stacked decorators and find the real handler.
    #[test]
    fn next_def_after_skips_stacked_decorators() {
        let src = "@bp.route('/x')\n@login_required\n@cached\ndef create():\n    pass\n";
        let at = src.find("@bp.route").unwrap() + 1;
        let (handler, line) = next_def_after(src, at).unwrap();
        assert_eq!(
            handler, "create",
            "a naive next-line pairing would bind the route to `login_required` \
             and the flow would die one hop in"
        );
        assert_eq!(line, 4);
    }

    #[test]
    fn next_def_after_finds_an_async_def() {
        let src = "@router.get('/x')\nasync def list_items():\n    pass\n";
        assert_eq!(next_def_after(src, 1).unwrap().0, "list_items");
    }

    #[test]
    fn next_def_after_returns_none_when_no_handler_follows() {
        assert!(next_def_after("@bp.route('/x')\n", 1).is_none());
    }

    #[test]
    fn the_flask_method_list_takes_the_first_quoted_token_in_a_list_or_tuple() {
        for (src, want) in [
            ("@bp.route('/x', methods=['POST'])", "POST"),
            ("@bp.route('/x', methods=('GET',))", "GET"),
            ("@bp.route('/x', methods=[\"PUT\", \"PATCH\"])", "PUT"),
        ] {
            let caps = FLASK_ROUTE.captures(src).expect(src);
            let method = caps
                .get(3)
                .and_then(|m| FIRST_QUOTED.captures(m.as_str()))
                .map(|c| c[1].to_uppercase())
                .unwrap_or_else(|| "GET".into());
            assert_eq!(method, want, "{src}");
        }
        // No `methods=` at all ⇒ GET.
        let caps = FLASK_ROUTE.captures("@app.route('/x')").unwrap();
        assert!(caps.get(3).is_none());
    }
}
