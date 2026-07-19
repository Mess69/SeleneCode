//! C/C++ `#include` and PHP `include`/`require` — each a file→file edge.

use selene_core::{Language, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::imports::extensions::extensions_for;
use crate::imports::path_helpers::{join_rel, parent_dir};
use crate::imports::path_walk::resolve_import_path;
use crate::imports::{file_node_at, imported};
use crate::types::ResolvedRef;

/// C/C++: the same-dir sibling first (**0.92**), then the `-I`-resolved path
/// (**0.9**).
///
/// A quoted `#include "X.h"` searches the INCLUDING file's own directory first
/// (the C standard's quoted-include order). Without that preference the include-dir
/// heuristic picks an arbitrary same-named header — a `windows/.../RNCAsyncStorage.h`
/// absorbing the include meant for the `apple/.../RNCAsyncStorage.h` next door —
/// and the real local header ends up with no dependents at all.
pub(super) fn resolve_c_include<C: ResolutionContext>(
    r: &UnresolvedRef,
    lang: Language,
    ctx: &C,
) -> Option<ResolvedRef> {
    let from_dir = parent_dir(&r.file_path);
    let sibling_path = join_rel(&from_dir, &r.reference_name);
    if let Some(node) = file_node_at(&sibling_path, ctx) {
        return Some(imported(r, &node.id, 0.92));
    }

    let resolved = resolve_import_path(&r.reference_name, &r.file_path, lang, ctx)?;
    let node = file_node_at(&resolved, ctx)?;
    Some(imported(r, &node.id, 0.9))
}

/// PHP `include`/`require` → the included file (**0.9**), or nothing.
///
/// PHP resolves an include relative to the INCLUDING file's directory (the
/// common case for procedural codebases); `php.ini`'s `include_path` is not
/// modeled. The literal may omit `.php`.
pub(super) fn resolve_php_include<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let from_dir = parent_dir(&r.file_path);
    let base = join_rel(&from_dir, &r.reference_name);

    let path = if ctx.file_exists(&base) {
        base
    } else {
        let mut found = None;
        for ext in extensions_for(Language::Php) {
            let candidate = format!("{base}{ext}");
            if ctx.file_exists(&candidate) {
                found = Some(candidate);
                break;
            }
        }
        found?
    };

    let node = file_node_at(&path, ctx)?;
    Some(imported(r, &node.id, 0.9))
}
