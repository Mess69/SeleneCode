//! Express (and Fastify / Koa / Hapi, which register routes the same way).
//!
//! # The flow this closes
//!
//! ```text
//! POST /users/login  →  (inline arrow handler)  →  login()  →  hashPassword()
//! ```
//!
//! **The route→handler hop is not the flow.** The dominant modern Express shape
//! is an inline arrow — `router.post('/x', async (req, res) => { … })` — which
//! is *not a node*. A bridge that stops at "the route exists" connects the route
//! to nothing at all, and the agent opens the file anyway. Partial coverage is
//! worse than none (PRD §8.2).
//!
//! So when the handler is an inline arrow, this extractor mines the arrow's
//! **body** and attributes its calls to the **route node**. That is what carries
//! the flow across the anonymous function into the service layer.
//!
//! The TS build got this wrong for a long time in an instructive way: it spanned
//! the call with a regex `\(([^)]+)\)`, which stops at the arrow's *own* closing
//! paren. Every inline-handler route therefore captured a truncated argument list
//! and bound to nothing. The fix is a **balanced, string-aware paren scan**
//! (`match_delim`) — not a cleverer regex.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, UnresolvedRef};

use super::{FrameworkExtraction, FrameworkResolver, RouteSpec, line_of, match_delim, route_node};
use crate::ResolutionContext;
use crate::strip_comments::strip_comments_for_regex;
use crate::types::{ResolvedBy, ResolvedRef};

/// Express's own response/util API, plus JS builtins — calls that appear in
/// every handler body and mean nothing structurally. Without this filter every
/// route in the repo would sprout a `json` / `status` / `send` reference.
///
/// Verbatim from the TS source (which lists `redirect` twice — it is a set, so
/// the duplicate is a no-op).
pub static RESERVED_CALLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "json",
        "jsonp",
        "send",
        "sendStatus",
        "sendFile",
        "status",
        "end",
        "redirect",
        "render",
        "set",
        "get",
        "header",
        "type",
        "format",
        "attachment",
        "download",
        "cookie",
        "clearCookie",
        "append",
        "location",
        "vary",
        "links",
        "accepts",
        "is",
        "next",
        "then",
        "catch",
        "finally",
        "resolve",
        "reject",
        "all",
        "race",
        "map",
        "filter",
        "forEach",
        "reduce",
        "find",
        "push",
        "pop",
        "slice",
        "splice",
        "includes",
        "keys",
        "values",
        "entries",
        "assign",
        "parse",
        "stringify",
        "log",
        "error",
        "warn",
        "info",
        "String",
        "Number",
        "Boolean",
        "Array",
        "Object",
        "Date",
        "Math",
        "JSON",
        "Promise",
        "require",
        "fail",
    ]
    .into_iter()
    .collect()
});

/// Control-flow keywords that are followed by `(` and would otherwise read as
/// calls (`if (`, `for (`, `switch (`, and — the one that actually bit — the
/// `async (req, res)` of the arrow's own parameter list).
static JS_KEYWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "if", "for", "while", "switch", "catch", "return", "function", "typeof", "await", "async",
        "new", "delete", "void", "in", "of", "do", "else", "throw", "yield",
    ]
    .into_iter()
    .collect()
});

const LANGS: &[Language] = &[
    Language::Typescript,
    Language::Tsx,
    Language::Javascript,
    Language::Jsx,
];

/// `app.get('/x', …` / `router.post('/x', …` — the head of a route registration.
/// The path must be a literal: a computed path (`app.get(routes.x, h)`) is a
/// dynamic construct we deliberately stay silent about (silent beats wrong).
static ROUTE_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal; `route_head_compiles` covers it
    Regex::new(r#"\b(app|router)\.(get|post|put|patch|delete|all|use)\s*\(\s*['"]([^'"]+)['"]"#)
        .unwrap()
});

/// A bare call inside an arrow body: `doWork(`, `await login(`.
static BODY_CALL: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal; `body_call_compiles` covers it
    Regex::new(r"\b([A-Za-z_$][\w$]*)\s*\(").unwrap()
});

/// Middleware names, by convention (`/i`).
static MIDDLEWARE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal; `middleware_re_compiles` covers it
    Regex::new(
        r"(?i)^(auth|authenticate|authorization|cors|helmet|logger|errorHandler|notFound)$|^(validate|sanitize|rateLimit)|Middleware$",
    )
    .unwrap()
});

/// `UserController.index`
static CONTROLLER_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal; covered by `controller_re_compiles`
    Regex::new(r"^(\w+)Controller\.(\w+)$").unwrap()
});

/// `UserService.find` / `DateHelper.fmt` / `StrUtils.pad`
static SERVICE_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal; covered by `service_re_compiles`
    Regex::new(r"^(\w+)(Service|Helper|Utils?)\.(\w+)$").unwrap()
});

pub struct ExpressResolver;

impl FrameworkResolver for ExpressResolver {
    fn name(&self) -> &'static str {
        "express"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        Some(LANGS)
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        if let Some(pkg) = ctx.read_file("package.json")
            && ["express", "fastify", "koa", "hapi"]
                .iter()
                .any(|d| pkg.contains(&format!("\"{d}\"")))
        {
            return true;
        }
        // Fallback: a repo that vendors its deps, or a monorepo whose manifest
        // sits elsewhere. Look for the *shape* instead.
        ctx.files_with_language().iter().any(|(path, lang)| {
            LANGS.contains(lang)
                && (path.contains("routes")
                    || path.contains("controllers")
                    || path.contains("middleware"))
                && ctx.read_file(path).is_some_and(|s| {
                    s.contains("express") || s.contains("app.get") || s.contains("router.get")
                })
        })
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        if !is_js_file(path) {
            return FrameworkExtraction::default();
        }
        // Comment-stripped: a commented-out route must not become a real one.
        let src = strip_comments_for_regex(content, language);
        let mut out = FrameworkExtraction::default();

        for caps in ROUTE_HEAD.captures_iter(&src) {
            let (Some(verb), Some(route_path)) = (caps.get(2), caps.get(3)) else {
                continue;
            };
            let verb_s = verb.as_str();
            let path_s = route_path.as_str();

            // `app.use(cors())` is middleware registration, not a route. Only a
            // mount PATH makes it one.
            if verb_s == "use" && !path_s.starts_with('/') {
                continue;
            }

            let Some(whole) = caps.get(0) else { continue };
            let line = line_of(&src, whole.start());
            let method = verb_s.to_uppercase();

            let node = route_node(
                &RouteSpec::new(self.name(), Some(&method), path_s, path, line),
                language,
                0,
            );

            // The argument list — balanced and string-aware. A regex cannot do
            // this (see the module docs).
            let open = src[whole.start()..].find('(').map(|i| whole.start() + i);
            if let Some(open) = open
                && let Some(args_range) = match_delim(&src, open)
            {
                let args = &src[args_range];
                out.refs
                    .extend(self.handler_refs(args, &node.id, path, line, language));
            }

            out.nodes.push(node);
        }
        out
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = r.reference_name.as_str();

        // `UserController.index` → the `index` method on class `UserController`.
        if let Some(c) = CONTROLLER_METHOD.captures(name) {
            let class = format!("{}Controller", &c[1]);
            return self.method_on(&class, &c[2], r, ctx, 0.85);
        }
        // `UserService.find` / `DateHelper.fmt` / `Utils.pad`
        if let Some(c) = SERVICE_METHOD.captures(name) {
            let class = format!("{}{}", &c[1], &c[2]);
            return self.method_on(&class, &c[3], r, ctx, 0.8);
        }
        // Middleware, by name convention.
        if MIDDLEWARE.is_match(name) {
            let group = ctx.nodes_by_name(name);
            let hits: Vec<_> = group
                .iter()
                .filter(|n| {
                    matches!(
                        n.kind,
                        selene_core::NodeKind::Function | selene_core::NodeKind::Method
                    )
                })
                .collect();
            // Ambiguous ⇒ drop. Never guess which `auth` was meant.
            if hits.len() == 1 {
                return Some(ResolvedRef {
                    original: r.clone(),
                    target_node_id: hits[0].id.clone(),
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        None
    }
}

impl ExpressResolver {
    /// The references a route's argument list yields.
    ///
    /// **Inline arrow** → mine the body: one `calls` ref per unique
    /// non-[`RESERVED_CALLS`] callee, attributed to the ROUTE. This is the hop
    /// that carries the flow past the anonymous function.
    ///
    /// **Otherwise** → the last argument's tail identifier is the handler
    /// (`router.get('/x', auth, getProfile)` → `getProfile`, never `auth`).
    fn handler_refs(
        &self,
        args: &str,
        route_id: &str,
        file: &str,
        line: u32,
        language: Language,
    ) -> Vec<UnresolvedRef> {
        let mk = |name: &str, kind: &str| UnresolvedRef {
            from_node_id: route_id.to_string(),
            reference_name: name.to_string(),
            reference_kind: kind.to_string(),
            line: Some(line),
            column: Some(0),
            candidates: vec![],
            file_path: file.to_string(),
            language,
            status: selene_core::RefStatus::Pending,
            name_tail: name.rsplit('.').next().unwrap_or(name).to_string(),
        };

        if let Some(arrow) = args.find("=>") {
            // Scan the arrow's BODY, not the whole argument list — the parameter
            // list is `async (req, res)`, and scanning it would capture `async`
            // as a callee (it matches `ident(`).
            let body = &args[arrow + 2..];

            // Deterministic: BTreeSet, and one ref per distinct callee.
            let mut seen = std::collections::BTreeSet::new();
            for c in BODY_CALL.captures_iter(body) {
                let callee = &c[1];
                if RESERVED_CALLS.contains(callee) || JS_KEYWORDS.contains(callee) {
                    continue;
                }
                seen.insert(callee.to_string());
            }
            return seen.iter().map(|n| mk(n, "calls")).collect();
        }

        // Named handler: the LAST argument. Anything before it is middleware.
        let parts = super::split_args(args);
        let Some(last) = parts.last() else {
            return vec![];
        };
        let ident = last.trim();
        // A tail identifier only — `UserController.index` keeps its receiver, a
        // call expression or object literal is not a handler name.
        if ident.is_empty()
            || ident.contains(['(', ')', '{', '}', '='])
            || ident.starts_with(['\'', '"', '`'])
        {
            return vec![];
        }
        vec![mk(ident, "references")]
    }

    /// The `method` declared on class `class_name`, in the same file as that
    /// class. Ambiguity ⇒ `None`.
    fn method_on(
        &self,
        class_name: &str,
        method: &str,
        r: &UnresolvedRef,
        ctx: &dyn ResolutionContext,
        confidence: f64,
    ) -> Option<ResolvedRef> {
        let class = ctx
            .nodes_by_name(class_name)
            .iter()
            .find(|n| n.kind == selene_core::NodeKind::Class)
            .cloned()?;
        let group = ctx.nodes_by_name(method);
        let hits: Vec<_> = group
            .iter()
            .filter(|n| {
                n.kind == selene_core::NodeKind::Method
                    && n.file_path == class.file_path
                    && n.start_line >= class.start_line
                    && n.end_line <= class.end_line
            })
            .collect();
        if hits.len() != 1 {
            return None;
        }
        Some(ResolvedRef {
            original: r.clone(),
            target_node_id: hits[0].id.clone(),
            confidence,
            resolved_by: ResolvedBy::Framework,
        })
    }
}

fn is_js_file(path: &str) -> bool {
    let p = path.rsplit('/').next().unwrap_or(path);
    p.ends_with(".js")
        || p.ends_with(".mjs")
        || p.ends_with(".cjs")
        || p.ends_with(".ts")
        || p.ends_with(".tsx")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn route_head_compiles() {
        assert!(ROUTE_HEAD.is_match("router.post('/x', h)"));
    }
    #[test]
    fn body_call_compiles() {
        assert!(BODY_CALL.is_match("doWork("));
    }
    #[test]
    fn middleware_re_compiles() {
        assert!(MIDDLEWARE.is_match("auth"));
        assert!(MIDDLEWARE.is_match("validateBody"));
        assert!(MIDDLEWARE.is_match("authMiddleware"));
        assert!(!MIDDLEWARE.is_match("login"));
    }
    #[test]
    fn controller_re_compiles() {
        assert!(CONTROLLER_METHOD.is_match("UserController.index"));
    }
    #[test]
    fn service_re_compiles() {
        assert!(SERVICE_METHOD.is_match("AuthService.login"));
        assert!(SERVICE_METHOD.is_match("StrUtils.pad"));
    }

    #[test]
    fn match_delim_spans_an_arrow_body() {
        let s = "router.post('/x', async (req, res) => { f(); })";
        let open = s.find('(').unwrap();
        let r = match_delim(s, open).unwrap();
        assert!(
            s[r].ends_with("=> { f(); }"),
            "the span must cover the WHOLE arg list, arrow parens included"
        );
    }

    #[test]
    fn match_delim_ignores_parens_inside_strings() {
        let s = "app.get('/a)b', h)";
        let open = s.find('(').unwrap();
        let r = match_delim(s, open).unwrap();
        assert_eq!(&s[r], "'/a)b', h");
    }
}
