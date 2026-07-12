//! TypeScript rules (shared by TSX) — verbatim port of
//! `languages/typescript.ts` (map §11 typescript row). The TS-specific core
//! machinery (HOC components, store collections, re-export/import-binding
//! refs, type-annotation refs) lives in the walker (Tasks 7/8), not here.

use selene_core::Visibility;
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::{ImportInfo, LanguageRules, MethodClass, NodeTypeTables};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &[
        "function_declaration",
        "arrow_function",
        "function_expression",
    ],
    class_types: &["class_declaration", "abstract_class_declaration"],
    method_types: &["method_definition", "public_field_definition"],
    interface_types: &["interface_declaration"],
    enum_types: &["enum_declaration"],
    enum_member_types: &["property_identifier", "enum_assignment"],
    type_alias_types: &["type_alias_declaration"],
    import_types: &["import_statement"],
    call_types: &["call_expression"],
    variable_types: &["lexical_declaration", "variable_declaration"],
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: Some("return_type"),
    ..NodeTypeTables::EMPTY
};

/// #808: a TS/JS class field (`public_field_definition` / `field_definition`)
/// is a METHOD only when its value is callable — an arrow function, a
/// function expression, or a HOF call wrapping one
/// (`onScroll = throttle(() => {…})`). Everything else is a PROPERTY.
/// Shared by the typescript and javascript rules.
pub(crate) fn classify_ts_class_member(node: Node<'_>) -> MethodClass {
    if node.kind() != "public_field_definition" && node.kind() != "field_definition" {
        return MethodClass::Method; // method_definition, getters/setters
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        if child.kind() == "arrow_function" || child.kind() == "function_expression" {
            return MethodClass::Method;
        }
        if child.kind() == "call_expression"
            && let Some(args) = get_child_by_field(child, "arguments")
        {
            let mut c2 = args.walk();
            if args
                .named_children(&mut c2)
                .any(|a| a.kind() == "arrow_function" || a.kind() == "function_expression")
            {
                return MethodClass::Method;
            }
        }
    }
    MethodClass::Property
}

/// Shared TS/JS `resolveBody`: a field definition nests its body inside an
/// arrow/function-expression child, possibly wrapped in a HOF call's
/// arguments (`field = throttle((e) => {…})`).
pub(crate) fn resolve_field_body<'t>(
    node: Node<'t>,
    body_field: &str,
    field_kind: &str,
) -> Option<Node<'t>> {
    if node.kind() != field_kind {
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
    for child in children {
        if child.kind() == "arrow_function" || child.kind() == "function_expression" {
            return get_child_by_field(child, body_field);
        }
        if child.kind() == "call_expression"
            && let Some(args) = get_child_by_field(child, "arguments")
        {
            let mut c2 = args.walk();
            let arg = args
                .named_children(&mut c2)
                .find(|a| a.kind() == "arrow_function" || a.kind() == "function_expression");
            if let Some(arg) = arg {
                return get_child_by_field(arg, body_field);
            }
        }
    }
    None
}

/// `isExported`: walk the ancestor chain for an `export_statement` — handles
/// deeply nested nodes (`export const X = () => {…}` puts the arrow 3
/// levels under the export). Shared by TS and JS.
pub(crate) fn is_exported_by_ancestor(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(p) = current {
        if p.kind() == "export_statement" {
            return true;
        }
        current = p.parent();
    }
    false
}

/// Any (unnamed included) child of `kind`? (`async`/`static`/`const`
/// keyword tokens are anonymous children.)
pub(crate) fn has_child_token(node: Node<'_>, kind: &str) -> bool {
    for i in 0..u32::try_from(node.child_count()).unwrap_or(0) {
        if node.child(i).is_some_and(|c| c.kind() == kind) {
            return true;
        }
    }
    false
}

/// TS/JS `extractImport`: module = the `source` field, quotes stripped.
pub(crate) fn extract_es_import(node: Node<'_>, source: &str) -> Option<ImportInfo> {
    let source_field = get_child_by_field(node, "source")?;
    let module_name: String = get_node_text(source_field, source)
        .chars()
        .filter(|c| *c != '\'' && *c != '"')
        .collect();
    if module_name.is_empty() {
        return None;
    }
    Some(ImportInfo {
        module_name,
        signature: get_node_text(node, source).trim().to_string(),
        handled_refs: false,
    })
}

pub(crate) struct TypescriptRules;

impl LanguageRules for TypescriptRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    fn classify_method_node(&self, node: Node<'_>, _source: &str) -> Option<MethodClass> {
        Some(classify_ts_class_member(node))
    }

    fn resolve_body<'t>(&self, node: Node<'t>, body_field: &str) -> Option<Node<'t>> {
        resolve_field_body(node, body_field, "public_field_definition")
    }

    /// `(params): ReturnType` (return annotation's leading `: ` normalized).
    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let mut sig = get_node_text(params, source).to_string();
        if let Some(ret) = get_child_by_field(node, "return_type") {
            let ret_text = get_node_text(ret, source);
            let ret_text = ret_text.trim_start_matches(':').trim_start();
            sig.push_str(": ");
            sig.push_str(ret_text);
        }
        Some(sig)
    }

    fn get_visibility(&self, node: Node<'_>, source: &str) -> Option<Visibility> {
        for i in 0..u32::try_from(node.child_count()).unwrap_or(0) {
            if let Some(c) = node.child(i)
                && c.kind() == "accessibility_modifier"
            {
                return match get_node_text(c, source) {
                    "public" => Some(Visibility::Public),
                    "private" => Some(Visibility::Private),
                    "protected" => Some(Visibility::Protected),
                    _ => None,
                };
            }
        }
        None
    }

    fn is_exported(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        Some(is_exported_by_ancestor(node))
    }

    fn is_async(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        Some(has_child_token(node, "async"))
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        Some(has_child_token(node, "static"))
    }

    /// `true` only for a `lexical_declaration` with a `const` token —
    /// `variable_declaration` (`var`) is never const.
    fn is_const(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        Some(node.kind() == "lexical_declaration" && has_child_token(node, "const"))
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        extract_es_import(node, source)
    }
}
