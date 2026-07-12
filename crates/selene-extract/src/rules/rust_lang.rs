//! Rust rules — verbatim port of `languages/rust.ts` (map §11 rust row) plus
//! the Rust-specific pieces the TS CORE carried (`tree-sitter.ts`): the
//! `impl_item` ladder branch (`impl Trait for Type` → `implements` ref from
//! the type's node), `emitRustUseBindingRefs` (per-leaf `imports` refs for
//! `use` declarations, incl. `pub use` re-export hubs), trait supertrait
//! `extends` refs (the `trait_bounds` slice of `extractInheritance`), and
//! the generic-fallback variable shape for `const_item`/`static_item`. All
//! hook-hosted in [`RustRules::visit_node`]: the walker is owned by the
//! parallel core chain, and the [`Session`] surface reaches everything these
//! need. Observable output is identical to the TS core placement, except
//! that a trait's supertrait refs are emitted against the trait node's
//! *predicted* id (`selene_core::node_id` is deterministic) just before the
//! ladder mints that node — same refs, same ids.
//!
//! Kept TS quirks (do not "fix"):
//! - `getVisibility` checks `text.includes('pub')`, so `pub(crate)`/
//!   `pub(super)` report **Public** (extraction-langs.md §port notes —
//!   intended).
//! - No `isConst` hook: `const_item`/`static_item` symbols are kind
//!   `variable`, not `constant` (the TS config never set one).
//! - The variable fallback scans ALL named children for identifiers, so a
//!   bare-identifier initializer (`const B: T = A;`) also mints a node for
//!   `A` — TS generic-fallback behavior, ported as-is.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::{EdgeKind, NodeKind, Visibility, node_id};
use tree_sitter::Node;

use crate::UnresolvedReference;
use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::{ImportInfo, LanguageRules, NodeTypeTables, scope_is_class_like};
use crate::walker::{NodeExtra, Session};

static TABLES: NodeTypeTables = NodeTypeTables {
    // `function_signature_item` is a bodiless trait-method DECLARATION
    // (`fn render(&self);`) — extracting it makes a trait's method set
    // first-class (impl-navigation + trait-dispatch synthesis match a
    // struct's method set against it).
    function_types: &["function_item", "function_signature_item"],
    // Rust has impl blocks, not classes.
    method_types: &["function_item", "function_signature_item"],
    interface_types: &["trait_item"],
    struct_types: &["struct_item"],
    enum_types: &["enum_item"],
    enum_member_types: &["enum_variant"],
    type_alias_types: &["type_item"],
    import_types: &["use_declaration"],
    call_types: &["call_expression"],
    variable_types: &["let_declaration", "const_item", "static_item"],
    interface_kind: Some(NodeKind::Trait),
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: Some("return_type"),
    ..NodeTypeTables::EMPTY
};

static BARE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"^[A-Za-z_]\w*$").unwrap()
});
static GENERIC_ARGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"<[^>]*>").unwrap()
});

pub(crate) struct RustRules;

impl LanguageRules for RustRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// A Rust function's declared return type, normalized to the bare type a
    /// chained `Foo::new().bar()` could be called on (#645/#608): `&Foo`
    /// unwraps, generics reduce to the base, `-> Self` yields the marker
    /// `"self"` (resolved to the impl's own type at resolution time, like
    /// PHP's `self`); primitives / unit / tuple yield `None`.
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut rt = get_child_by_field(node, "return_type")?;
        if rt.kind() == "reference_type" {
            let mut cursor = rt.walk();
            rt = rt
                .named_children(&mut cursor)
                .find(|c| {
                    matches!(
                        c.kind(),
                        "type_identifier" | "scoped_type_identifier" | "generic_type"
                    )
                })
                .unwrap_or(rt);
        }
        if matches!(rt.kind(), "primitive_type" | "unit_type" | "tuple_type") {
            return None;
        }
        let text = get_node_text(rt, source).trim();
        let text = GENERIC_ARGS_RE.replace_all(text, "");
        let last = text.rsplit("::").next()?.trim();
        if last.is_empty() || !BARE_NAME_RE.is_match(last) {
            return None;
        }
        Some(if last == "Self" {
            "self".to_string()
        } else {
            last.to_string()
        })
    }

    /// `(params) -> ReturnType` (the `" -> "` join is the TS spelling).
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = get_child_by_field(node, "return_type") {
            sig.push_str(" -> ");
            sig.push_str(get_node_text(rt, source));
        }
        Some(sig)
    }

    /// `async fn` — DIVERGENCE vs the TS config: tree-sitter-rust 0.24 wraps
    /// the keyword in a `function_modifiers` child (`(function_item
    /// (function_modifiers) …)`); the WASM-era grammar exposed `async` as a
    /// direct child (which TS checked). Both shapes are checked so a future
    /// grammar shift can't silently drop the flag (the same both-shapes
    /// pattern as Python's `is_async`).
    fn is_async(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        let mut cursor = node.walk();
        Some(node.children(&mut cursor).any(|c| {
            c.kind() == "async" || {
                c.kind() == "function_modifiers" && {
                    let mut c2 = c.walk();
                    c.children(&mut c2).any(|m| m.kind() == "async")
                }
            }
        }))
    }

    /// Default Private; any `visibility_modifier` containing `pub` — incl.
    /// `pub(crate)` — reports Public (intended TS quirk, module docs).
    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "visibility_modifier" {
                return Some(if get_node_text(child, source).contains("pub") {
                    Visibility::Public
                } else {
                    Visibility::Private
                });
            }
        }
        Some(Visibility::Private)
    }

    /// Receiver = the LAST direct `type_identifier` child of the enclosing
    /// `impl_item` (`impl Trait for Type` puts the trait first); generic
    /// `impl<T> MyStruct<T>` unwraps the `generic_type`'s inner identifier.
    /// Flips function→method and drives the same-file owner `contains` edge
    /// in the walker core.
    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut parent = node.parent();
        while let Some(p) = parent {
            if p.kind() == "impl_item" {
                let mut cursor = p.walk();
                let type_idents: Vec<Node<'_>> = p
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "type_identifier")
                    .collect();
                if let Some(last) = type_idents.last() {
                    return Some(get_node_text(*last, source).to_string());
                }
                let mut cursor = p.walk();
                let generic = p
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "generic_type")?;
                let mut c2 = generic.walk();
                let inner = generic
                    .named_children(&mut c2)
                    .find(|c| c.kind() == "type_identifier")?;
                return Some(get_node_text(inner, source).to_string());
            }
            parent = p.parent();
        }
        None
    }

    /// Root crate/module segment of a `use` path (`crate`/`super`/`self`
    /// kept). Consulted by `visit_node` below, which owns the whole
    /// use_declaration; declared here too so the hook surface matches the TS
    /// config shape.
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let use_arg = find_use_arg(node)?;
        Some(ImportInfo {
            module_name: root_module(use_arg, source),
            signature: get_node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }

    /// The TS-core Rust branches, hook-hosted (module docs): use-declaration
    /// imports (+ per-leaf binding refs), `impl Trait for Type` implements
    /// refs, trait supertrait extends refs, and the const/static variable
    /// fallback.
    fn visit_node(&self, node: Node<'_>, session: &mut Session<'_>) -> bool {
        match node.kind() {
            "use_declaration" => {
                extract_rust_use(self, node, session);
                true
            }
            "impl_item" => {
                emit_impl_trait_ref(node, session);
                false // ladder descends: fns inside become receiver methods
            }
            "trait_item" => {
                emit_supertrait_refs(node, session);
                false // ladder mints the trait node + walks its body
            }
            "const_item" | "static_item" | "let_declaration" => {
                if scope_is_class_like(session) {
                    return false; // TS parity: skipped inside class-like scopes
                }
                extract_rust_variables(node, session);
                true
            }
            _ => false,
        }
    }
}

/// The use argument: `scoped_use_list` / `scoped_identifier` / `use_list` /
/// `identifier` direct child. A `use_wildcard` (`use m::*;`) matches none —
/// exactly TS, which extracted nothing for bare-wildcard uses.
fn find_use_arg(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| {
        matches!(
            c.kind(),
            "scoped_use_list" | "scoped_identifier" | "use_list" | "identifier"
        )
    })
}

/// Root crate/module of a scoped path, recursing nested `scoped_identifier`s;
/// `crate`/`super`/`self` are kept as the root.
fn root_module(n: Node<'_>, source: &str) -> String {
    let Some(first) = n.named_child(0) else {
        return get_node_text(n, source).to_string();
    };
    match first.kind() {
        "identifier" | "crate" | "super" | "self" => get_node_text(first, source).to_string(),
        "scoped_identifier" => root_module(first, source),
        _ => get_node_text(first, source).to_string(),
    }
}

/// Import node (root module) + module `imports` ref + per-leaf binding refs
/// (`emitRustUseBindingRefs`): `use crate::m::{Widget, gadget as G}` links
/// `crate::m::Widget` and `crate::m::gadget` (the alias links its SOURCE
/// path — the definition). Covers `pub use` re-export hubs (#tokio-style
/// `mod.rs`) and items imported but used in non-call/non-type positions.
fn extract_rust_use(rules: &RustRules, node: Node<'_>, s: &mut Session<'_>) {
    let Some(info) = rules.extract_import(node, s.source()) else {
        return; // wildcard-only use: nothing extracted (TS parity)
    };
    s.create_node(
        &RustRules,
        NodeKind::Import,
        &info.module_name,
        node,
        NodeExtra {
            signature: Some(info.signature.clone()),
            ..NodeExtra::default()
        },
    );
    let Some(parent_id) = s.node_stack().last().cloned() else {
        return;
    };
    if !info.module_name.is_empty() {
        s.add_unresolved(UnresolvedReference {
            from_node_id: parent_id.clone(),
            reference_name: info.module_name,
            reference_kind: EdgeKind::Imports.as_str().to_string(),
            line: Some(u32::try_from(node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }
    emit_use_binding_refs(node, &parent_id, s);
}

/// Collect every bound path in a use declaration (`use_as_clause` follows its
/// `path`; `use_wildcard` contributes nothing), then emit one `imports` ref
/// per path whose leaf is a real name (not `self`/`super`/`crate`/`*`).
fn emit_use_binding_refs(node: Node<'_>, from_id: &str, s: &mut Session<'_>) {
    fn join(prefix: &str, seg: &str) -> String {
        if prefix.is_empty() {
            seg.to_string()
        } else {
            format!("{prefix}::{seg}")
        }
    }
    fn collect<'t>(n: Node<'t>, prefix: &str, source: &str, out: &mut Vec<(String, Node<'t>)>) {
        match n.kind() {
            "identifier" => out.push((join(prefix, get_node_text(n, source)), n)),
            "scoped_identifier" => {
                let full = get_node_text(n, source).trim().to_string();
                out.push((join(prefix, &full), n));
            }
            "scoped_use_list" => {
                let seg = get_child_by_field(n, "path")
                    .map(|p| get_node_text(p, source).trim().to_string())
                    .unwrap_or_default();
                let new_prefix = if seg.is_empty() {
                    prefix.to_string()
                } else {
                    join(prefix, &seg)
                };
                let list = get_child_by_field(n, "list").or_else(|| {
                    let mut c = n.walk();
                    n.named_children(&mut c).find(|x| x.kind() == "use_list")
                });
                if let Some(list) = list {
                    collect(list, &new_prefix, source, out);
                }
            }
            "use_list" => {
                let mut c = n.walk();
                let kids: Vec<Node<'t>> = n.named_children(&mut c).collect();
                for k in kids {
                    collect(k, prefix, source, out);
                }
            }
            "use_as_clause" => {
                let p = get_child_by_field(n, "path").or_else(|| n.named_child(0));
                if let Some(p) = p {
                    collect(p, prefix, source, out);
                }
            }
            _ => {} // use_wildcard → no specific binding to link
        }
    }

    let mut paths: Vec<(String, Node<'_>)> = Vec::new();
    let mut cursor = node.walk();
    let kids: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for c in kids {
        collect(c, "", s.source(), &mut paths);
    }
    for (text, pos) in paths {
        let leaf = text.rsplit("::").next().unwrap_or("");
        if leaf.is_empty() || matches!(leaf, "self" | "super" | "crate" | "*") {
            continue;
        }
        s.add_unresolved(UnresolvedReference {
            from_node_id: from_id.to_string(),
            reference_name: text,
            reference_kind: EdgeKind::Imports.as_str().to_string(),
            line: Some(u32::try_from(pos.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(pos.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }
}

/// `impl Trait for Type` → an `implements` ref FROM the implementing type's
/// (already-extracted, same-file) struct/enum/class node to the trait name
/// (scoped paths kept whole, e.g. `std::fmt::Display`). Plain `impl Type`
/// blocks (no `for`) emit nothing.
fn emit_impl_trait_ref(node: Node<'_>, s: &mut Session<'_>) {
    let mut cursor = node.walk();
    let has_for = node
        .children(&mut cursor)
        .any(|c| c.kind() == "for" && !c.is_named());
    if !has_for {
        return;
    }

    let mut cursor = node.walk();
    let type_idents: Vec<Node<'_>> = node
        .named_children(&mut cursor)
        .filter(|c| {
            matches!(
                c.kind(),
                "type_identifier" | "generic_type" | "scoped_type_identifier"
            )
        })
        .collect();
    if type_idents.len() < 2 {
        return;
    }
    let trait_node = type_idents[0];
    let type_node = type_idents[type_idents.len() - 1];

    let trait_name = get_node_text(trait_node, s.source()).to_string();
    let type_name = if type_node.kind() == "generic_type" {
        let mut c = type_node.walk();
        type_node
            .named_children(&mut c)
            .find(|c| c.kind() == "type_identifier")
            .map(|inner| get_node_text(inner, s.source()).to_string())
            .unwrap_or_else(|| get_node_text(type_node, s.source()).to_string())
    } else {
        get_node_text(type_node, s.source()).to_string()
    };

    let owner_id = s
        .nodes()
        .iter()
        .find(|n| {
            n.name == type_name
                && matches!(n.kind, NodeKind::Struct | NodeKind::Enum | NodeKind::Class)
        })
        .map(|n| n.id.clone());
    if let Some(from) = owner_id {
        s.add_unresolved(UnresolvedReference {
            from_node_id: from,
            reference_name: trait_name,
            reference_kind: EdgeKind::Implements.as_str().to_string(),
            line: Some(u32::try_from(trait_node.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(trait_node.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }
}

/// `trait Sub: Super + Display` → one `extends` ref per bound (the
/// `trait_bounds` slice of the TS core's `extractInheritance`). The refs
/// come FROM the trait's node id, which the ladder mints right after this
/// hook returns — `node_id` is deterministic, so the id is predicted here
/// (same inputs: file, `Trait` kind via `interface_kind`, name field,
/// 1-based start line).
fn emit_supertrait_refs(node: Node<'_>, s: &mut Session<'_>) {
    let Some(bounds) = node.child_by_field_name("bounds") else {
        return;
    };
    if bounds.kind() != "trait_bounds" {
        return;
    }
    let Some(name_node) = get_child_by_field(node, "name") else {
        return;
    };
    let name = get_node_text(name_node, s.source());
    if name.is_empty() {
        return;
    }
    let line = u32::try_from(node.start_position().row).unwrap_or(u32::MAX - 1) + 1;
    let trait_id = node_id(s.file_path(), NodeKind::Trait, name, line);

    let mut cursor = bounds.walk();
    let bound_nodes: Vec<Node<'_>> = bounds.named_children(&mut cursor).collect();
    for bound in bound_nodes {
        let (type_name, pos_node) = match bound.kind() {
            "type_identifier" => (Some(get_node_text(bound, s.source()).to_string()), bound),
            "generic_type" => {
                let mut c = bound.walk();
                match bound
                    .named_children(&mut c)
                    .find(|c| c.kind() == "type_identifier")
                {
                    Some(inner) => (Some(get_node_text(inner, s.source()).to_string()), inner),
                    None => (None, bound),
                }
            }
            "higher_ranked_trait_bound" => {
                let mut c = bound.walk();
                let generic = bound
                    .named_children(&mut c)
                    .find(|c| c.kind() == "generic_type");
                let type_id = generic
                    .and_then(|g| {
                        let mut c2 = g.walk();
                        g.named_children(&mut c2)
                            .find(|c| c.kind() == "type_identifier")
                    })
                    .or_else(|| {
                        let mut c3 = bound.walk();
                        bound
                            .named_children(&mut c3)
                            .find(|c| c.kind() == "type_identifier")
                    });
                match type_id {
                    Some(t) => (Some(get_node_text(t, s.source()).to_string()), t),
                    None => (None, bound),
                }
            }
            _ => (None, bound),
        };
        if let Some(type_name) = type_name {
            s.add_unresolved(UnresolvedReference {
                from_node_id: trait_id.clone(),
                reference_name: type_name,
                reference_kind: EdgeKind::Extends.as_str().to_string(),
                line: Some(u32::try_from(pos_node.start_position().row).unwrap_or(0) + 1),
                column: Some(u32::try_from(pos_node.start_position().column).unwrap_or(0)),
                file_path: None,
                language: None,
            });
        }
    }
}

/// The TS generic-fallback variable shape for `const_item`/`static_item`/
/// `let_declaration`: one `variable` node per direct identifier child
/// (kind `variable` — no isConst hook, module docs), `{docstring,
/// isExported:false}`, anchored at the identifier.
fn extract_rust_variables(node: Node<'_>, s: &mut Session<'_>) {
    let docstring = get_preceding_docstring(node, s.source());
    let mut cursor = node.walk();
    let kids: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in kids {
        if child.kind() != "identifier" {
            continue;
        }
        let name = get_node_text(child, s.source()).to_string();
        if name.is_empty() {
            continue;
        }
        s.create_node(
            &RustRules,
            NodeKind::Variable,
            &name,
            child,
            NodeExtra {
                docstring: docstring.clone(),
                is_exported: Some(false),
                ..NodeExtra::default()
            },
        );
    }
}
