//! **Laravel** (Task 19) — the `Controller@method` precise claim.
//!
//! # The reference must carry the controller. This is the whole task.
//!
//! ```php
//! Route::get('/articles', [ArticleController::class, 'index']);
//! ```
//!
//! Emit that route's handler reference as the bare name `index` and it
//! name-matches to **whichever `index` the matcher happens to find first** — in
//! the TS build, *every* Laravel route resolved to `ArticleController.index`,
//! because every controller has an `index`. The route map looked complete and was
//! systematically wrong.
//!
//! So the reference is `ArticleController@index`: a **precise claim**. It names
//! exactly one target, and [`Laravel::resolve`] returns it at **0.9** — which
//! short-circuits the ladder. That confidence is only earned because the claim is
//! exact; a wrong bind at 0.9 is a false edge the agent will *trust*, which is
//! strictly worse than no edge at all. So the resolver looks the controller up by
//! **file path** (`app/Http/Controllers/{C}.php`), and if it cannot find that
//! controller it returns `None` rather than degrading to a name match.
//!
//! # `claims_reference` is not optional
//!
//! `ArticleController@index` names **no declared symbol anywhere** — no class, no
//! method, nothing. So `resolve_one`'s step-3 pre-filter drops it before
//! `resolve()` is ever called, and every Laravel route silently binds to nothing.
//! The TS build shipped that bug. [`Laravel::claims_reference`] is the escape, and
//! `claims_reference_is_actually_consulted` is the test that proves it is wired.
//!
//! # Facades are external, and saying so is the point
//!
//! `Auth::user()`, `DB::table(…)` — these resolve to the *framework*, not to
//! project code. [`FACADE_MAPPINGS`] exists so the resolver can **recognize and
//! skip** them: a facade reference that fell through to the name matcher would
//! bind to whatever local class happened to be called `Cache`. (Phase 8's
//! `laravel-event` synthesizer consumes the same table.)

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::frameworks::{
    FrameworkExtraction, FrameworkResolver, RouteSpec, line_of, match_delim, route_node, split_args,
};
use crate::strip_comments::strip_comments_for_regex;
use crate::types::{ResolvedBy, ResolvedRef};

const NO_CLOCK: i64 = 0;
const LARAVEL: &str = "laravel";

macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by tests
            Regex::new($pat).unwrap()
        })
    };
}

/// `Route::get(` … — the verb router.
static ROUTE_VERB: LazyLock<Regex> = re!(r"Route::(get|post|put|patch|delete|options|any)\s*\(");

/// `Route::resource(` / `Route::apiResource(`.
static ROUTE_RESOURCE: LazyLock<Regex> = re!(r"Route::(?:api)?[Rr]esource\s*\(");

/// `[ArticleController::class, 'index']` — the modern, unambiguous handler form.
static TUPLE_HANDLER: LazyLock<Regex> =
    re!(r#"^\s*\[\s*([\w\\]+)::class\s*,\s*['"](\w+)['"]\s*\]\s*$"#);

/// `'ArticleController@index'` — the legacy string form, possibly namespaced.
static STRING_HANDLER: LazyLock<Regex> = re!(r#"^\s*['"]([\w\\]+@\w+)['"]\s*$"#);

/// `ArticleController::class` on its own — an invokable controller.
static CLASS_HANDLER: LazyLock<Regex> = re!(r"^\s*([\w\\]+)::class\s*$");

/// The first quoted argument — a route path, or a resource name.
static QUOTED: LazyLock<Regex> = re!(r#"^\s*['"]([^'"]*)['"]\s*$"#);

/// `ArticleController@index` — the shape [`Laravel::claims_reference`] claims.
static CONTROLLER_ACTION: LazyLock<Regex> = re!(r"^[A-Za-z_][A-Za-z0-9_]*Controller@\w+$");

/// `Model::method`.
static MODEL_STATIC: LazyLock<Regex> = re!(r"^([A-Z][A-Za-z0-9_]*)::(\w+)$");

/// The framework's facades. **Data, not behavior**: v0 uses this only to recognize
/// a facade and refuse it, so the reference does not fall through to the name
/// matcher and bind to a same-named project class. (Phase 8's `laravel-event`
/// synthesizer reads the same table to bridge `Event::dispatch`.)
pub const FACADE_MAPPINGS: &[(&str, &str)] = &[
    ("Auth", r"Illuminate\Auth\AuthManager"),
    ("Cache", r"Illuminate\Cache\CacheManager"),
    ("Config", r"Illuminate\Config\Repository"),
    ("DB", r"Illuminate\Database\DatabaseManager"),
    ("Event", r"Illuminate\Events\Dispatcher"),
    ("File", r"Illuminate\Filesystem\Filesystem"),
    ("Gate", r"Illuminate\Auth\Access\Gate"),
    ("Hash", r"Illuminate\Hashing\HashManager"),
    ("Log", r"Illuminate\Log\LogManager"),
    ("Mail", r"Illuminate\Mail\Mailer"),
    ("Queue", r"Illuminate\Queue\QueueManager"),
    ("Redis", r"Illuminate\Redis\RedisManager"),
    ("Request", r"Illuminate\Http\Request"),
    ("Response", r"Illuminate\Routing\ResponseFactory"),
    ("Route", r"Illuminate\Routing\Router"),
    ("Session", r"Illuminate\Session\SessionManager"),
    ("Storage", r"Illuminate\Filesystem\FilesystemManager"),
    ("URL", r"Illuminate\Routing\UrlGenerator"),
    ("Validator", r"Illuminate\Validation\Factory"),
    ("View", r"Illuminate\View\Factory"),
];

/// Laravel.
pub struct Laravel;

impl FrameworkResolver for Laravel {
    fn name(&self) -> &'static str {
        LARAVEL
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[Language::Php])
    }

    /// `artisan` has no extension and is therefore never an *indexed* file — so it
    /// is read off the working tree, not looked up in the index.
    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        ctx.read_file("artisan").is_some() || ctx.read_file("app/Http/Kernel.php").is_some()
    }

    /// See the module docs: without this, every Laravel route is dropped by the
    /// pre-filter before this resolver is asked.
    fn claims_reference(&self, name: &str) -> bool {
        CONTROLLER_ACTION.is_match(name)
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        if language != Language::Php {
            return out;
        }
        let src = strip_comments_for_regex(content, Language::Php);

        for m in ROUTE_VERB.captures_iter(&src) {
            let (Some(whole), Some(verb)) = (m.get(0), m.get(1)) else {
                continue;
            };
            let Some(args) = call_args(&src, whole.end() - 1) else {
                continue;
            };
            let parts = split_args(args);
            let (Some(route_path), Some(handler)) = (parts.first(), parts.get(1)) else {
                continue;
            };
            let Some(route_path) = QUOTED.captures(route_path).and_then(|c| c.get(1)) else {
                continue;
            };

            let line = line_of(&src, whole.start());
            let node = route_node(
                &RouteSpec::new(
                    LARAVEL,
                    Some(&verb.as_str().to_uppercase()),
                    route_path.as_str(),
                    path,
                    line,
                ),
                Language::Php,
                NO_CLOCK,
            );

            // A closure handler yields NO reference. Silent beats wrong: there is
            // no symbol to name, and inventing one is a false edge.
            if let Some(name) = handler_name(handler) {
                out.refs
                    .push(php_ref(&node.id, &name, "references", path, line));
            }
            out.nodes.push(node);
        }

        // `Route::resource('articles', ArticleController::class)` — seven actions
        // behind one registration. v0 emits the registration itself (the agent's
        // entry point) and points it at the controller.
        for m in ROUTE_RESOURCE.captures_iter(&src) {
            let Some(whole) = m.get(0) else { continue };
            let Some(args) = call_args(&src, whole.end() - 1) else {
                continue;
            };
            let parts = split_args(args);
            let (Some(name), Some(ctrl)) = (parts.first(), parts.get(1)) else {
                continue;
            };
            let Some(name) = QUOTED.captures(name).and_then(|c| c.get(1)) else {
                continue;
            };
            let Some(ctrl) = CLASS_HANDLER.captures(ctrl).and_then(|c| c.get(1)) else {
                continue;
            };

            let line = line_of(&src, whole.start());
            let mut spec = RouteSpec::new(LARAVEL, Some("RESOURCE"), name.as_str(), path, line);
            let display = format!("resource:{}", name.as_str());
            spec.name_override = Some(&display);
            let node = route_node(&spec, Language::Php, NO_CLOCK);

            // `imports`, not `references` — the registration pulls in a whole
            // controller, it does not name one action.
            out.refs.push(php_ref(
                &node.id,
                last_segment(ctrl.as_str()),
                "imports",
                path,
                line,
            ));
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
        if r.language != Language::Php {
            return None;
        }
        let name = r.reference_name.as_str();

        // A facade is the framework, not this project. Recognize and REFUSE — a
        // fallthrough would bind `Cache::get` to a local class named `Cache`.
        if let Some((facade, _)) = name.split_once("::")
            && FACADE_MAPPINGS.iter().any(|(f, _)| *f == facade)
        {
            return None;
        }

        if let Some((class, action)) = name.split_once('@') {
            return resolve_controller_action(r, class, action, ctx);
        }
        if let Some(caps) = MODEL_STATIC.captures(name) {
            let (Some(class), Some(method)) = (caps.get(1), caps.get(2)) else {
                return None;
            };
            return resolve_model(r, class.as_str(), method.as_str(), ctx);
        }
        None
    }
}

// =============================================================================
// Extract helpers
// =============================================================================

/// The arguments of the call whose `(` is at `open`.
fn call_args(src: &str, open: usize) -> Option<&str> {
    let span = match_delim(src, open)?;
    Some(&src[span])
}

/// The handler expression → the reference name it should carry.
///
/// **`Class@method`, never a bare `method`.** See the module docs: a bare action
/// name binds to whichever controller the matcher reaches first.
fn handler_name(expr: &str) -> Option<String> {
    if let Some(c) = TUPLE_HANDLER.captures(expr) {
        let (class, action) = (c.get(1)?, c.get(2)?);
        return Some(format!(
            "{}@{}",
            last_segment(class.as_str()),
            action.as_str()
        ));
    }
    if let Some(c) = STRING_HANDLER.captures(expr) {
        // `App\Http\Controllers\ArticleController@index` → drop the namespace.
        return Some(last_segment(c.get(1)?.as_str()).to_string());
    }
    if let Some(c) = CLASS_HANDLER.captures(expr) {
        return Some(last_segment(c.get(1)?.as_str()).to_string());
    }
    // A closure (`function (Request $r) { … }` / `fn () => …`) names nothing.
    None
}

/// `App\Http\Controllers\ArticleController` → `ArticleController`.
fn last_segment(expr: &str) -> &str {
    expr.rsplit('\\').next().unwrap_or(expr)
}

fn php_ref(from: &str, name: &str, kind: &str, file: &str, line: u32) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind.to_string(),
        line: Some(line),
        column: Some(0),
        candidates: vec![],
        file_path: file.to_string(),
        language: Language::Php,
        status: RefStatus::Pending,
        name_tail: name.to_string(),
    }
}

// =============================================================================
// Resolve
// =============================================================================

/// `ArticleController@index` → **that** controller's `index`.
///
/// The lookup is by **file path** first (`app/Http/Controllers/{C}.php`), then by
/// any class named `C` under a `Controllers` directory. A miss returns `None`: at
/// 0.9 this claim short-circuits the ladder, so a wrong bind here is a false edge
/// the agent will trust. Refusing is the only safe failure.
fn resolve_controller_action(
    r: &UnresolvedRef,
    class: &str,
    action: &str,
    ctx: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let by_path = format!("app/Http/Controllers/{class}.php");

    let file = if ctx.file_exists(&by_path) {
        Some(by_path)
    } else {
        // A controller in a subdirectory (`Api/V1/ArticleController.php`) or a
        // non-standard layout — but it must still BE a controller.
        crate::context::owned(ctx.nodes_by_name(class))
            .into_iter()
            .find(|n| n.kind == NodeKind::Class && n.file_path.contains("Controllers"))
            .map(|n| n.file_path)
    }?;

    // The action itself is the target — that is the hop the agent needs. If the
    // class exists but the action does not, the class is the honest answer.
    let target = method_in_file(ctx, &file, action).or_else(|| class_in_file(ctx, &file, class))?;

    Some(ResolvedRef {
        original: r.clone(), // the STORED ROW, unmutated (#760)
        target_node_id: target.id,
        confidence: 0.9,
        resolved_by: ResolvedBy::Framework,
    })
}

/// `Article::find` → the model's method, else the model class. **0.85.**
fn resolve_model(
    r: &UnresolvedRef,
    class: &str,
    method: &str,
    ctx: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let file = [
        format!("app/Models/{class}.php"),
        format!("app/{class}.php"),
    ]
    .into_iter()
    .find(|f| ctx.file_exists(f))?;

    let target = method_in_file(ctx, &file, method).or_else(|| class_in_file(ctx, &file, class))?;

    Some(ResolvedRef {
        original: r.clone(),
        target_node_id: target.id,
        confidence: 0.85,
        resolved_by: ResolvedBy::Framework,
    })
}

fn method_in_file(ctx: &dyn ResolutionContext, file: &str, name: &str) -> Option<Node> {
    crate::context::owned(ctx.nodes_by_name(name))
        .into_iter()
        .find(|n| n.file_path == file && matches!(n.kind, NodeKind::Method | NodeKind::Function))
}

fn class_in_file(ctx: &dyn ResolutionContext, file: &str, name: &str) -> Option<Node> {
    crate::context::owned(ctx.nodes_by_name(name))
        .into_iter()
        .find(|n| n.file_path == file && n.kind == NodeKind::Class)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_handler_form_carries_the_controller() {
        assert_eq!(
            handler_name("[ArticleController::class, 'index']").as_deref(),
            Some("ArticleController@index")
        );
        assert_eq!(
            handler_name("'ArticleController@index'").as_deref(),
            Some("ArticleController@index")
        );
        assert_eq!(
            handler_name(r"'App\Http\Controllers\ArticleController@index'").as_deref(),
            Some("ArticleController@index"),
            "the namespace is dropped; the CONTROLLER is not"
        );
        assert_eq!(
            handler_name("InvokableController::class").as_deref(),
            Some("InvokableController")
        );
    }

    #[test]
    fn a_closure_handler_names_nothing() {
        assert_eq!(handler_name("function (Request $r) { return 1; }"), None);
        assert_eq!(handler_name("fn () => 1"), None);
    }

    #[test]
    fn the_claim_is_exact() {
        assert!(Laravel.claims_reference("ArticleController@index"));
        assert!(
            !Laravel.claims_reference("index"),
            "a bare action is NOT a claim"
        );
        assert!(
            !Laravel.claims_reference("ArticleService@index"),
            "not a controller"
        );
    }
}
