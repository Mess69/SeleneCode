//! The route-node contract (Task 11).
//!
//! # Ids stay opaque; semantics become queryable
//!
//! CodeGraph TS encoded a route's semantics **into its id string**
//! (`route:{file}:{line}:{METHOD}:{path}`) and key-matched on that string
//! downstream. We do **not**. A route node's id is the ordinary hashed
//! [`selene_core::node_id`] — `"<kind>:" + sha256("{file}:{kind}:{name}:{line}")[..32]`
//! — with **no exception** (the only id exception in the system remains the
//! literal `file:<path>`). The semantics live in three indexed fields on the
//! node (`route_method`, `route_path`, `framework`), and every lookup is an
//! indexed query: [`find_route`] / `GraphStore::find_route`.
//!
//! Maintainer decision, 2026-07-13. It is what the locked SurrealQL-max
//! decision asks for: push the matching into the database, keep ids opaque.
//!
//! # The `name` spelling is load-bearing — do not "tidy" it
//!
//! The id hash input is `(file, kind, name, start_line)`. It does **not**
//! include the route fields. Several frameworks emit **more than one route from
//! a single source line**:
//!
//! - axum: `.route("/x", get(list).post(create))` → `GET /x` + `POST /x`
//! - rails: `resources :articles` → seven actions, one line
//! - flask: two stacked `@app.route(...)` decorators on one handler
//!
//! Those routes collide on `(file, kind, line)` and are separated **only by
//! `name`**. That is why the default name is `"{METHOD} {path}"` and not just
//! the path: the verb in the name is what keeps the ids distinct. A framework
//! author who names a route by its path alone will silently collapse N routes
//! into one — `route_node_ids_are_distinct_for_two_routes_on_one_line` in
//! `tests/fw_registry_test.rs` is the guard.

use selene_core::{Language, Node, NodeKind, node_id};
use selene_db::GraphStore;

use crate::Result;

/// What a framework extractor knows about a route it just found.
///
/// `file`/`line` are **not** duplicated into the route fields — they are already
/// `Node::file_path` / `Node::start_line`. Only `method`, `path` and `framework`
/// are genuinely new information.
pub struct RouteSpec<'a> {
    /// The emitting resolver's `name()` — `"express"`, `"django"`, …
    pub framework: &'a str,
    /// The HTTP verb, **uppercased** (`"GET"`), or `"ANY"` for a verb-less
    /// registration. `None` for a path-only router (django `path()`, React
    /// Router) — such a route's `name` is the bare path.
    pub method: Option<&'a str>,
    /// The path/prefix exactly as written in the source.
    pub path: &'a str,
    /// Project-relative file path.
    pub file: &'a str,
    /// 1-based line of the registration site.
    pub line: u32,
    /// Override the derived `name`. Only for the frameworks whose agent-visible
    /// spelling is not `"{METHOD} {path}"` — laravel's `resource:{name}`. Leave
    /// `None` otherwise; the derivation below is the contract.
    pub name_override: Option<&'a str>,
    /// Override the derived `qualified_name`. Same rule as `name_override`.
    pub qualified_name_override: Option<&'a str>,
}

impl<'a> RouteSpec<'a> {
    /// A route with the standard name/qualified-name derivation.
    pub fn new(
        framework: &'a str,
        method: Option<&'a str>,
        path: &'a str,
        file: &'a str,
        line: u32,
    ) -> Self {
        Self {
            framework,
            method,
            path,
            file,
            line,
            name_override: None,
            qualified_name_override: None,
        }
    }

    /// The agent-visible name. **Kept byte-identical to the TS build** — it is
    /// surfaced verbatim by explore, so it is a wire string, not an internal
    /// label.
    ///
    /// - verb router → `"{METHOD} {path}"` (one space; `"GET /users"`,
    ///   `"VIEWSET /articles"`, `"ANY /x"`)
    /// - path-only router → the raw path (`"/article/:slug"`)
    pub fn derived_name(&self) -> String {
        match self.name_override {
            Some(n) => n.to_string(),
            None => match self.method {
                Some(m) => format!("{m} {}", self.path),
                None => self.path.to_string(),
            },
        }
    }

    /// `"{file}::{METHOD}:{path}"`, or `"{file}::route:{path}"` for a path-only
    /// router. (A qualified name may carry a file path here — routes are the
    /// documented exception to the no-paths-in-qualified-names rule, because a
    /// route's identity *is* its file plus its path.)
    pub fn derived_qualified_name(&self) -> String {
        match self.qualified_name_override {
            Some(q) => q.to_string(),
            None => match self.method {
                Some(m) => format!("{}::{m}:{}", self.file, self.path),
                None => format!("{}::route:{}", self.file, self.path),
            },
        }
    }
}

/// Build a [`NodeKind::Route`] node from a [`RouteSpec`].
///
/// The id is the ordinary hashed node id over `(file, kind, name, line)` — see
/// the module docs for why `name` carries the verb.
pub fn route_node(spec: &RouteSpec<'_>, language: Language, updated_at: i64) -> Node {
    let name = spec.derived_name();
    let id = node_id(spec.file, NodeKind::Route, &name, spec.line);
    Node {
        id,
        kind: NodeKind::Route,
        name,
        qualified_name: spec.derived_qualified_name(),
        file_path: spec.file.to_string(),
        // A route belongs to a framework, not to a grammar, so its `language`
        // is simply the language of the file it was found in — which the
        // caller (a framework's `extract`) always knows. The enum made the old
        // empty-string sentinel + stamp pass unrepresentable, so the language
        // is now threaded explicitly instead of patched in afterwards.
        language,
        start_line: spec.line,
        end_line: spec.line,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: None,
        is_async: None,
        is_static: None,
        is_abstract: None,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        route_method: spec.method.map(str::to_string),
        route_path: Some(spec.path.to_string()),
        framework: Some(spec.framework.to_string()),
        updated_at,
    }
}

/// Look a route up by its **semantics** — the one supported way.
///
/// Never parse a route id, and never build one by hand: the id is a hash and
/// carries no method or path. `framework`/`method` are optional filters.
pub async fn find_route<S: GraphStore>(
    store: &S,
    framework: Option<&str>,
    method: Option<&str>,
    path: &str,
) -> Result<Vec<Node>> {
    Ok(store.find_route(framework, method, path).await?)
}
