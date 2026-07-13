//! JavaScript rules (shared by JSX) — verbatim port of
//! `languages/javascript.ts` (99 LOC). Key divergence from the TS config:
//! JS `field_definition` names its key with the `property` field (TS's
//! `public_field_definition` uses `name`) — without `resolve_name`, JS
//! class fields extracted no name and produced no node at all (#808).

use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::typescript::{
    classify_ts_class_member, extract_es_import, has_child_token, is_exported_by_ancestor,
    resolve_field_body,
};
use crate::rules::{ImportInfo, LanguageRules, MethodClass, NodeTypeTables};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &[
        "function_declaration",
        "arrow_function",
        "function_expression",
    ],
    class_types: &["class_declaration"],
    method_types: &["method_definition", "field_definition"],
    import_types: &["import_statement"],
    call_types: &["call_expression"],
    variable_types: &["lexical_declaration", "variable_declaration"],
    name_field: "name",
    body_field: "body",
    params_field: "parameters",
    return_field: None,
    ..NodeTypeTables::EMPTY
};

pub(crate) struct JavascriptRules;

impl LanguageRules for JavascriptRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// JS `field_definition` → the `property` field is the name.
    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() == "field_definition" {
            let prop = get_child_by_field(node, "property")?;
            return Some(get_node_text(prop, source).to_string());
        }
        None
    }

    fn classify_method_node(&self, node: Node<'_>, _source: &str) -> Option<MethodClass> {
        Some(classify_ts_class_member(node))
    }

    fn resolve_body<'t>(&self, node: Node<'t>, body_field: &str) -> Option<Node<'t>> {
        resolve_field_body(node, body_field, "field_definition")
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        Some(get_node_text(params, source).to_string())
    }

    fn is_exported(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        Some(is_exported_by_ancestor(node))
    }

    fn is_async(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        Some(has_child_token(node, "async"))
    }

    fn is_const(&self, node: Node<'_>, _source: &str) -> Option<bool> {
        Some(node.kind() == "lexical_declaration" && has_child_token(node, "const"))
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        extract_es_import(node, source)
    }
}
