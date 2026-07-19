//! Exported-symbol lookup: the re-export chase (`find_exported_symbol`) and the
//! static-member descent (#825).

use std::collections::HashSet;
use std::sync::Arc;

use selene_core::{Language, Node, NodeKind, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::imports::Want;
use crate::imports::path_walk::resolve_import_path;
use crate::types::ReExport;

/// How deep a re-export chase may go before it gives up. A barrel-of-barrels is
/// real; an infinite one is a cycle, and the `visited` set catches that — this
/// cap catches the pathological-but-acyclic case.
pub const REEXPORT_MAX_DEPTH: usize = 8;

/// Node kinds that own static members reachable as `Container.member` (#825).
const STATIC_MEMBER_CONTAINERS: [NodeKind; 6] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Enum,
    NodeKind::Trait,
    NodeKind::Protocol,
];

/// The symbol a module exports under `want` — chasing re-exports.
///
/// Order: a **direct hit** in the file, then **named** re-exports (following the
/// rename), then **wildcard** re-exports (the barrel-of-barrels case). Capped at
/// [`REEXPORT_MAX_DEPTH`], with a `visited` set so a cyclic barrel terminates.
pub(super) fn find_exported_symbol<C: ResolutionContext>(
    file_path: &str,
    want: &Want,
    lang: Language,
    ctx: &C,
    visited: &mut HashSet<String>,
    depth: usize,
) -> Option<Arc<Node>> {
    if depth > REEXPORT_MAX_DEPTH || !visited.insert(file_path.to_string()) {
        return None;
    }

    let nodes = ctx.nodes_in_file(file_path);

    // 1. A direct hit: the symbol is declared right here.
    let direct = if want.is_default {
        // A Svelte/Vue single-file component IS the module's default export, but
        // extracts as kind `component` — so prefer it, then fall back to an
        // exported function/class (the `export default fn` case). Without the
        // component branch, `export { default as X } from './X.svelte'` never
        // resolves and the component shows a false 0 callers (#629).
        nodes
            .iter()
            .find(|n| n.is_exported == Some(true) && n.kind == NodeKind::Component)
            .or_else(|| {
                nodes.iter().find(|n| {
                    n.is_exported == Some(true)
                        && matches!(n.kind, NodeKind::Function | NodeKind::Class)
                })
            })
    } else if want.is_namespace
        && let Some(member) = &want.member_name
    {
        nodes
            .iter()
            .find(|n| n.name == *member && n.is_exported == Some(true))
    } else {
        nodes
            .iter()
            .find(|n| n.name == want.exported_name && n.is_exported == Some(true))
    };
    if let Some(hit) = direct {
        return Some(hit.clone());
    }

    // 2. A re-export hit: this file forwards the symbol somewhere else.
    let re_exports = ctx.re_exports(file_path);
    if re_exports.is_empty() {
        return None;
    }

    let target_name = if want.is_default {
        "default"
    } else {
        want.exported_name.as_str()
    };

    // Named re-exports first — and the RENAME is followed: to chase `login`
    // through `export { signIn as login } from './auth'`, look for `signIn`.
    for rex in re_exports.iter() {
        if let ReExport::Named {
            exported_name,
            original_name,
            source,
        } = rex
            && exported_name == target_name
            && let Some(next) = resolve_import_path(source, file_path, lang, ctx)
        {
            let chained = Want {
                is_default: original_name == "default",
                is_namespace: false,
                exported_name: original_name.clone(),
                member_name: None,
            };
            if let Some(hit) = find_exported_symbol(&next, &chained, lang, ctx, visited, depth + 1)
            {
                return Some(hit);
            }
        }
    }

    // 3. Wildcard re-exports last — try every forwarding source.
    for rex in re_exports.iter() {
        if let ReExport::Wildcard { source } = rex
            && let Some(next) = resolve_import_path(source, file_path, lang, ctx)
            && let Some(hit) = find_exported_symbol(&next, want, lang, ctx, visited, depth + 1)
        {
            return Some(hit);
        }
    }

    None
}

/// `Container.member` on a NAMED class import → the member node (#825).
///
/// Members carry a `Container::member` qualified name, so look up
/// `{container.qualified_name}::{member}` **within the container's own file** —
/// the file filter is what disambiguates same-named classes in other modules.
/// `None` when the container is not a member-owning kind or the member is absent,
/// so the caller falls back to the container itself.
pub(super) fn resolve_static_member<C: ResolutionContext>(
    container: &Node,
    r: &UnresolvedRef,
    local_name: &str,
    ctx: &C,
) -> Option<Arc<Node>> {
    if !STATIC_MEMBER_CONTAINERS.contains(&container.kind) {
        return None;
    }
    // The first segment after the receiver: `Foo.bar.baz` → `bar`.
    let member = r.reference_name[local_name.len() + 1..].split('.').next()?;
    if member.is_empty() {
        return None;
    }

    let candidates: Vec<Arc<Node>> = ctx
        .nodes_by_qualified_name(&format!("{}::{member}", container.qualified_name))
        .iter()
        .filter(|n| n.file_path == container.file_path)
        .cloned()
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // A CALL prefers a callable member when several nodes share the qualified
    // name (a static property and a method can collide).
    if r.reference_kind == "calls"
        && let Some(callable) = candidates
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Method | NodeKind::Function))
    {
        return Some(callable.clone());
    }
    candidates.into_iter().next()
}
