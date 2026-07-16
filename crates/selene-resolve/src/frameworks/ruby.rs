//! **Rails** (Task 19) — the `controller#action` precise claim, and the RESTful
//! expansion.
//!
//! # Same lesson as Laravel: the reference must carry the controller
//!
//! `articles#index` names exactly one thing. A bare `index` name-matches to
//! whichever `index` the matcher reaches first — and every Rails controller has
//! one. So the reference is `articles#index`, [`Rails::resolve`] looks the
//! controller up **by file path** (`app/controllers/articles_controller.rb`), and
//! a miss returns `None` rather than degrading into a bare-name match. **The
//! no-fallthrough is the point**: a degraded match here is a false edge at high
//! confidence, which the agent will trust.
//!
//! # `claims_reference` — "the gotcha" (playbook §7)
//!
//! `articles#index` names **no declared symbol**, so the pre-filter drops it
//! before `resolve()` runs, and every Rails route binds to nothing while the map
//! still *looks* populated. The TS build shipped exactly that.
//!
//! # One line, seven routes
//!
//! ```ruby
//! resources :articles     # → index, create, new, show, edit, update, destroy
//! ```
//!
//! All seven share a file **and a line**, so they collide on everything the node id
//! hashes except `name` — which is why the name is `{VERB} {path}` (`GET
//! /articles` vs `POST /articles`; `GET /articles/:id` vs `GET /articles/:id/edit`).
//! Name them by path alone and seven routes silently collapse into three.
//!
//! # The naive pluralizer is a compat contract, not a bug
//!
//! [`pluralize`] is `y → ies`, `s|x|z|ch|sh → +es`, else `+s`. It gets `person`
//! wrong. It is **ported as-is**: it feeds the controller file name, which feeds
//! the resolution, and "fixing" it would silently move every id it touches.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::frameworks::{
    FrameworkExtraction, FrameworkResolver, RouteSpec, by_convention, line_of, route_node_in,
};
use crate::strip_comments::strip_comments_for_regex;
use crate::types::{ResolvedBy, ResolvedRef};

const NO_CLOCK: i64 = 0;
const RAILS: &str = "rails";

macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by tests
            Regex::new($pat).unwrap()
        })
    };
}

/// `get '/articles', to: 'articles#index'` — and the fat-arrow legacy form.
static EXPLICIT_ROUTE: LazyLock<Regex> = re!(
    r#"\b(get|post|put|patch|delete|match)\s+['"]([^'"]+)['"]\s*(?:,\s*to:\s*|=>\s*)['"]([\w/]+#\w+)['"]"#
);

/// `resources :articles, only: [:index, :create]` / `resource :profile`.
static RESOURCES: LazyLock<Regex> =
    re!(r"\b(resources|resource)\s+:(\w+)((?:\s*,\s*[\w:]+:\s*\[[^\]]*\])*)");

/// `only: [:index, :create]` / `except: [:destroy]`.
static ONLY: LazyLock<Regex> = re!(r"only:\s*\[([^\]]*)\]");
static EXCEPT: LazyLock<Regex> = re!(r"except:\s*\[([^\]]*)\]");
static SYMBOL: LazyLock<Regex> = re!(r":(\w+)");

/// `articles#index` — the shape [`Rails::claims_reference`] claims.
static CONTROLLER_ACTION: LazyLock<Regex> = re!(r"^[\w/]+#\w+$");

/// A bare model name.
static MODEL_NAME: LazyLock<Regex> = re!(r"^[A-Z][a-zA-Z]+$");

/// The seven RESTful actions, **verbatim**. Order is the emission order.
pub const RESTFUL_ROUTES: &[(&str, &str, &str)] = &[
    ("index", "GET", "/{r}"),
    ("create", "POST", "/{r}"),
    ("new", "GET", "/{r}/new"),
    ("show", "GET", "/{r}/:id"),
    ("edit", "GET", "/{r}/:id/edit"),
    ("update", "PATCH", "/{r}/:id"),
    ("destroy", "DELETE", "/{r}/:id"),
];

/// Rails.
pub struct Rails;

impl FrameworkResolver for Rails {
    fn name(&self) -> &'static str {
        RAILS
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[Language::Ruby])
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        if ctx
            .read_file("Gemfile")
            .is_some_and(|g| g.to_lowercase().contains("rails"))
        {
            return true;
        }
        [
            "config/application.rb",
            "config/routes.rb",
            "app/controllers/application_controller.rb",
        ]
        .iter()
        .any(|f| ctx.file_exists(f) || ctx.read_file(f).is_some())
    }

    /// The escape without which every Rails route is dropped before `resolve()`.
    fn claims_reference(&self, name: &str) -> bool {
        CONTROLLER_ACTION.is_match(name)
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        if language != Language::Ruby {
            return out;
        }
        let src = strip_comments_for_regex(content, Language::Ruby);

        // --- explicit routes ---------------------------------------------------
        for caps in EXPLICIT_ROUTE.captures_iter(&src) {
            let (Some(whole), Some(verb), Some(route_path), Some(handler)) =
                (caps.get(0), caps.get(1), caps.get(2), caps.get(3))
            else {
                continue;
            };
            let line = line_of(&src, whole.start());
            push(
                &mut out,
                path,
                &verb.as_str().to_uppercase(),
                route_path.as_str(),
                handler.as_str(),
                line,
            );
        }

        // --- `resources` / `resource` -----------------------------------------
        for caps in RESOURCES.captures_iter(&src) {
            let (Some(whole), Some(kind), Some(name)) = (caps.get(0), caps.get(1), caps.get(2))
            else {
                continue;
            };
            let filters = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            let singular = kind.as_str() == "resource";

            // A singular `resource :profile` has no index (there is nothing to
            // list) — but its CONTROLLER is still plural. Rails' own convention.
            let resource = name.as_str();
            let controller = if singular {
                pluralize(resource)
            } else {
                resource.to_string()
            };

            let actions = selected_actions(filters, singular);
            let line = line_of(&src, whole.start());

            for (action, verb, template) in RESTFUL_ROUTES {
                if !actions.iter().any(|a| a == action) {
                    continue;
                }
                let route_path = template.replace("{r}", resource);
                push(
                    &mut out,
                    path,
                    verb,
                    &route_path,
                    &format!("{controller}#{action}"),
                    line,
                );
            }
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
        if Language::from_wire(&r.language) != Some(Language::Ruby) {
            return None;
        }
        let name = r.reference_name.as_str();

        // Pattern 0 — the precise claim. NO FALLTHROUGH on a miss.
        if let Some((controller, action)) = name.split_once('#') {
            return resolve_controller_action(r, controller, action, ctx);
        }

        if name.ends_with("Controller") {
            return by_convention(r, ctx, &[NodeKind::Class], &["/controllers/"], 0.85);
        }
        if name.ends_with("Helper") {
            return by_convention(r, ctx, &[NodeKind::Module], &["/helpers/"], 0.8);
        }
        if name.ends_with("Service") || name.ends_with("Job") {
            return by_convention(r, ctx, &[NodeKind::Class], &["/services/", "/jobs/"], 0.8);
        }
        if MODEL_NAME.is_match(name) {
            return by_convention(r, ctx, &[NodeKind::Class], &["/models/"], 0.8);
        }
        None
    }
}

// =============================================================================
// Extract helpers
// =============================================================================

fn push(
    out: &mut FrameworkExtraction,
    file: &str,
    verb: &str,
    route_path: &str,
    handler: &str,
    line: u32,
) {
    let node = route_node_in(
        &RouteSpec::new(RAILS, Some(verb), route_path, file, line),
        Language::Ruby.as_str(),
        NO_CLOCK,
    );
    out.refs.push(UnresolvedRef {
        from_node_id: node.id.clone(),
        reference_name: handler.to_string(),
        reference_kind: "references".to_string(),
        line: Some(line),
        column: Some(0),
        candidates: vec![],
        file_path: file.to_string(),
        language: Language::Ruby.as_str().to_string(),
        status: RefStatus::Pending,
        name_tail: handler.to_string(),
    });
    out.nodes.push(node);
}

/// Which of the seven a `resources` line actually declares.
fn selected_actions(filters: &str, singular: bool) -> Vec<String> {
    let all: Vec<String> = RESTFUL_ROUTES
        .iter()
        .map(|(a, _, _)| a.to_string())
        // A singular resource has no `index`.
        .filter(|a| !(singular && a == "index"))
        .collect();

    if let Some(c) = ONLY.captures(filters) {
        let listed = symbols(c.get(1).map(|m| m.as_str()).unwrap_or(""));
        return all.into_iter().filter(|a| listed.contains(a)).collect();
    }
    if let Some(c) = EXCEPT.captures(filters) {
        let listed = symbols(c.get(1).map(|m| m.as_str()).unwrap_or(""));
        return all.into_iter().filter(|a| !listed.contains(a)).collect();
    }
    all
}

fn symbols(list: &str) -> Vec<String> {
    SYMBOL
        .captures_iter(list)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// **Naive on purpose** — see the module docs. `category → categories`,
/// `box → boxes`, `article → articles`. (`person → persons`. Yes. Ported as-is.)
pub fn pluralize(word: &str) -> String {
    if let Some(stem) = word.strip_suffix('y') {
        return format!("{stem}ies");
    }
    for suffix in ["s", "x", "z", "ch", "sh"] {
        if word.ends_with(suffix) {
            return format!("{word}es");
        }
    }
    format!("{word}s")
}

/// `articles` → `ArticlesController`.
fn camelize_controller(controller: &str) -> String {
    let mut out = String::new();
    for part in controller.split(['_', '/']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out.push_str("Controller");
    out
}

// =============================================================================
// Resolve
// =============================================================================

/// `articles#index` → **that** controller's `index`, or nothing.
fn resolve_controller_action(
    r: &UnresolvedRef,
    controller: &str,
    action: &str,
    ctx: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let by_path = format!("app/controllers/{controller}_controller.rb");

    let file = if ctx.file_exists(&by_path) {
        Some(by_path)
    } else {
        // The camelized class, wherever it lives (an engine, a namespaced module).
        let class = camelize_controller(controller);
        crate::context::owned(ctx.nodes_by_name(&class))
            .into_iter()
            .find(|n| n.kind == NodeKind::Class)
            .map(|n| n.file_path)
    }?;

    let target = crate::context::owned(ctx.nodes_by_name(action))
        .into_iter()
        .find(|n| n.file_path == file && matches!(n.kind, NodeKind::Method | NodeKind::Function))
        .or_else(|| class_in(ctx, &file))?;

    Some(ResolvedRef {
        original: r.clone(), // the STORED ROW, unmutated (#760)
        target_node_id: target.id,
        confidence: 0.85,
        resolved_by: ResolvedBy::Framework,
    })
    // NOTE the absence of a fallback. A `c#a` that finds no such controller
    // resolves to NOTHING — it must never degrade into a bare-name match on `a`,
    // which is the bug this whole design prevents.
}

fn class_in(ctx: &dyn ResolutionContext, file: &str) -> Option<Node> {
    crate::context::owned(ctx.nodes_in_file(file))
        .into_iter()
        .find(|n| n.kind == NodeKind::Class)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_naive_pluralizer_is_ported_as_is() {
        assert_eq!(pluralize("article"), "articles");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("dish"), "dishes");
        assert_eq!(
            pluralize("person"),
            "persons",
            "WRONG English, RIGHT parity — this feeds the controller file name and \
             therefore the ids. Fixing it silently moves every id it touches."
        );
    }

    #[test]
    fn a_singular_resource_omits_index_and_pluralizes_its_controller() {
        let out = Rails.extract("config/routes.rb", "resource :profile\n", Language::Ruby);
        assert!(
            !out.refs
                .iter()
                .any(|r| r.reference_name.ends_with("#index")),
            "there is nothing to list — a singular resource has no index"
        );
        assert!(
            out.refs
                .iter()
                .all(|r| r.reference_name.starts_with("profiles#")),
            "…but the CONTROLLER is plural: `profiles#show`"
        );
        assert_eq!(out.nodes.len(), 6);
    }

    #[test]
    fn only_and_except_select_the_actions() {
        let only = Rails.extract(
            "routes.rb",
            "resources :articles, only: [:index, :create]\n",
            Language::Ruby,
        );
        assert_eq!(only.nodes.len(), 2);

        let except = Rails.extract(
            "routes.rb",
            "resources :articles, except: [:destroy]\n",
            Language::Ruby,
        );
        assert_eq!(except.nodes.len(), 6);
    }

    #[test]
    fn the_claim_is_exact() {
        assert!(Rails.claims_reference("articles#index"));
        assert!(Rails.claims_reference("admin/articles#index"));
        assert!(
            !Rails.claims_reference("index"),
            "a bare action is NOT a claim"
        );
    }
}
