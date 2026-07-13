//! Python web frameworks. **Django** (Task 14) lives here; Flask + FastAPI
//! (Task 15) and the Django ORM descriptor (Task 26) append to this file.
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
//! # `include('api.urls')` needs `claims_reference`
//!
//! `api.urls` names no declared symbol anywhere in the project, so without a
//! claim the resolver's pre-filter drops the reference **before** `resolve()` is
//! ever called, and the include bridge is silently inert. That hook is the whole
//! reason `claims_reference` exists on the trait.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{Language, NodeKind, UnresolvedRef};

use super::{FrameworkExtraction, FrameworkResolver, RouteSpec, line_at, route_node};
use crate::ResolutionContext;
use crate::strip_comments::strip_comments_for_regex;
use crate::types::{ResolvedBy, ResolvedRef};

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
        name.ends_with(".urls")
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
            let line = line_at(&src, whole.start());
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
            let line = line_at(&src, whole.start());

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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
