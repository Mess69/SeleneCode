//! **Axum / Actix / Rocket** (Task 18) — the Rust router family.
//!
//! # The chained verb is the whole point
//!
//! ```rust,ignore
//! Router::new().route("/articles", get(list_articles).post(create_article))
//! ```
//!
//! That is **two routes on one line**. The TS build emitted only the first
//! verb+handler, so `post(create_article)` was invisible — realworld-axum showed
//! 12 routes where 19 existed, and `POST /articles` simply did not appear on the
//! map. An agent asking "where is an article created?" got nothing and went back
//! to reading files.
//!
//! So the arm below walks **every** chained verb in the `.route(...)` arguments
//! and emits one route node per verb. The two nodes share a file **and a line**,
//! so they collide on everything the id hashes except `name` — which is exactly
//! why `RouteSpec`'s name carries the verb (`GET /articles` vs `POST /articles`).
//! `two_verbs_on_one_line_are_two_routes_with_distinct_ids` is the guard.
//!
//! # A module reference must not lose to a coincidence
//!
//! `use my_crate::thing` names a **sibling crate in the workspace**, whose
//! directory the reference never mentions. Resolved through
//! [`cargo_workspace_crate_map`], it lands on that crate's `lib.rs` at **0.95** —
//! deliberately above the name matcher's 0.7 self-file score, because a local
//! same-named symbol must not outrank the crate the code actually meant.
//!
//! # ⚠ Two hops v0 does not close — stated, not half-drawn
//!
//! - **Actix `web::scope("/api")` prefixes are not prepended.** Like Gin's route
//!   groups: the prefix lives at the scope's declaration, arbitrarily far from the
//!   registration, and joining them needs dataflow this pass does not have. The
//!   route's file+line still land the agent on the registration site.
//! - **Anonymous closure handlers** (`.to(|| async {})`) get a route and **no
//!   reference** — a closure has no name to bind to, and inventing one would be a
//!   wrong edge dressed as an answer.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, NodeKind, RefStatus, UnresolvedRef, file_node_id};

use crate::context::ResolutionContext;
use crate::frameworks::cargo::cargo_workspace_crate_map;
use crate::frameworks::{
    FrameworkExtraction, FrameworkResolver, RouteSpec, by_convention, line_of, match_delim,
    route_node_in,
};
use crate::strip_comments::strip_comments_for_regex;
use crate::types::{ResolvedBy, ResolvedRef};

/// Route emission is byte-deterministic — no wall clock. (See `python.rs`.)
const NO_CLOCK: i64 = 0;

const RUST: &str = "rust";

/// How far past a `web::resource("p")` the verb chain may run. Ported verbatim.
const ACTIX_CHAIN_WINDOW: usize = 500;

macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by tests
            Regex::new($pat).unwrap()
        })
    };
}

/// Actix/Rocket attribute routes: `#[get("/articles")]`.
static ATTR_ROUTE: LazyLock<Regex> =
    re!(r#"#\[\s*(get|post|put|patch|delete|head|options)\s*\(\s*["']([^"']+)["'][^\]]*\]"#);

/// The `fn` an attribute decorates — the next one, so `#[get(...)]` stacked with
/// `#[instrument]` still finds it.
static NEXT_FN: LazyLock<Regex> = re!(r"\bfn\s+(\w+)");

/// `.route(` — the opening of both an Axum and an app-level Actix registration.
static ROUTE_CALL: LazyLock<Regex> = re!(r"\.route\s*\(");

/// The first argument of a `.route(...)`: the path.
static LEADING_PATH: LazyLock<Regex> = re!(r#"^\s*"([^"]+)"\s*,"#);

/// **Every** chained Axum verb: `get(list).post(create)` → two.
///
/// The leading `(?:^|[^:\w])` is what keeps Actix's `web::get()` out of this arm —
/// a verb preceded by `::` belongs to [`ACTIX_VERB`], and matching it here would
/// bind the route to nothing. (The regex crate has no lookbehind, so the guard
/// character is consumed; group 1 is the verb.)
static AXUM_VERB_OPEN: LazyLock<Regex> =
    re!(r"(?:^|[^:\w])(get|post|put|patch|delete|head|options|trace)\s*\(");

/// A verb's argument when it names a handler — as opposed to a closure.
static NAMED_HANDLER: LazyLock<Regex> = re!(r"^\s*([A-Za-z_][\w:]*)\s*$");

/// Actix's spelling of the same thing: `web::get().to(list)`.
static ACTIX_VERB: LazyLock<Regex> =
    re!(r"web::(get|post|put|patch|delete|head|options)\s*\(\s*\)\s*\.to\s*\(\s*([\w:]+)");

/// A verb-less Actix handler: `.to(handler)` ⇒ `ANY`.
static ACTIX_BARE_TO: LazyLock<Regex> = re!(r"\.to\s*\(\s*([\w:]+)\s*\)");

/// `web::resource("/articles")` — the builder form.
static ACTIX_RESOURCE: LazyLock<Regex> = re!(r#"web::resource\s*\(\s*"([^"]+)"\s*\)"#);

/// A bare lowercase identifier — a module (or a workspace crate).
static MODULE_NAME: LazyLock<Regex> = re!(r"^[a-z_][a-z0-9_]*$");

/// Axum / Actix / Rocket.
pub struct RustResolver;

impl FrameworkResolver for RustResolver {
    fn name(&self) -> &'static str {
        RUST
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[Language::Rust])
    }

    /// A `Cargo.toml`. Read from the **working tree**, not the index: `.toml` has
    /// no grammar, so it is not an indexed file and `file_exists` would say no.
    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        ctx.read_file("Cargo.toml").is_some()
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        if language != Language::Rust {
            return out;
        }
        // Comment-stripped, byte-offset preserving: a commented-out route is not a
        // route, and the line a route id hashes from must not shift.
        let src = strip_comments_for_regex(content, Language::Rust);

        extract_attribute_routes(&src, path, &mut out);
        extract_route_calls(&src, path, &mut out);
        extract_actix_resources(&src, path, &mut out);

        // Emission order is pass order, not source order — sort, because the node
        // order is observable (id ties, parity diffs).
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
        if Language::from_wire(&r.language) != Some(Language::Rust) {
            return None;
        }
        let name = r.reference_name.as_str();

        if name.ends_with("_handler") || name.starts_with("handle_") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Function],
                &["/handlers/", "/handler/", "/routes/", "/api/"],
                0.8,
            );
        }
        if name.ends_with("Service") || name.ends_with("Repository") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Struct, NodeKind::Trait],
                &["/service/", "/services/", "/repository/"],
                0.8,
            );
        }
        if MODULE_NAME.is_match(name) {
            return resolve_module(r, name, ctx);
        }
        if is_pascal_case(name) {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Struct],
                &["/models/", "/model/", "/entity/", "/domain/"],
                0.7,
            );
        }
        None
    }
}

// =============================================================================
// Extract
// =============================================================================

fn extract_attribute_routes(src: &str, path: &str, out: &mut FrameworkExtraction) {
    for caps in ATTR_ROUTE.captures_iter(src) {
        let (Some(whole), Some(verb), Some(route_path)) = (caps.get(0), caps.get(1), caps.get(2))
        else {
            continue;
        };
        let line = line_of(src, whole.start());
        let node = new_route(
            &verb.as_str().to_uppercase(),
            route_path.as_str(),
            path,
            line,
        );

        // The next `fn` — which skips any further attributes stacked between.
        if let Some(handler) = NEXT_FN
            .captures_at(src, whole.end())
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
        {
            out.refs.push(handler_ref(&node.id, &handler, path, line));
        }
        out.nodes.push(node);
    }
}

/// `.route("/articles", get(list).post(create))` — Axum — and the app-level Actix
/// `.route("/x", web::get().to(h))`, which is the same call with a different
/// spelling of the verb.
fn extract_route_calls(src: &str, path: &str, out: &mut FrameworkExtraction) {
    for m in ROUTE_CALL.find_iter(src) {
        // The `(` is the last byte of the match. Balanced-paren args — a regex
        // cannot span `get(list).post(create)` correctly. `match_delim` returns the
        // range of the args THEMSELVES (parens excluded).
        let open = m.end() - 1;
        let Some(span) = match_delim(src, open) else {
            continue;
        };
        let args = &src[span];
        let Some(route_path) = LEADING_PATH
            .captures(args)
            .and_then(|c| c.get(1))
            .map(|x| x.as_str().to_string())
        else {
            continue;
        };
        let line = line_of(src, m.start());

        let mut emitted = 0usize;

        // Axum: EVERY chained verb. This is the arm the task exists for.
        //
        // The verb's own argument is taken with balanced parens too, because it may
        // be a CLOSURE (`get(|| async { … })`) — which is a real route with no
        // nameable handler. A regex that demanded an identifier would drop the
        // whole route; one that grabbed whatever came next would bind it to
        // `async`. Both are worse than a route with no ref.
        for caps in AXUM_VERB_OPEN.captures_iter(args) {
            let (Some(whole), Some(verb)) = (caps.get(0), caps.get(1)) else {
                continue;
            };
            let verb_open = whole.end() - 1;
            let Some(inner) = match_delim(args, verb_open) else {
                continue;
            };
            let handler = NAMED_HANDLER
                .captures(&args[inner])
                .and_then(|c| c.get(1))
                .map(|h| tail_segment(h.as_str()));

            push_route(
                out,
                path,
                &verb.as_str().to_uppercase(),
                &route_path,
                handler,
                line,
            );
            emitted += 1;
        }
        if emitted > 0 {
            continue;
        }

        // Actix, app-level: `.route("/x", web::get().to(h))`.
        for caps in ACTIX_VERB.captures_iter(args) {
            let (Some(verb), Some(handler)) = (caps.get(1), caps.get(2)) else {
                continue;
            };
            push_route(
                out,
                path,
                &verb.as_str().to_uppercase(),
                &route_path,
                Some(tail_segment(handler.as_str())),
                line,
            );
            emitted += 1;
        }
        if emitted == 0 {
            // A verb-less `.to(h)` ⇒ ANY.
            if let Some(handler) = ACTIX_BARE_TO
                .captures(args)
                .and_then(|c| c.get(1))
                .map(|m| tail_segment(m.as_str()))
            {
                push_route(out, path, "ANY", &route_path, Some(handler), line);
            }
        }
    }
}

/// The Actix builder: `web::resource("/x").route(web::get().to(list))…`, whose
/// verbs trail the resource over the next [`ACTIX_CHAIN_WINDOW`] bytes — bounded
/// there, and at the next `web::resource`, so one resource cannot swallow the
/// next one's handlers.
fn extract_actix_resources(src: &str, path: &str, out: &mut FrameworkExtraction) {
    let starts: Vec<(usize, usize, String)> = ACTIX_RESOURCE
        .captures_iter(src)
        .filter_map(|c| {
            let whole = c.get(0)?;
            let p = c.get(1)?;
            Some((whole.start(), whole.end(), p.as_str().to_string()))
        })
        .collect();

    for (i, (start, end, route_path)) in starts.iter().enumerate() {
        let hard_cap = (end + ACTIX_CHAIN_WINDOW).min(src.len());
        let next_resource = starts.get(i + 1).map(|(s, _, _)| *s).unwrap_or(src.len());
        let stop = crate::frameworks::char_boundary_at_or_below(src, hard_cap.min(next_resource));
        if stop <= *end {
            continue;
        }
        let chain = &src[*end..stop];
        let line = line_of(src, *start);

        let mut emitted = 0usize;
        for caps in ACTIX_VERB.captures_iter(chain) {
            let (Some(verb), Some(handler)) = (caps.get(1), caps.get(2)) else {
                continue;
            };
            push_route(
                out,
                path,
                &verb.as_str().to_uppercase(),
                route_path,
                Some(tail_segment(handler.as_str())),
                line,
            );
            emitted += 1;
        }
        if emitted == 0
            && let Some(handler) = ACTIX_BARE_TO
                .captures(chain)
                .and_then(|c| c.get(1))
                .map(|m| tail_segment(m.as_str()))
        {
            push_route(out, path, "ANY", route_path, Some(handler), line);
        }
    }
}

/// A route, and the reference to its handler **if it has a nameable one**. A
/// closure handler yields `None` — the route is real, the reference would be a
/// guess (see the module docs).
fn push_route(
    out: &mut FrameworkExtraction,
    file: &str,
    method: &str,
    route_path: &str,
    handler: Option<String>,
    line: u32,
) {
    let node = new_route(method, route_path, file, line);
    if let Some(h) = handler {
        out.refs.push(handler_ref(&node.id, &h, file, line));
    }
    out.nodes.push(node);
}

fn new_route(method: &str, route_path: &str, file: &str, line: u32) -> selene_core::Node {
    route_node_in(
        &RouteSpec::new(RUST, Some(method), route_path, file, line),
        Language::Rust.as_str(),
        NO_CLOCK,
    )
}

/// `api::v1::list` → `list`. The handler is named by its path; only the last
/// segment is a symbol.
fn tail_segment(expr: &str) -> String {
    expr.rsplit("::").next().unwrap_or(expr).trim().to_string()
}

fn handler_ref(route_id: &str, handler: &str, file: &str, line: u32) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: route_id.to_string(),
        reference_name: handler.to_string(),
        reference_kind: "references".to_string(),
        line: Some(line),
        column: Some(0),
        candidates: vec![],
        file_path: file.to_string(),
        language: Language::Rust.as_str().to_string(),
        status: RefStatus::Pending,
        name_tail: handler.to_string(),
    }
}

// =============================================================================
// Resolve — the module arm
// =============================================================================

/// A bare `mod`-shaped name: a **local module** (0.6), else a **workspace crate**
/// (0.95).
///
/// The order is the TS build's and it is first-hit-wins: a local `src/db.rs`
/// shadows a workspace crate named `db`, which is also what `rustc` would do.
///
/// The confidences are the load-bearing part. 0.6 for a local module leaves it
/// *below* the name matcher (a local module reference is a weak claim, and a
/// same-named function in the file is often the better answer). 0.95 for a
/// workspace crate puts it **above** everything: `use my_crate::x` means that
/// crate, and a coincidentally same-named local symbol must not win.
fn resolve_module(
    r: &UnresolvedRef,
    name: &str,
    ctx: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    for candidate in [format!("src/{name}.rs"), format!("src/{name}/mod.rs")] {
        if ctx.file_exists(&candidate) {
            return Some(file_target(r, &candidate, 0.6));
        }
    }

    let map = cargo_workspace_crate_map(ctx);
    let dir = map.get(name)?;
    for candidate in [format!("{dir}/src/lib.rs"), format!("{dir}/src/main.rs")] {
        if ctx.file_exists(&candidate) {
            return Some(file_target(r, &candidate, 0.95));
        }
    }
    None
}

/// A reference that resolves to a **file** node — the one id in the system that is
/// not a hash (`file:<path>`).
fn file_target(r: &UnresolvedRef, path: &str, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: r.clone(), // the STORED ROW, unmutated (#760)
        target_node_id: file_node_id(path),
        confidence,
        resolved_by: ResolvedBy::Framework,
    }
}

/// `Article` — yes. `list_articles`, `HTTP` — no.
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
    fn every_chained_verb_is_a_route() {
        let src = r#"Router::new().route("/articles", get(list).post(create))"#;
        let out = RustResolver.extract("main.rs", src, Language::Rust);
        let names: Vec<&str> = out.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["GET /articles", "POST /articles"],
            "the TS build emitted only the FIRST verb, so `post(create)` was \
             invisible (realworld-axum: 12 routes where 19 existed)"
        );
        assert_ne!(
            out.nodes[0].id, out.nodes[1].id,
            "same file, same LINE — only `name` separates them, which is why the \
             verb lives in the name"
        );
    }

    #[test]
    fn a_namespaced_handler_binds_to_its_last_segment() {
        assert_eq!(tail_segment("api::v1::list"), "list");
        assert_eq!(tail_segment("list"), "list");
    }

    #[test]
    fn attribute_routes_find_the_next_fn() {
        let src =
            "#[get(\"/health\")]\n#[instrument]\nasync fn health() -> &'static str { \"ok\" }";
        let out = RustResolver.extract("main.rs", src, Language::Rust);
        assert_eq!(out.nodes[0].name, "GET /health");
        assert_eq!(
            out.refs[0].reference_name, "health",
            "a stacked #[instrument] must not become the handler"
        );
    }

    #[test]
    fn a_commented_out_route_is_not_a_route() {
        let src = "// .route(\"/dead\", get(dead))\n.route(\"/live\", get(live))";
        let out = RustResolver.extract("main.rs", src, Language::Rust);
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].name, "GET /live");
    }
}
