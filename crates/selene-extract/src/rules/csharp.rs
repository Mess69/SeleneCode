//! C# rules — verbatim port of `languages/csharp.ts` (map §11-adjacent row):
//! records are first-class type declarations (#237/#831 — the grammar parses
//! EVERY record form as `record_declaration`, `record struct` included;
//! [`LanguageRules::classify_class_node`] tells the value-type form apart by
//! its `struct` keyword child; `record_struct_declaration` in `struct_types`
//! is forward-compat only), namespaces (block + file-scoped) scope type
//! names via `package_types`, visibility defaults to Private, and
//! `const` / `static`+`readonly` fields are constants.
//!
//! `pre_parse` = [`blank_csharp_preprocessor_directives`] (#237): a `#if`
//! inside an enum member list detaches the enclosing class's members; both
//! branches are kept.

use std::sync::LazyLock;

use regex::Regex;
use selene_core::Visibility;
use tree_sitter::Node;

use selene_core::NodeKind;

use crate::helpers::{get_child_by_field, get_node_text, get_preceding_docstring};
use crate::rules::cpp_preparse::blank_csharp_preprocessor_directives;
use crate::rules::{ClassKind, ImportInfo, LanguageRules, NodeTypeTables};
use crate::walker::{NodeExtra, Session};

/// Generic args stripped the TS way (`<[^>]*>` → nothing; non-nested).
static GENERIC_ARGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"<[^>]*>").unwrap()
});

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &[],
    class_types: &["class_declaration", "record_declaration"],
    method_types: &["method_declaration", "constructor_declaration"],
    interface_types: &["interface_declaration"],
    struct_types: &["struct_declaration", "record_struct_declaration"],
    enum_types: &["enum_declaration"],
    enum_member_types: &["enum_member_declaration"],
    import_types: &["using_directive"],
    call_types: &["invocation_expression"],
    variable_types: &["local_declaration_statement"],
    field_types: &["field_declaration"],
    property_types: &["property_declaration"],
    // Both block (`namespace Foo { … }`) and file-scoped (`namespace Foo;`)
    // forms — the package pass pushes the namespace onto the scope so
    // nested/top-level types pick it up.
    package_types: &["namespace_declaration", "file_scoped_namespace_declaration"],
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: Some("type"),
    ..NodeTypeTables::EMPTY
};

/// The text of every direct `modifier` child, tested by `f`.
fn any_modifier(node: Node<'_>, source: &str, f: impl Fn(&str) -> bool) -> bool {
    let mut i = 0;
    while let Some(child) = node.child(i) {
        if child.kind() == "modifier" && f(get_node_text(child, source)) {
            return true;
        }
        i += 1;
    }
    false
}

pub(crate) struct CSharpRules;

impl LanguageRules for CSharpRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    fn pre_parse(&self, source: &str, _file_path: &str) -> Option<String> {
        match blank_csharp_preprocessor_directives(source) {
            std::borrow::Cow::Borrowed(_) => None,
            std::borrow::Cow::Owned(s) => Some(s),
        }
    }

    /// #831: positional records (`public record ValueRec(int X);`) have no
    /// body block — the walker's no-body struct gate exists for C/C++
    /// forward declarations, NOT records, so a bodiless record resolves to
    /// itself (its `parameter_list` children extract nothing, harmlessly).
    fn resolve_body<'t>(&self, node: Node<'t>, body_field: &str) -> Option<Node<'t>> {
        if node.kind() != "record_declaration" && node.kind() != "record_struct_declaration" {
            return None;
        }
        get_child_by_field(node, body_field).or(Some(node))
    }

    /// C# fields nest their declarators one level down
    /// (`field_declaration` → `variable_declaration` → `variable_declarator`),
    /// which the core field pass (direct-children declarators) can't see;
    /// handled here. `const` / `static`+`readonly` fields become `constant`
    /// nodes (value-reference targets), the rest stay fields.
    fn visit_node(&self, node: Node<'_>, s: &mut Session<'_>) -> bool {
        if node.kind() != "field_declaration" {
            return false;
        }
        let kind = if self.is_const(node, s.source()).unwrap_or(false) {
            NodeKind::Constant
        } else {
            NodeKind::Field
        };
        let docstring = get_preceding_docstring(node, s.source());
        let visibility = self.get_visibility(node, s.source());
        let is_static = self.is_static(node, s.source());

        let mut cursor = node.walk();
        let declarations: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "variable_declaration")
            .collect();
        for decl in declarations {
            let mut c2 = decl.walk();
            let declarators: Vec<Node<'_>> = decl
                .named_children(&mut c2)
                .filter(|c| c.kind() == "variable_declarator")
                .collect();
            for d in declarators {
                let Some(name_node) = get_child_by_field(d, "name").or_else(|| d.named_child(0))
                else {
                    continue;
                };
                let name = get_node_text(name_node, s.source()).to_string();
                let extra = NodeExtra {
                    docstring: docstring.clone(),
                    visibility,
                    is_static,
                    ..NodeExtra::default()
                };
                s.create_node(&CSharpRules, kind, &name, d, extra);
            }
        }
        true
    }

    /// #831: every record form parses as `record_declaration`; the
    /// value-type forms carry a `struct` keyword child.
    fn classify_class_node(&self, node: Node<'_>, _source: &str) -> Option<ClassKind> {
        if node.kind() == "record_declaration" {
            let mut i = 0;
            while let Some(child) = node.child(i) {
                if child.kind() == "struct" {
                    return Some(ClassKind::Struct);
                }
                i += 1;
            }
        }
        Some(ClassKind::Class)
    }

    /// C# defaults to private.
    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        let mut i = 0;
        while let Some(child) = node.child(i) {
            if child.kind() == "modifier" {
                match get_node_text(child, source) {
                    "public" => return Some(Visibility::Public),
                    "private" => return Some(Visibility::Private),
                    "protected" => return Some(Visibility::Protected),
                    "internal" => return Some(Visibility::Internal),
                    _ => {}
                }
            }
            i += 1;
        }
        Some(Visibility::Private)
    }

    fn is_static(&self, node: Node<'_>, source: &str) -> Option<bool> {
        Some(any_modifier(node, source, |t| t == "static"))
    }

    /// `const` and `static readonly` fields are C# constants (lookup tables,
    /// shared config) — drives the `constant` kind so value-reference edges
    /// target them; instance `readonly` / plain `static` fields stay fields.
    fn is_const(&self, node: Node<'_>, source: &str) -> Option<bool> {
        let mut has_static = false;
        let mut has_readonly = false;
        let mut i = 0;
        while let Some(child) = node.child(i) {
            if child.kind() == "modifier" {
                match get_node_text(child, source) {
                    "const" => return Some(true),
                    "static" => has_static = true,
                    "readonly" => has_readonly = true,
                    _ => {}
                }
            }
            i += 1;
        }
        Some(has_static && has_readonly)
    }

    fn is_async(&self, node: Node<'_>, source: &str) -> Option<bool> {
        Some(any_modifier(node, source, |t| t == "async"))
    }

    /// The declared return type normalized to the bare class a chained
    /// `Foo.Create().Bar()` could be called on (#645/#608): `predefined_type`
    /// (void/int/…) and arrays yield `None`, generics unwrap to the base,
    /// nullable `Foo?` strips, dotted namespaces reduce to the simple name.
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let type_node = get_child_by_field(node, "returns")?;
        if type_node.kind() == "predefined_type" || type_node.kind() == "array_type" {
            return None;
        }
        let t = get_node_text(type_node, source).trim();
        let t = t.trim_end_matches('?');
        let t = GENERIC_ARGS_RE.replace_all(t, "");
        let last = t.rsplit('.').next()?.trim();
        if last.is_empty()
            || !last
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            return None;
        }
        if !last.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(last.to_string())
    }

    /// Namespace name: the `name` field, else the first
    /// `qualified_name`/`identifier` child.
    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        let name = get_child_by_field(node, "name").or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| c.kind() == "qualified_name" || c.kind() == "identifier")
        })?;
        Some(get_node_text(name, source).to_string())
    }

    /// `using System;`, `using System.Collections.Generic;`, `using static X`,
    /// `using Alias = X` — the qualified name, else the first identifier.
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = get_node_text(node, source).trim().to_string();
        let mut cursor = node.walk();
        let target = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "qualified_name")
            .or_else(|| {
                let mut c2 = node.walk();
                node.named_children(&mut c2)
                    .find(|c| c.kind() == "identifier")
            })?;
        Some(ImportInfo {
            module_name: get_node_text(target, source).to_string(),
            signature,
            handled_refs: false,
        })
    }
}
