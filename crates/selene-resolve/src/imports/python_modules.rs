//! Python module-member references, absolute dotted-module imports, and the
//! dotted module-path → file lookup they share.

use std::sync::Arc;

use selene_core::{Language, Node, NodeKind, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::imports::imported;
use crate::imports::path_walk::resolve_import_path;
use crate::types::{ImportMapping, ResolvedRef};

/// Python `certs.where()` — the receiver names an imported **module** (a file),
/// not a symbol (**0.85**).
///
/// The generic symbol lookup would search the *package* for `certs` instead of
/// looking **inside** the module. `method` is deliberately excluded from the
/// accepted kinds, so `mod.foo` can never land on a same-named class method.
pub(super) fn resolve_python_module_member<C: ResolutionContext>(
    r: &UnresolvedRef,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    let dot = r.reference_name.find('.')?;
    if dot == 0 {
        return None;
    }
    let receiver = &r.reference_name[..dot];
    // The IMMEDIATE member of the module: the first segment after the receiver.
    let member = r.reference_name[dot + 1..].split('.').next()?;
    if member.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        // `import mod` binds the module at `source`; `from . import certs` /
        // `from pkg import mod` bind a SUBMODULE whose dotted path is the source
        // joined with the imported name.
        let module_path = if imp.is_namespace {
            imp.source.clone()
        } else if imp.source.ends_with('.') {
            format!("{}{}", imp.source, imp.local_name)
        } else {
            format!("{}.{}", imp.source, imp.local_name)
        };

        // `resolve_import_path` only maps RELATIVE dotted paths; an ABSOLUTE
        // package path resolves to nothing there, so fall back to the dotted
        // module-file lookup. Without this, `module.func()` after
        // `from pkg import module` dropped its `calls` edge even though the
        // import edge resolved (#578).
        let resolved = resolve_import_path(&module_path, &r.file_path, Language::Python, ctx)
            .or_else(|| {
                find_python_module_file(&module_path, &r.file_path, ctx)
                    .map(|n| n.file_path.clone())
            });

        let Some(resolved) = resolved else { continue };
        if resolved == r.file_path {
            continue;
        }

        let group = ctx.nodes_in_file(&resolved);
        let target = group.iter().find(|n| {
            n.name == member
                && matches!(
                    n.kind,
                    NodeKind::Function | NodeKind::Class | NodeKind::Variable | NodeKind::Constant
                )
        });
        if let Some(t) = target {
            return Some(imported(r, &t.id, 0.85));
        }
    }
    None
}

/// A Python ABSOLUTE dotted module import (`import a.b.c`) → its file (**0.9**).
///
/// The Django `AppConfig.ready(): import myapp.signals` pattern, and any
/// side-effect module import.
pub(super) fn resolve_python_absolute_module<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    if r.reference_kind != "imports" {
        return None;
    }
    let node = find_python_module_file(&r.reference_name, &r.file_path, ctx)?;
    Some(imported(r, &node.id, 0.9))
}

/// The file node for a Python dotted module path `a.b.c`: a module file ending
/// in `a/b/c.py`, or a package `a/b/c/__init__.py`.
///
/// Suffix-matched, so a package rooted under `src/` still resolves. `None` for
/// stdlib/external modules (no matching repo file), so `import os` creates no
/// edge.
pub(super) fn find_python_module_file<C: ResolutionContext>(
    module: &str,
    exclude: &str,
    ctx: &C,
) -> Option<Arc<Node>> {
    if module.is_empty() || module.starts_with('.') {
        return None; // relative imports are handled elsewhere
    }
    let rel = module.replace('.', "/");
    let last_seg = module.rsplit('.').next()?;

    let ends_with = |p: &str, want: &str| p == want || p.ends_with(&format!("/{want}"));

    let module_file = ctx
        .nodes_by_name(&format!("{last_seg}.py"))
        .iter()
        .find(|n| {
            n.kind == NodeKind::File
                && n.file_path != exclude
                && ends_with(&n.file_path, &format!("{rel}.py"))
        })
        .cloned();
    if module_file.is_some() {
        return module_file;
    }

    ctx.nodes_by_name("__init__.py")
        .iter()
        .find(|n| {
            n.kind == NodeKind::File
                && n.file_path != exclude
                && ends_with(&n.file_path, &format!("{rel}/__init__.py"))
        })
        .cloned()
}
