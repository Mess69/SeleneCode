//! C rules — verbatim port of `c-cpp.ts`'s `cExtractor` (map §11 C row):
//! structs/enums/typedef reclassification, `type_qualifier`-based const
//! detection, `#include` imports, content-gated CUDA blanking (llm.c keeps
//! `__device__` helpers in plain `.h`), and the universal macro-mangled
//! name recovery net shared with C++.

use selene_core::NodeKind;
use tree_sitter::Node;

use crate::helpers::{get_child_by_field, get_node_text};
use crate::rules::cpp_preparse::{
    blank_cuda_constructs, looks_like_cuda_source, normalize_cpp_return_type,
    recover_mangled_cpp_name,
};
use crate::rules::{ImportInfo, LanguageRules, NodeTypeTables};

static TABLES: NodeTypeTables = NodeTypeTables {
    function_types: &["function_definition"],
    struct_types: &["struct_specifier"],
    enum_types: &["enum_specifier"],
    enum_member_types: &["enumerator"],
    type_alias_types: &["type_definition"], // typedef
    import_types: &["preproc_include"],
    call_types: &["call_expression"],
    variable_types: &["declaration"],
    name_field: "declarator",
    body_field: "body",
    params_field: "parameters",
    return_field: None,
    ..NodeTypeTables::EMPTY
};

/// Shared C/C++ `#include` extraction: `<stdio.h>` (system_lib_string,
/// angle brackets stripped) or `"myheader.h"` (string_literal →
/// string_content).
pub(super) fn extract_c_include(node: Node<'_>, source: &str) -> Option<ImportInfo> {
    let signature = get_node_text(node, source).trim().to_string();
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in &children {
        if child.kind() == "system_lib_string" {
            let module = get_node_text(*child, source)
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            return Some(ImportInfo {
                module_name: module,
                signature,
                handled_refs: false,
            });
        }
    }
    for child in children {
        if child.kind() == "string_literal" {
            let mut c2 = child.walk();
            let content = child
                .named_children(&mut c2)
                .find(|c| c.kind() == "string_content")?;
            return Some(ImportInfo {
                module_name: get_node_text(content, source).to_string(),
                signature,
                handled_refs: false,
            });
        }
    }
    None
}

/// Shared C/C++ typedef reclassification: `typedef enum { … } name;` /
/// `typedef struct { … } name;` — the inner specifier is anonymous; the
/// typedef NAME becomes the enum/struct node.
pub(super) fn resolve_c_typedef_kind(node: Node<'_>) -> Option<NodeKind> {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    for child in children {
        if child.kind() == "enum_specifier" && get_child_by_field(child, "body").is_some() {
            return Some(NodeKind::Enum);
        }
        if child.kind() == "struct_specifier" && get_child_by_field(child, "body").is_some() {
            return Some(NodeKind::Struct);
        }
    }
    None
}

pub(crate) struct CRules;

impl LanguageRules for CRules {
    fn tables(&self) -> &'static NodeTypeTables {
        &TABLES
    }

    /// C-detected headers in CUDA projects: content-gated CUDA blank only
    /// (no macro-blanker chain — C's `struct TAG var;` idiom must never
    /// reach the C++ blankers).
    fn pre_parse(&self, source: &str, _file_path: &str) -> Option<String> {
        if !looks_like_cuda_source(source) {
            return None;
        }
        match blank_cuda_constructs(source) {
            std::borrow::Cow::Borrowed(_) => None,
            std::borrow::Cow::Owned(s) => Some(s),
        }
    }

    fn recover_mangled_name(&self, name: String) -> String {
        recover_mangled_cpp_name(name)
    }

    /// `const`/`static const` file-scope declarations carry a
    /// `type_qualifier` child reading `const` → `constant` kind.
    fn is_const(&self, node: Node<'_>, source: &str) -> Option<bool> {
        let mut cursor = node.walk();
        Some(
            node.named_children(&mut cursor)
                .any(|c| c.kind() == "type_qualifier" && get_node_text(c, source) == "const"),
        )
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let type_node = get_child_by_field(node, "type")?;
        normalize_cpp_return_type(get_node_text(type_node, source))
    }

    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        resolve_c_typedef_kind(node)
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        extract_c_include(node, source)
    }
}
