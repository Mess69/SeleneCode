//! **Gin / gorilla-mux / chi** (Task 17) — the Go router family.
//!
//! # The load-bearing detail: ANY receiver
//!
//! ```go
//! r := gin.Default()
//! v1 := r.Group("/api/v1")          // ← routes are registered on the GROUP
//! v1.POST("/articles", handlers.CreateArticle)
//! PublicGroup.GET("/health", handlers.Health)
//! ```
//!
//! The receiver is **any identifier**, not `r` / `router`. The TS build matched a
//! fixed receiver set and therefore missed *every group-routed app* — gin-vue-admin
//! went from **4 routes to 259** when this was generalized. Grouping is idiomatic
//! Gin, so a fixed receiver list finds the toy apps and misses the real ones.
//!
//! One regex covers Gin, **gorilla/mux** (`s.HandleFunc(…)` on a subrouter) and
//! chi/net-http, because they all spell registration the same way. `Handle` /
//! `HandleFunc` carry no verb ⇒ `ANY`; mux's trailing `.Methods("GET")` chain is a
//! label we do not read.
//!
//! # The handler is the LAST argument — a deliberate improvement on the TS
//!
//! ```go
//! r.GET("/x", auth.Required(), handlers.List)   // middleware, THEN the handler
//! ```
//!
//! The TS regex captured `([^)]+)\)`, which on this line stops at the first `)`
//! and yields `auth.Required(` — so the route bound to the **middleware** and the
//! handler was never reached. That is precisely the half-bridged flow PRD §8.2
//! forbids: the agent follows the route, lands on `Required`, and reads the file
//! to find the real handler. We scan the argument list with balanced parens and
//! take the **last** argument, which is the handler in every one of these routers.
//!
//! # ⚠ Two known open hops — stated, not half-bridged
//!
//! 1. **The middleware chain.** `c.Next()` inside a middleware runs the *next*
//!    registered `HandlerFunc`; nothing in the source names it. Closing that needs
//!    the `gin-middleware-chain` synthesizer, which is **Phase 8**. Until then the
//!    route binds to its handler and the middleware hop is *absent* — not
//!    half-drawn. (An edge to `Required` would be worse than no edge: it looks
//!    like an answer.)
//! 2. **The group prefix is not prepended.** `v1 := r.Group("/api/v1")` then
//!    `v1.POST("/articles", …)` produces the route `POST /articles`, not
//!    `POST /api/v1/articles`. This is TS parity, carried deliberately: the group
//!    variable's prefix lives at its *declaration*, arbitrarily far from the
//!    registration, and resolving it needs dataflow this pass does not have. The
//!    route's file+line still take the agent to the exact registration site.
//!
//! # An anonymous handler gets a route and no reference
//!
//! `r.GET("/ping", func(c *gin.Context) { … })` is a real route, so the node is
//! emitted — but a function literal has no name to bind to, so **no reference** is
//! emitted rather than a reference to `Context` (which is what a naive
//! tail-identifier of the expression yields, and it is a wrong edge).

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, NodeKind, RefStatus, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::frameworks::{
    FrameworkExtraction, FrameworkResolver, RouteSpec, by_convention, line_of, route_node,
};
use crate::strip_comments::strip_comments_for_regex;
use crate::types::ResolvedRef;

/// Route emission must be byte-deterministic; a wall clock is the one thing that
/// cannot be. (See `python.rs` for the full argument.)
const NO_CLOCK: i64 = 0;

const GO: &str = "go";

macro_rules! re {
    ($pat:expr) => {
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // compile-time literal, covered by tests
            Regex::new($pat).unwrap()
        })
    };
}

/// `<anything>.<VERB>("<path>",` — the receiver is `\w+`, i.e. **any** identifier.
///
/// The match deliberately ends at the comma after the path: the argument list is
/// then scanned with balanced parens (see [`args_after`]), because a regex cannot
/// tell `auth.Required(), handlers.List` from a single argument.
static ROUTE_CALL: LazyLock<Regex> = re!(
    r#"\b\w+\.(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Get|Post|Put|Patch|Delete|Handle|HandleFunc)\s*\(\s*"([^"]+)"\s*,"#
);

/// The rest of a call's arguments, from `from` (just past the path's comma) to the
/// call's closing paren — with parens/brackets/braces balanced and string bodies
/// skipped, so a `func(c *gin.Context) { … }` literal does not end the scan early.
fn args_after(src: &str, from: usize) -> Option<&str> {
    let b = src.as_bytes();
    let mut depth = 1usize;
    let mut i = from;
    let mut in_string = false;

    while i < b.len() {
        if in_string {
            match b[i] {
                // A `\"` inside the string does not end it.
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else {
            match b[i] {
                b'"' => in_string = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&src[from..i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    // An unterminated call (a truncated file): best-effort, never an error.
    None
}

/// The last **top-level** argument. `auth.Required(), handlers.List` → `handlers.List`.
fn last_argument(args: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut last_start = 0usize;
    let mut in_string = false;

    for (i, byte) in args.bytes().enumerate() {
        if in_string {
            if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            // Only a comma at depth 0 separates arguments — one inside
            // `gin.H{"a": 1, "b": 2}` does not.
            b',' if depth == 0 => last_start = i + 1,
            _ => {}
        }
    }
    let last = args[last_start..].trim();
    (!last.is_empty()).then_some(last)
}

/// The name a handler expression binds to: `handlers.CreateArticle` → `CreateArticle`.
///
/// `None` for an anonymous function literal — see the module docs. A literal's
/// tail identifier would be `Context` (out of `func(c *gin.Context)`), and an edge
/// to `gin.Context` is a wrong edge dressed as an answer.
fn handler_name(expr: &str) -> Option<String> {
    let e = expr.trim().trim_start_matches(['&', '*']);
    if e.starts_with("func") {
        return None;
    }
    let tail = e.rsplit('.').next()?.trim();
    let ident: String = tail
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
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
        language: Language::Go,
        status: RefStatus::Pending,
        name_tail: handler.to_string(),
    }
}

/// Gin / gorilla-mux / chi.
pub struct Go;

impl FrameworkResolver for Go {
    fn name(&self) -> &'static str {
        GO
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(&[Language::Go])
    }

    /// Deliberately broad — a `go.mod`, or any `.go` file at all. The extract
    /// regex is the real gate: a Go project with no router registers no routes,
    /// and there is nothing to be gained by refusing to look.
    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        ctx.file_exists("go.mod") || ctx.all_files().iter().any(|f| f.ends_with(".go"))
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        if language != Language::Go {
            return out;
        }

        // Comment-stripped, byte-offset preserving: a commented-out route must not
        // become a node, and the line an id is hashed from must not shift.
        let src = strip_comments_for_regex(content, Language::Go);

        for caps in ROUTE_CALL.captures_iter(&src) {
            let (Some(whole), Some(verb), Some(route_path)) =
                (caps.get(0), caps.get(1), caps.get(2))
            else {
                continue;
            };

            // `Handle` / `HandleFunc` carry no verb: the method is whatever the
            // request is.
            let method = match verb.as_str() {
                "Handle" | "HandleFunc" => "ANY".to_string(),
                v => v.to_uppercase(),
            };

            let line = line_of(&src, whole.start());
            let node = route_node(
                &RouteSpec::new(GO, Some(&method), route_path.as_str(), path, line),
                Language::Go,
                NO_CLOCK,
            );

            if let Some(handler) = args_after(&src, whole.end())
                .and_then(last_argument)
                .and_then(handler_name)
            {
                out.refs.push(handler_ref(&node.id, &handler, path, line));
            }
            out.nodes.push(node);
        }

        out
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if r.language != Language::Go {
            return None;
        }
        let name = r.reference_name.as_str();

        // Order is precedence. `AuthHandler` is a handler, not middleware — the
        // `*Handler` suffix is the stronger claim and is tested first.
        if name.ends_with("Handler") || name.starts_with("Handle") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Function],
                &["/handler/", "/handlers/", "/api/", "/controller/"],
                0.8,
            );
        }
        if name.ends_with("Service") || name.ends_with("Repository") || name.ends_with("Store") {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Struct, NodeKind::Interface],
                &["/service/", "/repository/", "/store/"],
                0.8,
            );
        }
        if name.ends_with("Middleware") || name.starts_with("Auth") || name.starts_with("Log") {
            return by_convention(r, ctx, &[NodeKind::Function], &["/middleware/"], 0.75);
        }
        if is_pascal_case(name) {
            return by_convention(
                r,
                ctx,
                &[NodeKind::Struct],
                &["/model/", "/models/", "/entity/"],
                0.7,
            );
        }
        None
    }
}

/// `Article` — yes. `createArticle`, `ARTICLE` — no.
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
    fn the_last_argument_is_the_handler_even_behind_middleware() {
        let args = " auth.Required(), rate.Limit(10), handlers.List)";
        // `args_after` is given the text after the path's comma; emulate that here
        // by scanning the same shape.
        let inner = args_after(args, 0).unwrap();
        assert_eq!(
            last_argument(inner).unwrap(),
            "handlers.List",
            "the TS regex stopped at the first `)` and bound the route to \
             `auth.Required(` — the middleware, not the handler"
        );
    }

    #[test]
    fn handler_name_takes_the_tail_and_refuses_a_literal() {
        assert_eq!(
            handler_name("handlers.CreateArticle").unwrap(),
            "CreateArticle"
        );
        assert_eq!(handler_name("CreateArticle").unwrap(), "CreateArticle");
        assert_eq!(handler_name("&ctrl.List").unwrap(), "List");
        assert_eq!(
            handler_name("func(c *gin.Context) { c.JSON(200, nil) }"),
            None,
            "a literal's tail identifier is `Context` — an edge to gin.Context is a \
             wrong edge dressed as an answer"
        );
    }

    #[test]
    fn is_pascal_case_accepts_a_model_and_rejects_a_local() {
        assert!(is_pascal_case("Article"));
        assert!(!is_pascal_case("createArticle"));
        assert!(!is_pascal_case("ARTICLE"));
    }
}
