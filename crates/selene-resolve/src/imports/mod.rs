//! Import resolution: the *inputs* (Task 4 — the four submodules below), the
//! module-specifier → file walk ([`resolve_import_path`], Task 5), and
//! `resolve_via_import` (Task 6).
//!
//! # ⚠ A shared seam
//!
//! Task 5 created [`resolve_import_path`] here and Task 6 appends
//! `resolve_via_import` — **strictly sequential**. The four input modules
//! (`mappings`, `aliases`, `workspace`, `go_module`) are independent of each
//! other and of 5/6.

pub mod aliases;
pub mod cpp_includes;
pub mod go_module;
pub mod mappings;
pub mod workspace;

mod exports;
mod extensions;
mod external;
mod go_cross_package;
mod includes;
mod jvm;
mod module_imports;
mod path_helpers;
mod path_walk;
mod python_modules;
mod rust_paths;

pub use exports::REEXPORT_MAX_DEPTH;
pub use external::is_external_import;
pub use jvm::resolve_jvm_import;
pub use path_walk::resolve_import_path;

use std::collections::HashSet;

use selene_core::{Language, Node, NodeKind, UnresolvedRef};
use std::sync::Arc;

use crate::context::ResolutionContext;
use crate::imports::exports::{find_exported_symbol, resolve_static_member};
use crate::imports::go_cross_package::resolve_go_cross_package;
use crate::imports::includes::{resolve_c_include, resolve_php_include};
use crate::imports::jvm::resolve_jvm_imported_reference;
use crate::imports::module_imports::resolve_module_import_to_file;
use crate::imports::python_modules::{
    resolve_python_absolute_module, resolve_python_module_member,
};
use crate::imports::rust_paths::resolve_rust_path_reference;
use crate::types::{ResolvedBy, ResolvedRef};

// =============================================================================
// resolve_via_import (Task 6)
// =============================================================================

/// What `find_exported_symbol` is looking for in a module.
#[derive(Debug, Clone)]
struct Want {
    is_default: bool,
    is_namespace: bool,
    exported_name: String,
    member_name: Option<String>,
}

fn imported(r: &UnresolvedRef, target: &str, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        // ⚠ The STORED ROW, unmutated — the keyed delete matches on it (#760).
        original: r.clone(),
        target_node_id: target.to_string(),
        confidence,
        resolved_by: ResolvedBy::Import,
    }
}

/// Bind a reference through the imports its file declares.
///
/// Ladder step 8. The branch order below **is the contract** — the ecosystem
/// branches each own their reference shapes completely, and several of them
/// deliberately **do not fall through** on a miss (a path-shaped reference that
/// finds no file must not then go name-matching: a wrong edge is worse than
/// none, #660).
pub fn resolve_via_import<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    let lang = r.language;

    // --- C/C++ `#include` → a file→file edge -------------------------------
    if matches!(lang, Language::C | Language::Cpp) && r.reference_kind == "imports" {
        return resolve_c_include(r, lang, ctx);
    }

    // Wave 2 (Phase 8), each a NO-FALLTHROUGH branch of its own: COBOL
    // copybooks, Nix path imports (`import ./x.nix`).

    // --- PHP include/require → a file→file edge ----------------------------
    if crate::resolver::is_php_include_path_ref(r) {
        // NO FALLTHROUGH on a miss (#660): falling back to the symbol matcher
        // would mis-connect `inc/db.php` to an unrelated `db.php` elsewhere.
        return resolve_php_include(r, ctx);
    }

    let imports = ctx.import_mappings(&r.file_path);

    // --- Go cross-package (`pkga.FuncX`) ------------------------------------
    if lang == Language::Go
        && let Some(hit) = resolve_go_cross_package(r, &imports, ctx)
    {
        return Some(hit);
    }

    // --- Java/Kotlin (`Foo.bar()` / bare `Foo`, through an imported FQN) -----
    if matches!(lang, Language::Java | Language::Kotlin)
        && let Some(hit) = resolve_jvm_imported_reference(r, lang, &imports, ctx)
    {
        return Some(hit);
    }

    // --- Python module members + absolute dotted modules ---------------------
    if lang == Language::Python {
        if let Some(hit) = resolve_python_module_member(r, &imports, ctx) {
            return Some(hit);
        }
        if let Some(hit) = resolve_python_absolute_module(r, ctx) {
            return Some(hit);
        }
    }

    // --- Rust `crate::a::b::Item` / `self::` / `super::` ---------------------
    if lang == Language::Rust
        && r.reference_name.contains("::")
        && let Some(hit) = resolve_rust_path_reference(r, ctx)
    {
        return Some(hit);
    }

    // Wave 2: Lua/Luau `require(...)`.

    // --- whole-module / namespace imports → the module FILE ------------------
    if matches!(
        lang,
        Language::Python
            | Language::Typescript
            | Language::Tsx
            | Language::Javascript
            | Language::Jsx
            | Language::Arkts
    ) && let Some(hit) = resolve_module_import_to_file(r, lang, &imports, ctx)
    {
        return Some(hit);
    }

    // --- the generic loop: a name bound by an import --------------------------
    for imp in imports.iter() {
        let matches_bare = r.reference_name == imp.local_name;
        let matches_member = r
            .reference_name
            .starts_with(&format!("{}.", imp.local_name));
        if !matches_bare && !matches_member {
            continue;
        }

        let Some(resolved_path) = resolve_import_path(&imp.source, &r.file_path, lang, ctx) else {
            continue;
        };

        let want = Want {
            is_default: imp.is_default,
            is_namespace: imp.is_namespace,
            exported_name: if imp.is_default {
                "default".to_string()
            } else {
                imp.exported_name.clone()
            },
            member_name: if imp.is_namespace {
                Some(
                    r.reference_name
                        .replacen(&format!("{}.", imp.local_name), "", 1),
                )
            } else {
                None
            },
        };

        let Some(target) =
            find_exported_symbol(&resolved_path, &want, lang, ctx, &mut HashSet::new(), 0)
        else {
            continue;
        };

        // #825 — `Foo.bar()` on a NAMED class import: `find_exported_symbol`
        // resolved `Foo` to the class, so descend into it and bind the MEMBER.
        // Without this the edge points at the class, `create_edges` then promotes
        // the call to `instantiates`, and the static method shows zero callers
        // and a hollow impact radius.
        if !imp.is_namespace
            && matches_member
            && let Some(member) = resolve_static_member(&target, r, &imp.local_name, ctx)
        {
            return Some(imported(r, &member.id, 0.9));
        }

        return Some(imported(r, &target.id, 0.9));
    }

    None
}

/// The `file`-kind node at `path`.
fn file_node_at<C: ResolutionContext>(path: &str, ctx: &C) -> Option<Arc<Node>> {
    ctx.nodes_in_file(path)
        .iter()
        .find(|n| n.kind == NodeKind::File)
        .cloned()
}
