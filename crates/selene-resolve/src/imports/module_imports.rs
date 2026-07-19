//! Whole-module / namespace imports → the module FILE (a file→file dependency).

use selene_core::{Language, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::imports::path_walk::resolve_import_path;
use crate::imports::python_modules::find_python_module_file;
use crate::imports::{file_node_at, imported};
use crate::types::{ImportMapping, ResolvedRef};

/// A whole-MODULE import → that module's file (**0.9**) — a file→file dependency.
///
/// The imported name is a module, not a symbol, so there is nothing to bind to —
/// but importing a module IS a dependency on it. It is also the backstop for the
/// Python module-member path and for TS namespace usage: it records the
/// dependency even when the used member is re-exported elsewhere, or the usage is
/// module-level code that is not extracted as a call. A NAMED TS/JS import binds a
/// symbol, not a module, and is deliberately left alone.
pub(super) fn resolve_module_import_to_file<C: ResolutionContext>(
    r: &UnresolvedRef,
    lang: Language,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    if r.reference_kind != "imports" || r.reference_name.contains('.') {
        return None;
    }

    for imp in imports {
        if imp.local_name != r.reference_name {
            continue;
        }

        let module_path = if imp.is_namespace || imp.is_default {
            // `import * as ns from './x'` / `import x from './x'` — the
            // dependency is on the module FILE. A default import binds a
            // (possibly renamed) local to whatever the module default-exports, so
            // the binding name is not findable as a symbol. An external module
            // resolves to no file, so `import React from 'react'` creates no edge.
            imp.source.clone()
        } else if lang == Language::Python {
            // `from . import certs` — the imported NAME is a submodule of the source.
            if imp.source.ends_with('.') {
                format!("{}{}", imp.source, imp.local_name)
            } else {
                format!("{}.{}", imp.source, imp.local_name)
            }
        } else {
            // A named TS/JS import binds a symbol, not a module.
            continue;
        };

        if let Some(resolved) = resolve_import_path(&module_path, &r.file_path, lang, ctx)
            && resolved != r.file_path
            && let Some(node) = file_node_at(&resolved, ctx)
        {
            return Some(imported(r, &node.id, 0.9));
        }

        // Python's absolute `from a.b import submodule` (a FastAPI router
        // aggregator's `from app.api.routes import authentication`):
        // `resolve_import_path` maps only RELATIVE dotted paths.
        if lang == Language::Python
            && let Some(node) = find_python_module_file(&module_path, &r.file_path, ctx)
        {
            return Some(imported(r, &node.id, 0.9));
        }
    }
    None
}
