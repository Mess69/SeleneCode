//! The declaration extractors — functions/methods, classes, interfaces,
//! structs, enums (+ members) — and decorator refs.

use selene_core::{Edge, EdgeKind, NodeKind, Provenance};
use tree_sitter::Node;

use crate::UnresolvedReference;
use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::LanguageRules;

use super::{NodeExtra, Session, extract_inheritance, extract_name, resolve_body, visit};

pub(super) fn extract_function(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) {
    extract_function_named(rules, s, node, None);
}

/// [`extract_function`] with an optional explicit name — supplied only for
/// explicitly-named anonymous functions the caller resolved itself (object-
/// literal function members, RTK endpoints — `src/walker/ts_core.rs`).
pub(super) fn extract_function_named(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
    name_override: Option<&str>,
) {
    // Receiver-typed functions (Rust impl fns, Task 9) route to method.
    if rules.get_receiver_type(node, s.source()).is_some() {
        extract_method(rules, s, node);
        return;
    }

    let mut name = name_override
        .map(str::to_string)
        .unwrap_or_else(|| extract_name(rules, node, s.source()));
    // TS/JS arrow-const naming: `const useAuth = () => {…}` — the arrow
    // node has no `name` field; the name lives on the parent
    // `variable_declarator` (Task 7).
    if name_override.is_none()
        && name == "<anonymous>"
        && (node.kind() == "arrow_function" || node.kind() == "function_expression")
        && let Some(parent) = node.parent()
        && parent.kind() == "variable_declarator"
        && let Some(var_name) = get_child_by_field(parent, "name")
    {
        name = get_node_text(var_name, s.source()).to_string();
    }
    if name == "<anonymous>" || rules.is_misparsed_function(&name, node) {
        // No node, but the body is still walked — AMD/CommonJS module
        // wrappers hold named inner functions and calls (#528).
        if let Some(body) = resolve_body(rules, node) {
            s.visit_function_body(body, "");
        }
        return;
    }

    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        signature: rules.get_signature(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        is_async: rules.is_async(node, s.source()),
        is_static: rules.is_static(node, s.source()),
        return_type: rules.get_return_type(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(NodeKind::Function, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // Type refs from parameter/return annotations (Task 8).
    s.extract_type_annotations(rules, node, &id);
    extract_decorators_for(s, node, &id);

    s.push_scope(id.clone());
    if let Some(body) = resolve_body(rules, node) {
        s.visit_function_body(body, &id);
    }
    s.pop_scope();
}

pub(super) fn extract_method(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) {
    let receiver = rules.get_receiver_type(node, s.source());

    if !s.is_inside_class_like() && !rules.tables().methods_are_top_level && receiver.is_none() {
        // (Object-literal method skip — TS/JS, Task 7.) Not in a class and
        // no receiver: it's a function.
        extract_function(rules, s, node);
        return;
    }

    let name = extract_name(rules, node, s.source());
    if rules.is_misparsed_function(&name, node) {
        if let Some(body) = resolve_body(rules, node) {
            s.visit_function_body(body, "");
        }
        return;
    }

    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        signature: rules.get_signature(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_async: rules.is_async(node, s.source()),
        is_static: rules.is_static(node, s.source()),
        return_type: rules.get_return_type(node, s.source()),
        // Receiver methods: `Receiver::name` qualified-name override.
        qualified_name: receiver.as_ref().map(|r| format!("{r}::{name}")),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(NodeKind::Method, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // Receiver method with no class-like parent (Rust impl blocks, Task 9):
    // contains edge from the same-file owner node found by name.
    if let Some(recv) = receiver
        && !s.is_inside_class_like()
    {
        let owner = s.nodes.iter().find(|n| {
            n.name == recv
                && matches!(
                    n.kind,
                    NodeKind::Struct | NodeKind::Class | NodeKind::Enum | NodeKind::Trait
                )
        });
        if let Some(owner_id) = owner.map(|o| o.id.clone()) {
            s.add_edge(Edge {
                source: owner_id,
                target: id.clone(),
                kind: EdgeKind::Contains,
                metadata: None,
                line: None,
                column: None,
                provenance: Some(Provenance::TreeSitter),
            });
        }
    }

    // Type refs from parameter/return annotations (Task 8).
    s.extract_type_annotations(rules, node, &id);
    extract_decorators_for(s, node, &id);

    s.push_scope(id.clone());
    if let Some(body) = resolve_body(rules, node) {
        s.visit_function_body(body, &id);
    }
    s.pop_scope();
}

pub(super) fn extract_class(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
    kind: NodeKind,
) {
    let body = resolve_body(rules, node);
    if rules.tables().skip_bodiless_class && body.is_none() {
        return; // forward declaration (#1093)
    }

    let name = extract_name(rules, node, s.source());
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(kind, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // extends/implements refs (TS core calls it here: tree-sitter.ts:1642).
    extract_inheritance(s, node, &id);
    // A C# primary constructor's parameter types are the type's declared
    // dependencies — and EVERY positional record carries one
    // (tree-sitter.ts:1645, #237).
    s.extract_csharp_primary_ctor_param_refs(node, &id);
    extract_decorators_for(s, node, &id);

    s.push_scope(id);
    let body = body.unwrap_or(node);
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
    // Lombok synthesis (Task 10) runs here, class still on the stack.
    rules.synthesize_members(node, s);
    s.pop_scope();
}

pub(super) fn extract_interface(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) {
    let name = extract_name(rules, node, s.source());
    let kind = rules.tables().interface_kind.unwrap_or(NodeKind::Interface);
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(kind, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // Interface inheritance refs (tree-sitter.ts:1788).
    extract_inheritance(s, node, &id);

    s.push_scope(id);
    let body = resolve_body(rules, node).unwrap_or(node);
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
    s.pop_scope();
}

pub(super) fn extract_struct(
    rules: &'static dyn LanguageRules,
    s: &mut Session<'_>,
    node: Node<'_>,
) {
    // No body = forward declaration / type reference, not a definition.
    let Some(body) = resolve_body(rules, node) else {
        return;
    };
    let name = extract_name(rules, node, s.source());
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(NodeKind::Struct, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    // Struct inheritance + C# primary-ctor deps (tree-sitter.ts:1829, 1833).
    // A `record struct` lands here via `classify_class_node`.
    extract_inheritance(s, node, &id);
    s.extract_csharp_primary_ctor_param_refs(node, &id);

    s.push_scope(id);
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        visit(rules, s, child);
    }
    s.pop_scope();
}

pub(super) fn extract_enum(rules: &'static dyn LanguageRules, s: &mut Session<'_>, node: Node<'_>) {
    let Some(body) = resolve_body(rules, node) else {
        return;
    };
    let name = extract_name(rules, node, s.source());
    let extra = NodeExtra {
        docstring: get_preceding_docstring(node, s.source()),
        visibility: rules.get_visibility(node, s.source()),
        is_exported: rules.is_exported(node, s.source()),
        ..NodeExtra::default()
    };
    let Some(idx) = s.create_node(NodeKind::Enum, &name, node, extra) else {
        return;
    };
    let Some(id) = s.id_of(idx) else { return };

    s.push_scope(id);
    let member_types = rules.tables().enum_member_types;
    let mut cursor = body.walk();
    let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
    for child in children {
        if member_types.contains(&child.kind()) {
            extract_enum_members(s, child);
        } else {
            visit(rules, s, child);
        }
    }
    s.pop_scope();
}

/// Enum member names: `name` field first (Rust enum_variant), else
/// identifier-like children (multi-case declarations), else the node itself
/// when it is a bare identifier.
pub(super) fn extract_enum_members(s: &mut Session<'_>, node: Node<'_>) {
    if let Some(name_node) = get_child_by_field(node, "name") {
        let name = get_node_text(name_node, s.source()).to_string();
        s.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let mut found = false;
    for child in children {
        if matches!(
            child.kind(),
            "simple_identifier" | "identifier" | "property_identifier"
        ) {
            let name = get_node_text(child, s.source()).to_string();
            s.create_node(NodeKind::EnumMember, &name, child, NodeExtra::default());
            found = true;
        }
    }
    if !found && node.named_child_count() == 0 {
        let name = get_node_text(node, s.source()).to_string();
        s.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
    }
}

/// Decorators/annotations on a declaration → `decorates` refs: direct
/// children (+ inside a `modifiers` wrapper), then preceding siblings
/// stopping at the first non-decorator. Callee unwrapped from invoked
/// decorators; generic args and qualifier prefixes stripped.
pub(super) fn extract_decorators_for(s: &mut Session<'_>, decl: Node<'_>, decorated_id: &str) {
    fn consider(s: &mut Session<'_>, n: Node<'_>, decorated_id: &str) {
        // (Solidity modifier_invocation branch — wave 2.)
        if !matches!(
            n.kind(),
            "decorator" | "annotation" | "marker_annotation" | "attribute"
        ) {
            return;
        }
        // Find the leading identifier: skip the `@` punct, unwrap a
        // `call_expression` if the decorator is invoked with args
        // (tree-sitter.ts:4799-4822).
        //
        // The accepted node types are TS's list EXACTLY, and the two that are
        // NOT in it are load-bearing omissions:
        //
        // - Python's call node is `call`, not `call_expression`, and its callee
        //   `app.route` is an `attribute`. Accepting either made `@app.route("/x")`
        //   emit a bogus `decorates:route` — the LAST dotted segment, which names
        //   nothing: there is no `route` symbol, the decorator is `app.route`. TS
        //   matches neither node type, so `target` stays null and it emits NO
        //   `decorates` ref at all; the hop is already carried by `calls:app.route`
        //   from the ordinary call walk. This was a Rust-side OVER-emission — the
        //   only one the parity corpus has ever found.
        let mut target: Option<Node<'_>> = None;
        let mut cursor = n.walk();
        let children: Vec<Node<'_>> = n.named_children(&mut cursor).collect();
        for child in children {
            if child.kind() == "call_expression" {
                target = get_child_by_field(child, "function").or_else(|| child.named_child(0));
                if target.is_some() {
                    break;
                }
            }
            if matches!(
                child.kind(),
                "identifier"
                    | "member_expression"
                    | "scoped_identifier"
                    | "navigation_expression"
                    | "user_type"
                    | "type_identifier"
            ) {
                target = Some(child);
                break;
            }
        }
        let Some(target) = target else { return };
        let mut name = get_node_text(target, s.source()).to_string();
        if let Some(lt) = name.find('<')
            && lt > 0
        {
            name.truncate(lt);
        }
        let last_dot = name.rfind('.').map(|i| i + 1);
        let last_colons = name.rfind("::").map(|i| i + 2);
        if let Some(cut) = last_dot.max(last_colons) {
            name = name[cut..].trim_start_matches([':', '.']).to_string();
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        s.add_unresolved(UnresolvedReference {
            from_node_id: decorated_id.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::Decorates.as_str().to_string(),
            line: Some(u32::try_from(n.start_position().row).unwrap_or(0) + 1),
            column: Some(u32::try_from(n.start_position().column).unwrap_or(0)),
            file_path: None,
            language: None,
        });
    }

    // 1. Direct children (+ descend into a `modifiers` wrapper).
    let mut cursor = decl.walk();
    let children: Vec<Node<'_>> = decl.named_children(&mut cursor).collect();
    for child in children {
        consider(s, child, decorated_id);
        if child.kind() == "modifiers" {
            let mut c2 = child.walk();
            let inner: Vec<Node<'_>> = child.named_children(&mut c2).collect();
            for j in inner {
                consider(s, j, decorated_id);
            }
        }
    }

    // 2. Preceding siblings, walking backwards, stopping at the first
    // non-decorator (so an earlier declaration's decorators never leak in).
    let mut sib = decl.prev_named_sibling();
    while let Some(p) = sib {
        if !matches!(p.kind(), "decorator" | "annotation" | "marker_annotation") {
            break;
        }
        consider(s, p, decorated_id);
        sib = p.prev_named_sibling();
    }
}
