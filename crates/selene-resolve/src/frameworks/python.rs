//! The Python web frameworks: **Django** (Task 14), **Flask** and **FastAPI**
//! (Task 15). The Django ORM descriptor (Task 26) appends here too.
//!
//! # Detection is NOT exclusive, and neither framework shadows the other
//!
//! A repo can hold Django *and* Flask (a Django service with a Flask sidecar
//! script is an ordinary thing), so `detect` is asked of each independently and
//! both may fire. Nothing here is an if/else chain, and adding Flask did not take
//! anything away from Django:
//!
//! - **The detection signals are disjoint.** Django keys on `manage.py` or
//!   `django` in a manifest; Flask on `flask` in a manifest or a `Flask(` app
//!   factory; FastAPI on `fastapi` or a `FastAPI(` entrypoint. A Django project
//!   whose `requirements.txt` happens to name flask detects as **both** — which is
//!   the truth about that repo, not a bug.
//! - **The extract regexes only match their own registration syntax.** Django's
//!   `path(...)`/`re_path(...)`/`router.register(...)` never appear in Flask code;
//!   Flask's `@bp.route(...)` and FastAPI's `@router.get(...)` never appear in a
//!   urlconf. So a framework that fires spuriously emits **zero** routes rather
//!   than wrong ones.
//! - **Precedence, when both do claim a name:** `REGISTRY_ORDER` puts django ahead
//!   of flask/fastapi, and `resolve_one` walks it in that order. It only matters
//!   for a sub-0.9 result (all three resolve below that), so both become
//!   *candidates* and the highest confidence wins — first-wins only on an exact
//!   tie. Nothing here can silently outrank the name matcher.
//!
//! # The flow Django must close
//!
//! ```text
//! path('articles/<slug>/', ArticleDetail.as_view())
//!     →  ArticleDetail  →  .get()  →  get_article()  →  Article.objects.filter
//! ```
//!
//! Django's *other* flow — QuerySet → SQL compiler, via the `_iterable_class`
//! descriptor — is a **separate chain** and belongs to Task 26. This one ends at
//! the view's own calls.
//!
//! `include('api.urls')` names no declared symbol anywhere, so it needs
//! `claims_reference` — without the claim the pre-filter drops the reference
//! before `resolve()` is ever called and the include bridge is silently inert.
//! That hook is the whole reason `claims_reference` exists on the trait.
//!
//! # The Flask/FastAPI decorator route engine
//!
//! Both register routes with a decorator on the handler:
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
use crate::types::{ResolvedBy, ResolvedRef};

/// `updated_at` for every node this module emits.
///
/// **Zero, deliberately.** Route emission must be byte-deterministic — the same
/// source must produce the same nodes on every run, on every machine — and a
/// wall-clock stamp is the one thing that cannot be. (Phase 2's extraction stamps
/// `now` because its nodes are re-emitted per file on re-index; a route node is
/// re-derived from the same bytes, so there is nothing for a timestamp to say.)
const NO_CLOCK: i64 = 0;

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
// Django
// =============================================================================

const LANGS: &[Language] = &[Language::Python];

/// `path('x/', view)` / `re_path(r'^x$', view)` / `url(...)`.
static URLCONF: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r#"\b(path|re_path|url)\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+(?:\s*\([^)]*\))?)"#)
        .unwrap()
});

/// DRF: `router.register(r'articles', ArticleViewSet)`.
///
/// The **string first argument** is what separates this from
/// `admin.register(Model, ModelAdmin)`, whose first argument is a class. Without
/// that discriminator every registered admin model would become a route.
static DRF_REGISTER: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r#"\.register\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+)"#).unwrap()
});

/// `include('api.urls')` — the handler expression form that means "another
/// urlconf", not a view.
static INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r#"^include\s*\(\s*['"]([^'"]+)['"]"#).unwrap()
});

/// The runtime-chosen attribute that Django's `QuerySet` calls to build its
/// iterable. It is an attribute NAME, which is what makes this a resolver case.
const ITERABLE_CLASS_ATTR: &str = "_iterable_class";
/// Django's default `_iterable_class`.
const MODEL_ITERABLE: &str = "ModelIterable";
const DUNDER_ITER: &str = "__iter__";

const MODEL_DIRS: &[&str] = &["models", "app/models", "src/models"];
const VIEW_DIRS: &[&str] = &["views", "app/views", "src/views"];

pub struct DjangoResolver;

impl FrameworkResolver for DjangoResolver {
    fn name(&self) -> &'static str {
        "django"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(LANGS)
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        if ctx.file_exists("manage.py") {
            return true;
        }
        ["requirements.txt", "setup.py", "pyproject.toml"]
            .iter()
            .any(|f| {
                ctx.read_file(f)
                    .is_some_and(|s| s.to_lowercase().contains("django"))
            })
    }

    /// `include('x.y')` names no symbol — claim it past the pre-filter, or the
    /// bridge never gets a chance to run. (Task 26 adds `_iterable_class` here.)
    fn claims_reference(&self, name: &str) -> bool {
        // `include('api.urls')` — another urlconf (Task 14).
        name.ends_with(".urls")
            // `self._iterable_class(self)` — the ORM descriptor (Task 26). It is an
            // ATTRIBUTE name, not a declared symbol, so without this claim the
            // pre-filter drops the reference before `resolve()` ever runs and the
            // query→SQL flow silently does not exist.
            || name == ITERABLE_CLASS_ATTR
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        if !path.ends_with(".py") {
            return FrameworkExtraction::default();
        }
        let src = strip_comments_for_regex(content, language);
        let mut out = FrameworkExtraction::default();

        // --- urlconf ---------------------------------------------------------
        for caps in URLCONF.captures_iter(&src) {
            let (Some(whole), Some(url), Some(handler)) = (caps.get(0), caps.get(2), caps.get(3))
            else {
                continue;
            };
            let line = line_of(&src, whole.start());
            // Path-only router: no HTTP verb. The name is the RAW url string.
            let node = route_node(
                &RouteSpec::new(self.name(), None, url.as_str(), path, line),
                0,
            );

            let expr = handler.as_str().trim();
            let (ref_name, kind) = match INCLUDE.captures(expr) {
                // `include('api.urls')` → another urlconf, not a view.
                Some(c) => (c[1].to_string(), "imports"),
                None => (view_name(expr), "references"),
            };
            if !ref_name.is_empty() {
                out.refs
                    .push(self.mk_ref(&node.id, &ref_name, kind, path, line));
            }
            out.nodes.push(node);
        }

        // --- DRF router.register --------------------------------------------
        for caps in DRF_REGISTER.captures_iter(&src) {
            let (Some(whole), Some(prefix), Some(cls)) = (caps.get(0), caps.get(1), caps.get(2))
            else {
                continue;
            };
            let cls = cls.as_str();
            // ONLY a ViewSet/View class. `admin.register(Article, ArticleAdmin)`
            // has a class first arg and never reaches here, but a `register` on
            // some other registry with a string key might — so gate on the shape
            // of the second argument too.
            if !(cls.ends_with("ViewSet") || cls.ends_with("View")) {
                continue;
            }
            let prefix = prefix
                .as_str()
                .trim_start_matches('^')
                .trim_end_matches('$')
                .trim_end_matches('/');
            let route_path = format!("/{prefix}");
            let line = line_of(&src, whole.start());

            let node = route_node(
                &RouteSpec::new(self.name(), Some("VIEWSET"), &route_path, path, line),
                0,
            );
            let tail = cls.rsplit('.').next().unwrap_or(cls);
            out.refs
                .push(self.mk_ref(&node.id, tail, "references", path, line));
            out.nodes.push(node);
        }

        out
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = r.reference_name.as_str();

        // The ORM descriptor (Task 26) — see `resolve_iterable_class`.
        if name == ITERABLE_CLASS_ATTR {
            return self.resolve_iterable_class(r, ctx);
        }

        // Views: `*View` / `*ViewSet` — class OR function-based.
        if name.ends_with("View") || name.ends_with("ViewSet") {
            return self.pick(
                r,
                ctx,
                &[NodeKind::Class, NodeKind::Function],
                VIEW_DIRS,
                0.85,
            );
        }
        // Forms.
        if name.ends_with("Form") {
            return self.pick(r, ctx, &[NodeKind::Class], &[], 0.8);
        }
        // Models: `*Model`, or a bare Capitalized word (`Article`).
        if name.ends_with("Model") || is_simple_pascal(name) {
            return self.pick(r, ctx, &[NodeKind::Class], MODEL_DIRS, 0.8);
        }
        None
    }
}

impl DjangoResolver {
    /// The ORM descriptor bridge (Task 26) — `self._iterable_class(self)` →
    /// `ModelIterable.__iter__`.
    ///
    /// # This is a RESOLVER, not a synthesizer — and the difference is the point
    ///
    /// The roadmap files this under "the 5 synthesizers", but it is a framework
    /// `resolve()` branch, and it must stay one. The playbook's central mechanism
    /// lesson (§2, §3a):
    ///
    /// | The reference is… | Mechanism | Provenance |
    /// |---|---|---|
    /// | **named** — `_iterable_class` IS an attribute name | `claims_reference` + `resolve()` | ordinary `tree-sitter` edge |
    /// | **anonymous** — `cb()`, `emit('e')`, `<Child/>` | a whole-graph synth pass | `heuristic` + `synthesizedBy` |
    ///
    /// So this emits **no heuristic edge and no `synthesizedBy`**. A reviewer who
    /// expects one is expecting the wrong contract, and a test that asserts
    /// `Heuristic` here is asserting the wrong thing.
    ///
    /// # The hole
    ///
    /// `QuerySet._fetch_all` calls `self._iterable_class(self)` — a *runtime-chosen*
    /// iterable class (default `ModelIterable`) whose `__iter__` runs the SQL
    /// compiler. Statically, `_fetch_all`'s only callee was
    /// `_prefetch_related_objects`, and the query→SQL flow did not exist at all.
    fn resolve_iterable_class(
        &self,
        r: &UnresolvedRef,
        ctx: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // The default iterable. (A project that swaps it for a custom class is the
        // frontier: the choice is made at runtime, so silence is the right answer.)
        let class = ctx
            .nodes_by_name(MODEL_ITERABLE)
            .into_iter()
            .find(|n| n.kind == NodeKind::Class)?;

        // `__iter__` **on that class** — the membership test is the whole precision
        // story. Taking any `__iter__` in the project would bind the ORM's hottest
        // flow to whichever iterator the name matcher happened to see first.
        let iter_method = ctx.nodes_by_name(DUNDER_ITER).into_iter().find(|n| {
            n.kind == NodeKind::Method
                && n.file_path == class.file_path
                && n.start_line >= class.start_line
                && n.end_line <= class.end_line
        })?;

        Some(ResolvedRef {
            original: r.clone(),
            target_node_id: iter_method.id,
            confidence: 0.7,
            resolved_by: ResolvedBy::Framework,
        })
    }

    fn mk_ref(&self, from: &str, name: &str, kind: &str, file: &str, line: u32) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: kind.to_string(),
            line: Some(line),
            column: Some(0),
            candidates: vec![],
            file_path: file.to_string(),
            language: Language::Python.as_str().to_string(),
            status: selene_core::RefStatus::Pending,
            name_tail: name.rsplit('.').next().unwrap_or(name).to_string(),
        }
    }

    /// Preferred-dir, then unique-only. **Ambiguous ⇒ `None`.**
    fn pick(
        &self,
        r: &UnresolvedRef,
        ctx: &dyn ResolutionContext,
        kinds: &[NodeKind],
        dirs: &[&str],
        confidence: f64,
    ) -> Option<ResolvedRef> {
        let hits: Vec<_> = ctx
            .nodes_by_name(&r.reference_name)
            .into_iter()
            .filter(|n| kinds.contains(&n.kind))
            .collect();

        let chosen = if let Some(n) = hits.iter().find(|n| {
            dirs.iter().any(|d| {
                n.file_path.contains(&format!("{d}/")) || n.file_path.contains(&format!("/{d}."))
            })
        }) {
            n
        } else if hits.len() == 1 {
            &hits[0]
        } else {
            return None;
        };

        Some(ResolvedRef {
            original: r.clone(),
            target_node_id: chosen.id.clone(),
            confidence,
            resolved_by: ResolvedBy::Framework,
        })
    }
}

/// The view name out of a urlconf handler expression.
///
/// `ArticleDetail.as_view()` → `ArticleDetail`; `views.article_detail` →
/// `article_detail`; `ArticleDetail.as_view(foo=1)` → `ArticleDetail`.
fn view_name(expr: &str) -> String {
    // Strip a trailing call — `.as_view(...)` or `(...)`.
    let base = match expr.find('(') {
        Some(i) => &expr[..i],
        None => expr,
    };
    let base = base.strip_suffix(".as_view").unwrap_or(base);
    base.rsplit('.').next().unwrap_or(base).trim().to_string()
}

/// `Article` — a bare Capitalized identifier, no underscores, no dots.
fn is_simple_pascal(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

// =============================================================================
// The shared decorator engine (Flask + FastAPI)
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

    #[test]
    fn regexes_compile() {
        assert!(URLCONF.is_match("path('x/', V.as_view())"));
        assert!(DRF_REGISTER.is_match("router.register(r'a', AViewSet)"));
        assert!(INCLUDE.is_match("include('api.urls')"));
    }

    #[test]
    fn view_names_are_stripped_to_the_last_segment() {
        assert_eq!(view_name("ArticleDetail.as_view()"), "ArticleDetail");
        assert_eq!(view_name("ArticleDetail.as_view(x=1)"), "ArticleDetail");
        assert_eq!(view_name("views.article_detail"), "article_detail");
        assert_eq!(view_name("article_detail"), "article_detail");
    }
}
