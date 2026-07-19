//! Go cross-package references (`pkga.FuncX`) through the in-module import map.

use selene_core::{Language, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::imports::imported;
use crate::imports::path_helpers::parent_dir;
use crate::types::{ImportMapping, ResolvedRef};

/// Go `pkga.FuncX` — the receiver names an imported PACKAGE DIRECTORY, not a
/// symbol (**0.9**).
///
/// The generic file-based lookup cannot follow that: an import maps to a
/// *directory* containing one or more `.go` files (#388). The candidate must be
/// an **exported** Go node whose **immediate parent directory** is exactly the
/// package dir — matching loosely would let `pkga.FuncX` land on a `FuncX`
/// declared in `pkga/subpkg/`.
pub(super) fn resolve_go_cross_package<C: ResolutionContext>(
    r: &UnresolvedRef,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    let module = ctx.go_module()?;
    let dot = r.reference_name.find('.')?;
    if dot == 0 {
        return None;
    }
    let receiver = &r.reference_name[..dot];
    let member = &r.reference_name[dot + 1..];
    if member.is_empty() {
        return None;
    }

    for imp in imports {
        if imp.local_name != receiver {
            continue;
        }
        // Only an IN-MODULE import maps to a directory we know.
        let pkg_dir = if imp.source == module.module_path {
            String::new()
        } else if let Some(rest) = imp.source.strip_prefix(&format!("{}/", module.module_path)) {
            rest.to_string()
        } else {
            continue;
        };

        for node in ctx.nodes_by_name(member).iter() {
            if node.language != Language::Go || node.is_exported != Some(true) {
                continue;
            }
            // '\\' only appears in Windows-indexed paths — skip the per-candidate
            // alloc when absent (the common case).
            let same_pkg = if node.file_path.contains('\\') {
                parent_dir(&node.file_path.replace('\\', "/")) == pkg_dir
            } else {
                parent_dir(&node.file_path) == pkg_dir
            };
            if same_pkg {
                return Some(imported(r, &node.id, 0.9));
            }
        }
    }
    None
}
