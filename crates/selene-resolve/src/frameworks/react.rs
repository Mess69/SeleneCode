//! React Router (v5 / v6 / data-router) + Next.js file routes.
//!
//! # The flow this closes
//!
//! ```text
//! <Route path="/article/:slug" element={<Article/>}/>  →  Article
//!                                                      →  useArticle()  →  fetchArticle()
//! ```
//!
//! Route→component alone is **not** the flow: it leaves the agent reading
//! `Article.tsx` to discover what the page actually fetches. The chain has to
//! reach the data call.
//!
//! (The *component→child* hop — `<Article/>` rendering `<Header/>` — is the JSX
//! child synthesizer, Task 25. That one and this one together are what make a
//! React question answerable; shipping route→component alone would be exactly
//! the partial coverage PRD §8.2 forbids, which is why Task 25 exists.)
//!
//! # Extraction runs on RAW source, not comment-stripped
//!
//! One of the few. A `<Route>` inside a JSX comment (`{/* … */}`) is rare enough
//! that the TS build accepted the false positive rather than pay for JSX-aware
//! comment handling. Kept as a compat contract.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, NodeKind, UnresolvedRef};

use super::{FrameworkExtraction, FrameworkResolver, RouteSpec, line_of, route_node};
use crate::ResolutionContext;
use crate::types::{ResolvedBy, ResolvedRef};

const LANGS: &[Language] = &[
    Language::Javascript,
    Language::Jsx,
    Language::Typescript,
    Language::Tsx,
];

/// How far after a `<Route` we look for its `path` and its component. Byte
/// window, clamped to EOF — a `<Route>` whose element is 400 bytes away is not
/// a pairing we are confident about, and a wrong pairing is worse than none.
const JSX_WINDOW: usize = 400;
/// The same, for an object-literal data-router entry.
const OBJ_WINDOW: usize = 300;

static ROUTE_TAG: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"<Route\b").unwrap()
});
static PATH_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r#"path=["']([^"']*)["']"#).unwrap()
});
/// v5 `component={Comp}` / v6 `element={<Comp`.
static JSX_COMPONENT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"component=\{\s*([A-Z]\w*)|element=\{\s*<\s*([A-Z]\w*)").unwrap()
});
/// A data-router object entry: `path: '/x'`.
static OBJ_PATH: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r#"path:\s*["']([^"']*)["']"#).unwrap()
});
/// `element: <Comp` / `Component: Comp`.
static OBJ_COMPONENT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"element:\s*<\s*([A-Z]\w*)|Component:\s*([A-Z]\w*)").unwrap()
});
/// The default export's name — the Next.js page component.
static DEFAULT_EXPORT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal; `regexes_compile` covers it
    Regex::new(r"export\s+default\s+(?:async\s+)?(?:function\s+)?([A-Z]\w*)").unwrap()
});

pub struct ReactResolver;

impl FrameworkResolver for ReactResolver {
    fn name(&self) -> &'static str {
        "react"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        // tsx/jsx MUST be here: they are the files that hold the routes.
        Some(LANGS)
    }

    fn detect(&self, ctx: &dyn ResolutionContext) -> bool {
        if let Some(pkg) = ctx.read_file("package.json")
            && ["react", "next", "react-native"]
                .iter()
                .any(|d| pkg.contains(&format!("\"{d}\"")))
        {
            return true;
        }
        ctx.files_with_language()
            .iter()
            .any(|(_, l)| matches!(l, Language::Tsx | Language::Jsx))
    }

    fn extract(&self, path: &str, content: &str, language: Language) -> FrameworkExtraction {
        let mut out = FrameworkExtraction::default();
        // RAW source — see the module docs.
        self.jsx_routes(path, content, language, &mut out);
        self.object_routes(path, content, language, &mut out);
        self.next_file_route(path, content, language, &mut out);
        out
    }

    fn resolve(&self, r: &UnresolvedRef, ctx: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = r.reference_name.as_str();

        // Hooks: `useArticle` → a function, hook dirs preferred.
        if name.starts_with("use") && name.len() > 3 {
            return self.pick(
                r,
                ctx,
                &[NodeKind::Function, NodeKind::Method],
                &["/hooks/", "/hook/"],
                0.85,
            );
        }
        // Contexts / providers.
        if name.ends_with("Context") || name.ends_with("Provider") {
            return self.pick(
                r,
                ctx,
                &[
                    NodeKind::Variable,
                    NodeKind::Constant,
                    NodeKind::Function,
                    NodeKind::Component,
                ],
                &[],
                0.8,
            );
        }
        // Components — PascalCase, and ONLY from a tsx/jsx reference.
        //
        // The language gate is #764: a PascalCase name in a plain `.ts` file is
        // as likely to be a class or a type as a component, and binding it here
        // would out-rank the name matcher with a worse answer. Let the matcher
        // decide instead.
        if !matches!(r.language, Language::Tsx | Language::Jsx) {
            return None;
        }
        if !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return None;
        }
        self.pick(
            r,
            ctx,
            &[
                NodeKind::Component,
                NodeKind::Function,
                NodeKind::Class,
                NodeKind::Variable,
            ],
            &["/components/", "/component/"],
            0.8,
        )
    }
}

impl ReactResolver {
    /// `<Route path="…" element={<Comp/>}/>` (v6) and `component={Comp}` (v5).
    fn jsx_routes(&self, path: &str, src: &str, language: Language, out: &mut FrameworkExtraction) {
        for m in ROUTE_TAG.find_iter(src) {
            let end = (m.start() + JSX_WINDOW).min(src.len());
            // Clamp to a char boundary — a byte window can land mid-UTF-8.
            let end = floor_boundary(src, end);
            let window = &src[m.start()..end];

            // No path ⇒ not a route we can name. Skip the whole thing.
            let Some(p) = PATH_ATTR.captures(window).and_then(|c| c.get(1)) else {
                continue;
            };
            let Some(comp) = JSX_COMPONENT
                .captures(window)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
            else {
                continue;
            };
            let line = line_of(src, m.start());
            self.emit(path, p.as_str(), comp.as_str(), line, language, out);
        }
    }

    /// `createBrowserRouter([{ path: '/x', element: <Comp/> }])`.
    fn object_routes(
        &self,
        path: &str,
        src: &str,
        language: Language,
        out: &mut FrameworkExtraction,
    ) {
        // Gate: without a data-router factory in the file, `path:` is just an
        // ordinary object key and pairing it with a nearby component would
        // invent routes out of config objects.
        if ![
            "createBrowserRouter",
            "createHashRouter",
            "createMemoryRouter",
            "createRoutesFromElements",
        ]
        .iter()
        .any(|f| src.contains(f))
        {
            return;
        }
        for caps in OBJ_PATH.captures_iter(src) {
            let (Some(whole), Some(p)) = (caps.get(0), caps.get(1)) else {
                continue;
            };
            let end = floor_boundary(src, (whole.start() + OBJ_WINDOW).min(src.len()));
            let window = &src[whole.start()..end];
            let Some(comp) = OBJ_COMPONENT
                .captures(window)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
            else {
                continue;
            };
            let route_path = if p.as_str().is_empty() {
                "/"
            } else {
                p.as_str()
            };
            let line = line_of(src, whole.start());
            self.emit(path, route_path, comp.as_str(), line, language, out);
        }
    }

    /// Next.js file-system routing.
    ///
    /// `pages/articles/[slug].tsx` → `/articles/:slug`;
    /// `app/articles/page.tsx`     → `/articles`.
    fn next_file_route(
        &self,
        path: &str,
        src: &str,
        language: Language,
        out: &mut FrameworkExtraction,
    ) {
        let Some(base) = path.rsplit('/').next() else {
            return;
        };
        // `_app.tsx`, `_document.tsx` are Next.js internals, not routes.
        if base.starts_with('_') || base.contains(".config.") {
            return;
        }
        if !["tsx", "ts", "jsx", "js"]
            .iter()
            .any(|e| base.ends_with(&format!(".{e}")))
        {
            return;
        }
        if !src.contains("export default") {
            return;
        }

        let segs: Vec<&str> = path.split('/').collect();
        let route_path = if let Some(i) = segs.iter().position(|s| *s == "pages") {
            let rest = &segs[i + 1..];
            Some(next_path_from(rest, false))
        } else if let Some(i) = segs.iter().position(|s| *s == "app") {
            // ⚠ The TS build tested `filePath.includes('page.')`, which also
            // matches `mypage.tsx`. Match the BASENAME. This is a deliberate
            // bug-fix deviation: route counts may differ from TS by design.
            if !is_page_file(base) {
                return;
            }
            let rest = &segs[i + 1..];
            Some(next_path_from(rest, true))
        } else {
            None
        };

        let Some(route_path) = route_path else { return };
        let Some(comp) = DEFAULT_EXPORT.captures(src).and_then(|c| c.get(1)) else {
            return;
        };
        // Next.js routes are file-level: line 1.
        self.emit(path, &route_path, comp.as_str(), 1, language, out);
    }

    fn emit(
        &self,
        file: &str,
        route_path: &str,
        component: &str,
        line: u32,
        language: Language,
        out: &mut FrameworkExtraction,
    ) {
        // Path-only router: no HTTP verb.
        let node = route_node(
            &RouteSpec::new(self.name(), None, route_path, file, line),
            language,
            0,
        );
        out.refs.push(UnresolvedRef {
            from_node_id: node.id.clone(),
            reference_name: component.to_string(),
            reference_kind: "references".to_string(),
            line: Some(line),
            column: Some(0),
            candidates: vec![],
            file_path: file.to_string(),
            language: Language::Tsx,
            status: selene_core::RefStatus::Pending,
            name_tail: component.to_string(),
        });
        out.nodes.push(node);
    }

    /// Same-dir first, then a preferred dir, then unique-only. **Ambiguous ⇒
    /// `None`** — never guess which `Article` was meant; the name matcher gets
    /// its turn (#764).
    fn pick(
        &self,
        r: &UnresolvedRef,
        ctx: &dyn ResolutionContext,
        kinds: &[NodeKind],
        prefer_dirs: &[&str],
        confidence: f64,
    ) -> Option<ResolvedRef> {
        let hits: Vec<_> = ctx
            .nodes_by_name(&r.reference_name)
            .into_iter()
            .filter(|n| kinds.contains(&n.kind))
            .collect();
        if hits.is_empty() {
            return None;
        }

        let same_dir = |a: &str, b: &str| dir_of(a) == dir_of(b);
        let chosen = if let Some(n) = hits.iter().find(|n| same_dir(&n.file_path, &r.file_path)) {
            n
        } else if let Some(n) = hits
            .iter()
            .find(|n| prefer_dirs.iter().any(|d| n.file_path.contains(d)))
        {
            n
        } else if hits.len() == 1 {
            &hits[0]
        } else {
            return None; // ambiguous — decline
        };

        Some(ResolvedRef {
            original: r.clone(),
            target_node_id: chosen.id.clone(),
            confidence,
            resolved_by: ResolvedBy::Framework,
        })
    }
}

/// `page.tsx` / `page.jsx` / `page.ts` / `page.js` — and NOT `mypage.tsx`.
fn is_page_file(base: &str) -> bool {
    matches!(base, "page.tsx" | "page.ts" | "page.jsx" | "page.js")
}

/// Turn the path segments under `pages/` or `app/` into a route path.
/// `["articles", "[slug].tsx"]` → `/articles/:slug`.
fn next_path_from(rest: &[&str], drop_last: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    let take = if drop_last {
        rest.len().saturating_sub(1) // `app/…/page.tsx` — the file itself is not a segment
    } else {
        rest.len()
    };
    for (i, seg) in rest.iter().take(take).enumerate() {
        let last = i + 1 == take;
        let mut s = (*seg).to_string();
        if last && !drop_last {
            s = strip_ext(&s);
            if s == "index" {
                continue; // `pages/articles/index.tsx` → `/articles`
            }
        }
        // `[slug]` → `:slug`
        if s.starts_with('[') && s.ends_with(']') {
            s = format!(":{}", &s[1..s.len() - 1]);
        }
        parts.push(s);
    }
    format!("/{}", parts.join("/"))
}

fn strip_ext(s: &str) -> String {
    match s.rfind('.') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Largest char boundary ≤ `i` — a byte window must not split a UTF-8 char.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn regexes_compile() {
        assert!(ROUTE_TAG.is_match("<Route "));
        assert!(PATH_ATTR.is_match(r#"path="/x""#));
        assert!(JSX_COMPONENT.is_match("element={<Foo"));
        assert!(JSX_COMPONENT.is_match("component={Foo}"));
        assert!(OBJ_PATH.is_match("path: '/x'"));
        assert!(OBJ_COMPONENT.is_match("element: <Foo"));
        assert!(DEFAULT_EXPORT.is_match("export default function Page()"));
    }

    #[test]
    fn next_paths() {
        assert_eq!(
            next_path_from(&["articles", "[slug].tsx"], false),
            "/articles/:slug"
        );
        assert_eq!(
            next_path_from(&["articles", "index.tsx"], false),
            "/articles"
        );
        assert_eq!(next_path_from(&["articles", "page.tsx"], true), "/articles");
    }

    #[test]
    fn only_page_dot_ext_is_an_app_route() {
        assert!(is_page_file("page.tsx"));
        assert!(!is_page_file("mypage.tsx"), "the TS bug — do not port it");
    }
}
